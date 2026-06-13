// swift-tools-version:5.9
import PackageDescription

// Consumable SwiftPM package for the iOS app. The xcframework and the generated
// StackCore.swift are produced by `./build/build-xcframework.sh` (both gitignored).
let package = Package(
    name: "StackCore",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "StackCore", targets: ["StackCore"]),
    ],
    targets: [
        .binaryTarget(
            name: "StackCoreFFI",
            path: "StackCore.xcframework"
        ),
        .target(
            name: "StackCore",
            dependencies: ["StackCoreFFI"],
            path: "Sources/StackCore"
        ),
        .testTarget(
            name: "StackCoreTests",
            dependencies: ["StackCore"],
            path: "Tests/StackCoreTests"
        ),
    ]
)
