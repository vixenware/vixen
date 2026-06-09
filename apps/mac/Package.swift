// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "VixenMac",
    platforms: [
        .macOS(.v15)
    ],
    products: [
        .library(name: "VixenApp", targets: ["VixenApp"]),
        .library(name: "VixenFS", targets: ["VixenFS"]),
    ],
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle.git", from: "2.8.1"),
        // Same vox the FSKit extension speaks (0.8.2); the in-app Swift VFS backend
        // is wire-compatible. Pinned by commit (tag vox-v0.8.2 isn't semver-matchable).
        .package(
            url: "https://github.com/bearcove/vox.git",
            revision: "230dcd5bb1e5aedd1309308c64de07781fad5b57"),
    ],
    targets: [
        .target(
            name: "VixenApp",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle"),
                .product(name: "VoxRuntime", package: "vox"),
            ],
            path: "xcode/Vixen",
            linkerSettings: [
                .linkedFramework("FSKit"),
                .linkedFramework("ServiceManagement")
            ]
        ),
        .target(
            name: "VixenFS",
            path: "xcode/VixenFS",
            linkerSettings: [
                .linkedFramework("FSKit")
            ]
        ),
    ]
)
