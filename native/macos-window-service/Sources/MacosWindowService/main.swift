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
                ForegroundSession.shared.clear()
            }
        }
        let service = Service()
        let server = SocketServer(socketPath: path, service: service)
        // Write pid file next to socket for doctor/adapter.
        let pidPath = (path as NSString).deletingPathExtension + ".pid"
        try? "\(getpid())".write(toFile: pidPath, atomically: true, encoding: .utf8)
        defer {
            invalidationObservers.forEach(workspaceCenter.removeObserver)
            try? FileManager.default.removeItem(atPath: pidPath)
            server.stop()
        }
        try server.start()
        exit(0)

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
          serve [--socket <path>]   Listen on per-user private Unix socket (JSON lines)
          permissions               Print TCC probe JSON
          list                      List on-screen windows (pid + window_id)
          socket-path               Print default socket path
          self-check                Run the foreground-session gate regression check
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
          {"id":"8","method":"set_foreground_session","params":{"pid":1,"window_id":2,"active":true}}
          {"id":"9","method":"foreground_activate","params":{"pid":1,"window_id":2}}
          {"id":"10","method":"suspend_foreground_session","params":{"pid":1,"window_id":2}}
          {"id":"11","method":"resume_foreground_session","params":{"pid":1,"window_id":2}}

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

/// Smallest regression check for the shared session-mode gate (human and Agent
/// actors share the Runtime contract; this gate is the native enforcement
/// point). Runs without a live app or window: an approved session + foreground
/// app must allow input with no AX window proof, and everything else must not.
func runSelfCheck() throws {
    let session = ForegroundSession.shared
    session.clear()
    func must(_ condition: Bool, _ label: String) throws {
        guard condition else {
            throw ServiceError.actionFailed("self-check failed: \(label)")
        }
    }
    session.begin(pid: 4242, windowID: 9)
    try must(session.isActive(pid: 4242, windowID: 9), "begin must activate exact slot")
    try must(
        !session.allowsSessionInput(pid: 4242, windowID: 9, appIsFrontmost: true),
        "frontmost pid alone must not authorize input"
    )
    try must(UserInputMonitor.shared.selfCheck(), "HID baseline must detect later input")
    session.suspend(pid: 4242, windowID: 9)
    try must(!session.isActive(pid: 4242, windowID: 9), "suspend must deactivate slot")
    session.clear()
    try must(
        !session.allowsSessionInput(pid: 4242, windowID: 9, appIsFrontmost: true),
        "clear must close the session"
    )
    fputs("self-check ok: foreground session gate\n", stderr)
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
