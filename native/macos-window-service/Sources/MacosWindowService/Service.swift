import Foundation
import AppKit
import CoreGraphics
import Security

/// JSON-RPC method handlers for the macOS window control service.
final class Service {
    private struct SemanticSnapshot {
        let pid: pid_t
        let windowID: CGWindowID
        let size: CGSize
        let imageHash: String
        let elements: [[String: Any]]
    }

    let store = ElementStore()
    private let lock = NSLock()
    private var semanticSnapshot: SemanticSnapshot?

    func handle(method: String, params: [String: Any]?) throws -> Any {
        switch method {
        case "ping":
            return [
                "service": "macos-window-service",
                "version": 1,
                "pid": Int(getpid()),
                "session_epoch": UserInputMonitor.shared.sessionEpoch
            ]
        case "permissions":
            return encodePermissions(Permissions.probe())
        case "list":
            return WindowResolver.listOnScreenWindows().map { encodeTarget($0) }
        case "resolve":
            return try encodeTarget(resolveParams(params))
        case "launch":
            return try launch(params)
        case "app_identity":
            return try appIdentity(params)
        case "set_takeover_watch":
            let target = try resolveTargetRequired(params)
            let active = (params?["active"] as? Bool) ?? false
            if active {
                try UserInputMonitor.shared.arm(pid: target.pid, windowID: target.windowID)
            } else {
                UserInputMonitor.shared.clear(pid: target.pid, windowID: target.windowID)
            }
            return ["ok": true, "active": active]
        case "observe":
            return try observe(params)
        case "semantic":
            return try semantic(params)
        case "targeted":
            return try targeted(params)
        case "foreground_activate":
            return try foregroundActivate(params)
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

    /// Launch an explicitly selected app without taking frontmost, then resolve
    /// the first window it publishes. Existing windows are returned unchanged.
    private func launch(_ params: [String: Any]?) throws -> [String: Any] {
        let p = params ?? [:]
        guard let appID = p["app_id"] as? String, !appID.isEmpty else {
            throw ServiceError.invalidRequest("launch requires app_id bundle identifier")
        }
        if let target = try? WindowResolver.resolveSelector(
            appId: appID,
            pid: nil,
            windowTitleContains: p["window_title_contains"] as? String
        ) {
            return encodeTarget(target)
        }
        guard let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: appID) else {
            throw ServiceError.notFound("no installed application for bundle id \(appID)")
        }

        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        configuration.createsNewApplicationInstance = false
        let completion = DispatchSemaphore(value: 0)
        var launchError: Error?
        NSWorkspace.shared.openApplication(at: appURL, configuration: configuration) { _, error in
            launchError = error
            completion.signal()
        }
        guard completion.wait(timeout: .now() + 5) == .success else {
            throw ServiceError.actionFailed("launch timed out for \(appID)")
        }
        if let launchError {
            throw ServiceError.actionFailed("launch failed for \(appID): \(launchError)")
        }

        for _ in 0..<50 {
            if let target = try? WindowResolver.resolveSelector(
                appId: appID,
                pid: nil,
                windowTitleContains: p["window_title_contains"] as? String
            ) {
                var result = encodeTarget(target)
                result["launched"] = true
                return result
            }
            usleep(100_000)
        }
        throw ServiceError.notFound("\(appID) launched but published no controllable window")
    }

    private func appIdentity(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        guard let app = NSRunningApplication(processIdentifier: target.pid) else {
            throw ServiceError.targetLost("cannot resolve application identity for pid \(target.pid)")
        }
        let bundleID = app.bundleIdentifier ?? target.appId
        let path = app.bundleURL?.resolvingSymlinksInPath().standardizedFileURL.path ?? ""
        let attributes = [kSecGuestAttributePid as String: NSNumber(value: target.pid)] as CFDictionary
        var code: SecCode?
        var staticCode: SecStaticCode?
        var signingInfo: CFDictionary?
        if SecCodeCopyGuestWithAttributes(nil, attributes, [], &code) == errSecSuccess,
           let code,
           SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess,
           let staticCode,
           SecStaticCodeCheckValidity(staticCode, SecCSFlags(rawValue: kSecCSCheckAllArchitectures), nil) == errSecSuccess,
           SecCodeCopySigningInformation(staticCode, SecCSFlags(rawValue: kSecCSSigningInformation), &signingInfo) == errSecSuccess,
           let info = signingInfo as? [String: Any],
           let teamID = info[kSecCodeInfoTeamIdentifier as String] as? String,
           !teamID.isEmpty,
           let codeHash = info[kSecCodeInfoUnique as String] as? Data,
           !codeHash.isEmpty
        {
            let signingID = (info[kSecCodeInfoIdentifier as String] as? String) ?? bundleID
            let codeHashHex = codeHash.map { String(format: "%02x", $0) }.joined()
            return [
                "stable_key": "mac:\(bundleID):team:\(teamID):signing:\(signingID):cdhash:\(codeHashHex)",
                "bundle_id": bundleID,
                "team_id": teamID,
                "signing_id": signingID,
                "code_hash": codeHashHex,
                "signed": true
            ]
        }
        guard let executableURL = app.executableURL else {
            throw ServiceError.permission("unsigned app identity has no executable path")
        }
        let executableHash = try sha256File(executableURL)
        return [
            "stable_key": "mac:\(bundleID):unsigned:\(path):sha256:\(executableHash)",
            "bundle_id": bundleID,
            "path": path,
            "executable_sha256": executableHash,
            "signed": false
        ]
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
        var semanticError: String?
        do {
            elements = try store.observeElements(target: target)
        } catch {
            // AX-only may fail; still try capture.
            semanticError = String(String(describing: error).prefix(4096))
        }
        if elements.isEmpty {
            // Bounded settle retry: some renderer-backed apps accept AX
            // enablement only after a short grace period. Retry only when the
            // canonical AX walk produced no UI.
            let settle = AXBridge.enableAccessibility(pid: target.pid, allowFallback: true)
            defer { settle.disable() }
            do {
                elements = try store.observeElements(target: target)
                semanticError = nil
            } catch {
                semanticError = String(String(describing: error).prefix(4096))
            }
            settle.disable()
        }
        var captureBackend: String?
        var captureError: String?
        var imageB64: String?
        var imageWidth = max(1, Int(target.bounds.width))
        var imageHeight = max(1, Int(target.bounds.height))
        var imageHash: String?
        var semanticBackend = elements.isEmpty ? "none" : "ax"

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
            // ScreenCaptureKit failures are often transient (window bounds
            // changing mid-capture, first-use). One immediate retry before
            // falling back to AX-only; a second failure is likely persistent.
            do {
                let retry = try awaitMain({
                    try await WindowCapture.captureWindow(windowID: target.windowID)
                })
                imageB64 = retry.pngData.base64EncodedString()
                imageWidth = retry.width
                imageHeight = retry.height
                captureBackend = retry.backend
                imageHash = sha256Hex(retry.pngData)
            } catch let finalError {
                // AX-only observation is still useful, but keep the final
                // capture error so callers never mistake it for a healthy
                // observation.
                captureError = String(String(describing: finalError).prefix(4096))
                captureBackend = nil
            }
        }

        // Finder and other background apps may temporarily omit an unchanged
        // rendered window from AXWindows. Reuse the last semantic snapshot only
        // when target identity, window size and exact screenshot pixels match,
        // and only while the native element cache is still bound to that target.
        if elements.isEmpty,
           let imageHash,
           let snapshot = semanticSnapshot,
           snapshot.pid == target.pid,
           snapshot.windowID == target.windowID,
           abs(snapshot.size.width - target.bounds.width) <= 1,
           abs(snapshot.size.height - target.bounds.height) <= 1,
           snapshot.imageHash == imageHash,
           store.matches(pid: target.pid, windowID: target.windowID)
        {
            elements = snapshot.elements
            semanticBackend = "ax_cached_same_pixels"
            semanticError = nil
        } else if !elements.isEmpty, let imageHash {
            semanticSnapshot = SemanticSnapshot(
                pid: target.pid,
                windowID: target.windowID,
                size: target.bounds.size,
                imageHash: imageHash,
                elements: elements
            )
        }

        // Both observation channels empty: never return a false healthy
        // observation. ScreenCaptureKit errors are already typed permission
        // failures, so surface that with the capture detail.
        if elements.isEmpty {
            let msg =
                "observe empty AX pid=\(target.pid) window=\(target.windowID) "
                    + "semantic=\(semanticError ?? "none") capture=\(captureError ?? "ok")\n"
            fputs("macos-window-service \(msg)", stderr)
            let logDir = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Application Support/AnythingUse/logs")
            try? FileManager.default.createDirectory(at: logDir, withIntermediateDirectories: true)
            let logUrl = logDir.appendingPathComponent("macos-window-service.log")
            if let data = msg.data(using: .utf8) {
                if FileManager.default.fileExists(atPath: logUrl.path),
                   let handle = try? FileHandle(forWritingTo: logUrl)
                {
                    defer { try? handle.close() }
                    _ = try? handle.seekToEnd()
                    try? handle.write(contentsOf: data)
                } else {
                    try? data.write(to: logUrl)
                }
            }
        }
        if elements.isEmpty && imageB64 == nil {
            throw ServiceError.permission(
                "no observation data: AX elements empty and screen capture failed"
                    + (captureError.map { ": \($0)" } ?? "")
            )
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
            "semantic_backend": semanticBackend,
            "semantic_error": semanticError as Any,
            "capture_error": captureError as Any,
            "image_png_b64": imageB64 as Any,
            "image_hash": imageHash as Any,
            "capture_backend": captureBackend as Any,
            "control_state": try Takeover.detect(pid: target.pid, windowID: target.windowID).rawValue
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
                // Re-prove the advertised generic capability on the live AX tree.
                // Finder sidebar rows use the outline's writable AXSelectedRows,
                // not AXPress or a coordinate fallback.
                if AXBridge.canSelect(el)
                    && !metadata.actions.contains("AXPress")
                    && !metadata.actions.contains("AXConfirm")
                {
                    try AXBridge.select(el)
                    return okAction(path: "ax_select", detail: "select \(elementId)")
                }
                if metadata.actions.contains("AXConfirm")
                    && !metadata.actions.contains("AXPress")
                    && (metadata.role.contains("Text") || metadata.role.contains("Field"))
                {
                    _ = try AXBridge.press(el)
                    return okAction(path: "ax_confirm", detail: "confirm editable \(elementId)")
                }
                if !metadata.actions.contains("AXPress")
                    && !metadata.actions.contains("AXConfirm")
                {
                    throw ServiceError.unsupported(
                        "invoke refused: \(elementId) has no proven semantic action"
                    )
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

                // Primary path: AX setValue + readback. Native AppKit apps
                // (TextEdit etc.) update their model directly, so background
                // set_value works without focus or keyboard delivery.
                do {
                    try AXBridge.setValue(el, value)
                } catch {
                    throw ServiceError.unsupported(
                        "set_value refused: \(elementId) has no proven AXSetValue capability"
                    )
                }
                let readback = AXBridge.getValue(el)
                guard readback == value || (!value.isEmpty && readback.contains(value)) else {
                    throw ServiceError.actionFailed(
                        "ax_set_value effect unverified for \(elementId)"
                    )
                }
                return okAction(path: "ax_set_value", detail: "set_value \(elementId)")

            case "focus":
                let elementId = try requireString(action, "element_id")
                throw ServiceError.unsupported(
                    "focus \(elementId) is disabled on macOS: agent actions never write AX focus"
                )

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
                let button = (action["button"] as? String ?? "left").lowercased()
                guard button == "left" else {
                    throw ServiceError.unsupported(
                        "targeted \(button) click is not supported on macOS"
                    )
                }
                let x = doubleValue(action["x"]) ?? 0.5
                let y = doubleValue(action["y"]) ?? 0.5
                // Hit requested coordinates (or element under that point) — never first pressable.
                let report = try DirectedInput.click(target: target, normalizedX: x, normalizedY: y)
                return okAction(path: report.path, detail: report.detail)

            case "type_text":
                let text = try requireString(action, "text")
                guard let x = doubleValue(action["x"]), let y = doubleValue(action["y"]) else {
                    throw ServiceError.unsupported(
                        "screenshot-only type_text requires fresh normalized x/y coordinates"
                    )
                }
                // Background targeted typing: click first (element-bound AX press
                // when possible, which also makes the target window key
                // in-process), then type under strict key-window proof. When the
                // proof cannot be established no input has occurred and the
                // exact-target foreground fallback applies.
                let click = try DirectedInput.click(
                    target: target,
                    normalizedX: x,
                    normalizedY: y
                )
                try DirectedInput.typeIntoKeyWindow(target: target, text: text)
                return okAction(
                    path: "\(click.path)+cgevent_post_to_key_window_type",
                    detail: "background type_text len=\(text.count); effect pending fresh observation"
                )

            case "key_combo":
                throw ServiceError.unsupported(
                    "targeted key_combo has no element binding and could submit the wrong control"
                )

            default:
                throw ServiceError.unsupported("unknown targeted action type: \(type)")
            }
        }
    }

