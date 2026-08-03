import ApplicationServices
import CoreGraphics
import Foundation

/// Directed (non-global) synthetic input to a target **pid + windowID**.
///
/// Rules:
/// - Prefer AX semantic actions (setValue / press / scroll attributes) on elements
///   proven to belong to the target window — never the first arbitrary control.
/// - `CGEvent.postToPid` is process-scoped only. Use it only when the target window
///   is already the app's key/focused window (`pid + windowID` proof). Otherwise
///   return `unsupported` — never risk operating the user's same-app window A.
/// - Never call `CGEvent.post(.cghidEventTap)` for mouse moves (would move real cursor).
/// - Never activate / frontmost the target app; never post-hoc restore focus.
enum DirectedInput {
    struct ActionReport {
        var path: String
        var detail: String
        var mouseEventsPosted: Bool
        var keyEventsPosted: Bool
    }

    /// Best-effort editable discovery inside the **proven** target window only.
    static func resolveEditable(target: MacWindowTarget) -> AXUIElement? {
        guard let axWindow = try? AXBridge.axWindow(for: target) else { return nil }
        if let editable = AXBridge.findEditable(in: axWindow) {
            return editable
        }
        // Hit-test only accepts elements under the target window (AXWindowNumber).
        let points: [CGPoint] = [
            CGPoint(x: target.bounds.midX, y: target.bounds.minY + target.bounds.height * 0.45),
            CGPoint(x: target.bounds.midX, y: target.bounds.minY + target.bounds.height * 0.60),
            CGPoint(x: target.bounds.minX + target.bounds.width * 0.30, y: target.bounds.midY)
        ]
        for p in points {
            if let hit = AXBridge.elementAtScreenPoint(p, expectedPID: target.pid),
               AXBridge.elementBelongsToTargetWindow(hit, target: target)
            {
                if let editable = AXBridge.findEditable(in: hit) {
                    return editable
                }
                var ref: CFTypeRef?
                AXUIElementCopyAttributeValue(hit, kAXRoleAttribute as CFString, &ref)
                let role = ref as? String ?? ""
                let preferred = ["AXTextArea", "AXTextField", kAXTextAreaRole as String, kAXTextFieldRole as String]
                if preferred.contains(role) {
                    return hit
                }
            }
        }
        return nil
    }

    /// Click at the requested normalized window coordinates (or the element under that point).
    ///
    /// Must hit the requested location — never degrade to "first pressable control" in the tree.
    static func click(target: MacWindowTarget, normalizedX: Double = 0.5, normalizedY: Double = 0.5) throws -> ActionReport {
        let nx = max(0.0, min(1.0, normalizedX))
        let ny = max(0.0, min(1.0, normalizedY))
        let x = target.bounds.minX + target.bounds.width * nx
        let y = target.bounds.minY + target.bounds.height * ny
        let point = CGPoint(x: x, y: y)

        // Path A: AX press on the element under the requested point, only if it belongs
        // to the target window. Skip synthetic focus when not key (press alone).
        if let hit = AXBridge.elementAtScreenPoint(point, expectedPID: target.pid),
           AXBridge.elementBelongsToTargetWindow(hit, target: target)
        {
            if let pressable = AXBridge.pressableAtOrAbove(hit),
               AXBridge.elementBelongsToTargetWindow(pressable, target: target)
            {
                let focused = AXBridge.syntheticFocus(target: target, element: pressable)
                try AXBridge.press(pressable)
                return ActionReport(
                    path: focused ? "ax_press_at_point+synthetic_focus" : "ax_press_at_point",
                    detail: String(
                        format: "pressed element under (%.1f, %.1f) nx=%.3f ny=%.3f → pid %d window_id=%u",
                        x, y, nx, ny, target.pid, target.windowID
                    ),
                    mouseEventsPosted: false,
                    keyEventsPosted: false
                )
            }
        }

        // Path B: coordinate click via postToPid — only when target is already key window.
        try requireKeyWindowForCGEvent(target: target, capability: "click")
        try postMouseClick(pid: target.pid, point: point)
        return ActionReport(
            path: "cgevent_post_to_pid_click",
            detail: String(
                format: "left click at (%.1f, %.1f) nx=%.3f ny=%.3f → pid %d window_id=%u",
                x, y, nx, ny, target.pid, target.windowID
            ),
            mouseEventsPosted: true,
            keyEventsPosted: false
        )
    }

