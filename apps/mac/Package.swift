// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "VixenMac",
    platforms: [
        .macOS(.v26)
    ],
    products: [
        .library(name: "VixenApp", targets: ["VixenApp"]),
        .library(name: "VixenFS", targets: ["VixenFS"]),
        .executable(name: "vixenfs-mount", targets: ["vixenfs-mount"]),
    ],
    dependencies: [
        .package(
            url: "https://github.com/bearcove/roam.git",
            revision: "11cc31b02be62cfc5a89781949aed312b838b3f8"
        ),
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
            name: "VixenFS",
            dependencies: [
                .product(name: "RoamRuntime", package: "roam")
            ],
            path: "xcode/VixenFS",
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
