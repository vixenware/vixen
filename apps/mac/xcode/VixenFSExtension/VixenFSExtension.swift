import FSKit
import Foundation
import os

private let bootLog = Logger(subsystem: "eu.bearcove.vixenfs", category: "Boot")

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

extension Logger {
    func d(_ message: String) {
        self.debug("\(message, privacy: .public)")
    }

    func e(_ message: String) {
        self.error("\(message, privacy: .public)")
    }

    func posixError(_ function: String, _ code: Int32) {
        self.e("\(function): failure (code = \(code))")
    }
}
