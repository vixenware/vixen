// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Vixen",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/bearcove/roam.git", branch: "main"),
    ],
    targets: [
        .executableTarget(
            name: "Vixen",
            dependencies: [
                .product(name: "RoamRuntime", package: "roam"),
            ],
            path: "Sources/Vixen"
        ),
    ]
)
