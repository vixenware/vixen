import Darwin
@preconcurrency import FSKit
import Foundation
import os

private let log = Logger(subsystem: "eu.bearcove.vixenfs.mount", category: "Main")

enum Command {
    case mount(Config)
    case health(json: Bool)
    case appGroupPath
}

struct Config {
    static let extensionBundleIdentifier = "eu.bearcove.vixenfs"
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"

    let mountPoint: String
    let sid: String
    let scheme: String
    let fsType: String
    let preload: Bool
    let debug: Bool
    let logFile: String?  // if set, redirect stdout/stderr to this file

    var source: String {
        var components = URLComponents()
        components.scheme = scheme
        components.host = "session"
        var items = [URLQueryItem(name: "sid", value: sid)]
        if preload {
            items.append(URLQueryItem(name: "preload", value: "1"))
        }
        if debug {
            items.append(URLQueryItem(name: "debug", value: "1"))
        }
        components.queryItems = items
        if let source = components.string {
            return source
        }
        return "\(scheme)://session?sid=\(sid)"
    }

    static func parse(from args: [String]) throws -> Command {
        var mountPoint: String?
        var sid: String?
        var scheme = "vixen"
        let fsType = "vixenfs"
        var preload = false
        var debug = false
        var logFile: String?
        var healthRequested = false
        var jsonRequested = false
        var appGroupPathRequested = false
        var mountArgumentsProvided = false

        var index = 0
        while index < args.count {
            let arg = args[index]
            switch arg {
            case "--mount-point":
                mountArgumentsProvided = true
                index += 1
                mountPoint = try valueArg(args, index: index, name: "--mount-point")
            case "--sid":
                mountArgumentsProvided = true
                index += 1
                let value = try valueArg(args, index: index, name: "--sid")
                guard isValidSid(value) else {
                    throw MountError.invalidArgument("--sid must be 32 hex chars or UUID format")
                }
                sid = value
            case "--scheme":
                mountArgumentsProvided = true
                index += 1
                scheme = try valueArg(args, index: index, name: "--scheme")
            case "--preload":
                mountArgumentsProvided = true
                preload = true
            case "--debug":
                mountArgumentsProvided = true
                debug = true
            case "--log-file":
                mountArgumentsProvided = true
                index += 1
                logFile = try valueArg(args, index: index, name: "--log-file")
            case "--health":
                healthRequested = true
            case "--json":
                jsonRequested = true
            case "--app-group-path":
                appGroupPathRequested = true
            case "--help":
                throw MountError.usage
            default:
                throw MountError.invalidArgument("Unknown argument: \(arg)")
            }
            index += 1
        }

        if appGroupPathRequested {
            if mountArgumentsProvided || healthRequested {
                throw MountError.invalidArgument(
                    "--app-group-path cannot be combined with other arguments")
            }
            return .appGroupPath
        }

        if healthRequested {
            if mountArgumentsProvided {
                throw MountError.invalidArgument(
                    "--health cannot be combined with mount arguments")
            }
            return .health(json: jsonRequested)
        }

        if jsonRequested {
            throw MountError.invalidArgument("--json can only be used with --health")
        }

        guard let mountPoint else {
            throw MountError.invalidArgument("Missing required --mount-point argument")
        }
        guard let sid else {
            throw MountError.invalidArgument("Missing required --sid argument")
        }

        return .mount(
            Config(
                mountPoint: mountPoint,
                sid: sid,
                scheme: scheme,
                fsType: fsType,
                preload: preload,
                debug: debug,
                logFile: logFile
            ))
    }

    private static func valueArg(_ args: [String], index: Int, name: String) throws -> String {
        guard index < args.count else {
            throw MountError.invalidArgument("\(name) requires a value")
        }
        return args[index]
    }

