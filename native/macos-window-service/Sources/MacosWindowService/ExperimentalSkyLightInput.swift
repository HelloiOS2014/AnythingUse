import AppKit
import CoreGraphics
import Darwin
import Foundation

// Experimental, explicit opt-in only. Derived from the MIT-licensed
// open-codex-computer-use target-only sky_click path; see THIRD_PARTY_NOTICES.md.
// Never resolve or notify the real foreground process: synthetic focus belongs
// only to the requested target and is removed after the click.

struct SkyLightActivationCommand: Equatable {
    let psn: [UInt8]
    let windowID: CGWindowID
    let focused: Bool
}

struct SkyLightSyntheticFocusPlan: Equatable {
    let activateTarget: SkyLightActivationCommand
    let deactivateTarget: SkyLightActivationCommand
}

func skyLightSyntheticTargetFocusPlan(
    targetPSN: [UInt8],
    targetWindowID: CGWindowID
) -> SkyLightSyntheticFocusPlan {
    SkyLightSyntheticFocusPlan(
        activateTarget: SkyLightActivationCommand(
            psn: targetPSN,
            windowID: targetWindowID,
            focused: true
        ),
        deactivateTarget: SkyLightActivationCommand(
            psn: targetPSN,
            windowID: targetWindowID,
            focused: false
        )
    )
}

func skyLightActivationRecord(windowID: CGWindowID, focused: Bool) -> [UInt8] {
    var record = [UInt8](repeating: 0, count: 0xF8)
    record[0x04] = 0xF8
    record[0x08] = 0x0D
    record[0x3C] = UInt8(truncatingIfNeeded: windowID)
    record[0x3D] = UInt8(truncatingIfNeeded: windowID >> 8)
    record[0x3E] = UInt8(truncatingIfNeeded: windowID >> 16)
    record[0x3F] = UInt8(truncatingIfNeeded: windowID >> 24)
    record[0x8A] = focused ? 0x01 : 0x02
    return record
}

enum ExperimentalSkyLightInput {
    static var isEnabled: Bool {
        ProcessInfo.processInfo.environment["LCU_MACOS_EXPERIMENTAL_SKY_INPUT"] == "1"
    }

    private static let dispatchLock = NSLock()
    private static let spi = SPI()

    static func click(target: MacWindowTarget, screenPoint: CGPoint) throws {
        guard isEnabled else {
            throw ServiceError.unsupported("experimental SkyLight click is disabled")
        }
        guard target.bounds.contains(screenPoint) else {
            throw ServiceError.invalidRequest("SkyLight click point is outside target window")
        }
        try withSyntheticTargetFocus(target: target) {
            try postClick(target: target, screenPoint: screenPoint)
        }
    }

    static func withSyntheticTargetFocus<T>(
        target: MacWindowTarget,
        _ body: () throws -> T
    ) throws -> T {
        guard spi.isAvailable else {
            throw ServiceError.unsupported(
                "experimental SkyLight input unavailable: missing \(spi.missingSymbols.joined(separator: ", "))"
            )
        }
        dispatchLock.lock()
        defer { dispatchLock.unlock() }

        guard !FocusGuard.isFrontmost(pid: target.pid) else {
            throw ServiceError.unsupported(
                "experimental SkyLight input refused: user is active in target pid=\(target.pid)"
            )
        }
        try validateWindow(target)
        let plan = try spi.focusPlan(target: target)
        try spi.post(plan.activateTarget)
        usleep(40_000)

        do {
            let result = try body()
            usleep(100_000)
            try spi.post(plan.deactivateTarget)
            usleep(40_000)
            return result
        } catch {
            try? spi.post(plan.deactivateTarget)
            throw error
        }
    }