    // MARK: - foreground fallback

    /// The only entry in this process that may activate another application.
    /// App access is checked by Runtime before this call. Activation never
    /// executes the rejected old action; Runtime observes again afterwards.
    private func foregroundActivate(_ params: [String: Any]?) throws -> [String: Any] {
        let p = params ?? [:]
        guard let pid = intValue(p["pid"]), let wid = uintValue(p["window_id"]) else {
            throw ServiceError.invalidRequest("foreground_activate requires pid and window_id")
        }
        let target = try WindowResolver.resolve(pid: pid_t(pid), windowID: CGWindowID(wid))
        // AXPress/click may already have promoted this exact window; a second
        // NSWorkspace.openApplication is the extra 抢焦点 users see in Agent loops.
        if FocusGuard.isFrontmost(pid: target.pid),
           FocusGuard.provesExactWindow(pid: target.pid, windowID: target.windowID)
        {
            return ["ok": true, "pid": Int(pid), "window_id": Int(wid)]
        }
        let accessibility = AXBridge.enableAccessibility(pid: target.pid)
        defer { accessibility.disable() }
        _ = AXBridge.raiseExactWindow(target)
        guard let app = NSRunningApplication(processIdentifier: pid_t(pid)) else {
            throw ServiceError.targetLost(
                "foreground_activate: process \(pid) is no longer running"
            )
        }
        guard let appURL = app.bundleURL else {
            throw ServiceError.actionFailed("foreground_activate failed: app bundle URL unavailable")
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = true
        configuration.createsNewApplicationInstance = false
        let completion = DispatchSemaphore(value: 0)
        NSWorkspace.shared.openApplication(at: appURL, configuration: configuration) { _, _ in
            completion.signal()
        }
        guard completion.wait(timeout: .now() + 3) == .success else {
            throw ServiceError.actionFailed("foreground_activate failed: activation timed out")
        }
        _ = AXBridge.raiseExactWindow(target)
        // AppKit activation is asynchronous. A same-process sheet or popover
        // may legitimately become key; the exact target remains the session
        // scope and every input re-proves its destination separately.
        for _ in 0..<20 {
            if FocusGuard.isFrontmost(pid: pid_t(pid)),
               FocusGuard.provesExactWindow(pid: pid_t(pid), windowID: CGWindowID(wid))
            {
                return ["ok": true, "pid": Int(pid), "window_id": Int(wid)]
            }
            usleep(50_000)
        }
        let frontmost = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
        let focused = FocusGuard.focusedWindowNumber(pid: pid_t(pid)).map(String.init) ?? "nil"
        let topmost = FocusGuard.topmostSamePIDWindow(pid: pid_t(pid)).map(String.init) ?? "nil"
        let center = CGPoint(x: target.bounds.midX, y: target.bounds.midY)
        let hit = WindowResolver.windowAtScreenPoint(center)
            .map { "\($0.0):\($0.1)" } ?? "nil"
        throw ServiceError.actionFailed(
            "foreground_activate failed: target app/window was not available after activation "
                + "frontmost=\(frontmost) focused=\(focused) topmost=\(topmost) center_hit=\(hit)"
        )
    }

    // MARK: - control state

    /// Unified control-state / health probe (replaces separate session_health RPC).
    private func detectControlState(_ params: [String: Any]?) throws -> [String: Any] {
        let target = try resolveTargetRequired(params)
        let alive = WindowResolver.processAlive(target.pid)
        let exists = WindowResolver.windowExists(pid: target.pid, windowID: target.windowID)
        let state = try Takeover.detect(pid: target.pid, windowID: target.windowID)
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
        let state = try Takeover.detect(pid: target.pid, windowID: target.windowID)
        switch state {
        case .takenOver:
            throw ServiceError.takenOver(
                "real user input reached the target (pid=\(target.pid) window_id=\(target.windowID))"
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
            "input_monitoring": p.inputMonitoringTrusted ? "granted" : "denied",
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

    private func sha256File(_ url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var context = CC_SHA256_CTX()
        CC_SHA256_Init(&context)
        while let data = try handle.read(upToCount: 1024 * 1024), !data.isEmpty {
            data.withUnsafeBytes { bytes in
                _ = CC_SHA256_Update(&context, bytes.baseAddress, CC_LONG(data.count))
            }
        }
        var hash = [UInt8](repeating: 0, count: Int(CC_SHA256_DIGEST_LENGTH))
        CC_SHA256_Final(&hash, &context)
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
