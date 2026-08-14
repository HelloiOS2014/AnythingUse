import AppKit
import CoreGraphics
import Foundation

/// Listen-only HID monitor. Synthetic events emitted by AnythingUse carry a
/// private tag; untagged input on the reserved window is user takeover.
final class UserInputMonitor {
    static let shared = UserInputMonitor()
    static let syntheticEventTag: Int64 = 0x4C_43_55 // "LCU"

    private struct Target: Hashable {
        let pid: pid_t
        let windowID: CGWindowID
    }

    private let lock = NSLock()
    private var started = false
    private var eventTap: CFMachPort?
    private var generations: [Target: UInt64] = [:]
    private var baselines: [Target: UInt64] = [:]
    private var invalidated: Set<Target> = []
    private var epoch: UInt64 = 1

    @discardableResult
    func start() -> Bool {
        lock.lock()
        if started {
            lock.unlock()
            return true
        }
        lock.unlock()

        guard CGPreflightListenEventAccess() || CGRequestListenEventAccess() else {
            return false
        }

        let mask = [
            CGEventType.keyDown,
            .leftMouseDown,
            .rightMouseDown,
            .otherMouseDown,
            .scrollWheel
        ].reduce(CGEventMask(0)) { $0 | (CGEventMask(1) << $1.rawValue) }
        guard let eventTap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .listenOnly,
            eventsOfInterest: mask,
            callback: { _, type, event, userInfo in
                if let userInfo {
                    Unmanaged<UserInputMonitor>.fromOpaque(userInfo)
                        .takeUnretainedValue()
                        .record(type: type, event: event)
                }
                return Unmanaged.passUnretained(event)
            },
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        ) else {
            return false
        }
        CGEvent.tapEnable(tap: eventTap, enable: true)
        lock.lock()
        started = true
        self.eventTap = eventTap
        lock.unlock()
        Thread.detachNewThread {
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, eventTap, 0)
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .commonModes)
            CFRunLoopRun()
        }
        return true
    }

    var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return started && (eventTap.map { CGEvent.tapIsEnabled(tap: $0) } ?? false)
    }

    func arm(pid: pid_t, windowID: CGWindowID) throws {
        guard isRunning else {
            throw ServiceError.permission("Input Monitoring is required for real-user takeover detection")
        }
        let target = Target(pid: pid, windowID: windowID)
        lock.lock()
        invalidated.remove(target)
        baselines[target] = generations[target, default: 0]
        lock.unlock()
    }

    func clear(pid: pid_t, windowID: CGWindowID) {
        let target = Target(pid: pid, windowID: windowID)
        lock.lock()
        baselines.removeValue(forKey: target)
        generations.removeValue(forKey: target)
        invalidated.remove(target)
        lock.unlock()
    }

    func hasInput(pid: pid_t, windowID: CGWindowID) throws -> Bool {
        let target = Target(pid: pid, windowID: windowID)
        lock.lock()
        let wasInvalidated = invalidated.contains(target)
        lock.unlock()
        if wasInvalidated { return true }
        guard isRunning else {
            throw ServiceError.permission("Input Monitoring became unavailable during control")
        }
        lock.lock()
        defer { lock.unlock() }
        guard let baseline = baselines[target] else {
            throw ServiceError.permission("real-user takeover watch is not armed for the target")
        }
        return generations[target, default: 0] > baseline
    }

    func invalidateAll() {
        lock.lock()
        invalidated.formUnion(baselines.keys)
        epoch &+= 1
        lock.unlock()
    }

    var sessionEpoch: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return epoch
    }

    /// Small deterministic hook used by the native self-check.
    func selfCheck() -> Bool {
        let target = Target(pid: 4242, windowID: 9)
        lock.lock()
        generations[target] = 0
        baselines[target] = 0
        generations[target] = 1
        let passed = generations[target, default: 0] > baselines[target, default: 0]
        generations.removeValue(forKey: target)
        baselines.removeValue(forKey: target)
        invalidated.remove(target)
        lock.unlock()
        return passed
    }

    private func record(type: CGEventType, event: CGEvent) {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            lock.lock()
            let tap = eventTap
            lock.unlock()
            if let tap {
                CGEvent.tapEnable(tap: tap, enable: true)
            }
            return
        }
        guard event.getIntegerValueField(.eventSourceUserData) != Self.syntheticEventTag else {
            return
        }
        switch type {
        case .leftMouseDown, .rightMouseDown, .otherMouseDown, .scrollWheel:
            if let (pid, windowID) = WindowResolver.windowAtScreenPoint(event.location) {
                noteInput(Target(pid: pid, windowID: windowID))
            }
        case .keyDown:
            let pid = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
            if let windowID = FocusGuard.currentExactWindowNumber(pid: pid) {
                noteInput(Target(pid: pid, windowID: windowID))
            } else {
                noteInputForWatchedPID(pid)
            }
        default:
            break
        }
    }

    private func noteInput(_ target: Target) {
        lock.lock()
        defer { lock.unlock() }
        guard baselines[target] != nil else { return }
        generations[target, default: 0] &+= 1
    }

    private func noteInputForWatchedPID(_ pid: pid_t) {
        lock.lock()
        defer { lock.unlock() }
        for target in baselines.keys where target.pid == pid {
            generations[target, default: 0] &+= 1
        }
    }
}
