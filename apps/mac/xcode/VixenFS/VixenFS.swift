import FSKit
import Foundation
import os

private let bootLog = Logger(subsystem: VixenFSConstants.subsystem, category: "Boot")

@main
struct VixenFS: UnaryFileSystemExtension {
    init() {
        bootLog.error("🚀 VixenFS init() called - EXTENSION IS RUNNING")
    }

    var fileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
        bootLog.error("🔌 fileSystem property accessed")
        return RustFSKitBootstrap.fileSystem
    }
}
