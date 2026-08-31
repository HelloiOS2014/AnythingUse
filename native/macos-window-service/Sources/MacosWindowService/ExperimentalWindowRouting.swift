import CoreGraphics
import Foundation

/// Opt-in probe for PID + window-directed CGEvent delivery.
enum ExperimentalWindowRouting {
    static var isEnabled: Bool {
        ProcessInfo.processInfo.environment["LCU_MACOS_EXPERIMENTAL_WINDOW_ROUTING"] == "1"
    }

    private static let targetWindowField = CGEventField(rawValue: 51)
    private static let windowRoutingField = CGEventField(rawValue: 58)

    static func stamp(_ event: CGEvent, target: MacWindowTarget) {
        event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(target.pid))
        event.setIntegerValueField(
            .mouseEventWindowUnderMousePointer,
            value: Int64(target.windowID)
        )
        event.setIntegerValueField(
            .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            value: Int64(target.windowID)
        )
        if let targetWindowField {
            event.setIntegerValueField(targetWindowField, value: Int64(target.windowID))
        }
        if let windowRoutingField {
            event.setIntegerValueField(windowRoutingField, value: 1)
        }
    }

    static func selfCheck() -> Bool {
        guard let event = CGEvent(
            mouseEventSource: nil,
            mouseType: .leftMouseDown,
            mouseCursorPosition: .zero,
            mouseButton: .left
        ) else {
            return false
        }
        let target = MacWindowTarget(
            pid: 123,
            windowID: 456,
            title: "probe",
            ownerName: "probe",
            bounds: .zero,
            appId: "probe"
        )
        stamp(event, target: target)
        return event.getIntegerValueField(.eventTargetUnixProcessID) == 123
            && event.getIntegerValueField(.mouseEventWindowUnderMousePointer) == 456
            && event.getIntegerValueField(
                .mouseEventWindowUnderMousePointerThatCanHandleThisEvent
            ) == 456
            && targetWindowField.map { event.getIntegerValueField($0) == 456 } == true
            && windowRoutingField.map { event.getIntegerValueField($0) == 1 } == true
    }
}
