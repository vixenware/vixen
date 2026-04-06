import FSKit
import Foundation
import os

enum VixenFSConstants {
    static let subsystem = "eu.bearcove.vixenfs"
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"
    static let vfsSocketName = "vixenfs.sock"
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
        guard let raw = bundleVersion.withCString({ VixenRustCreateFileSystem($0) }) else {
            fatalError("Rust FSKit bootstrap returned null")
        }
        let object = Unmanaged<AnyObject>.fromOpaque(raw).takeRetainedValue()
        guard let fileSystem = object as? (FSUnaryFileSystem & FSUnaryFileSystemOperations) else {
            rustBootstrapLog.error(
                "Rust bootstrap returned unexpected object type \(String(describing: type(of: object)), privacy: .public)"
            )
            fatalError("Rust FSKit bootstrap returned wrong object type")
        }
        rustBootstrapLog.error(
            "Rust FSKit bootstrap returned \(String(describing: type(of: object)), privacy: .public)"
        )
        return fileSystem
    }
}
