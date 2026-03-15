import Darwin
import Foundation
import os

private let log = Logger(subsystem: "eu.bearcove.vixenfs.mount", category: "Main")

/// Log with PID and ISO-8601 timestamp prefix to stderr + unified logging.
private func emit(_ message: String) {
    let fmt = ISO8601DateFormatter()
    fmt.formatOptions = [.withInternetDateTime]
    let prefix =
        "\(ProcessInfo.processInfo.processIdentifier) \(fmt.string(from: Date()))"
    fputs("\(prefix) \(message)\n", stderr)
    fflush(stderr)
    log.notice("\(message, privacy: .public)")
}

enum Command {
    case appGroupPath
}

struct Config {
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"

    static func parse(from args: [String]) throws -> Command {
        var appGroupPathRequested = false

        for arg in args {
            switch arg {
            case "--app-group-path":
                appGroupPathRequested = true
            case "--help":
                throw MountError.usage
            default:
                throw MountError.invalidArgument("Unknown argument: \(arg)")
            }
        }

        guard appGroupPathRequested else {
            throw MountError.invalidArgument("Missing required --app-group-path argument")
        }

        return .appGroupPath
    }
}

enum MountError: Error, CustomStringConvertible {
    case invalidArgument(String)
    case usage

    var description: String {
        switch self {
        case .invalidArgument(let message):
            return message
        case .usage:
            return """
                Usage:
                  vixenfs-mount --app-group-path
                """
        }
    }
}

do {
    let command = try Config.parse(from: Array(CommandLine.arguments.dropFirst()))
    switch command {
    case .appGroupPath:
        guard
            let url = FileManager.default.containerURL(
                forSecurityApplicationGroupIdentifier: Config.appGroupIdentifier)
        else {
            emit("failed to resolve App Group container for \(Config.appGroupIdentifier)")
            exit(2)
        }
        fputs(url.path, stdout)
        exit(0)
    }
} catch let error as MountError {
    emit("\(error.description)")
    if case .usage = error {
        exit(0)
    }
    exit(2)
} catch {
    emit("unexpected error: \(error)")
    exit(3)
}
