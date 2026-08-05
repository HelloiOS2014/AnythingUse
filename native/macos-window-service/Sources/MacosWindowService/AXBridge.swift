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

/// Accessibility tree read + semantic actions + synthetic in-app focus.
/// Never activates the application or raises it to system frontmost.
enum AXBridge {
    /// Keeps the compatibility fallback enabled only for the duration of one
    /// service request and restores the caller's previous preference value.
    final class AccessibilityEnablementAssertion {
        private static let lock = NSLock()
        private static var fallbackUsers = 0
        private static var fallbackPreviousValue: Any?
        private static var fallbackHadValue = false
        private static let preferenceDomain = "com.apple.universalaccess"
        private static let preferenceKey = "voiceOverOnOffKey"
        private static let notification = Notification.Name(
            "com.apple.universalaccess.VoiceOverSettingsDidChange"
        )

        private let usesFallback: Bool
        private var active = true

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

            usesFallback = allowFallback && !attributeEnabled
            if usesFallback {
                Self.acquireFallback()
            }
        }

        func disable() {
            guard active else { return }
            active = false
            if usesFallback {
                Self.releaseFallback()
            }
        }

        deinit {
            disable()
        }

        private static func acquireFallback() {
            lock.lock()
            defer { lock.unlock() }

            if fallbackUsers == 0 {
                let defaults = UserDefaults(suiteName: preferenceDomain)
                fallbackPreviousValue = defaults?.object(forKey: preferenceKey)
                fallbackHadValue = fallbackPreviousValue != nil
                if (fallbackPreviousValue as? Bool) != true {
                    defaults?.set(true, forKey: preferenceKey)
                    // The target process reads this preference through cfprefsd.
                    // Flush before broadcasting or a short-lived assertion can
                    // restore the value before Chromium ever observes `true`.
                    defaults?.synchronize()
                    DistributedNotificationCenter.default().post(
                        name: notification,
                        object: nil
                    )
                    // Match the native Computer Use initialization grace period.
                    usleep(100_000)
                }
            }
            fallbackUsers += 1
        }

        private static func releaseFallback() {
            lock.lock()
            defer { lock.unlock() }

            fallbackUsers = max(0, fallbackUsers - 1)
            guard fallbackUsers == 0 else { return }
            if (fallbackPreviousValue as? Bool) != true {
                let defaults = UserDefaults(suiteName: preferenceDomain)
                if fallbackHadValue {
                    defaults?.set(fallbackPreviousValue, forKey: preferenceKey)
                } else {
                    defaults?.removeObject(forKey: preferenceKey)
                }
                defaults?.synchronize()
                DistributedNotificationCenter.default().post(
                    name: notification,
                    object: nil
                )
            }
            fallbackPreviousValue = nil
            fallbackHadValue = false
        }
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

    /// Chromium/CEF apps commonly keep their full AX tree disabled until an
    /// accessibility client opts in. Keep the returned assertion alive while
    /// reading or acting on the target UI.
    static func enableAccessibility(
        pid: pid_t,
        allowFallback: Bool = false
    ) -> AccessibilityEnablementAssertion {
        AccessibilityEnablementAssertion(pid: pid, allowFallback: allowFallback)
    }

    static func keyboardEventPID(applicationPID: pid_t) -> pid_t {
        let app = application(pid: applicationPID)
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
            app,
            kAXFocusedUIElementAttribute as CFString,
            &ref
        ) == .success,
            let ref
        else { return applicationPID }

