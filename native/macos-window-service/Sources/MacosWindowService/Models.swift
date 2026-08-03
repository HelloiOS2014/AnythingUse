import CoreGraphics
import Foundation

/// Stable window-level control target (product: MacWindow(pid, window_id)).
struct MacWindowTarget: Equatable, Codable, CustomStringConvertible {
    var pid: pid_t
    var windowID: CGWindowID
    var title: String
    var ownerName: String
    var bounds: CGRect
    /// Bundle id when known (best-effort).
    var appId: String

    var description: String {
        "MacWindow(pid=\(pid), windowID=\(windowID), owner=\(ownerName), title=\(title.prefix(60)))"
    }

    enum CodingKeys: String, CodingKey {
        case pid
        case windowID = "window_id"
        case title
        case ownerName = "owner_name"
        case bounds
        case appId = "app_id"
    }

    init(
        pid: pid_t,
        windowID: CGWindowID,
        title: String,
        ownerName: String,
        bounds: CGRect,
        appId: String = ""
    ) {
        self.pid = pid
        self.windowID = windowID
        self.title = title
        self.ownerName = ownerName
        self.bounds = bounds
        self.appId = appId.isEmpty ? ownerName : appId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pid = try c.decode(Int32.self, forKey: .pid)
        windowID = try c.decode(UInt32.self, forKey: .windowID)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        ownerName = try c.decodeIfPresent(String.self, forKey: .ownerName) ?? ""
        appId = try c.decodeIfPresent(String.self, forKey: .appId) ?? ownerName
        if let b = try c.decodeIfPresent(BoundsDTO.self, forKey: .bounds) {
            bounds = CGRect(x: b.x, y: b.y, width: b.width, height: b.height)
        } else {
            bounds = .zero
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(pid, forKey: .pid)
        try c.encode(windowID, forKey: .windowID)
        try c.encode(title, forKey: .title)
        try c.encode(ownerName, forKey: .ownerName)
        try c.encode(appId, forKey: .appId)
        try c.encode(
            BoundsDTO(x: bounds.origin.x, y: bounds.origin.y, width: bounds.width, height: bounds.height),
            forKey: .bounds
        )
    }
}

struct BoundsDTO: Codable {
    var x: Double
    var y: Double
    var width: Double
    var height: Double
}

struct PermissionStatus: Codable {
    var accessibilityTrusted: Bool
    var screenRecordingLikely: Bool
    var notes: [String]

    enum CodingKeys: String, CodingKey {
        case accessibilityTrusted = "accessibility"
        case screenRecordingLikely = "screen_recording"
        case notes
    }
}

/// Product control-session state (P3 contract: none / taken_over / target_lost).
enum ControlState: String, Codable {
    case none
    case takenOver = "taken_over"
    case targetLost = "target_lost"
}

enum ServiceError: Error, CustomStringConvertible {
    case permission(String)
    case notFound(String)
    case actionFailed(String)
    case unsupported(String)
    case invalidRequest(String)
    case targetLost(String)
    case takenOver(String)

    var code: String {
        switch self {
        case .permission: return "permission_denied"
        case .notFound: return "not_found"
        case .actionFailed: return "action_failed"
        case .unsupported: return "unsupported_capability"
        case .invalidRequest: return "invalid_request"
        case .targetLost: return "target_lost"
        case .takenOver: return "taken_over"
        }
    }

    var description: String {
        switch self {
        case .permission(let m): return m
        case .notFound(let m): return m
        case .actionFailed(let m): return m
        case .unsupported(let m): return m
        case .invalidRequest(let m): return m
        case .targetLost(let m): return m
        case .takenOver(let m): return m
        }
    }
}
