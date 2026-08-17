import Foundation
import Darwin

/// Per-user private Unix domain socket, newline-delimited JSON request/response.
///
/// Protocol (one request → one response per line):
///   Request:  {"id":"...","method":"...","params":{...}}
///   Response: {"id":"...","ok":true,"result":{...}}
///          or {"id":"...","ok":false,"error":{"code":"...","message":"..."}}
final class SocketServer {
    private let socketPath: String
    private let service: Service
    private let parentPID: pid_t?
    private var serverFd: Int32 = -1
    private var running = false

    init(socketPath: String, service: Service, parentPID: pid_t? = nil) {
        self.socketPath = socketPath
        self.service = service
        self.parentPID = parentPID
    }

    func start() throws {
        let dir = (socketPath as NSString).deletingLastPathComponent
        try FileManager.default.createDirectory(
            atPath: dir,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        // Remove stale socket.
        if FileManager.default.fileExists(atPath: socketPath) {
            try? FileManager.default.removeItem(atPath: socketPath)
        }

        serverFd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard serverFd >= 0 else {
            throw ServiceError.actionFailed("socket() failed: \(String(cString: strerror(errno)))")
        }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = socketPath.utf8CString
        guard pathBytes.count <= MemoryLayout.size(ofValue: addr.sun_path) else {
            close(serverFd)
            throw ServiceError.invalidRequest("socket path too long: \(socketPath)")
        }
        withUnsafeMutablePointer(to: &addr.sun_path.0) { dst in
            pathBytes.withUnsafeBufferPointer { src in
                _ = memcpy(dst, src.baseAddress!, src.count)
            }
        }

        let bindResult = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                bind(serverFd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bindResult == 0 else {
            let err = String(cString: strerror(errno))
            close(serverFd)
            throw ServiceError.actionFailed("bind(\(socketPath)) failed: \(err)")
        }

        // Owner-only socket file.
        chmod(socketPath, 0o600)

        guard listen(serverFd, 16) == 0 else {
            let err = String(cString: strerror(errno))
            close(serverFd)
            throw ServiceError.actionFailed("listen failed: \(err)")
        }

        running = true
        fputs("macos-window-service listening on \(socketPath)\n", stderr)

        while running {
            if let parentPID, getppid() != parentPID {
                stop()
                break
            }
            // Accept with short select so RunLoop can still process MainActor capture if needed.
            var fds = fd_set()
            // swift-format: fd_set helpers
            zeroFdSet(&fds)
            setFd(serverFd, set: &fds)
            var tv = timeval(tv_sec: 0, tv_usec: 200_000)
            let sel = select(serverFd + 1, &fds, nil, nil, &tv)
            if sel <= 0 {
                // Pump run loop for any pending MainActor work.
                RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
                continue
            }

            let client = accept(serverFd, nil, nil)
            if client < 0 { continue }
            // Handle client on a dedicated queue so multiple adapters can talk.
            DispatchQueue.global(qos: .userInitiated).async { [service] in
                Self.handleClient(fd: client, service: service)
            }
        }
    }

    func stop() {
        running = false
        if serverFd >= 0 {
            close(serverFd)
            serverFd = -1
        }
        try? FileManager.default.removeItem(atPath: socketPath)
        let pidPath = (socketPath as NSString).deletingPathExtension + ".pid"
        try? FileManager.default.removeItem(atPath: pidPath)
    }

    private static func handleClient(fd: Int32, service: Service) {
        defer { close(fd) }
        var buffer = Data()
        var tmp = [UInt8](repeating: 0, count: 64 * 1024)

        while true {
            let n = read(fd, &tmp, tmp.count)
            if n <= 0 { break }
            buffer.append(contentsOf: tmp[0..<n])

            while let range = buffer.range(of: Data([0x0A])) { // \n
                let line = buffer.subdata(in: buffer.startIndex..<range.lowerBound)
                buffer.removeSubrange(buffer.startIndex...range.lowerBound)
                if line.isEmpty { continue }
                let response = processLine(line, service: service)
                var out = response
                out.append(0x0A)
                let written = out.withUnsafeBytes { raw in
                    write(fd, raw.baseAddress!, out.count)
                }
                if written < 0 {
                    // Client closed while we were processing (EPIPE); stop serving it.
                    return
                }
            }
        }
    }

    private static func processLine(_ line: Data, service: Service) -> Data {
        // Use JSONSerialization for free-form result dictionaries.
        guard
            let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
            let method = obj["method"] as? String
        else {
            return errorJSON(id: nil, code: "invalid_request", message: "expected JSON object with method")
        }
        let id = obj["id"]
        let params = obj["params"] as? [String: Any]
        do {
            let result = try service.handle(method: method, params: params)
            var resp: [String: Any] = ["ok": true, "result": result]
            if let id { resp["id"] = id }
            return try JSONSerialization.data(withJSONObject: sanitize(resp), options: [])
        } catch let e as ServiceError {
            return errorJSON(id: id, code: e.code, message: e.description)
        } catch {
            return errorJSON(id: id, code: "internal_error", message: String(describing: error))
        }
    }

    private static func errorJSON(id: Any?, code: String, message: String) -> Data {
        var resp: [String: Any] = [
            "ok": false,
            "error": ["code": code, "message": message]
        ]
        if let id { resp["id"] = id }
        return (try? JSONSerialization.data(withJSONObject: resp, options: []))
            ?? Data(#"{"ok":false,"error":{"code":"internal_error","message":"encode failed"}}"#.utf8)
    }

    /// Drop NSNull placeholders for cleaner wire output.
    private static func sanitize(_ value: Any) -> Any {
        switch value {
        case let dict as [String: Any]:
            var out: [String: Any] = [:]
            for (k, v) in dict {
                if v is NSNull { continue }
                // Optional Any boxes from `x as Any` may still be Optional.none — skip nils.
                let mirror = Mirror(reflecting: v)
                if mirror.displayStyle == .optional, mirror.children.isEmpty {
                    continue
                }
                if case Optional<Any>.none = v as Any? {
                    continue
                }
                out[k] = sanitize(v)
            }
            return out
        case let arr as [Any]:
            return arr.map { sanitize($0) }
        default:
            return value
        }
    }
}

// MARK: - fd_set helpers (Darwin)

private func zeroFdSet(_ set: inout fd_set) {
    #if os(macOS)
    set.fds_bits = (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    #endif
}

private func setFd(_ fd: Int32, set: inout fd_set) {
    let intOffset = Int(fd / 32)
    let bitOffset = fd % 32
    let mask: Int32 = 1 << bitOffset
    withUnsafeMutableBytes(of: &set.fds_bits) { raw in
        let ptr = raw.bindMemory(to: Int32.self)
        ptr[intOffset] |= mask
    }
}
