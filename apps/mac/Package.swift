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
        .executable(name: "VoxAttachProbe", targets: ["VoxAttachProbe"]),
    ],
    dependencies: [
        .package(path: "../../../vox/swift/vox-runtime"),
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
                .product(name: "VoxRuntime", package: "vox-runtime")
            ],
            path: "xcode/VixenFS",
            linkerSettings: [
                .linkedFramework("FSKit")
            ]
        ),
        .executableTarget(
            name: "VoxAttachProbe",
            dependencies: [
                .product(name: "VoxRuntime", package: "vox-runtime")
            ],
            path: "Sources/VoxAttachProbe"
        ),
        .testTarget(
            name: "VixenFSTests",
            dependencies: ["VixenFS"],
            path: "tests/VixenFSTests"
        ),
    ]
)
