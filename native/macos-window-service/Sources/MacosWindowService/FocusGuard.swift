import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Prevent agent actions from stealing system frontmost / key window.
///
/// Product rule: background actions must never change system `frontmost app`,
/// key window, user-active tab, real mouse, or keyboard ownership.
///
/// Hard rules:
/// - App activation is allowed only through the disclosed exact-target fallback
///   (`Service.foregroundActivate`); never post-hoc restore.
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

    /// Exact-window proof after a foreground activation (realignment §4.5).
    /// Preferred: the app's AX key-window number equals the target window.
    /// Without an AX identity: a layer-0 hit-test at the target window's center
    /// must return the same PID + CGWindowID. PID-frontmost alone is insufficient.
    static func provesExactWindow(pid: pid_t, windowID: CGWindowID) -> Bool {
        if focusedWindowNumber(pid: pid) == windowID {
            return true
        }
        guard let target = try? WindowResolver.resolve(pid: pid, windowID: windowID),
              let hit = WindowResolver.windowAtScreenPoint(
                  CGPoint(x: target.bounds.midX, y: target.bounds.midY)
              )
        else {
            return false
        }
        return hit.0 == pid && hit.1 == windowID
    }

    static func currentExactWindowNumber(pid: pid_t) -> CGWindowID? {
        focusedWindowNumber(pid: pid) ?? topmostSamePIDWindow(pid: pid)
    }

    /// The topmost on-screen CGWindow for `pid`. The API returns windows
    /// front-to-back, so the first same-PID entry is authoritative.
    static func topmostSamePIDWindow(pid: pid_t) -> CGWindowID? {
        guard let list = CGWindowListCopyWindowInfo(
            [.optionOnScreenOnly, .excludeDesktopElements],
            kCGNullWindowID
        ) as? [[String: Any]] else {
            return nil
        }
        for info in list {
            guard let owner = info[kCGWindowOwnerPID as String] as? Int,
                  owner == pid,
                  (info[kCGWindowLayer as String] as? Int ?? 0) == 0
            else {
                continue
            }
            if let number = info[kCGWindowNumber as String] as? Int {
                return CGWindowID(number)
            }
        }
        return nil
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
        if isFrontmost(pid: target.pid),
           provesExactWindow(pid: target.pid, windowID: target.windowID)
        {
            return try body()
        }
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
            throw ServiceError.foregroundRequired(
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
