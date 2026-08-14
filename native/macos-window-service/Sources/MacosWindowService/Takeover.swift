import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Exact-target real-HID takeover / target-lost detection.
enum Takeover {
    static func detect(pid: pid_t, windowID: CGWindowID) throws -> ControlState {
        if !WindowResolver.processAlive(pid) {
            return .targetLost
        }
        if !WindowResolver.windowExists(pid: pid, windowID: windowID) {
            return .targetLost
        }

        if try UserInputMonitor.shared.hasInput(pid: pid, windowID: windowID) {
            return .takenOver
        }
        return .none
    }
}
