import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// Resolve and list windows by PID + CGWindowID (CoreGraphics layer).
enum WindowResolver {
    static func listOnScreenWindows(minSize: CGFloat = 40) -> [MacWindowTarget] {
        let onScreen = listWindows(options: [.optionOnScreenOnly, .excludeDesktopElements], minSize: minSize)
        if !onScreen.isEmpty {
            let all = listWindows(options: [.excludeDesktopElements], minSize: max(minSize, 120))
            var byID = Dictionary(uniqueKeysWithValues: onScreen.map { ($0.windowID, $0) })
            for w in all where byID[w.windowID] == nil {
                byID[w.windowID] = w
            }
            return Array(byID.values).sorted {
                $0.bounds.width * $0.bounds.height > $1.bounds.width * $1.bounds.height
            }
        }
        return listWindows(options: [.excludeDesktopElements], minSize: minSize)
    }

    private static func listWindows(options: CGWindowListOption, minSize: CGFloat) -> [MacWindowTarget] {
        guard let raw = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else {
            return []
        }

        var out: [MacWindowTarget] = []
        for info in raw {
            guard
                let windowID = info[kCGWindowNumber as String] as? UInt32,
                let pid = info[kCGWindowOwnerPID as String] as? pid_t,
                let boundsDict = info[kCGWindowBounds as String] as? [String: Any],
                let x = boundsDict["X"] as? CGFloat,
                let y = boundsDict["Y"] as? CGFloat,
                let w = boundsDict["Width"] as? CGFloat,
                let h = boundsDict["Height"] as? CGFloat
            else { continue }

            let layer = info[kCGWindowLayer as String] as? Int ?? 0
            if layer != 0 { continue }
            if w < minSize || h < minSize { continue }
            if h <= 40 && w > 400 { continue }

            let title = info[kCGWindowName as String] as? String ?? ""
            let owner = info[kCGWindowOwnerName as String] as? String ?? ""
            if owner.isEmpty { continue }

            let appId = bundleId(for: pid) ?? owner
            out.append(
                MacWindowTarget(
                    pid: pid,
                    windowID: windowID,
                    title: title,
                    ownerName: owner,
                    bounds: CGRect(x: x, y: y, width: w, height: h),
                    appId: appId
                )
            )
        }
        return out
    }

    static func resolve(pid: pid_t, windowID: CGWindowID) throws -> MacWindowTarget {
        if let hit = listOnScreenWindows(minSize: 1).first(where: {
            $0.pid == pid && $0.windowID == windowID
        }) {
            return hit
        }
        let options: CGWindowListOption = [.excludeDesktopElements]
        if let raw = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] {
            for info in raw {
                guard
                    let wid = info[kCGWindowNumber as String] as? UInt32,
                    let p = info[kCGWindowOwnerPID as String] as? pid_t,
                    p == pid, wid == windowID,
                    let boundsDict = info[kCGWindowBounds as String] as? [String: Any],
                    let x = boundsDict["X"] as? CGFloat,
                    let y = boundsDict["Y"] as? CGFloat,
                    let w = boundsDict["Width"] as? CGFloat,
                    let h = boundsDict["Height"] as? CGFloat
                else { continue }
                let title = info[kCGWindowName as String] as? String ?? ""
                let owner = info[kCGWindowOwnerName as String] as? String ?? ""
                return MacWindowTarget(
                    pid: p,
                    windowID: wid,
                    title: title,
                    ownerName: owner,
                    bounds: CGRect(x: x, y: y, width: w, height: h),
                    appId: bundleId(for: p) ?? owner
                )
            }
        }
        throw ServiceError.notFound("no window for pid=\(pid) windowID=\(windowID)")
    }

    /// Resolve by selector fields (app_id / pid / window_title_contains).
    static func resolveSelector(
        appId: String?,
        pid: pid_t?,
        windowTitleContains: String?
    ) throws -> MacWindowTarget {
        var windows = listOnScreenWindows(minSize: 2)
        if let pid {
            windows = windows.filter { $0.pid == pid }
        }
        if let appId, !appId.isEmpty {
            let needle = appId.lowercased()
            windows = windows.filter {
                $0.appId.lowercased().contains(needle)
                    || $0.ownerName.lowercased().contains(needle)
            }
        }
        if let title = windowTitleContains, !title.isEmpty {
            let needle = title.lowercased()
            windows = windows.filter { $0.title.lowercased().contains(needle) }
        }
        if let best = windows.max(by: {
            $0.bounds.width * $0.bounds.height < $1.bounds.width * $1.bounds.height
        }) {
            return best
        }
        throw ServiceError.notFound(
            "no window matching selector app_id=\(appId ?? "-") pid=\(pid.map(String.init) ?? "-") title=\(windowTitleContains ?? "-")"
        )
    }

    static func processAlive(_ pid: pid_t) -> Bool {
        if pid <= 0 { return false }
        return kill(pid, 0) == 0
    }

    static func windowExists(pid: pid_t, windowID: CGWindowID) -> Bool {
        (try? resolve(pid: pid, windowID: windowID)) != nil
    }

    /// CGWindowList is front-to-back. Restricting that order to one process lets
    /// application-scoped AX hit-testing prove which same-process window owns a point,
    /// even when the app omits AXWindowNumber and usable AX window parents.
    static func isTopmostProcessWindow(
        target: MacWindowTarget,
        at point: CGPoint
    ) -> Bool {
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let raw = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else {
            return false
        }
        for info in raw {
            guard (info[kCGWindowOwnerPID as String] as? pid_t) == target.pid,
                  (info[kCGWindowLayer as String] as? Int ?? 0) == 0,
                  let windowID = info[kCGWindowNumber as String] as? UInt32,
                  let boundsDict = info[kCGWindowBounds as String] as? [String: Any],
                  let x = boundsDict["X"] as? CGFloat,
                  let y = boundsDict["Y"] as? CGFloat,
                  let width = boundsDict["Width"] as? CGFloat,
                  let height = boundsDict["Height"] as? CGFloat
            else { continue }
            if CGRect(x: x, y: y, width: width, height: height).contains(point) {
                return windowID == target.windowID
            }
        }
        return false
    }

    private static func bundleId(for pid: pid_t) -> String? {
        NSRunningApplication(processIdentifier: pid)?.bundleIdentifier
    }
}