    private static func isValidSid(_ sid: String) -> Bool {
        if sid.count == 32 {
            return sid.unicodeScalars.allSatisfy {
                CharacterSet(charactersIn: "0123456789abcdefABCDEF").contains($0)
            }
        }
        if sid.count == 36 {
            let chars = Array(sid)
            let hyphenPositions: Set<Int> = [8, 13, 18, 23]
            for i in 0..<chars.count {
                if hyphenPositions.contains(i) {
                    if chars[i] != "-" {
                        return false
                    }
                    continue
                }
                if !chars[i].isHexDigit {
                    return false
                }
            }
            return true
        }
        return false
    }
}

enum MountError: Error, CustomStringConvertible {
    case invalidArgument(String)
    case mountLaunchFailed(String)
    case mountCommandFailed(exitCode: Int32, stdout: String, stderr: String)
    case usage

    var description: String {
        switch self {
        case .invalidArgument(let message):
            return message
        case .mountLaunchFailed(let message):
            return "mount failed to launch: \(message)"
        case .mountCommandFailed(let exitCode, let stdout, let stderr):
            return
                "mount failed: exit=\(exitCode) stdout=\(stdout) stderr=\(stderr)"
        case .usage:
            return """
                Usage:
                  vixenfs-mount --health [--json]
                  vixenfs-mount --app-group-path
                  vixenfs-mount --sid <sid> --mount-point <path> [--scheme <scheme>] [--debug] [--log-file <path>]
                """
        }
    }
}

/// Mounts the FSKit filesystem and keeps the process alive.
///
/// Lifecycle management (unmounting, killing) is handled by vx-reaper,
/// which watches the parent vx-runner process via kqueue and cleans up
/// when it exits. This process just mounts and stays alive via dispatchMain().
///
/// Exits when stdin reaches EOF (the spawning process closed or died).
final class MountLifecycle {
    private let config: Config
    private var stdinSource: DispatchSourceRead?

    init(config: Config) {
        self.config = config
    }

    /// Log to both stderr (which may be redirected to a log file) and unified logging.
    func emit(_ message: String) {
        fputs("vixenfs-mount: \(message)\n", stderr)
        fflush(stderr)
        log.notice("\(message, privacy: .public)")
    }

    func run() throws {
        try ensureMountPointExists()
        try mount()

        // Ignore signals — vx-reaper is responsible for killing us
        signal(SIGINT, SIG_IGN)
        signal(SIGTERM, SIG_IGN)
        signal(SIGHUP, SIG_IGN)

        // Exit on stdin EOF — when the spawning process dies, the pipe closes
        installStdinEOFWatcher()

        let mountedPath = self.config.mountPoint
        let mountedSource = self.config.source
        emit("mounted \(mountedPath) from \(mountedSource)")
        dispatchMain()
    }

    private func installStdinEOFWatcher() {
        let source = DispatchSource.makeReadSource(
            fileDescriptor: STDIN_FILENO,
            queue: DispatchQueue.global()
        )
        source.setEventHandler { [weak self] in
            var buf = [UInt8](repeating: 0, count: 64)
            let n = read(STDIN_FILENO, &buf, buf.count)
            if n <= 0 {
                self?.emit("stdin closed, exiting")
                exit(0)
            }
        }
        source.setCancelHandler {}
        source.resume()
        stdinSource = source
    }

    private func ensureMountPointExists() throws {
        // Skip creating mount directory for /Volumes - diskarbitrationd creates it
        if config.mountPoint.hasPrefix("/Volumes/") {
            return
        }

        var isDir: ObjCBool = false
        if FileManager.default.fileExists(atPath: config.mountPoint, isDirectory: &isDir) {
            if !isDir.boolValue {
                throw MountError.invalidArgument(
                    "Mount point exists but is not a directory: \(config.mountPoint)")
            }
            return
        }
        try FileManager.default.createDirectory(
            atPath: config.mountPoint, withIntermediateDirectories: true)
    }

    private func mount() throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/sbin/mount")
        process.arguments = ["-F", "-t", config.fsType, config.source, config.mountPoint]

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        do {
            try process.run()
        } catch {
            throw MountError.mountLaunchFailed(String(describing: error))
        }

