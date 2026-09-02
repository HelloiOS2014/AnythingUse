import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

// macOS Accessibility SPI used by native Computer Use implementations to route
// keyboard events into an app's out-of-process renderer instead of its shell PID.
@_silgen_name("_AXUIElementGetActualPid")
private func lcuAXUIElementGetActualPid(
    _ element: AXUIElement,
    _ pid: UnsafeMutablePointer<pid_t>
) -> AXError

@_silgen_name("_AXUIElementGetWindow")
private func lcuAXUIElementGetWindow(
    _ element: AXUIElement,
    _ windowID: UnsafeMutablePointer<CGWindowID>
) -> AXError

/// Accessibility tree read + semantic actions.
/// Never activates the application or raises it to system frontmost.
enum AXBridge {
    /// Holds per-process AXManualAccessibility/AXEnhancedUserInterface
    /// enablement for the duration of one service request. Never touches
    /// global preferences or other processes.
    final class AccessibilityEnablementAssertion {
        fileprivate init(pid: pid_t, allowFallback: Bool) {
            let app = AXBridge.application(pid: pid)
            var attributeEnabled = false
            for attribute in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
                if AXUIElementSetAttributeValue(
                    app,
                    attribute as CFString,
                    kCFBooleanTrue
                ) == .success {
                    attributeEnabled = true
                }
            }
            if allowFallback && attributeEnabled {
                // Renderer-backed apps may accept AX enablement before their tree is ready.
                // This path runs only after the first observation returned no UI.
                usleep(500_000)
            }
        }

