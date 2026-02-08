// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "VixenMac",
    platforms: [
        .macOS(.v26)
    ],
    products: [
        .library(name: "VixenApp", targets: ["VixenApp"]),
        .library(name: "VixenFSExtension", targets: ["VixenFSExtension"]),
        .executable(name: "vixenfs-mount", targets: ["vixenfs-mount"]),
    ],
    dependencies: [
        // We cannot use branch main here because Xcode caches aggressively.
        // We also duplicate these definitions in `xcode/project.yml`.
        // If you know how to avoid that duplication, please open an issue on GitHub.
        .package(
            url: "https://github.com/bearcove/roam.git",
            revision: "88070908b7083f295eb41d09fabd111caa06f43c"),
        .package(url: "https://github.com/sparkle-project/Sparkle.git", from: "2.8.1"),
    ],
    targets: [
        .target(
            name: "VixenApp",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle")
            ],
            path: "xcode/Vixen",
            linkerSettings: [
                .linkedFramework("FSKit")
            ]
        ),
        .target(
            name: "VixenFSExtension",
            dependencies: [
                .product(name: "RoamRuntime", package: "roam")
            ],
            path: "xcode/VixenFSExtension",
            linkerSettings: [
                .linkedFramework("FSKit")
            ]
        ),
        .executableTarget(
            name: "vixenfs-mount",
            path: "xcode/VixenFSMount",
            linkerSettings: [
                .linkedFramework("FSKit")
            ]
        ),
    ]
)
