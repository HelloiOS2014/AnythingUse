import AppKit
import CoreGraphics
import Foundation
import ScreenCaptureKit

/// Window-level observation via ScreenCaptureKit (PID/WindowID target).
enum WindowCapture {
    struct CaptureResult {
        var pngData: Data
        var width: Int
        var height: Int
        var backend: String
    }

    @MainActor
    static func captureWindow(windowID: CGWindowID, scale: CGFloat = 1.0) async throws -> CaptureResult {
        // Prefer ScreenCaptureKit with a hard timeout — first-use TCC prompts can hang forever.
        do {
            return try await withThrowingTimeout(seconds: 8) {
                try await captureWithScreenCaptureKit(windowID: windowID, scale: scale)
            }
        } catch {
            // Fallback: CGWindowListCreateImage still proves window-id observation when SCK is denied/slow.
            do {
                return try captureWithCGWindowList(windowID: windowID)
            } catch {
                throw ServiceError.permission(
                    "window capture failed (SCK: \(error); CG fallback also failed). Grant Screen Recording."
                )
            }
        }
    }

    @MainActor
    private static func captureWithScreenCaptureKit(windowID: CGWindowID, scale: CGFloat) async throws -> CaptureResult {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let window = content.windows.first(where: { $0.windowID == windowID }) else {
            throw ServiceError.notFound("SCShareableContent has no windowID=\(windowID)")
        }

        let filter = SCContentFilter(desktopIndependentWindow: window)
        let config = SCStreamConfiguration()
        let w = max(1, Int(window.frame.width * scale))
        let h = max(1, Int(window.frame.height * scale))
        config.width = w
        config.height = h
        config.showsCursor = false
        config.captureResolution = .best

        let image = try await SCScreenshotManager.captureImage(
            contentFilter: filter,
            configuration: config
        )
        guard let png = Self.encodePNG(image) else {
            throw ServiceError.actionFailed("SCK image PNG encode failed")
        }
        return CaptureResult(pngData: png, width: image.width, height: image.height, backend: "ScreenCaptureKit")
    }

    private static func withThrowingTimeout<T: Sendable>(
        seconds: Double,
        operation: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask {
                try await operation()
            }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                throw ServiceError.actionFailed("capture timed out after \(seconds)s")
            }
            guard let first = try await group.next() else {
                throw ServiceError.actionFailed("capture task group empty")
            }
            group.cancelAll()
            return first
        }
    }

    private static func captureWithCGWindowList(windowID: CGWindowID) throws -> CaptureResult {
        // CGWindowListCreateImage is obsoleted on newer SDKs; call only when available.
        if #available(macOS 15.0, *) {
            throw ServiceError.unsupported(
                "CGWindowListCreateImage unavailable on this OS; Screen Recording/SCK required"
            )
        }
        guard let cgImage = CGWindowListCreateImage(
            .null,
            .optionIncludingWindow,
            windowID,
            [.boundsIgnoreFraming, .bestResolution]
        ) else {
            throw ServiceError.permission(
                "CGWindowListCreateImage failed for windowID=\(windowID) (Screen Recording?)"
            )
        }
        guard let png = encodePNG(cgImage) else {
            throw ServiceError.actionFailed("CGWindow PNG encode failed")
        }
        return CaptureResult(
            pngData: png,
            width: cgImage.width,
            height: cgImage.height,
            backend: "CGWindowListCreateImage_fallback"
        )
    }

    private static func encodePNG(_ image: CGImage) -> Data? {
        let rep = NSBitmapImageRep(cgImage: image)
        return rep.representation(using: .png, properties: [:])
    }
}
