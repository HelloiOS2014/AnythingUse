import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Same-window user takeover / target-lost detection.
///
/// Product rule (D2): do **not** pause when the target *app* is frontmost.
/// Only emit `taken_over` when the user focuses the **same** window (pid+window_id).
/// Emit `target_lost` when the process or CG window disappears.
enum Takeover {
    static func detect(pid: pid_t, windowID: CGWindowID) -> ControlState {
        if !WindowResolver.processAlive(pid) {
            return .targetLost
        }
        if !WindowResolver.windowExists(pid: pid, windowID: windowID) {
            return .targetLost
        }

        // User owns the same window when:
        // 1) that PID is frontmost, AND
        // 2) the focused/main AX window's CG id matches target windowID.
        // Agent must not activate apps; FocusGuard fails closed on steals
        // (no post-hoc activate) rather than reporting them as user takeover.
        let frontPID = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
        guard frontPID == pid, frontPID != 0 else {
            return .none
        }

        // Prefer AXWindowNumber; apps like TextEdit omit it — then match focused frame to CG bounds.
        if let focusedWindowID = FocusGuard.focusedWindowNumber(pid: pid), focusedWindowID == windowID {
            return .takenOver
        }
        if FocusGuard.focusedWindowNumber(pid: pid) == nil {
            // Resolve live CG bounds for this window id, then compare to focused AX frame.
            if let target = try? WindowResolver.resolve(pid: pid, windowID: windowID),
               FocusGuard.focusedWindowMatches(target: target)
            {
                return .takenOver
            }
        }

        return .none
    }
}
