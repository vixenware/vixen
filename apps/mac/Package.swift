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
    ],
    dependencies: [
        .package(
            url: "https://github.com/bearcove/roam.git",
            revision: "5cabc262c48dfd16ca799157e19ff705ae02c71d"
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
        .testTarget(
            name: "VixenFSTests",
            dependencies: ["VixenFS"],
            path: "tests/VixenFSTests"
        ),
    ]
)
