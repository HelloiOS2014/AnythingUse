import Foundation
import AppKit
import CoreGraphics

/// JSON-RPC method handlers for the macOS window control service.
final class Service {
    let store = ElementStore()
    private let lock = NSLock()

    func handle(method: String, params: [String: Any]?) throws -> Any {
        switch method {
        case "ping":
            return [
                "service": "macos-window-service",
                "version": 1,
                "pid": Int(getpid())
            ]
        case "permissions":
            return encodePermissions(Permissions.probe())
        case "list":
            return WindowResolver.listOnScreenWindows().map { encodeTarget($0) }
        case "resolve":
            return try encodeTarget(resolveParams(params))
        case "observe":
            return try observe(params)
        case "semantic":
            return try semantic(params)
        case "targeted":
            return try targeted(params)
        case "detect_conflict", "detect_control_state", "session_health":
            // Single control-state RPC (aliases kept for older adapters).
            return try detectControlState(params)
        default:
            throw ServiceError.invalidRequest("unknown method: \(method)")
        }
    }

    // MARK: - resolve / observe

    private func resolveParams(_ params: [String: Any]?) throws -> MacWindowTarget {
        let p = params ?? [:]
        if let pid = intValue(p["pid"]), let wid = uintValue(p["window_id"]) {
            return try WindowResolver.resolve(pid: pid_t(pid), windowID: CGWindowID(wid))
        }
        return try WindowResolver.resolveSelector(
            appId: p["app_id"] as? String,
            pid: intValue(p["pid"]).map { pid_t($0) },
            windowTitleContains: p["window_title_contains"] as? String
        )
    }

    private func observe(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        let maxW = intValue(params?["max_width"]) ?? 1440
        let maxH = intValue(params?["max_height"]) ?? 900

        if !WindowResolver.processAlive(target.pid) {
            throw ServiceError.targetLost("target process exited")
        }

        lock.lock()
        defer { lock.unlock() }

        let accessibility = AXBridge.enableAccessibility(pid: target.pid)
        defer { accessibility.disable() }
        var elements: [[String: Any]] = []
        do {
            elements = try store.observeElements(target: target)
        } catch {
            // AX-only may fail; still try capture.
            elements = []
        }
        if elements.isEmpty {
            // Avoid touching the global compatibility preference for ordinary
            // native apps. Retry only when the canonical AX walk produced no UI.
            let fallback = AXBridge.enableAccessibility(pid: target.pid, allowFallback: true)
            defer { fallback.disable() }
            elements = (try? store.observeElements(target: target)) ?? []
        }

        var captureBackend: String?
        var imageB64: String?
        var imageWidth = max(1, Int(target.bounds.width))
        var imageHeight = max(1, Int(target.bounds.height))
        var imageHash: String?

        do {
            let shot = try awaitMain {
                try await WindowCapture.captureWindow(windowID: target.windowID)
            }
            // Downscale for model budget is handled by returning full window PNG;
            // adapter may resize. Cap huge captures via note only.
            _ = (maxW, maxH)
            imageB64 = shot.pngData.base64EncodedString()
            imageWidth = shot.width
            imageHeight = shot.height
            captureBackend = shot.backend
            imageHash = sha256Hex(shot.pngData)
        } catch {
            // AX-only observation is still useful.
            captureBackend = nil
        }

        return [
            "target": encodeTarget(target),
            "window_frame": [
                "x": target.bounds.origin.x,
                "y": target.bounds.origin.y,
                "width": target.bounds.width,
                "height": target.bounds.height
            ],
            "model_size": [
                "width": imageWidth,
                "height": imageHeight
            ],
            "elements": elements,
            "image_png_b64": imageB64 as Any,
            "image_hash": imageHash as Any,
            "capture_backend": captureBackend as Any,
            "control_state": Takeover.detect(pid: target.pid, windowID: target.windowID).rawValue
        ]
    }

    // MARK: - semantic

