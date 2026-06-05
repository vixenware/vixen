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
        .package(url: "https://github.com/sparkle-project/Sparkle.git", from: "2.8.1")
    ],
    targets: [
        .target(
            name: "VixenApp",
            dependencies: [
                .product(name: "Sparkle", package: "Sparkle")
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
