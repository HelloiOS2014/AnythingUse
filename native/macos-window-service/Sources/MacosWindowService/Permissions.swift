import ApplicationServices
import CoreGraphics
import Foundation

enum Permissions {
    static func probe() -> PermissionStatus {
        var notes: [String] = []
        let ax = AXIsProcessTrusted()
        if !ax {
            notes.append(
                "Accessibility not trusted for this process. Grant in System Settings → Privacy & Security → Accessibility, then re-run the service."
            )
            let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(opts)
        }

        // Truthful Screen Recording check: native TCC preflight for this
        // process. Never requests or opens Settings — doctor reports the truth.
        let screenOK = CGPreflightScreenCaptureAccess()
        if !screenOK {
            notes.append(
                "Screen Recording not granted for this process. Grant in System Settings → Privacy & Security → Screen & System Audio Recording for macos-window-service."
            )
        }

        let inputOK = CGPreflightListenEventAccess()
        if !inputOK {
            notes.append(
                "Input Monitoring not granted. It is required to distinguish real user HID from tagged AnythingUse input."
                    + " Restart macos-window-service after granting it."
            )
        }

        return PermissionStatus(
            accessibilityTrusted: ax,
            screenRecordingLikely: screenOK,
            inputMonitoringTrusted: inputOK,
            notes: notes
        )
    }

    static func requireAccessibility() throws {
        guard AXIsProcessTrusted() else {
            let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(opts)
            throw ServiceError.permission("Accessibility not granted for this process")
        }
    }
}