    private func semantic(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        try ensureNotBlocking(target)
        let accessibility = AXBridge.enableAccessibility(pid: target.pid)
        defer { accessibility.disable() }

        guard let action = params?["action"] as? [String: Any],
              let type = action["type"] as? String
        else {
            throw ServiceError.invalidRequest("semantic requires action.type")
        }

        return try FocusGuard.withoutFrontmostSteal(target: target) {
            lock.lock()
            defer { lock.unlock() }

            switch type {
            case "invoke":
                let elementId = try requireString(action, "element_id")
                let el = try store.resolveElement(target: target, elementId: elementId)
                let metadata = try store.metadata(for: elementId)
                if metadata.actions.contains("AXConfirm")
                    && !metadata.actions.contains("AXPress")
                    && (metadata.role.contains("Text") || metadata.role.contains("Field"))
                {
                    _ = AXBridge.syntheticFocus(target: target, element: el)
                    let returnPath = try DirectedInput.pressReturn(target: target)
                    return okAction(
                        path: returnPath,
                        detail: "confirm editable \(elementId) with directed Return"
                    )
                }
                if !metadata.actions.contains("AXPress")
                    && !metadata.actions.contains("AXConfirm")
                    && metadata.frame.width > 0
                    && metadata.frame.height > 0
                {
                    // Element-bound fallback used by native Computer Use: click the
                    // observed node through postToPid, never a model-invented point.
                    let report = try DirectedInput.click(
                        target: target,
                        normalizedX: metadata.frame.midX,
                        normalizedY: metadata.frame.midY
                    )
                    return okAction(path: report.path, detail: "invoke \(elementId): \(report.detail)")
                }
                let usedPress = try AXBridge.press(el)
                return okAction(
                    path: usedPress ? "ax_press" : "ax_confirm",
                    detail: "invoke \(elementId)"
                )

            case "set_value":
                let elementId = try requireString(action, "element_id")
                let value = try requireString(action, "value")
                let el = try store.resolveElement(target: target, elementId: elementId)
                let metadata = try store.metadata(for: elementId)

                // Editable Chromium-style controls often report AXSetValue success
                // without dispatching input/change events. Use an element-bound,
                // process-directed click and real typing so application code sees
                // the same interaction, while the user's cursor/focus stay put.
                if (metadata.role.contains("Text") || metadata.role.contains("Field"))
                    && metadata.frame.width > 0
                    && metadata.frame.height > 0
                {
                    // Click first: it is the main fail-closed rejection point
                    // (key-window gate), and a rejected click must not have cleared
                    // the user's content. Clear only after the click landed.
                    let click = try DirectedInput.click(
                        target: target,
                        normalizedX: metadata.frame.midX,
                        normalizedY: metadata.frame.midY
                    )
                    try AXBridge.setValue(el, "")
                    let typePath = value.isEmpty
                        ? "empty"
                        : try DirectedInput.typeUnicode(target: target, text: value)
                    return okAction(
                        path: "\(click.path)+\(typePath)",
                        detail: "set_value \(elementId) len=\(value.count)"
                    )
                }

                // Background-first: AX setValue without requiring focus.
                do {
                    try AXBridge.setValue(el, value)
                    let readback = AXBridge.getValue(el)
                    if readback.contains(value) || readback == value {
                        return okAction(path: "ax_set_value", detail: "set_value \(elementId)")
                    }
                } catch {
                    // Fall through to directed type only when key window proven.
                }
                _ = try DirectedInput.typeUnicode(target: target, text: value)
                return okAction(
                    path: "cgevent_post_to_pid_type",
                    detail: "set_value/type \(elementId) len=\(value.count)"
                )

            case "focus":
                let elementId = try requireString(action, "element_id")
                let el = try store.resolveElement(target: target, elementId: elementId)
                // Soft-skip when not key window so the product loop can continue with
                // set_value/invoke. Never activate, never re-key, never fail the task
                // solely because the model asked for focus in the background.
                guard FocusGuard.isTargetKeyWindow(target: target) else {
                    return okAction(
                        path: "focus_noop_background",
                        detail: "focus \(elementId) skipped: target not key window "
                            + "(pid=\(target.pid) window_id=\(target.windowID)); "
                            + "will not activate or re-key"
                    )
                }
                let ok = AXBridge.syntheticFocus(target: target, element: el)
                if !ok {
                    return okAction(
                        path: "focus_noop_failed",
                        detail: "focus \(elementId) no-op: synthetic focus failed without raise"
                    )
                }
                return okAction(path: "ax_synthetic_focus", detail: "focus \(elementId)")

            case "scroll":
                let deltaY = doubleValue(action["delta_y"]) ?? -0.15
                let lines = Int32((deltaY * 20).rounded())
                let report = try DirectedInput.scroll(target: target, lines: lines == 0 ? -3 : lines)
                return okAction(path: report.path, detail: report.detail)

            default:
                throw ServiceError.unsupported("unknown semantic action type: \(type)")
            }
        }
    }

