import ApplicationServices
import CoreGraphics
import Foundation

/// Cached element paths for a single observation (pid+window_id).
/// Used so semantic actions can re-resolve element_id without re-walking blindly.
final class ElementStore {
    struct Entry {
        var element: AXUIElement
        var role: String
        var actions: [String]
        var frame: CGRect
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

    func put(
        id: String,
        element: AXUIElement,
        role: String,
        actions: [String],
        frame: CGRect
    ) {
        entries[id] = Entry(
            element: element,
            role: role,
            actions: actions,
            frame: frame
        )
    }

    func matches(pid: pid_t, windowID: CGWindowID) -> Bool {
        self.pid == pid && self.windowID == windowID
    }

    func metadata(for elementId: String) throws -> (
        role: String,
        actions: [String],
        frame: CGRect
    ) {
        guard let entry = entries[elementId] else {
            throw ServiceError.invalidRequest("element_id \(elementId) not in last observation")
        }
        return (entry.role, entry.actions, entry.frame)
    }

    var frame: CGRect { windowFrame }

    /// Walk AX tree under window root, assign e1..eN, return JSON-ready nodes.
    func observeElements(
        target: MacWindowTarget,
        maxNodes: Int = 750
    ) throws -> [[String: Any]] {
        bind(pid: target.pid, windowID: target.windowID, frame: target.bounds)
        // ponytail: if AX cannot prove this exact window, return no semantic tree.
        // Screenshot-directed postToPid input is safer than acting on another window.
        let root = try AXBridge.axWindow(for: target)
        var nodes: [[String: Any]] = []
        var counter = 0
        walk(
            element: root,
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
        guard let entry = entries[elementId] else {
            throw ServiceError.invalidRequest("element_id \(elementId) not in last observation")
        }
        guard AXBridge.elementBelongsToTargetWindow(entry.element, target: target) else {
            throw ServiceError.notFound("element_id \(elementId) is stale; re-observe before acting")
        }
        return entry.element
    }

    private func walk(
        element: AXUIElement,
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
        let description = copyString(element, kAXDescriptionAttribute as CFString)
        let placeholder = copyString(element, "AXPlaceholderValue" as CFString)
        let label = firstNonEmpty([
            title,
            identifier,
            description,
            placeholder,
            value.map { String($0.prefix(80)) }
        ])

        let frame = normalizedFrame(element, window: windowFrame)
        var actions: [String] = []
        var actionNames: CFArray?
        if AXUIElementCopyActionNames(element, &actionNames) == .success,
           let names = actionNames as? [String]
        {
            actions.append(contentsOf: names)
        }
        if isPressable(role) { actions.append("AXPress") }
        if isEditable(role) { actions.append("AXSetValue") }
        if role.contains("Scroll") { actions.append("AXScroll") }
        actions = Array(Set(actions)).sorted()

        counter += 1
        let id = "e\(counter)"
        put(
            id: id,
            element: element,
            role: role,
            actions: actions,
            frame: CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height)
        )
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
        for child in children(of: element) {
            if nodes.count >= maxNodes { break }
            walk(
                element: child,
                depth: depth + 1,
                maxNodes: maxNodes,
                windowFrame: windowFrame,
                nodes: &nodes,
                counter: &counter
            )
        }
    }

    private func children(of element: AXUIElement) -> [AXUIElement] {
        // Chromium/CEF trees may expose only visible children until an
        // accessibility client walks them. Prefer the canonical collection,
        // then use the same fallback as native Computer Use implementations.
        for attribute in [kAXChildrenAttribute as String, "AXVisibleChildren"] {
            var childrenRef: CFTypeRef?
            if AXUIElementCopyAttributeValue(
                element,
                attribute as CFString,
                &childrenRef
            ) == .success,
                let children = childrenRef as? [AXUIElement],
                !children.isEmpty
            {
                return children
            }
        }
        return []
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