    /// Scroll via AX scroll bar if present, else PID-directed scroll only when key window proven.
    static func scroll(target: MacWindowTarget, lines: Int32 = -3) throws -> ActionReport {
        if let axWindow = try? AXBridge.axWindow(for: target),
           AXBridge.scrollDown(in: axWindow)
        {
            return ActionReport(
                path: "ax_scroll_bar",
                detail: "AX increment/setValue on scroll bar (window_id=\(target.windowID))",
                mouseEventsPosted: false,
                keyEventsPosted: false
            )
        }

        try requireKeyWindowForCGEvent(target: target, capability: "scroll")
        let x = target.bounds.midX
        let y = target.bounds.midY
        try postScroll(pid: target.pid, point: CGPoint(x: x, y: y), lines: lines)
        return ActionReport(
            path: "cgevent_post_to_pid_scroll",
            detail: "scroll lines=\(lines) at window center → pid \(target.pid) window_id=\(target.windowID)",
            mouseEventsPosted: true,
            keyEventsPosted: false
        )
    }

    // MARK: - CGEvent postToPid helpers (window-bound)

    /// Unicode typing. Prefer AX setValue on a proven editable; CGEvent only when
    /// the target window is already the app's key window.
    ///
    /// Long input re-proves key-window ownership every segment so a user switch
    /// mid-type aborts immediately (never continue dumping into the wrong window).
    static func typeUnicode(target: MacWindowTarget, text: String) throws {
        try requireKeyWindowForCGEvent(target: target, capability: "type")
        try postUnicode(target: target, text: text)
    }

    /// Fail closed when CGEvent cannot be proven to land on `target.windowID`.
    /// `CGEvent.postToPid` is PID-only; without key-window proof it can hit the
    /// user's same-app window A while the agent intended window B.
    private static func requireKeyWindowForCGEvent(target: MacWindowTarget, capability: String) throws {
        // Key window proven by AXWindowNumber or focused-frame == target.bounds.
        // Does **not** require system frontmost: background apps still have an
        // internal key window; we only allow postToPid when that key is target.
        if let key = FocusGuard.focusedWindowNumber(pid: target.pid) {
            // Authoritative number present: must equal target — no frame fallback.
            if key == target.windowID {
                return
            }
            throw ServiceError.unsupported(
                "\(capability) via CGEvent.postToPid refused: process key window is "
                    + "window_id=\(key), not agent window_id=\(target.windowID)"
            )
        }
        if FocusGuard.focusedWindowMatches(target: target) {
            return
        }
        throw ServiceError.unsupported(
            "\(capability) via CGEvent.postToPid refused: cannot prove target window "
                + "(pid=\(target.pid) window_id=\(target.windowID)) is the process key window; "
                + "PID-only delivery would risk same-app window isolation"
        )
    }

    /// Segment size for mid-type key-window re-proof (characters).
    private static let typeSegmentChars = 24

    private static func postUnicode(target: MacWindowTarget, text: String) throws {
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw ServiceError.actionFailed("CGEventSource create failed")
        }
        var index = 0
        for ch in text.unicodeScalars {
            if index % typeSegmentChars == 0 {
                // Re-confirm target is still the process key window before each segment.
                try requireKeyWindowForCGEvent(target: target, capability: "type")
            }
            let s = String(ch)
            guard
                let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false)
            else {
                throw ServiceError.actionFailed("keyboard event create failed")
            }
            var utf16 = Array(s.utf16)
            down.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
            up.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
            down.postToPid(target.pid)
            up.postToPid(target.pid)
            usleep(8_000)
            index += 1
        }
    }

    static func postMouseClick(pid: pid_t, point: CGPoint) throws {
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw ServiceError.actionFailed("CGEventSource create failed")
        }
        // Important: postToPid only — do not post HID mouse-moved (would move real cursor).
        guard
            let down = CGEvent(
                mouseEventSource: source,
                mouseType: .leftMouseDown,
                mouseCursorPosition: point,
                mouseButton: .left
            ),
            let up = CGEvent(
                mouseEventSource: source,
                mouseType: .leftMouseUp,
                mouseCursorPosition: point,
                mouseButton: .left
            )
        else {
            throw ServiceError.actionFailed("mouse event create failed")
        }
        down.setIntegerValueField(.mouseEventClickState, value: 1)
        up.setIntegerValueField(.mouseEventClickState, value: 1)
        down.postToPid(pid)
        usleep(30_000)
        up.postToPid(pid)
    }

    static func postScroll(pid: pid_t, point: CGPoint, lines: Int32) throws {
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw ServiceError.actionFailed("CGEventSource create failed")
        }
        guard let event = CGEvent(
            scrollWheelEvent2Source: source,
            units: .line,
            wheelCount: 1,
            wheel1: lines,
            wheel2: 0,
            wheel3: 0
        ) else {
            throw ServiceError.actionFailed("scroll event create failed")
        }
        event.location = point
        event.postToPid(pid)
    }
}
