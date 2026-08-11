import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Prevent agent actions from stealing system frontmost / key window.
///
/// Product rule: agent must never change system `frontmost app`, key window,
/// user-active tab, real mouse, or keyboard ownership.
///
/// Hard rules:
/// - Never call `NSRunningApplication.activate()` (or any post-hoc restore).
/// - Detecting a steal after the fact is fail-closed only — damage may already
///   be visible; the fix is to refuse paths that can steal, not to "undo" them.
/// - Same-app window A vs window B must be distinguished by `pid + windowID`.
enum FocusGuard {
    struct Snapshot: Equatable {
        let frontmostPid: pid_t
        /// Focused/main AX window of the frontmost app, when known.
        let keyWindowID: CGWindowID?
        /// When AXWindowNumber is missing: whether focused AX frame matched target at snapshot time.
        /// Optional because snapshot() without target leaves this nil.
        let focusedMatchedTarget: Bool?
    }

    /// Capture frontmost pid + key window before an action.
    static func snapshot(target: MacWindowTarget? = nil) -> Snapshot {
        let front = NSWorkspace.shared.frontmostApplication
        let pid = front?.processIdentifier ?? 0
        let key = pid != 0 ? focusedWindowNumber(pid: pid) : nil
        let matched: Bool? = {
            guard let target, pid == target.pid else { return nil }
            return focusedWindowMatches(target: target)
        }()
        return Snapshot(frontmostPid: pid, keyWindowID: key, focusedMatchedTarget: matched)
    }

    /// Whether `pid` currently owns system frontmost.
    static func isFrontmost(pid: pid_t) -> Bool {
        let front = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
        return front != 0 && front == pid
    }

    /// Whether the target window is already the system key window (pid + windowID).
    /// Required before PID-directed input that depends on system key-window routing.
    static func isTargetKeyWindow(target: MacWindowTarget) -> Bool {
        guard isFrontmost(pid: target.pid) else { return false }
        // Prefer AXWindowNumber when the app exposes it.
        if let key = focusedWindowNumber(pid: target.pid) {
            return key == target.windowID
        }
        // TextEdit etc. omit AXWindowNumber: match focused AX window frame to CG bounds.
        return focusedWindowMatches(target: target)
    }

    /// CGWindowNumber of the app's focused/main AX window when available.
    static func focusedWindowNumber(pid: pid_t) -> CGWindowID? {
        let app = AXUIElementCreateApplication(pid)
        for attr in [kAXFocusedWindowAttribute as String, kAXMainWindowAttribute as String] {
            var ref: CFTypeRef?
            if AXUIElementCopyAttributeValue(app, attr as CFString, &ref) == .success,
               let el = ref
            {
                let element = el as! AXUIElement
                if let n = copyInt(element, "AXWindowNumber" as CFString) {
                    return CGWindowID(n)
                }
            }
        }
        return nil
    }

    /// Whether the focused/main AX window's frame matches `target.bounds`
    /// (for apps that do not expose AXWindowNumber).
    static func focusedWindowMatches(target: MacWindowTarget) -> Bool {
        let app = AXUIElementCreateApplication(target.pid)
        for attr in [kAXFocusedWindowAttribute as String, kAXMainWindowAttribute as String] {
            var ref: CFTypeRef?
            if AXUIElementCopyAttributeValue(app, attr as CFString, &ref) == .success,
               let el = ref
            {
                let element = el as! AXUIElement
                if AXBridge.windowIdentityMatches(element, target: target) {
                    return true
                }
            }
        }
        return false
    }

    /// Run a mutating action under post-hoc steal detection.
    ///
    /// **Not a prevention mechanism.** Callers must refuse undirected / focus-stealing
    /// paths *before* invoking `body` (key-window proof, element→window identity).
    /// This wrapper only fails closed *after* damage may already be visible — useful
    /// as evidence and loop abort, never as acceptance of "non-interference".
    ///
    /// Post-hoc `previous.activate()` is forbidden: it causes a second focus
    /// switch and can override a window/app the user chose during the action.
    static func withoutFrontmostSteal<T>(
        target: MacWindowTarget,
        _ body: () throws -> T
    ) throws -> T {
        let before = snapshot(target: target)
        // body may throw after a partial steal; still assert so defects surface.
        do {
            let result = try body()
            try assertNoSteal(before: before, target: target)
            return result
        } catch {
            // Prefer reporting steal when body failed after damaging focus.
            do {
                try assertNoSteal(before: before, target: target)
            } catch let steal {
                throw steal
            }
            throw error
        }
    }

    /// Fail closed if frontmost app or key window was moved onto the agent target.
    /// Diagnostic only: does not prevent the steal that already happened.
    static func assertNoSteal(before: Snapshot, target: MacWindowTarget) throws {
        let after = snapshot()

        // Case 1: target process was not frontmost; now it is → steal.
        if before.frontmostPid != 0,
           before.frontmostPid != target.pid,
           after.frontmostPid == target.pid
        {
            throw ServiceError.actionFailed(
                "refused: action promoted target app to frontmost (pid=\(target.pid)); "
                    + "background control must not steal frontmost (no post-hoc activate)"
            )
        }

        // Case 2: same app was already frontmost, but key window flipped to agent target
        // while the user had a different window of the same process (A vs B).
        if before.frontmostPid == target.pid,
           after.frontmostPid == target.pid,
           let beforeKey = before.keyWindowID,
           beforeKey != target.windowID,
           let afterKey = after.keyWindowID,
           afterKey == target.windowID
        {
            throw ServiceError.actionFailed(
                "refused: action re-keyed same-app window "
                    + "(from window_id=\(beforeKey) to agent window_id=\(target.windowID)); "
                    + "same-app windows must stay isolated"
            )
        }

        // Case 2b: no AXWindowNumber — focused frame flipped onto agent target (A→B rekey).
        if before.frontmostPid == target.pid,
           after.frontmostPid == target.pid,
           before.focusedMatchedTarget == false,
           focusedWindowMatches(target: target)
        {
            throw ServiceError.actionFailed(
                "refused: action re-keyed same-app window onto agent window_id=\(target.windowID) "
                    + "(frame identity; no post-hoc activate)"
            )
        }

        // Do not treat "user switched away during the action" as agent steal.
        // Only cases above (promote target / re-key onto agent window) are agent faults.
        // Never call previous.activate() — that creates a second interference.
    }

    // MARK: - private

    private static func copyInt(_ el: AXUIElement, _ attr: CFString) -> Int? {
        var ref: CFTypeRef?
        guard AXUIElementCopyAttributeValue(el, attr, &ref) == .success, let ref else { return nil }
        if let n = ref as? Int { return n }
        if let n = ref as? NSNumber { return n.intValue }
        return nil
    }
}
