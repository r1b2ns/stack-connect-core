// swift-tools-version:5.9
import PackageDescription

// Consumable SwiftPM package for the iOS app. The xcframework and the generated
// StackCoreRust.swift are produced by `./build/build-xcframework.sh` (both gitignored).
// The module is named `StackCoreRust` to avoid colliding with the app's native
// `StackCore` Swift package.
let package = Package(
    name: "StackCoreRust",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "StackCoreRust", targets: ["StackCoreRust"]),
    ],
    targets: [
        .binaryTarget(
            name: "StackCoreFFI",
            path: "StackCoreRust.xcframework"
        ),
        .target(
            name: "StackCoreRust",
            dependencies: ["StackCoreFFI"],
            path: "Sources/StackCoreRust"
        ),
        .testTarget(
            name: "StackCoreTests",
            dependencies: ["StackCoreRust"],
            path: "Tests/StackCoreTests"
        ),
    ]
)
