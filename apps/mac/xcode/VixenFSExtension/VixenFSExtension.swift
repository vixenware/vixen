import FSKit
import Foundation
import os

private let bootLog = Logger(subsystem: VixenFS.subsystem, category: "Boot")

@main
struct VixenFSExtension: UnaryFileSystemExtension {
    init() {
        bootLog.error("🚀 VixenFSExtension init() called - EXTENSION IS RUNNING")
    }

    var fileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
        bootLog.error("🔌 fileSystem property accessed")
        return Bridge.shared
    }
}