        process.waitUntilExit()

        let stdoutData = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
        let stderrData = stderrPipe.fileHandleForReading.readDataToEndOfFile()
        let stdout =
            String(data: stdoutData, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let stderr =
            String(data: stderrData, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        guard process.terminationStatus == 0 else {
            throw MountError.mountCommandFailed(
                exitCode: process.terminationStatus, stdout: stdout, stderr: stderr)
        }
    }
}

private func emitHealthJSON(installed: Bool, enabled: Bool) {
    fputs(
        "{\"extensionInstalled\": \(installed ? "true" : "false"), \"extensionEnabled\": \(enabled ? "true" : "false")}\n",
        stdout
    )
}

private func runHealthCheck(json: Bool) -> Never {
    FSClient.shared.fetchInstalledExtensions { modules, error in
        if let error {
            if json {
                emitHealthJSON(installed: false, enabled: false)
            }
            fputs(
                "vixenfs-mount: FSKit health check failed: \(error.localizedDescription)\n", stderr)
            exit(2)
        }

        guard let modules else {
            if json {
                emitHealthJSON(installed: false, enabled: false)
            }
            fputs("vixenfs-mount: FSKit health check failed: no module list returned\n", stderr)
            exit(2)
        }

        let module = modules.first(where: {
            $0.bundleIdentifier == Config.extensionBundleIdentifier
        })
        let installed = module != nil
        let enabled = module?.isEnabled ?? false

        if json {
            emitHealthJSON(installed: installed, enabled: enabled)
        }

        guard installed else {
            if !json {
                fputs(
                    "vixenfs-mount: FSKit module \(Config.extensionBundleIdentifier) is not installed\n",
                    stderr
                )
            }
            exit(2)
        }

        guard enabled else {
            if !json {
                fputs(
                    "vixenfs-mount: FSKit module \(Config.extensionBundleIdentifier) is installed but disabled\n",
                    stderr
                )
            }
            exit(2)
        }

        if !json {
            fputs("ok: FSKit module \(Config.extensionBundleIdentifier) is enabled\n", stdout)
        }
        exit(0)
    }
    dispatchMain()
}

do {
    let command = try Config.parse(from: Array(CommandLine.arguments.dropFirst()))
    switch command {
    case .health(let json):
        runHealthCheck(json: json)
    case .appGroupPath:
        guard
            let url = FileManager.default.containerURL(
                forSecurityApplicationGroupIdentifier: Config.appGroupIdentifier)
        else {
            fputs(
                "vixenfs-mount: failed to resolve App Group container for \(Config.appGroupIdentifier)\n",
                stderr)
            exit(2)
        }
        fputs(url.path, stdout)
        exit(0)
    case .mount(let config):
        if let logFile = config.logFile {
            let fd = open(logFile, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
            guard fd >= 0 else {
                fputs("vixenfs-mount: failed to open log file \(logFile): errno \(errno)\n", stderr)
                exit(2)
            }
            dup2(fd, STDOUT_FILENO)
            dup2(fd, STDERR_FILENO)
            close(fd)
            // After dup2 to a file, stderr becomes fully buffered — force line buffering
            setvbuf(stderr, nil, _IOLBF, 0)
            setvbuf(stdout, nil, _IOLBF, 0)
        }
        let lifecycle = MountLifecycle(config: config)
        lifecycle.emit(
            "starting (pid=\(ProcessInfo.processInfo.processIdentifier), mount-point=\(config.mountPoint))"
        )
        try lifecycle.run()
    }
} catch let error as MountError {
    fputs("vixenfs-mount: \(error.description)\n", stderr)
    fflush(stderr)
    if case .usage = error {
        exit(0)
    }
    exit(2)
} catch {
    fputs("vixenfs-mount: unexpected error: \(error)\n", stderr)
    fflush(stderr)
    exit(3)
}
