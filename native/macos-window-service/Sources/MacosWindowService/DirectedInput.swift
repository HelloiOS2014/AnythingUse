import ApplicationServices
import CoreGraphics
import Foundation

/// Directed (non-global) synthetic input to a target **pid + windowID**.
///
/// Rules:
/// - Prefer AX semantic actions (setValue / press / scroll attributes) on elements
///   proven to belong to the target window — never the first arbitrary control.
/// - `CGEvent.postToPid` is process-scoped only. Background apps may receive it
///   without activation; if the user is in the same app, require exact key-window
///   proof so their window A cannot be mistaken for the agent's window B.
/// - Never call `CGEvent.post(.cghidEventTap)` for mouse moves (would move real cursor).
/// - Never activate / frontmost the target app; never post-hoc restore focus.
enum DirectedInput {
    struct ActionReport {
        var path: String
        var detail: String
        var mouseEventsPosted: Bool
        var keyEventsPosted: Bool
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

        // Path A: operate on the element under the requested point, only if it belongs
        // to the target window. Never write AX focus attributes.
        if let hit = AXBridge.elementAtScreenPoint(point, expectedPID: target.pid) {
            let hitIsInTarget = AXBridge.elementBelongsToTargetWindow(hit, target: target)
                || WindowResolver.isTopmostProcessWindow(target: target, at: point)
            if hitIsInTarget {
                if AXBridge.editableAtOrAbove(hit) != nil
                    || AXBridge.editableBelow(hit, containing: point) != nil
                {
                    try postMouseClick(pid: target.pid, point: point)
                    return ActionReport(
                        path: "editable_at_point+cgevent_post_to_pid_click",
                        detail: String(
                            format: "clicked editable under (%.1f, %.1f) nx=%.3f ny=%.3f → pid %d window_id=%u",
                            x, y, nx, ny, target.pid, target.windowID
                        ),
                        mouseEventsPosted: true,
                        keyEventsPosted: false
                    )
                }
                if let pressable = AXBridge.pressableAtOrAbove(hit) {
                    try AXBridge.press(pressable)
                    return ActionReport(
                        path: "ax_press_at_point",
                        detail: String(
                            format: "pressed element under (%.1f, %.1f) nx=%.3f ny=%.3f → pid %d window_id=%u",
                            x, y, nx, ny, target.pid, target.windowID
                        ),
                        mouseEventsPosted: false,
                        keyEventsPosted: false
                    )
                }
            }
        }

        // Path B: coordinate click via postToPid. Prove the requested point belongs
        // to this exact same-process window before using the PID-only event route.
        if ExperimentalSkyLightInput.isEnabled && !FocusGuard.isFrontmost(pid: target.pid) {
            try ExperimentalSkyLightInput.click(target: target, screenPoint: point)
            return ActionReport(
                path: "experimental_skylight_target_only_click",
                detail: String(
                    format: "target-only background click at (%.1f, %.1f) nx=%.3f ny=%.3f → pid %d window_id=%u",
                    x, y, nx, ny, target.pid, target.windowID
                ),
                mouseEventsPosted: true,
                keyEventsPosted: false
            )
        }
        guard WindowResolver.isTopmostProcessWindow(target: target, at: point) else {
            throw ServiceError.unsupported(
                "click refused: cannot bind point to pid=\(target.pid) window_id=\(target.windowID)"
            )
        }
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

    /// Unicode typing routed to the focused AX element's actual process. Apps with
    /// out-of-process renderers do not necessarily handle key events on the shell PID.
    ///
    /// Keyboard events route by the process key window, so delivery requires the
    /// target window to *be* the key window: foreground apps must pass the strict
    /// key-window gate, background apps get an in-process key-window lead-in first.
    /// Long input re-proves key-window ownership every segment so a user switch
    /// mid-type aborts immediately (never continue dumping into the wrong window).
    static func typeUnicode(target: MacWindowTarget, text: String) throws -> String {
        if ExperimentalSkyLightInput.isEnabled && !FocusGuard.isFrontmost(pid: target.pid) {
            return try ExperimentalSkyLightInput.withSyntheticTargetFocus(target: target) {
                let eventPID = AXBridge.keyboardEventPID(applicationPID: target.pid)
                try postUnicode(
                    target: target,
                    eventPID: eventPID,
                    text: text,
                    useSkyLight: true
                )
                return eventPID == target.pid
                    ? "experimental_skylight_target_only_type"
                    : "experimental_skylight_renderer_type"
            }
        }
        try establishKeyboardLead(target: target)
        let eventPID = AXBridge.keyboardEventPID(applicationPID: target.pid)
        try postUnicode(target: target, eventPID: eventPID, text: text, useSkyLight: false)
        return eventPID == target.pid
            ? "cgevent_post_to_pid_type"
            : "cgevent_post_to_renderer_pid_type"
    }

