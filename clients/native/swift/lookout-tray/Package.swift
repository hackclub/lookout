// swift-tools-version:5.5
import PackageDescription

let package = Package(
    name: "lookout-tray",
    platforms: [.macOS(.v10_15)],
    products: [
        .library(name: "lookout-tray", type: .static, targets: ["lookout-tray"])
    ],
    targets: [
        .target(name: "lookout-tray", path: "Sources")
    ]
)
