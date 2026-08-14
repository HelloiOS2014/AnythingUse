import CoreGraphics
import Foundation

/// Single-slot, GUI-approved foreground session (pid + window_id).
///
/// Runtime places the session via `set_foreground_session` only after a
/// ForegroundGrant; `foreground_activate` requires a matching active session.
/// While active, the agent's own approved promotion is not a FocusGuard steal
/// and not user takeover; when inactive, all background protections stay in
/// place. There is exactly one slot (serial FIFO).
final class ForegroundSession {
    static let shared = ForegroundSession()

    private let lock = NSLock()
    private var pid: pid_t = 0
    private var windowID: CGWindowID = 0
    private var active = false

    func begin(pid: pid_t, windowID: CGWindowID) {
        lock.lock()
        defer { lock.unlock() }
        self.pid = pid
        self.windowID = windowID
        self.active = true
    }

    func suspend(pid: pid_t, windowID: CGWindowID) {
        lock.lock()
        defer { lock.unlock() }
        if self.pid == pid, self.windowID == windowID {
            active = false
        }
    }

    func resume(pid: pid_t, windowID: CGWindowID) throws {
        lock.lock()
        defer { lock.unlock() }
        guard self.pid == pid, self.windowID == windowID else {
            throw ServiceError.foregroundRequired("foreground authorization is no longer resident")
        }
        guard !(try UserInputMonitor.shared.hasInput(pid: pid, windowID: windowID)) else {
            throw ServiceError.takenOver("user operated the target window while Agent was waiting")
        }
        guard FocusGuard.isFrontmost(pid: pid),
              FocusGuard.provesExactWindow(pid: pid, windowID: windowID)
        else {
            throw ServiceError.foregroundRequired(
                "target is no longer the exact foreground window; another activation requires approval"
            )
        }
        active = true
    }

    func clear() {
        lock.lock()
        defer { lock.unlock() }
        pid = 0
        windowID = 0
        active = false
    }

    func clear(pid: pid_t, windowID: CGWindowID) {
        lock.lock()
        defer { lock.unlock() }
        guard self.pid == pid, self.windowID == windowID else { return }
        self.pid = 0
        self.windowID = 0
        active = false
    }

    func isActive(pid: pid_t, windowID: CGWindowID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return active && self.pid == pid && self.windowID == windowID
    }

    /// Session-mode input gate (realignment §4.5). The session only marks the
    /// agent's activation owner; the app being frontmost is PID-level and NOT
    /// sufficient for input. After activation, before any input, the exact
    /// window must be re-proven:
    /// - preferred: proven AX key-window identity equals the target window;
    /// - no AX identity available: the target CGWindowID equals the unique
    ///   topmost same-PID window; multiple candidates or unprovable → fail.
    func allowsSessionInput(pid: pid_t, windowID: CGWindowID, appIsFrontmost: Bool) -> Bool {
        guard isActive(pid: pid, windowID: windowID), appIsFrontmost else {
            return false
        }
        return FocusGuard.provesExactWindow(pid: pid, windowID: windowID)
    }
}
