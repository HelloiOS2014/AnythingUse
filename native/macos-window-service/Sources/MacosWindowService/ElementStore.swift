import ApplicationServices
import CoreGraphics
import Foundation

/// Cached element paths for a single observation (pid+window_id).
/// Used so semantic actions can re-resolve element_id without re-walking blindly.
final class ElementStore {
    struct Entry {
        var pathIndices: [Int]
        var role: String
    }

    private var pid: pid_t = 0
    private var windowID: CGWindowID = 0
    private var entries: [String: Entry] = [:]
    private var windowFrame: CGRect = .zero

    func bind(pid: pid_t, windowID: CGWindowID, frame: CGRect) {
        self.pid = pid
        self.windowID = windowID
        self.windowFrame = frame
        self.entries.removeAll(keepingCapacity: true)
    }

    func put(id: String, pathIndices: [Int], role: String) {
        entries[id] = Entry(pathIndices: pathIndices, role: role)
    }

    func matches(pid: pid_t, windowID: CGWindowID) -> Bool {
        self.pid == pid && self.windowID == windowID
    }

    func path(for elementId: String) throws -> [Int] {
        guard let e = entries[elementId] else {
            throw ServiceError.invalidRequest("element_id \(elementId) not in last observation")
        }
        return e.pathIndices
    }

    var frame: CGRect { windowFrame }

    /// Walk AX tree under window root, assign e1..eN, return JSON-ready nodes.
    func observeElements(
        target: MacWindowTarget,
        maxNodes: Int = 250
    ) throws -> [[String: Any]] {
        bind(pid: target.pid, windowID: target.windowID, frame: target.bounds)
        let root = try AXBridge.axWindow(for: target)
        var nodes: [[String: Any]] = []
        var counter = 0
        walk(
            element: root,
            path: [],
            depth: 0,
            maxNodes: maxNodes,
            windowFrame: target.bounds,
            nodes: &nodes,
            counter: &counter
        )
        return nodes
    }

    func resolveElement(target: MacWindowTarget, elementId: String) throws -> AXUIElement {
        guard matches(pid: target.pid, windowID: target.windowID) else {
            throw ServiceError.invalidRequest(
                "element cache bound to a different window; re-observe before acting"
            )
        }
        let indices = try path(for: elementId)
        let root = try AXBridge.axWindow(for: target)
        return try follow(root: root, indices: indices)
    }

    private func walk(
        element: AXUIElement,
        path: [Int],
        depth: Int,
        maxNodes: Int,
        windowFrame: CGRect,
        nodes: inout [[String: Any]],
        counter: inout Int
    ) {
        if nodes.count >= maxNodes || depth > 14 { return }

        let role = copyString(element, kAXRoleAttribute as CFString) ?? ""
        // Skip pure structural noise at depth 0 sometimes; keep most nodes.
        let title = copyString(element, kAXTitleAttribute as CFString)
        let value = copyString(element, kAXValueAttribute as CFString)
        let identifier = copyString(element, kAXIdentifierAttribute as CFString)
        let label = firstNonEmpty([title, identifier, value.map { String($0.prefix(80)) }])

        let frame = normalizedFrame(element, window: windowFrame)
        var actions: [String] = []
        if isPressable(role) { actions.append("AXPress") }
        if isEditable(role) { actions.append("AXSetValue") }
        if role.contains("Scroll") { actions.append("AXScroll") }

        counter += 1
        let id = "e\(counter)"
        put(id: id, pathIndices: path, role: role)
        var node: [String: Any] = [
            "id": id,
            "role": role,
            "frame": [
                "x": frame.x,
                "y": frame.y,
                "width": frame.width,
                "height": frame.height
            ],
            "actions": actions
        ]
        if let label, !label.isEmpty { node["label"] = label }
        if let value, !value.isEmpty {
            node["value"] = String(value.prefix(500))
        }
        nodes.append(node)

        if depth >= 12 { return }
        var childrenRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            element,
            kAXChildrenAttribute as CFString,
            &childrenRef
        ) == .success,
            let children = childrenRef as? [AXUIElement]
        else { return }

        for (i, child) in children.enumerated() {
            if nodes.count >= maxNodes { break }
            var childPath = path
            childPath.append(i)
            walk(
                element: child,
                path: childPath,
                depth: depth + 1,
                maxNodes: maxNodes,
                windowFrame: windowFrame,
                nodes: &nodes,
                counter: &counter
            )
        }
    }

    private func follow(root: AXUIElement, indices: [Int]) throws -> AXUIElement {
        var current = root
        for idx in indices {
            var childrenRef: CFTypeRef?
            guard AXUIElementCopyAttributeValue(
                current,
                kAXChildrenAttribute as CFString,
                &childrenRef
            ) == .success,
                let children = childrenRef as? [AXUIElement],
                idx < children.count
            else {
                throw ServiceError.notFound("AX path broken at index \(idx)")
            }
            current = children[idx]
        }
        return current
    }

    private func normalizedFrame(_ el: AXUIElement, window: CGRect) -> (
        x: Double, y: Double, width: Double, height: Double
    ) {
        var posRef: CFTypeRef?
        var sizeRef: CFTypeRef?
        var point = CGPoint.zero
        var size = CGSize.zero
        if AXUIElementCopyAttributeValue(el, kAXPositionAttribute as CFString, &posRef) == .success,
           let pos = posRef
        {
            AXValueGetValue(pos as! AXValue, .cgPoint, &point)
        }
        if AXUIElementCopyAttributeValue(el, kAXSizeAttribute as CFString, &sizeRef) == .success,
           let sz = sizeRef
        {
            AXValueGetValue(sz as! AXValue, .cgSize, &size)
        }
        let w = max(window.width, 1)
        let h = max(window.height, 1)
        return (
            x: max(0, min(1, (point.x - window.minX) / w)),
            y: max(0, min(1, (point.y - window.minY) / h)),
            width: max(0, min(1, size.width / w)),
            height: max(0, min(1, size.height / h))
        )
    }

    private func isPressable(_ role: String) -> Bool {
        ["AXButton", "AXCheckBox", "AXRadioButton", "AXPopUpButton", "AXLink", "AXMenuItem"]
            .contains(where: { role.contains($0) || role == $0 })
    }

    private func isEditable(_ role: String) -> Bool {
        role.contains("Text") || role.contains("Field") || role == "AXComboBox" || role == "AXWebArea"
    }

    private func firstNonEmpty(_ opts: [String?]) -> String? {
        for o in opts {
            if let s = o, !s.isEmpty { return s }
        }
        return nil
    }

    private func copyString(_ el: AXUIElement, _ attr: CFString) -> String? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(el, attr, &ref) == .success, let ref else { return nil }
        return ref as? String
    }
}