        let focused = ref as! AXUIElement
        var actualPID: pid_t = applicationPID
        if lcuAXUIElementGetActualPid(focused, &actualPID) == .success,
           actualPID > 0
        {
            return actualPID
        }
        if AXUIElementGetPid(focused, &actualPID) == .success, actualPID > 0 {
            return actualPID
        }
        return applicationPID
    }

    /// Match CG window id to an AX window element — **fail closed**.
    ///
    /// Identity proof (never "first/main/focused/title" alone):
    /// 1) `AXWindowNumber == target.windowID` when the app exposes it
    /// 2) else AX window **frame ≈ target.bounds**, only when that frame match
    ///    is **unique** under the same PID (apps like TextEdit omit AXWindowNumber).
    /// When AXWindowNumber exists but differs: immediate mismatch — never frame fallback.
    static func axWindow(for target: MacWindowTarget) throws -> AXUIElement {
        let app = application(pid: target.pid)
        _ = AXUIElementSetMessagingTimeout(app, 2.0)

        var windowsRef: CFTypeRef?
        let err = AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windowsRef)
        if err == .success, let windows = windowsRef as? [AXUIElement], !windows.isEmpty {
            // Prefer authoritative AXWindowNumber hits.
            for w in windows {
                if windowNumberEquals(w, target.windowID) {
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

        // Main / focused only when they prove the same window identity.
        for attr in [kAXMainWindowAttribute as String, kAXFocusedWindowAttribute as String] {
            var ref: CFTypeRef?
            if AXUIElementCopyAttributeValue(app, attr as CFString, &ref) == .success,
               let el = ref
            {
                let element = el as! AXUIElement
                let role = copyString(element, kAXRoleAttribute as CFString) ?? ""
                if (role == (kAXWindowRole as String) || role == "AXWindow"),
                   windowIdentityMatches(element, target: target)
                {
                    return element
                }
            }
        }

        // Hit-test at CG window center; accept only if climbed window matches identity.
        if let hit = elementAtScreenPoint(
            CGPoint(x: target.bounds.midX, y: target.bounds.midY),
            expectedPID: target.pid
        ), let window = climbToWindow(from: hit), windowIdentityMatches(window, target: target) {
            return window
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

        throw ServiceError.notFound(
            "strict AX window match failed for pid=\(target.pid) windowID=\(target.windowID) "
                + "(no AXWindowNumber or unique frame match to CG bounds)"
        )
    }

    /// True when AX element is the target CG window (number or frame).
    ///
    /// If `AXWindowNumber` is present it is authoritative: a mismatch never falls
    /// through to frame comparison (same-position windows must not alias).
    static func windowIdentityMatches(_ el: AXUIElement, target: MacWindowTarget) -> Bool {
        if let cgid = copyInt(el, kAXWindowNumberAttribute) {
            return CGWindowID(cgid) == target.windowID
        }
        // Secondary: apps that omit AXWindowNumber (e.g. TextEdit).
        return windowFrameMatches(el, target: target)
    }

    /// `AXWindowNumber` present and equal to target.
    private static func windowNumberEquals(_ el: AXUIElement, _ windowID: CGWindowID) -> Bool {
        guard let cgid = copyInt(el, kAXWindowNumberAttribute) else { return false }
        return CGWindowID(cgid) == windowID
    }

    /// Frame-only identity (only valid when AXWindowNumber is absent on the element).
    private static func windowFrameMatches(_ el: AXUIElement, target: MacWindowTarget) -> Bool {
        if copyInt(el, kAXWindowNumberAttribute) != nil {
            return false
        }
        guard let frame = copyFrame(el), target.bounds.width > 1, target.bounds.height > 1 else {
            return false
        }
        return framesRoughlyEqual(frame, target.bounds)
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

    /// Synthetic in-app focus. **Window-scoped and background-safe.**
    ///
    /// Only mutates focus when the target window is already the system key window
    /// (`pid + windowID`). PID-only frontmost checks are insufficient: they allow
    /// re-keying user window A → agent window B inside the same app.
    ///
    /// Background apps may change their in-process focused element without becoming
    /// frontmost. If the user is already in that app, require exact target-window proof.
    @discardableResult
    static func syntheticFocus(target: MacWindowTarget, element: AXUIElement) -> Bool {
        if FocusGuard.isFrontmost(pid: target.pid) {
            guard FocusGuard.isTargetKeyWindow(target: target),
                  elementBelongsToTargetWindow(element, target: target)
            else {
                return false
            }
        }
        let app = application(pid: target.pid)
        // Background in-process focus is not system frontmost/key ownership.
        // Do NOT set kAXFrontmostAttribute or raise a window.
        let focused = AXUIElementSetAttributeValue(
            app,
            kAXFocusedUIElementAttribute as CFString,
            element
        )
        _ = AXUIElementSetAttributeValue(
            element,
            kAXFocusedAttribute as CFString,
            kCFBooleanTrue!
        )
        return focused == .success
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