    // MARK: - targeted (window/PID directed; never global HID mouse-move)

    private func targeted(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        try ensureNotBlocking(target)
        let accessibility = AXBridge.enableAccessibility(pid: target.pid)
        defer { accessibility.disable() }

        guard let action = params?["action"] as? [String: Any],
              let type = action["type"] as? String
        else {
            throw ServiceError.invalidRequest("targeted requires action.type")
        }

        return try FocusGuard.withoutFrontmostSteal(target: target) {
            switch type {
            case "click":
                let x = doubleValue(action["x"]) ?? 0.5
                let y = doubleValue(action["y"]) ?? 0.5
                // Hit requested coordinates (or element under that point) — never first pressable.
                let report = try DirectedInput.click(target: target, normalizedX: x, normalizedY: y)
                return okAction(path: report.path, detail: report.detail)

            case "type_text":
                let text = try requireString(action, "text")
                let typePath = try DirectedInput.typeUnicode(target: target, text: text)
                return okAction(
                    path: typePath,
                    detail: "typed \(text.count) chars → pid \(target.pid) window_id=\(target.windowID)"
                )

            case "key_combo":
                guard let keys = action["keys"] as? [String], keys.count == 1 else {
                    throw ServiceError.unsupported("only one directed key is supported")
                }
                let key = keys[0].uppercased()
                guard key == "RETURN" || key == "ENTER" else {
                    throw ServiceError.unsupported("directed key not supported: \(keys[0])")
                }
                let returnPath = try DirectedInput.pressReturn(target: target)
                return okAction(
                    path: returnPath,
                    detail: "Return → pid \(target.pid) window_id=\(target.windowID)"
                )

            default:
                throw ServiceError.unsupported("unknown targeted action type: \(type)")
            }
        }
    }

    // MARK: - control state

    /// Unified control-state / health probe (replaces separate session_health RPC).
    private func detectControlState(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        let alive = WindowResolver.processAlive(target.pid)
        let exists = WindowResolver.windowExists(pid: target.pid, windowID: target.windowID)
        let state = Takeover.detect(pid: target.pid, windowID: target.windowID)
        return [
            "control_state": state.rawValue,
            // Legacy ConflictState mapping for PlatformBackend adapters:
            // taken_over → user_active_in_target; target_lost → target_lost; none → none
            "conflict": conflictAlias(state),
            "target_process_alive": alive,
            "window_exists": exists,
            "target": [
                "pid": Int(target.pid),
                "window_id": Int(target.windowID)
            ]
        ]
    }

    // MARK: - helpers

    private func resolveTargetRequired(_ params: [String: Any]?) throws -> MacWindowTarget {
        let p = params ?? [:]
        guard let pid = intValue(p["pid"]), let wid = uintValue(p["window_id"]) else {
            // Allow resolve-style params for convenience.
            return try resolveParams(params)
        }
        return try WindowResolver.resolve(pid: pid_t(pid), windowID: CGWindowID(wid))
    }

