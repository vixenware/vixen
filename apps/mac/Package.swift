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
        // Local ~/vox (path dep) so the Swift VoxRuntime matches the Rust vox the FSKit
        // extension links (both ~/vox) — same wire. ~/vox phon-codec serializes via phon
        // (from ~/phon), so use the swift/vox-runtime package. Local-only until published.
        .package(path: "/Users/amos/vox/swift/vox-runtime"),
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
