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

        // Soft Screen Recording signal: foreign windows with readable bounds.
        let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
        let foreignWithBounds = list.filter { info in
            guard let ownerPID = info[kCGWindowOwnerPID as String] as? pid_t else { return false }
            if ownerPID == getpid() { return false }
            if let bounds = info[kCGWindowBounds as String] as? [String: Any],
               let w = bounds["Width"] as? CGFloat, w > 1
            {
                return true
            }
            return false
        }
        let screenOK = foreignWithBounds.count >= 1
        if !screenOK {
            notes.append(
                "Screen Recording may be missing. Grant in System Settings → Privacy & Security → Screen & System Audio Recording for macos-window-service."
            )
        }

        return PermissionStatus(
            accessibilityTrusted: ax,
            screenRecordingLikely: screenOK,
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
