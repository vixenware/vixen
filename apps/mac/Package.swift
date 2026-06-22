// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "VixenMac",
    platforms: [
        .macOS("15.4")
    ],
    products: [
        .library(name: "VixenApp", targets: ["VixenApp"]),
        .library(name: "VixenFS", targets: ["VixenFS"]),
    ],
    dependencies: [
        .package(url: "https://github.com/sparkle-project/Sparkle.git", exact: "2.8.1"),
        .package(
            url: "https://github.com/facet-rs/facet.git",
            revision: "f4f0f90de9cb46172d03bcdb02fb8bdeb56c491b"
        ),
    ],
    targets: [
        .target(
            name: "VixenApp",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle"),
                .product(name: "VoxRuntime", package: "facet"),
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
