// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "macos-window-service",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "macos-window-service", targets: ["MacosWindowService"])
    ],
    targets: [
        .executableTarget(
            name: "MacosWindowService",
            path: "Sources/MacosWindowService",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("ApplicationServices"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("ScreenCaptureKit"),
                .linkedFramework("UniformTypeIdentifiers")
            ]
        ),
        .testTarget(
            name: "MacosWindowServiceTests",
            dependencies: ["MacosWindowService"]
        )
    ]
)