    static func validateWindow(_ target: MacWindowTarget) throws {
        let info = CGWindowListCopyWindowInfo(
            [.optionIncludingWindow],
            target.windowID
        ) as? [[String: Any]] ?? []
        let matches = info.contains { window in
            (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value == target.windowID
                && (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == target.pid
                && (window[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true
        }
        guard matches else {
            throw ServiceError.notFound(
                "SkyLight target is stale, off-screen, or no longer owned by pid=\(target.pid)"
            )
        }
    }

    static func postKeyboard(_ event: CGEvent, pid: pid_t) throws {
        try spi.postKeyboard(event, pid: pid)
    }

    private static func postClick(target: MacWindowTarget, screenPoint: CGPoint) throws {
        guard let source = CGEventSource(stateID: .hidSystemState) else {
            throw ServiceError.actionFailed("SkyLight CGEventSource create failed")
        }
        let local = CGPoint(
            x: screenPoint.x - target.bounds.minX,
            y: screenPoint.y - target.bounds.minY
        )
        let group = Int64(DispatchTime.now().uptimeNanoseconds % 1_000_000_000)
        let primer = CGPoint(x: -1, y: -1)
        let steps: [(CGEventType, CGPoint, CGPoint, Int64, Int64, useconds_t)] = [
            (.mouseMoved, screenPoint, local, 0, 2, 15_000),
            (.leftMouseDown, primer, primer, 1, 1, 1_000),
            (.leftMouseUp, primer, primer, 1, 2, 100_000),
            (.leftMouseDown, screenPoint, local, 1, 3, 1_000),
            (.leftMouseUp, screenPoint, local, 1, 3, 0),
        ]

        for (type, point, windowPoint, clickState, phase, delay) in steps {
            guard let event = CGEvent(
                mouseEventSource: source,
                mouseType: type,
                mouseCursorPosition: point,
                mouseButton: .left
            ) else {
                throw ServiceError.actionFailed("SkyLight mouse event create failed")
            }
            try spi.stamp(
                event,
                pid: target.pid,
                windowID: target.windowID,
                windowPoint: windowPoint,
                clickState: clickState,
                phase: phase,
                clickGroupID: group
            )
            try spi.post(event, pid: target.pid)
            event.postToPid(target.pid)
            if delay > 0 { usleep(delay) }
        }
    }

    private final class SPI: @unchecked Sendable {
        private typealias PostToPid = @convention(c) (pid_t, UnsafeMutableRawPointer?) -> Void
        private typealias SetInteger = @convention(c) (UnsafeMutableRawPointer?, UInt32, Int64) -> Void
        private typealias SetWindowLocation = @convention(c) (UnsafeMutableRawPointer?, Double, Double) -> Void
        private typealias SetAuthentication = @convention(c) (UnsafeMutableRawPointer?, UnsafeMutableRawPointer?) -> Void
        private typealias PostRecord = @convention(c) (UnsafeRawPointer?, UnsafePointer<UInt8>?) -> Int32
        private typealias GetProcessForPID = @convention(c) (pid_t, UnsafeMutableRawPointer?) -> Int32
        private typealias ObjCGetClass = @convention(c) (UnsafePointer<CChar>?) -> UnsafeMutableRawPointer?
        private typealias SelRegisterName = @convention(c) (UnsafePointer<CChar>?) -> UnsafeMutableRawPointer?
        private typealias ClassResponds = @convention(c) (UnsafeMutableRawPointer?, UnsafeMutableRawPointer?) -> Bool
        private typealias FactoryMessage = @convention(c) (
            UnsafeMutableRawPointer?,
            UnsafeMutableRawPointer?,
            UnsafeMutableRawPointer?,
            Int32,
            UInt32
        ) -> UnsafeMutableRawPointer?

        private let postToPid: PostToPid?
        private let setInteger: SetInteger?
        private let setWindowLocation: SetWindowLocation?
        private let setAuthentication: SetAuthentication?
        private let postRecord: PostRecord?
        private let getProcessForPID: GetProcessForPID?
        private let objcGetClass: ObjCGetClass?
        private let selRegisterName: SelRegisterName?
        private let classResponds: ClassResponds?
        private let factoryMessage: FactoryMessage?
        let missingSymbols: [String]

        var isAvailable: Bool { missingSymbols.isEmpty }

        init() {
            let sky = dlopen(
                "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight",
                RTLD_LAZY | RTLD_GLOBAL
            )
            let appServices = dlopen(
                "/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices",
                RTLD_LAZY | RTLD_GLOBAL
            )
            let objc = dlopen("/usr/lib/libobjc.A.dylib", RTLD_LAZY | RTLD_GLOBAL)
            postToPid = Self.resolve(sky, "SLEventPostToPid")
            setInteger = Self.resolve(sky, "SLEventSetIntegerValueField")
            setWindowLocation = Self.resolve(sky, "CGEventSetWindowLocation")
            setAuthentication = Self.resolve(sky, "SLEventSetAuthenticationMessage")
            postRecord = Self.resolve(sky, "SLPSPostEventRecordTo")
            getProcessForPID = Self.resolve(appServices, "GetProcessForPID")
            objcGetClass = Self.resolve(objc, "objc_getClass")
            selRegisterName = Self.resolve(objc, "sel_registerName")
            classResponds = Self.resolve(objc, "class_respondsToSelector")
            factoryMessage = Self.resolve(objc, "objc_msgSend")

            var missing: [String] = []
            if postToPid == nil { missing.append("SLEventPostToPid") }
            if setInteger == nil { missing.append("SLEventSetIntegerValueField") }
            if setWindowLocation == nil { missing.append("CGEventSetWindowLocation") }
            if postRecord == nil { missing.append("SLPSPostEventRecordTo") }
            if getProcessForPID == nil { missing.append("GetProcessForPID") }
            missingSymbols = missing
        }

        func focusPlan(target: MacWindowTarget) throws -> SkyLightSyntheticFocusPlan {
            guard let getProcessForPID else {
                throw ServiceError.unsupported("GetProcessForPID unavailable")
            }
            var psn = [UInt8](repeating: 0, count: 8)
            let status = psn.withUnsafeMutableBytes {
                getProcessForPID(target.pid, $0.baseAddress)
            }
            guard status == 0 else {
                throw ServiceError.actionFailed(
                    "cannot resolve target pid=\(target.pid) to PSN (OSStatus \(status))"
                )
            }
            return skyLightSyntheticTargetFocusPlan(
                targetPSN: psn,
                targetWindowID: target.windowID
            )
        }

        func post(_ command: SkyLightActivationCommand) throws {
            guard let postRecord else {
                throw ServiceError.unsupported("SLPSPostEventRecordTo unavailable")
            }
            let record = skyLightActivationRecord(
                windowID: command.windowID,
                focused: command.focused
            )
            let status = command.psn.withUnsafeBytes { psn in
                record.withUnsafeBufferPointer {
                    postRecord(psn.baseAddress, $0.baseAddress)
                }
            }
            guard status == 0 else {
                throw ServiceError.actionFailed(
                    "target-only synthetic focus failed (OSStatus \(status))"
                )
            }
        }

        func stamp(
            _ event: CGEvent,
            pid: pid_t,
            windowID: CGWindowID,
            windowPoint: CGPoint,
            clickState: Int64,
            phase: Int64,
            clickGroupID: Int64
        ) throws {
            guard let setInteger, let setWindowLocation else {
                throw ServiceError.unsupported("SkyLight event stamping unavailable")
            }
            let eventPointer = Unmanaged.passUnretained(event).toOpaque()
            let window = Int64(windowID)
            for (field, value): (UInt32, Int64) in [
                (0, phase), (1, clickState), (3, 0), (7, 3),
                (40, Int64(pid)), (51, window), (58, clickGroupID),
                (91, window), (92, window),
            ] {
                setInteger(eventPointer, field, value)
            }
            setWindowLocation(eventPointer, windowPoint.x, windowPoint.y)
        }

        func post(_ event: CGEvent, pid: pid_t) throws {
            guard let postToPid else {
                throw ServiceError.unsupported("SLEventPostToPid unavailable")
            }
            postToPid(pid, Unmanaged.passUnretained(event).toOpaque())
        }

        func postKeyboard(_ event: CGEvent, pid: pid_t) throws {
            guard let postToPid else {
                throw ServiceError.unsupported("SLEventPostToPid unavailable")
            }
            let eventPointer = Unmanaged.passUnretained(event).toOpaque()
            attachAuthentication(to: eventPointer, pid: pid)
            postToPid(pid, eventPointer)
        }

        private func attachAuthentication(to event: UnsafeMutableRawPointer, pid: pid_t) {
            guard
                let setAuthentication,
                let objcGetClass,
                let selRegisterName,
                let classResponds,
                let factoryMessage
            else { return }

            let cls = "SLSEventAuthenticationMessage".withCString { objcGetClass($0) }
            let selector = "messageWithEventRecord:pid:version:".withCString {
                selRegisterName($0)
            }
            guard
                let cls,
                let selector,
                classResponds(cls, selector),
                let record = Self.eventRecord(in: event),
                let message = factoryMessage(cls, selector, record, pid, 0)
            else { return }
            setAuthentication(event, message)
        }

        private static func eventRecord(in event: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer? {
            // __CGEvent stores its SLSEventRecord pointer after CFRuntimeBase.
            // Keep the same bounded probes as Cua for OS layout compatibility.
            for offset in [24, 32, 16] {
                let pointer = event
                    .advanced(by: offset)
                    .assumingMemoryBound(to: UnsafeMutableRawPointer?.self)
                    .pointee
                if let pointer { return pointer }
            }
            return nil
        }

        private static func resolve<T>(_ handle: UnsafeMutableRawPointer?, _ symbol: String) -> T? {
            guard let handle, let pointer = dlsym(handle, symbol) else { return nil }
            return unsafeBitCast(pointer, to: T.self)
        }
    }
}
