import AppKit
import Foundation
import Darwin

// Ensure AppKit is initialized for NSWorkspace / capture side effects.
_ = NSApplication.shared

// A client that closes its connection mid-response (e.g. Rust 45s read timeout)
// must not SIGPIPE-kill the whole service. Ignore the signal; writes to a dead
// socket fail with EPIPE and the client handler just closes.
signal(SIGPIPE, SIG_IGN)

let args = Array(CommandLine.arguments.dropFirst())
let command = args.first ?? "serve"

do {
    switch command {
    case "help", "-h", "--help":
        printHelp()
        exit(0)

    case "permissions":
        let status = Permissions.probe()
        printJSON([
            "accessibility": status.accessibilityTrusted ? "granted" : "denied",
            "screen_recording": status.screenRecordingLikely ? "granted" : "denied",
            "input_monitoring": status.inputMonitoringTrusted ? "granted" : "denied",
            "notes": status.notes
        ])
        exit(status.accessibilityTrusted && status.screenRecordingLikely && status.inputMonitoringTrusted ? 0 : 2)

    case "list":
        let windows = WindowResolver.listOnScreenWindows()
        print("count=\(windows.count)")
        for (i, w) in windows.enumerated() {
            print(
                String(
                    format: "%2d  pid=%-6d win=%-6u  %4.0fx%-4.0f  %@  %@",
                    i, w.pid, w.windowID, w.bounds.width, w.bounds.height,
                    w.ownerName, String(w.title.prefix(50))
                )
            )
        }
        exit(0)

    case "serve":
        let path = resolveSocketPath(args: args)
        fputs("starting macos-window-service at \(path)\n", stderr)
        _ = UserInputMonitor.shared.start()
        let workspaceCenter = NSWorkspace.shared.notificationCenter
        let invalidationNames: [Notification.Name] = [
            NSWorkspace.willSleepNotification,
            NSWorkspace.didWakeNotification,
            NSWorkspace.sessionDidResignActiveNotification,
            NSWorkspace.sessionDidBecomeActiveNotification
        ]
        let invalidationObservers = invalidationNames.map { name in
            workspaceCenter.addObserver(forName: name, object: nil, queue: nil) { _ in
                UserInputMonitor.shared.invalidateAll()
            }
        }
        let service = Service()
        let parentPID = flagValue("--parent-pid", in: args).flatMap(Int32.init)
        let server = SocketServer(socketPath: path, service: service, parentPID: parentPID)
        // Write pid file next to socket for doctor/adapter.
        let pidPath = (path as NSString).deletingPathExtension + ".pid"
        try? "\(getpid())".write(toFile: pidPath, atomically: true, encoding: .utf8)
        defer {
            invalidationObservers.forEach(workspaceCenter.removeObserver)
            try? FileManager.default.removeItem(atPath: pidPath)
            server.stop()
        }
        try server.start()

    case "socket-path":
        print(resolveSocketPath(args: args))
        exit(0)

    case "self-check":
        try runSelfCheck()
        exit(0)

    default:
        fputs("Unknown command: \(command)\n\n", stderr)
        printHelp()
        exit(64)
    }
} catch {
    fputs("error: \(error)\n", stderr)
    exit(1)
}

func printHelp() {
    print(
        """
        macos-window-service — D2 product native macOS window control

        Commands:
          serve [--socket <path>] [--parent-pid <pid>]
                                    Listen on per-user private Unix socket (JSON lines)
          permissions               Print TCC probe JSON
          list                      List on-screen windows (pid + window_id)
          socket-path               Print default socket path
          self-check                Run the user-input monitor regression check
          help

        Socket protocol (newline-delimited JSON):
          {"id":"1","method":"ping"}
          {"id":"2","method":"permissions"}
          {"id":"3","method":"resolve","params":{"app_id":"TextEdit"}}
          {"id":"3b","method":"app_identity","params":{"pid":1,"window_id":2}}
          {"id":"3c","method":"set_takeover_watch","params":{"pid":1,"window_id":2,"active":true}}
          {"id":"4","method":"observe","params":{"pid":1,"window_id":2}}
          {"id":"5","method":"semantic","params":{"pid":1,"window_id":2,"action":{"type":"invoke","element_id":"e1"}}}
          {"id":"6","method":"targeted","params":{"pid":1,"window_id":2,"action":{"type":"type_text","text":"hi"}}}
          {"id":"7","method":"detect_conflict","params":{"pid":1,"window_id":2}}
          {"id":"8","method":"foreground_activate","params":{"pid":1,"window_id":2}}

        Default socket:
          ~/Library/Application Support/AnythingUse/macos-window.sock
          override: --socket PATH or env LCU_MACOS_WINDOW_SOCK

        Build:
          cd native/macos-window-service && swift build -c release
        """
    )
}

func resolveSocketPath(args: [String]) -> String {
    if let idx = args.firstIndex(of: "--socket"), args.index(after: idx) < args.endIndex {
        return args[args.index(after: idx)]
    }
    if let env = ProcessInfo.processInfo.environment["LCU_MACOS_WINDOW_SOCK"], !env.isEmpty {
        return env
    }
    let home = FileManager.default.homeDirectoryForCurrentUser
    return home
        .appendingPathComponent("Library/Application Support/AnythingUse/macos-window.sock")
        .path
}

func runSelfCheck() throws {
    guard UserInputMonitor.shared.selfCheck() else {
        throw ServiceError.actionFailed("self-check failed: HID baseline")
    }
    fputs("self-check ok: user input monitor\n", stderr)
}

func printJSON(_ value: Any) {
    if let data = try? JSONSerialization.data(withJSONObject: value, options: [.prettyPrinted, .sortedKeys]),
       let s = String(data: data, encoding: .utf8)
    {
        print(s)
    }
}

func flagValue(_ name: String, in args: [String]) -> String? {
    guard let idx = args.firstIndex(of: name), args.index(after: idx) < args.endIndex else {
        return nil
    }
    return args[args.index(after: idx)]
}