    /// Submit an already AX-bound editable without activating its app.
    static func pressReturn(target: MacWindowTarget) throws -> String {
        if ExperimentalSkyLightInput.isEnabled && !FocusGuard.isFrontmost(pid: target.pid) {
            return try ExperimentalSkyLightInput.withSyntheticTargetFocus(target: target) {
                let eventPID = AXBridge.keyboardEventPID(applicationPID: target.pid)
                try postKey(
                    target: target,
                    pid: eventPID,
                    virtualKey: 36,
                    useSkyLight: true
                )
                return eventPID == target.pid
                    ? "experimental_skylight_target_only_return"
                    : "experimental_skylight_renderer_return"
            }
        }
        try establishKeyboardLead(target: target)
        let eventPID = AXBridge.keyboardEventPID(applicationPID: target.pid)
        try postKey(target: target, pid: eventPID, virtualKey: 36, useSkyLight: false)
        return eventPID == target.pid
            ? "cgevent_post_to_pid_return"
            : "cgevent_post_to_renderer_pid_return"
    }

    /// Keyboard lead-in: before delivering any keyboard event, the process key
    /// window must be the target window (keyboard routes by key window, not by
    /// coordinates). Foreground apps pass through the strict gate unchanged.
    ///
    /// Background apps: only *prove* — never reassign the process key window.
    /// Writing focused/main window attributes on a background app promotes it
    /// to frontmost (a visible focus steal); the window can become key only as
    /// a side effect of a directed click (the semantic set_value path clicks
    /// the element first, which switches the in-process key window without
    /// activating the app). Pure keyboard delivery to a background window that
    /// is not already key is refused.
    private static func establishKeyboardLead(target: MacWindowTarget) throws {
        if FocusGuard.isFrontmost(pid: target.pid) { return }
        guard AXBridge.proofOfProcessKeyWindow(target: target) else {
            throw ServiceError.unsupported(
                "keyboard refused for background pid=\(target.pid) window_id=\(target.windowID): "
                    + "target is not the process key window and we never re-key a background "
                    + "app (would steal frontmost); click the element first or fail-closed"
            )
        }
    }

    /// Per-segment re-proof for keyboard delivery. Foreground: identical to the
    /// mouse gate. Background: the in-process key window must still be the
    /// target (a user switching apps may re-key the process to their window).
    private static func requireKeyboardTargetProof(target: MacWindowTarget, capability: String) throws {
        if FocusGuard.isFrontmost(pid: target.pid) {
            try requireKeyWindowForCGEvent(target: target, capability: capability)
            return
        }
        guard AXBridge.proofOfProcessKeyWindow(target: target) else {
            throw ServiceError.unsupported(
                "\(capability) via CGEvent.postToPid refused: process key window not proven "
                    + "== target (background pid=\(target.pid) window_id=\(target.windowID)); fail-closed"
            )
        }
    }

    /// Fail closed when CGEvent cannot be proven to land on `target.windowID`.
    /// `CGEvent.postToPid` is PID-only; without key-window proof it can hit the
    /// user's same-app window A while the agent intended window B.
    private static func requireKeyWindowForCGEvent(target: MacWindowTarget, capability: String) throws {
        // A background process cannot receive the user's live keyboard/mouse stream;
        // postToPid stays inside that process and never activates it. Exact same-app
        // window isolation is required only when the user is actively in that app.
        if !FocusGuard.isFrontmost(pid: target.pid) {
            return
        }
        // Key window proven by AXWindowNumber or focused-frame == target.bounds.
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

    private static func postUnicode(
        target: MacWindowTarget,
        eventPID: pid_t,
        text: String,
        useSkyLight: Bool
    ) throws {
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw ServiceError.actionFailed("CGEventSource create failed")
        }
        var index = 0
        for ch in text.unicodeScalars {
            if useSkyLight {
                guard !FocusGuard.isFrontmost(pid: target.pid) else {
                    throw ServiceError.unsupported(
                        "experimental typing stopped: user became active in target pid=\(target.pid)"
                    )
                }
                try ExperimentalSkyLightInput.validateWindow(target)
            } else if index % typeSegmentChars == 0 {
                // Re-confirm target is still the process key window before each segment.
                try requireKeyboardTargetProof(target: target, capability: "type")
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
            down.flags = []
            up.flags = []
            if useSkyLight {
                try ExperimentalSkyLightInput.postKeyboard(down, pid: eventPID)
            } else {
                down.postToPid(eventPID)
            }
            usleep(8_000)
            if useSkyLight {
                try ExperimentalSkyLightInput.postKeyboard(up, pid: eventPID)
            } else {
                up.postToPid(eventPID)
            }
            usleep(8_000)
            index += 1
        }
    }

    private static func postKey(
        target: MacWindowTarget,
        pid: pid_t,
        virtualKey: CGKeyCode,
        useSkyLight: Bool
    ) throws {
        guard let source = CGEventSource(stateID: .hidSystemState),
              let down = CGEvent(keyboardEventSource: source, virtualKey: virtualKey, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: virtualKey, keyDown: false)
        else {
            throw ServiceError.actionFailed("keyboard event create failed")
        }
        if useSkyLight {
            guard !FocusGuard.isFrontmost(pid: target.pid) else {
                throw ServiceError.unsupported(
                    "experimental key stopped: user became active in target pid=\(target.pid)"
                )
            }
            try ExperimentalSkyLightInput.validateWindow(target)
            try ExperimentalSkyLightInput.postKeyboard(down, pid: pid)
        } else {
            down.postToPid(pid)
        }
        usleep(8_000)
        if useSkyLight {
            try ExperimentalSkyLightInput.postKeyboard(up, pid: pid)
        } else {
            up.postToPid(pid)
        }
        usleep(8_000)
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