        /// No-op: retained as a lifetime token at call sites.
        func disable() {}
    }

    struct ElementInfo {
        var role: String
        var subrole: String
        var title: String
        var value: String
        var identifier: String
        var element: AXUIElement
        var depth: Int
    }

    static func application(pid: pid_t) -> AXUIElement {
        AXUIElementCreateApplication(pid)
    }

    /// Raise only the AX window already proven to be the requested CGWindowID.
    static func raiseExactWindow(_ target: MacWindowTarget) -> Bool {
        guard let window = try? axWindow(for: target) else { return false }
        return AXUIElementPerformAction(window, kAXRaiseAction as CFString) == .success
    }

    /// Renderer-backed apps may keep their full AX tree disabled until an
    /// accessibility client opts in. Keep the returned assertion alive while
    /// reading or acting on the target UI.
    static func enableAccessibility(
        pid: pid_t,
        allowFallback: Bool = false
    ) -> AccessibilityEnablementAssertion {
        AccessibilityEnablementAssertion(pid: pid, allowFallback: allowFallback)
    }

    static func keyboardEventPID(applicationPID: pid_t) -> pid_t {
        if let focused = focusedElement(pid: applicationPID) {
            var actualPID: pid_t = applicationPID
            if lcuAXUIElementGetActualPid(focused, &actualPID) == .success,
               actualPID > 0
            {
                return actualPID
            }
            if AXUIElementGetPid(focused, &actualPID) == .success, actualPID > 0 {
                return actualPID
            }
        }

        return applicationPID
    }

    /// Prove that keyboard input is still bound to the exact editable selected
    /// by the preceding action. Process/window routing alone is not enough: a
    /// stale first responder in the same window can receive the text.
    static func focusedEditableMatches(
        _ expected: AXUIElement,
        target: MacWindowTarget
    ) -> Bool {
        guard elementBelongsToTargetWindow(expected, target: target),
              let focused = focusedElement(pid: target.pid),
              elementBelongsToTargetWindow(focused, target: target),
              let editable = editableAtOrAbove(focused)
        else { return false }
        return isSameElementLineage(editable, expected)
    }

    static func waitForFocusedEditable(
        _ expected: AXUIElement,
        target: MacWindowTarget
    ) -> Bool {
        for _ in 0..<5 {
            if focusedEditableMatches(expected, target: target) { return true }
            usleep(40_000)
        }
        return false
    }

    /// Prove the process's focused/main AX window is the target window.
    /// Independent of system frontmost: background apps keep an in-process key
    /// window that CGEvent.postToPid keyboard events would route into.
    ///
    /// Never *set* focused/main window attributes on a background app: macOS
    /// promotes the app to frontmost when a background app's key window is
    /// reassigned (same effect as kAXFocusedUIElement on TextEdit), which is a
    /// user-visible focus steal. Keyboard delivery therefore only ever proves,
    /// and relies on the app switching its key window as a side effect of a
    /// directed click (mouse path) — never on our writes.
    static func proofOfProcessKeyWindow(target: MacWindowTarget) -> Bool {
        if let key = FocusGuard.focusedWindowNumber(pid: target.pid) {
            return key == target.windowID
        }
        return FocusGuard.focusedWindowMatches(target: target)
    }

    /// Match CG window id to an AX window element — **fail closed**.
    ///
    /// Identity proof (never "first/main/focused/title" alone):
    /// 1) `_AXUIElementGetWindow == target.windowID`
    /// 2) else `AXWindowNumber == target.windowID` when the app exposes it
    /// 3) else AX window **frame ≈ target.bounds**, only when that frame match
    ///    is **unique** under the same PID (apps like TextEdit omit AXWindowNumber).
    /// When AXWindowNumber exists but differs: immediate mismatch — never frame fallback.
    static func axWindow(for target: MacWindowTarget) throws -> AXUIElement {
        let app = application(pid: target.pid)
        _ = AXUIElementSetMessagingTimeout(app, 2.0)

        var windowsRef: CFTypeRef?
        let err = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windowsRef)
        let windows = (err == .success ? windowsRef as? [AXUIElement] : nil) ?? []
        if !windows.isEmpty {
            // Prefer authoritative exact window-id hits.
            for w in windows {
                if windowIDEquals(w, target.windowID) {
                    return w
                }
            }
            // Frame fallback only when no number on candidates and unique geometry match.
            let frameHits = windows.filter { windowFrameMatches($0, target: target) }
            if frameHits.count == 1 {
                return frameHits[0]
            }
            if frameHits.count > 1 {
                throw ServiceError.unsupported(
                    "AX window frame match not unique for pid=\(target.pid) windowID=\(target.windowID) "
                        + "(hits=\(frameHits.count)); refuse ambiguous same-app windows"
                )
            }
        }

        // Some apps expose the requested background window only through their
        // main/focused AX attributes, not AXWindows. Accept geometry only when
        // it identifies exactly one same-process CG window.
        let sameProcessWindows = WindowResolver.listOnScreenWindows(minSize: 1)
            .filter { $0.pid == target.pid }
        let frameUniquelyIdentifiesTarget = sameProcessWindows
            .filter { framesRoughlyEqual($0.bounds, target.bounds) }
            .map(\.windowID) == [target.windowID]
        let sizeUniquelyIdentifiesTarget = sameProcessWindows
            .filter { sizesRoughlyEqual($0.bounds, target.bounds) }
            .map(\.windowID) == [target.windowID]

        var attributedWindows: [(String, AXUIElement)] = []
        for attr in [kAXMainWindowAttribute as String, kAXFocusedWindowAttribute as String] {
            var ref: CFTypeRef?
            if AXUIElementCopyAttributeValue(app, attr as CFString, &ref) == .success,
               let el = ref
            {
                let element = el as! AXUIElement
                attributedWindows.append((attr, element))
            }
        }
        if let focused = focusedElement(pid: target.pid),
           let window = climbToWindow(from: focused)
        {
            attributedWindows.append(("AXFocusedUIElement ancestor", window))
        }
        for (_, element) in attributedWindows {
            let role = copyString(element, kAXRoleAttribute as CFString) ?? ""
            guard role == (kAXWindowRole as String) || role == "AXWindow" else { continue }
            if windowIDEquals(element, target.windowID)
                || (frameUniquelyIdentifiesTarget && windowFrameMatches(element, target: target))
                || (sizeUniquelyIdentifiesTarget && windowSizeMatches(element, target: target))
            {
                return element
            }
        }

        // Hit-test at CG window center. Background Finder often exposes only the
        // desktop AX window (full display frame) while CGWindowID is the folder
        // window; identity on the climbed AX window then fails. If CoreGraphics
        // z-order at that point is still the target window, accept a unique
        // descendant whose AX frame matches the CG window.
        let center = CGPoint(x: target.bounds.midX, y: target.bounds.midY)
        let cgPointIsTarget = WindowResolver.windowAtScreenPoint(center)
            .map { $0.0 == target.pid && $0.1 == target.windowID } ?? false
        // App-restricted hit-test misses background Finder folder windows that
        // are absent from AXWindows; system-wide hit-test at a CG-proven point
        // on a secondary display still lands in that folder UI.
        let hit = elementAtScreenPoint(center, expectedPID: target.pid)
            ?? elementAtScreenPoint(center, expectedPID: nil).flatMap { el -> AXUIElement? in
                var pid: pid_t = 0
                guard AXUIElementGetPid(el, &pid) == .success, pid == target.pid else {
                    return nil
                }
                return el
            }
        if let hit,
           let window = climbToWindow(from: hit)
        {
            if windowIDEquals(window, target.windowID)
                || (frameUniquelyIdentifiesTarget && windowFrameMatches(window, target: target))
            {
                return window
            }
            if cgPointIsTarget,
               let framed = uniqueFrameMatch(from: window, target: target, maxNodes: 200)
                ?? uniqueSizeMatch(from: window, target: target, maxNodes: 200)
            {
                return framed
            }
        }
        if cgPointIsTarget,
           let framed = uniqueFrameMatch(from: app, target: target, maxNodes: 250)
            ?? uniqueSizeMatch(from: app, target: target, maxNodes: 250)
        {
            return framed
        }

        // Walk application children for a window matching identity.
        var walkHits: [AXUIElement] = []
        for node in walk(from: app, maxNodes: 80) {
            if node.role == (kAXWindowRole as String) || node.role == "AXWindow",
               windowIdentityMatches(node.element, target: target)
            {
                walkHits.append(node.element)
            }
        }
        if walkHits.count == 1 {
            return walkHits[0]
        }
        if walkHits.count > 1 {
            throw ServiceError.unsupported(
                "AX window identity not unique for pid=\(target.pid) windowID=\(target.windowID) "
                    + "(hits=\(walkHits.count))"
            )
        }

        let roundedFrame: (CGRect?) -> String = { frame in
            guard let frame else { return "-" }
            return "\(Int(frame.minX.rounded())),\(Int(frame.minY.rounded())),"
                + "\(Int(frame.width.rounded())),\(Int(frame.height.rounded()))"
        }
        let candidateSummary = windows.map { window in
            let exact = exactWindowID(window).map(String.init) ?? "-"
            let attribute = copyInt(window, kAXWindowNumberAttribute).map(String.init) ?? "-"
            return "\(exact)/\(attribute)/\(roundedFrame(copyFrame(window)))"
        }.joined(separator: ";")
        let attributedSummary = attributedWindows.map { name, window in
            let exact = exactWindowID(window).map(String.init) ?? "-"
            let attribute = copyInt(window, kAXWindowNumberAttribute).map(String.init) ?? "-"
            return "\(name)=\(exact)/\(attribute)/\(roundedFrame(copyFrame(window)))"
        }.joined(separator: ";")
        throw ServiceError.notFound(
            "strict AX window match failed pid=\(target.pid) wid=\(target.windowID) "
                + "axerr=\(err.rawValue) n=\(windows.count) "
                + "target=\(roundedFrame(target.bounds)) candidates(e/a/f)=\(candidateSummary) "
                + "attributed(e/a/f)=\(attributedSummary)"
        )
    }

    /// True when AX element is the target CG window (number or frame).
    ///
    /// If an exact window id is present it is authoritative: a mismatch never falls
    /// through to frame comparison (same-position windows must not alias).
    static func windowIdentityMatches(_ el: AXUIElement, target: MacWindowTarget) -> Bool {
        if let windowID = exactWindowID(el) {
            return windowID == target.windowID
        }
        if let cgid = copyInt(el, kAXWindowNumberAttribute) {
            return CGWindowID(cgid) == target.windowID
        }
        // Secondary: apps that omit AXWindowNumber (e.g. TextEdit).
        return windowFrameMatches(el, target: target)
    }

    /// Exact SPI window id, or exposed `AXWindowNumber`, equal to target.
    private static func windowIDEquals(_ el: AXUIElement, _ windowID: CGWindowID) -> Bool {
        if let exact = exactWindowID(el) {
            return exact == windowID
        }
        guard let cgid = copyInt(el, kAXWindowNumberAttribute) else { return false }
        return CGWindowID(cgid) == windowID
    }

    /// Unique descendant (or self) whose AX frame matches the CG window.
    /// Used when Finder lists only the desktop AX window for a background folder.
    private static func uniqueFrameMatch(
        from root: AXUIElement,
        target: MacWindowTarget,
        maxNodes: Int
    ) -> AXUIElement? {
        var hits: [AXUIElement] = []
        if let frame = copyFrame(root), framesRoughlyEqual(frame, target.bounds) {
            hits.append(root)
        }
        for node in walk(from: root, maxNodes: maxNodes) {
            if let frame = copyFrame(node.element), framesRoughlyEqual(frame, target.bounds) {
                hits.append(node.element)
            }
        }
        if hits.count == 1 {
            return hits[0]
        }
        let windows = hits.filter {
            let role = copyString($0, kAXRoleAttribute as CFString) ?? ""
            return role == (kAXWindowRole as String) || role == "AXWindow"
        }
        return windows.count == 1 ? windows[0] : nil
    }

    /// Same-size unique descendant. AX origin may not match CGWindowBounds on
    /// a secondary display even when the folder window is in the tree.
    private static func uniqueSizeMatch(
        from root: AXUIElement,
        target: MacWindowTarget,
        maxNodes: Int
    ) -> AXUIElement? {
        var hits: [AXUIElement] = []
        if let frame = copyFrame(root),
           abs(frame.width - target.bounds.width) <= 8,
           abs(frame.height - target.bounds.height) <= 8
        {
            hits.append(root)
        }
        for node in walk(from: root, maxNodes: maxNodes) {
            if let frame = copyFrame(node.element),
               abs(frame.width - target.bounds.width) <= 8,
               abs(frame.height - target.bounds.height) <= 8
            {
                hits.append(node.element)
            }
        }
        if hits.count == 1 {
            return hits[0]
        }
        let windows = hits.filter {
            let role = copyString($0, kAXRoleAttribute as CFString) ?? ""
            return role == (kAXWindowRole as String) || role == "AXWindow"
        }
        return windows.count == 1 ? windows[0] : nil
    }

    /// Frame-only identity, valid only when exact ids are unavailable.
    private static func windowFrameMatches(_ el: AXUIElement, target: MacWindowTarget) -> Bool {
        if exactWindowID(el) != nil || copyInt(el, kAXWindowNumberAttribute) != nil {
            return false
        }
        guard let frame = copyFrame(el), target.bounds.width > 1, target.bounds.height > 1 else {
            return false
        }
        return framesRoughlyEqual(frame, target.bounds)
    }

    /// Size-only identity is allowed only when the caller has already proved
    /// that exactly one same-process CG window has this size. Finder can report
    /// a different AX origin for a background window on another display.
    private static func windowSizeMatches(_ el: AXUIElement, target: MacWindowTarget) -> Bool {
        if exactWindowID(el) != nil || copyInt(el, kAXWindowNumberAttribute) != nil {
            return false
        }
        guard let frame = copyFrame(el), target.bounds.width > 1, target.bounds.height > 1 else {
            return false
        }
        return sizesRoughlyEqual(frame, target.bounds)
    }

    private static func sizesRoughlyEqual(_ lhs: CGRect, _ rhs: CGRect) -> Bool {
        abs(lhs.width - rhs.width) <= 8 && abs(lhs.height - rhs.height) <= 8
    }

    /// True when `element` lives under the target window (climb + identity).
    static func elementBelongsToTargetWindow(_ element: AXUIElement, target: MacWindowTarget) -> Bool {
        if windowIdentityMatches(element, target: target) { return true }
        guard let window = climbToWindow(from: element) else { return false }
        return windowIdentityMatches(window, target: target)
    }

    /// Frame of an AX element (window), if available.
    static func axFrame(_ el: AXUIElement) -> CGRect? {
        copyFrame(el)
    }

    /// System-wide element at a global top-left-origin screen point.
    static func elementAtScreenPoint(_ point: CGPoint, expectedPID: pid_t? = nil) -> AXUIElement? {
        // Restrict hit-testing to the requested application when its pid is known.
        // The system-wide root follows z-order and would return the user's frontmost
        // window when it overlaps a background target at the same screen point.
        let root = expectedPID.map(application(pid:)) ?? AXUIElementCreateSystemWide()
        var ref: AXUIElement?
        let err = AXUIElementCopyElementAtPosition(root, Float(point.x), Float(point.y), &ref)
        guard err == .success, let el = ref else { return nil }
        if let expectedPID {
            var pid: pid_t = 0
            guard AXUIElementGetPid(el, &pid) == .success, pid == expectedPID else {
                return nil
            }
        }
        return el
    }

    static func editableAtOrAbove(_ element: AXUIElement) -> AXUIElement? {
        var current: AXUIElement? = element
        for _ in 0..<12 {
            guard let el = current else { return nil }
            let role = copyString(el, kAXRoleAttribute as CFString) ?? ""
            if role.contains("Text") || role.contains("Field") || role == "AXComboBox" {
                return el
            }
            if role == (kAXWindowRole as String) || role == "AXWindow" {
                return nil
            }
            var parentRef: CFTypeRef?
            guard AXUIElementCopyAttributeValue(
                el,
                kAXParentAttribute as CFString,
                &parentRef
            ) == .success else { return nil }
            current = parentRef.map { $0 as! AXUIElement }
        }
        return nil
    }

    /// Focus an editable control only inside an already approved foreground
    /// session. Callers must enforce that gate before invoking this method.
    static func focusEditable(_ element: AXUIElement, target: MacWindowTarget) throws {
        guard elementBelongsToTargetWindow(element, target: target) else {
            throw ServiceError.notFound("editable is not in the target window")
        }
        let err = AXUIElementSetAttributeValue(
            element,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue
        )
        guard err == .success, waitForFocusedEditable(element, target: target) else {
            throw ServiceError.actionFailed("AX editable focus failed: \(err.rawValue)")
        }
    }

    /// Some custom views return a container for hit-testing. Find only a text
    /// descendant whose own AX frame contains the requested point.
    static func editableBelow(_ element: AXUIElement, containing point: CGPoint) -> AXUIElement? {
        for node in walk(from: element, maxNodes: 80) {
            let editable = node.role.contains("Text")
                || node.role.contains("Field")
                || node.role == "AXComboBox"
            if editable, let frame = copyFrame(node.element), frame.contains(point) {
                return node.element
            }
        }
        return nil
    }

    static func climbToWindow(from element: AXUIElement) -> AXUIElement? {
        var current: AXUIElement? = element
        for _ in 0..<16 {
            guard let el = current else { return nil }
            let role = copyString(el, kAXRoleAttribute as CFString) ?? ""
            if role == (kAXWindowRole as String) || role == "AXWindow" {
                return el
            }
            var parent: CFTypeRef?
            if AXUIElementCopyAttributeValue(el, kAXParentAttribute as CFString, &parent) != .success {
                return nil
            }
            current = parent.map { $0 as! AXUIElement }
        }
        return nil
    }

    static func walk(from root: AXUIElement, maxNodes: Int = 400) -> [ElementInfo] {
        var out: [ElementInfo] = []
        var stack: [(AXUIElement, Int)] = [(root, 0)]
        while let (el, depth) = stack.popLast() {
            if out.count >= maxNodes { break }
            let role = copyString(el, kAXRoleAttribute as CFString) ?? ""
            let subrole = copyString(el, kAXSubroleAttribute as CFString) ?? ""
            let title = copyString(el, kAXTitleAttribute as CFString) ?? ""
            let value = copyString(el, kAXValueAttribute as CFString) ?? ""
            let identifier = copyString(el, kAXIdentifierAttribute as CFString) ?? ""
            out.append(
                ElementInfo(
                    role: role,
                    subrole: subrole,
                    title: title,
                    value: value,
                    identifier: identifier,
                    element: el,
                    depth: depth
                )
            )
            if depth >= 12 { continue }
            var childrenRef: CFTypeRef?
            if AXUIElementCopyAttributeValue(el, kAXChildrenAttribute as CFString, &childrenRef) == .success,
               let children = childrenRef as? [AXUIElement]
            {
                // Reverse so first child is processed first with popLast stack.
                for child in children.reversed() {
                    stack.append((child, depth + 1))
                }
            }
        }
        return out
    }

    /// Find a text-like element suitable for writing a marker.
    static func findEditable(in root: AXUIElement) -> AXUIElement? {
        let nodes = walk(from: root)
        let preferredRoles: Set<String> = [
            kAXTextAreaRole as String,
            kAXTextFieldRole as String,
            "AXTextArea",
            "AXTextField",
            "AXComboBox"
        ]
        if let hit = nodes.first(where: { preferredRoles.contains($0.role) }) {
            return hit.element
        }
        // Some editors expose AXTextArea under scroll areas; also accept any settable value attribute.
        for n in nodes {
            var settable: DarwinBoolean = false
            if AXUIElementIsAttributeSettable(n.element, kAXValueAttribute as CFString, &settable) == .success,
               settable.boolValue,
               n.role.contains("Text") || n.role.contains("Field") || n.role == "AXWebArea"
            {
                return n.element
            }
        }
        return nil
    }

    static func findPressable(in root: AXUIElement) -> AXUIElement? {
        let nodes = walk(from: root)
        return nodes.first(where: { isPressableRole($0.role) })?.element
    }

    /// Whether an AX role typically supports `kAXPressAction`.
    static func isPressableRole(_ role: String) -> Bool {
        let roles: Set<String> = [
            kAXButtonRole as String,
            kAXCheckBoxRole as String,
            kAXRadioButtonRole as String,
            "AXButton",
            "AXPopUpButton",
            "AXMenuItem",
            "AXLink",
            "AXCheckBox",
            "AXRadioButton",
            "AXDisclosureTriangle"
        ]
        return roles.contains(role)
    }

    /// Prefer the hit element if pressable; otherwise climb ancestors for a pressable control.
    /// Used by directed click so we act at the requested point, not the first pressable in the tree.
    static func pressableAtOrAbove(_ element: AXUIElement) -> AXUIElement? {
        var current: AXUIElement? = element
        var depth = 0
        while let el = current, depth < 12 {
            let role = copyString(el, kAXRoleAttribute as CFString) ?? ""
            if isPressableRole(role) {
                return el
            }
            // Some controls advertise press even with generic roles (e.g. AXGroup wrappers).
            var actionNames: CFArray?
            if AXUIElementCopyActionNames(el, &actionNames) == .success,
               let names = actionNames as? [String],
               names.contains(kAXPressAction as String)
            {
                return el
            }
            var parentRef: CFTypeRef?
            if AXUIElementCopyAttributeValue(el, kAXParentAttribute as CFString, &parentRef) != .success {
                break
            }
            current = parentRef.map { $0 as! AXUIElement }
            // Stop at window root — do not climb into the whole app and pick unrelated controls.
            if role == (kAXWindowRole as String) || role == "AXWindow" {
                break
            }
            depth += 1
        }
        return nil
    }

    static func setValue(_ element: AXUIElement, _ value: String) throws {
        let err = AXUIElementSetAttributeValue(
            element,
            kAXValueAttribute as CFString,
            value as CFTypeRef
        )
        guard err == .success else {
            throw ServiceError.actionFailed("AX setValue failed: \(err.rawValue)")
        }
    }

    static func getValue(_ element: AXUIElement) -> String {
        copyString(element, kAXValueAttribute as CFString) ?? ""
    }

    static func canSelect(_ element: AXUIElement) -> Bool {
        selectionTarget(for: element) != nil
    }

    static func select(_ element: AXUIElement) throws {
        guard let (container, row) = selectionTarget(for: element) else {
            throw ServiceError.unsupported("AX selected rows attribute is not settable")
        }
        let err = AXUIElementSetAttributeValue(
            container,
            kAXSelectedRowsAttribute as CFString,
            [row] as CFArray
        )
        guard err == .success else {
            throw ServiceError.actionFailed("AX select failed: \(err.rawValue)")
        }
    }

    private static func selectionTarget(
        for element: AXUIElement
    ) -> (container: AXUIElement, row: AXUIElement)? {
        var current: AXUIElement? = element
        var row: AXUIElement?
        for _ in 0..<12 {
            guard let el = current else { return nil }
            let role = copyString(el, kAXRoleAttribute as CFString) ?? ""
            if role == (kAXRowRole as String) || role == "AXRow" {
                row = el
            }
            var settable = DarwinBoolean(false)
            if let row,
               AXUIElementIsAttributeSettable(
                   el,
                   kAXSelectedRowsAttribute as CFString,
                   &settable
               ) == .success,
               settable.boolValue
            {
                return (el, row)
            }
            var parent: CFTypeRef?
            guard AXUIElementCopyAttributeValue(
                el,
                kAXParentAttribute as CFString,
                &parent
            ) == .success else { return nil }
            current = parent.map { $0 as! AXUIElement }
        }
        return nil
    }

    /// Returns true for AXPress and false when only AXConfirm was accepted.
    @discardableResult
    static func press(_ element: AXUIElement) throws -> Bool {
        let press = AXUIElementPerformAction(element, kAXPressAction as CFString)
        if press == .success { return true }
        let confirm = AXUIElementPerformAction(element, kAXConfirmAction as CFString)
        guard confirm == .success else {
            throw ServiceError.actionFailed(
                "AX press/confirm failed: press=\(press.rawValue) confirm=\(confirm.rawValue)"
            )
        }
        return false
    }

    /// Scroll area: try AXShowMenu-free increment via AXValue on scroll bar, or return false.
    @discardableResult
    static func scrollDown(in root: AXUIElement) -> Bool {
        let nodes = walk(from: root)
        if let scrollBar = nodes.first(where: {
            $0.role == (kAXScrollBarRole as String) || $0.role == "AXScrollBar"
        }) {
            // Try increase action.
            if AXUIElementPerformAction(scrollBar.element, "AXIncrement" as CFString) == .success {
                return true
            }
            if let current = copyNumber(scrollBar.element, kAXValueAttribute as CFString) {
                let next = min(1.0, current + 0.15)
                if AXUIElementSetAttributeValue(
                    scrollBar.element,
                    kAXValueAttribute as CFString,
                    next as CFNumber
                ) == .success {
                    return true
                }
            }
        }
        if let area = nodes.first(where: {
            $0.role == (kAXScrollAreaRole as String) || $0.role == "AXScrollArea"
        }) {
            // Some scroll areas accept AXRaise / no-op; treat presence as soft success path elsewhere.
            _ = area
        }
        return false
    }

    // MARK: - helpers

    // kAXWindowNumberAttribute is not always in the Swift overlay; use string.
    private static let kAXWindowNumberAttribute = "AXWindowNumber" as CFString

    private static func focusedElement(pid: pid_t) -> AXUIElement? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            application(pid: pid),
            kAXFocusedUIElementAttribute as CFString,
            &ref
        ) == .success,
            let ref
        else { return nil }
        return (ref as! AXUIElement)
    }

    private static func isSameElementLineage(
        _ first: AXUIElement,
        _ second: AXUIElement
    ) -> Bool {
        func isDescendant(_ element: AXUIElement, of ancestor: AXUIElement) -> Bool {
            var current: AXUIElement? = element
            for _ in 0..<12 {
                guard let el = current else { return false }
                if CFEqual(el, ancestor) { return true }
                var parent: CFTypeRef?
                guard AXUIElementCopyAttributeValue(
                    el,
                    kAXParentAttribute as CFString,
                    &parent
                ) == .success else { return false }
                current = parent.map { $0 as! AXUIElement }
            }
            return false
        }
        return isDescendant(first, of: second) || isDescendant(second, of: first)
    }

    private static func exactWindowID(_ el: AXUIElement) -> CGWindowID? {
        var windowID = CGWindowID(0)
        guard lcuAXUIElementGetWindow(el, &windowID) == .success, windowID != 0 else {
            return nil
        }
        return windowID
    }

    private static func copyString(_ el: AXUIElement, _ attr: CFString) -> String? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(el, attr, &ref) == .success, let ref else { return nil }
        return ref as? String
    }

    private static func copyInt(_ el: AXUIElement, _ attr: CFString) -> Int? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(el, attr, &ref) == .success, let ref else { return nil }
        if let n = ref as? Int { return n }
        if let n = ref as? NSNumber { return n.intValue }
        return nil
    }

    private static func copyNumber(_ el: AXUIElement, _ attr: CFString) -> Double? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(el, attr, &ref) == .success, let ref else { return nil }
        if let n = ref as? Double { return n }
        if let n = ref as? NSNumber { return n.doubleValue }
        return nil
    }

    private static func copyFrame(_ el: AXUIElement) -> CGRect? {
        var posRef: CFTypeRef?
        var sizeRef: CFTypeRef?
        var point = CGPoint.zero
        var size = CGSize.zero
        guard AXUIElementCopyAttributeValue(el, kAXPositionAttribute as CFString, &posRef) == .success,
              let pos = posRef
        else { return nil }
        guard AXUIElementCopyAttributeValue(el, kAXSizeAttribute as CFString, &sizeRef) == .success,
              let sz = sizeRef
        else { return nil }
        AXValueGetValue(pos as! AXValue, .cgPoint, &point)
        AXValueGetValue(sz as! AXValue, .cgSize, &size)
        guard size.width > 1, size.height > 1 else { return nil }
        return CGRect(origin: point, size: size)
    }

    /// Frames match when origin/size are within a few points (AX vs CG rounding).
    private static func framesRoughlyEqual(_ a: CGRect, _ b: CGRect, tolerance: CGFloat = 8) -> Bool {
        abs(a.minX - b.minX) <= tolerance
            && abs(a.minY - b.minY) <= tolerance
            && abs(a.width - b.width) <= tolerance
            && abs(a.height - b.height) <= tolerance
    }
}
