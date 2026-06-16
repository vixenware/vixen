import FSKit
import Foundation
import os

enum VixenFSConstants {
    static let subsystem = "eu.bearcove.vixenfs"
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"
}

private let rustBootstrapLog = Logger(
    subsystem: VixenFSConstants.subsystem,
    category: "RustFSKitBootstrap"
)

enum RustFSKitBootstrap {
    nonisolated(unsafe) static let fileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations =
        makeFileSystem()

    static func makeFileSystem() -> FSUnaryFileSystem & FSUnaryFileSystemOperations {
        let bundleVersion = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "unknown"
        let bundleIdentifier = Bundle.main.bundleIdentifier ?? "eu.bearcove.vixenfs"

        guard
            let appGroupURL = FileManager.default.containerURL(
                forSecurityApplicationGroupIdentifier: VixenFSConstants.appGroupIdentifier
            )
        else {
            fatalError("Failed to resolve app group container URL")
        }
        let appGroupPath = appGroupURL.path
        let socketPath = appGroupURL.appendingPathComponent("vixenfs.sock").path

        let cVersion = strdup(bundleVersion)!
        let cIdentifier = strdup(bundleIdentifier)!
        let cAppGroup = strdup(appGroupPath)!
        let cSocket = strdup(socketPath)!
        defer { free(cVersion); free(cIdentifier); free(cAppGroup); free(cSocket) }

        var config = VixenFsConfig(
            bundle_version: cVersion,
            bundle_identifier: cIdentifier,
            app_group_path: cAppGroup,
            socket_path: cSocket
        )
        let rawPtr = VixenRustCreateFileSystem(&config)
        guard let rawPtr else {
            fatalError("Rust FSKit bootstrap returned null")
        }
        let object = Unmanaged<AnyObject>.fromOpaque(rawPtr).takeRetainedValue()
        guard let fileSystem = object as? (FSUnaryFileSystem & FSUnaryFileSystemOperations) else {
            rustBootstrapLog.error(
                "Rust bootstrap returned unexpected object type \(String(describing: type(of: object)), privacy: .public)"
            )
            fatalError("Rust FSKit bootstrap returned wrong object type")
        }
        rustBootstrapLog.info(
            "Rust FSKit bootstrap complete bundle_version=\(bundleVersion) app_group=\(appGroupPath)"
        )
        return fileSystem
    }
}