    private func ensureNotBlocking(_ target: MacWindowTarget) throws {
        let state = Takeover.detect(pid: target.pid, windowID: target.windowID)
        switch state {
        case .takenOver:
            throw ServiceError.takenOver(
                "user focused the same window (pid=\(target.pid) window_id=\(target.windowID))"
            )
        case .targetLost:
            throw ServiceError.targetLost("target process/window lost")
        case .none:
            return
        }
    }

    private func okAction(path: String, detail: String) -> [String: Any] {
        [
            "success": true,
            "path": path,
            "detail": detail,
            "global_mouse_moved": false
        ]
    }

    private func encodeTarget(_ t: MacWindowTarget) -> [String: Any] {
        [
            "app_id": t.appId,
            "pid": Int(t.pid),
            "window_id": Int(t.windowID),
            "window_title": t.title,
            "owner_name": t.ownerName,
            "bounds": [
                "x": t.bounds.origin.x,
                "y": t.bounds.origin.y,
                "width": t.bounds.width,
                "height": t.bounds.height
            ]
        ]
    }

    private func encodePermissions(_ p: PermissionStatus) -> [String: Any] {
        [
            "accessibility": p.accessibilityTrusted ? "granted" : "denied",
            "screen_recording": p.screenRecordingLikely ? "granted" : "denied",
            "input_monitoring": "not_determined",
            "notes": p.notes
        ]
    }

    private func conflictAlias(_ s: ControlState) -> String {
        switch s {
        case .none: return "none"
        case .takenOver: return "user_active_in_target"
        case .targetLost: return "target_lost"
        }
    }

    private func requireString(_ d: [String: Any], _ key: String) throws -> String {
        guard let s = d[key] as? String else {
            throw ServiceError.invalidRequest("missing \(key)")
        }
        return s
    }

    private func intValue(_ v: Any?) -> Int? {
        if let i = v as? Int { return i }
        if let i = v as? Int64 { return Int(i) }
        if let n = v as? NSNumber { return n.intValue }
        if let s = v as? String { return Int(s) }
        return nil
    }

    private func uintValue(_ v: Any?) -> UInt32? {
        intValue(v).map { UInt32(clamping: $0) }
    }

    private func doubleValue(_ v: Any?) -> Double? {
        if let d = v as? Double { return d }
        if let i = v as? Int { return Double(i) }
        if let n = v as? NSNumber { return n.doubleValue }
        if let s = v as? String { return Double(s) }
        return nil
    }

    private func sha256Hex(_ data: Data) -> String {
        // Minimal SHA-256 without CryptoKit dependency complications on all targets.
        // Use CommonCrypto via Security.
        var hash = [UInt8](repeating: 0, count: 32)
        data.withUnsafeBytes { buf in
            _ = CC_SHA256(buf.baseAddress, CC_LONG(data.count), &hash)
        }
        return hash.map { String(format: "%02x", $0) }.joined()
    }
}

import CommonCrypto

// MARK: - awaitMain (same as P1 spike: pump RunLoop for MainActor)

func awaitMain<T>(_ body: @escaping @MainActor () async throws -> T) throws -> T {
    let box = ConcurrentBox<Result<T, Error>>()
    Task { @MainActor in
        do {
            box.value = .success(try await body())
        } catch {
            box.value = .failure(error)
        }
    }
    let deadline = Date().addingTimeInterval(30)
    while box.value == nil {
        if Date() > deadline {
            throw ServiceError.actionFailed("awaitMain timed out after 30s")
        }
        RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.05))
    }
    switch box.value! {
    case .success(let v): return v
    case .failure(let e): throw e
    }
}

final class ConcurrentBox<T>: @unchecked Sendable {
    var value: T?
}
