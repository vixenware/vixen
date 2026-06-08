import AppKit
import Darwin
@preconcurrency import FSKit
import OSLog
import ServiceManagement
import Sparkle
import SwiftUI

@main
enum VixenMain {
    static func main() {
        let logger = Logger(subsystem: "eu.bearcove.vixen", category: "App")
        let arguments = Array(CommandLine.arguments.dropFirst())
        logger.notice("Vixen process starting pid=\(getpid(), privacy: .public) arguments=\(arguments.joined(separator: " "), privacy: .public)")
        if arguments.first == "management" {
            logger.notice("Vixen entering management CLI mode")
            Darwin.exit(ManagementCommand.run(Array(arguments.dropFirst())))
        }
        logger.notice("Vixen entering GUI app mode")
        VixenApp.main()
    }
}

struct VixenApp: App {
    private let updaterController = SparkleBootstrap.makeUpdaterController()

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .windowResizability(.contentSize)
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") {
                    updaterController?.checkForUpdates(nil)
                }
                .disabled(updaterController == nil)

                Button("Uninstall Vixen…") {
                    NotificationCenter.default.post(name: .vixenUninstallRequested, object: nil)
                }
            }
        }
    }

}

private extension Notification.Name {
    static let vixenUninstallRequested = Notification.Name("eu.bearcove.vixen.uninstallRequested")
}

enum ManagementService {
    fileprivate static let logger = Logger(subsystem: "eu.bearcove.vixen", category: "Management")
    private static let label = "eu.bearcove.vixen.management"
    private static let plistName = "eu.bearcove.vixen.management.plist"
    private static let queueRoot = URL(
        fileURLWithPath: "/Library/Application Support/Vixen/management/queue",
        isDirectory: true
    )
    private static let lastActionURL = URL(
        fileURLWithPath: "/Library/Application Support/Vixen/management/last-action.json",
        isDirectory: false
    )
    private static let repairStatusURL = URL(
        fileURLWithPath: "/Library/Application Support/Vixen/management/repair-status.json",
        isDirectory: false
    )
    private static var helperURL: URL {
        Bundle.main.bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("MacOS", isDirectory: true)
            .appendingPathComponent("vixen-management", isDirectory: false)
    }

    struct StatusSnapshot {
        let helperAvailable: Bool
        let helperDetail: String?
        let serviceStatus: SMAppService.Status
        let launchdStatus: LaunchdStatus?
        let lastActionStatus: LastActionStatus?

        var isReady: Bool {
            guard helperAvailable else { return false }
            switch serviceStatus {
            case .enabled:
                return true
            default:
                return false
            }
        }

        var description: String {
            guard helperAvailable else {
                return helperDetail ?? "Helper missing from app bundle"
            }
            switch serviceStatus {
            case .enabled:
                return "Ready"
            case .requiresApproval:
                return "Needs approval"
            case .notRegistered:
                return "Not configured"
            case .notFound:
                return "Missing from app bundle"
            @unknown default:
                return "Status unknown"
            }
        }

        var backgroundItemDescription: String? {
            guard helperAvailable else { return nil }
            switch serviceStatus {
            case .enabled:
                if launchdStatus?.failureDescription != nil {
                    return "Background item launch failed"
                }
                return "Background item allowed"
            case .requiresApproval:
                return "Background item needs approval"
            case .notRegistered:
                return "Background item not registered"
            case .notFound:
                return "Background item missing from app bundle"
            @unknown default:
                return "Background item status unknown"
            }
        }

        var detail: String? {
            guard helperAvailable else { return helperDetail }
            return launchdStatus?.failureDescription ?? backgroundItemDescription
        }
    }

    struct LastActionStatus {
        let button: String
        let ok: Bool
        let message: String

        var failureDescription: String? {
            guard !ok else { return nil }
            let action = button == "remount" ? "repair" : button
            return "last \(action) failed: \(message)"
        }
    }

    struct RepairStatus {
        let active: Bool
        let step: String
        let state: String
        let message: String
        let updatedUnixMillis: UInt64

        var displayText: String {
            if !message.isEmpty {
                return message
            }
            switch state {
            case "ok":
                return "Finished \(step)"
            case "failed":
                return "Failed \(step)"
            default:
                return "Running \(step)"
            }
        }
    }

    struct LaunchdStatus {
        let jobState: String?
        let lastExitCode: String?
        let commandError: String?

        var failureDescription: String? {
            if let commandError {
                return commandError
            }
            if let jobState, jobState.localizedCaseInsensitiveContains("failed") {
                if let lastExitCode, isFailureExit(lastExitCode) {
                    return "launchd: \(jobState); last exit \(lastExitCode)"
                }
                return "launchd: \(jobState)"
            }
            if let lastExitCode, isFailureExit(lastExitCode) {
                return "launchd: last exit \(lastExitCode)"
            }
            return nil
        }

        private func isFailureExit(_ value: String) -> Bool {
            let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
            return !trimmed.hasPrefix("0") && !trimmed.hasPrefix("(")
        }
    }

    static var service: SMAppService {
        SMAppService.daemon(plistName: plistName)
    }

    static var status: SMAppService.Status {
        service.status
    }

    static func snapshot() -> StatusSnapshot {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("management snapshot started")
        let helper = helperStatus()
        let serviceStatus = status
        let launchdStatus = serviceStatus == .enabled ? inspectLaunchd() : nil
        let lastActionStatus = helper.available ? readLastActionStatus() : nil
        let snapshot = StatusSnapshot(
            helperAvailable: helper.available,
            helperDetail: helper.detail,
            serviceStatus: serviceStatus,
            launchdStatus: launchdStatus,
            lastActionStatus: lastActionStatus
        )
        logger.notice("management snapshot finished in \(elapsedMilliseconds(since: started), privacy: .public)ms helperAvailable=\(helper.available, privacy: .public) helperDetail=\(helper.detail ?? "none", privacy: .public) service=\(statusName(serviceStatus), privacy: .public) ready=\(snapshot.isReady, privacy: .public) detail=\(snapshot.detail ?? "none", privacy: .public)")
        return snapshot
    }

    private static func helperStatus() -> (available: Bool, detail: String?) {
        let path = helperURL.path
        let exists = FileManager.default.fileExists(atPath: path)
        let executable = FileManager.default.isExecutableFile(atPath: path)
        logger.notice("bundled helper availability checked path=\(path, privacy: .public) exists=\(exists, privacy: .public) executable=\(executable, privacy: .public)")
        if executable {
            return (true, nil)
        }
        if exists {
            return (false, "Bundled helper is not executable")
        }
        return (false, "Bundled helper missing from app bundle")
    }

    static func repairExtension() throws {
        logger.notice("repair-extension started helper=\(helperURL.path, privacy: .public)")
        let result = runHelper(arguments: ["repair-extension"])
        guard result.status == 0 else {
            let detail =
                result.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
                : result.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(result.status),
                userInfo: [
                    NSLocalizedDescriptionKey: detail.isEmpty
                        ? "vixen-management repair-extension exited \(result.status)"
                        : detail
                ]
            )
        }
        logger.notice("repair-extension succeeded stdout=\(result.stdout.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
    }

    static func repairAppRecords() throws {
        logger.notice("repair-app-records started helper=\(helperURL.path, privacy: .public)")
        let result = runHelper(arguments: ["repair-app-records"])
        guard result.status == 0 else {
            let detail =
                result.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
                : result.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(result.status),
                userInfo: [
                    NSLocalizedDescriptionKey: detail.isEmpty
                        ? "vixen-management repair-app-records exited \(result.status)"
                        : detail
                ]
            )
        }
        logger.notice("repair-app-records succeeded stdout=\(result.stdout.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
    }

    static func repairVixenFS(progress: @escaping @Sendable (String) -> Void) async throws {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("VixenFS repair transaction requested from app")
        progress("Requesting Vixen Management")
        let lastAction = try await runQueuedButton("mount") { repairStatus in
            progress(repairStatus.displayText)
        }
        try requireQueuedActionSucceeded(lastAction, name: "mount")
        progress("VixenFS mounted")
        logger.notice("VixenFS repair transaction finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
    }

    static func uninstallVixenFS(progress: @escaping @Sendable (String) -> Void) async throws {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("VixenFS uninstall transaction requested from app")
        progress("Requesting Vixen Management")
        let lastAction = try await runQueuedButton("uninstall") { repairStatus in
            progress(repairStatus.displayText)
        }
        try requireQueuedActionSucceeded(lastAction, name: "uninstall", allowMissingStatus: true)
        progress("VixenFS setup removed")
        logger.notice("VixenFS uninstall transaction finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
    }

    static func uninstallVixenFSSync() throws {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("VixenFS synchronous uninstall transaction requested")
        let lastAction = try runQueuedButtonSync("uninstall")
        try requireQueuedActionSucceeded(lastAction, name: "uninstall", allowMissingStatus: true)
        logger.notice("VixenFS synchronous uninstall transaction finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
    }

    static func runQueuedButton(
        _ name: String,
        progress: (@Sendable (RepairStatus) -> Void)? = nil
    ) async throws -> LastActionStatus? {
        let requestedAfterMillis = unixMillis()
        let trigger = try fireQueue(name)
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("waiting for management queue name=\(name, privacy: .public) trigger=\(trigger.path, privacy: .public)")
        while elapsedMilliseconds(since: started) < 90_000 {
            if !FileManager.default.fileExists(atPath: trigger.path) {
                let lastAction = readLastActionStatus()
                logger.notice("management queue consumed name=\(name, privacy: .public) elapsedMs=\(elapsedMilliseconds(since: started), privacy: .public) lastActionOk=\(lastAction?.ok.description ?? "none", privacy: .public)")
                return lastAction
            }
            if let repairStatus = readRepairStatus(),
                repairStatus.updatedUnixMillis >= requestedAfterMillis
            {
                progress?(repairStatus)
            }
            try await Task.sleep(for: .milliseconds(250))
        }

        logger.error("management queue timed out name=\(name, privacy: .public) trigger=\(trigger.path, privacy: .public)")
        throw NSError(
            domain: NSPOSIXErrorDomain,
            code: Int(ETIMEDOUT),
            userInfo: [NSLocalizedDescriptionKey: "Timed out waiting for Vixen Management"]
        )
    }

    static func runQueuedButtonSync(_ name: String) throws -> LastActionStatus? {
        let trigger = try fireQueue(name)
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("waiting synchronously for management queue name=\(name, privacy: .public) trigger=\(trigger.path, privacy: .public)")
        while elapsedMilliseconds(since: started) < 90_000 {
            if !FileManager.default.fileExists(atPath: trigger.path) {
                let lastAction = readLastActionStatus()
                logger.notice("synchronous management queue consumed name=\(name, privacy: .public) elapsedMs=\(elapsedMilliseconds(since: started), privacy: .public) lastActionOk=\(lastAction?.ok.description ?? "none", privacy: .public)")
                if let lastAction, !lastAction.ok {
                    throw NSError(
                        domain: NSPOSIXErrorDomain,
                        code: Int(EIO),
                        userInfo: [
                            NSLocalizedDescriptionKey: lastAction.failureDescription ?? lastAction.message
                        ]
                    )
                }
                return lastAction
            }
            Thread.sleep(forTimeInterval: 0.25)
        }

        logger.error("synchronous management queue timed out name=\(name, privacy: .public) trigger=\(trigger.path, privacy: .public)")
        throw NSError(
            domain: NSPOSIXErrorDomain,
            code: Int(ETIMEDOUT),
            userInfo: [NSLocalizedDescriptionKey: "Timed out waiting for Vixen Management"]
        )
    }

    fileprivate static func requireQueuedActionSucceeded(
        _ lastAction: LastActionStatus?,
        name: String,
        allowMissingStatus: Bool = false
    ) throws {
        guard let lastAction else {
            logger.notice("management queue finished without last-action status name=\(name, privacy: .public) allowMissing=\(allowMissingStatus, privacy: .public)")
            if allowMissingStatus {
                return
            }
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(EIO),
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "Vixen Management finished \(name) without recording a result"
                ]
            )
        }
        guard lastAction.ok else {
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(EIO),
                userInfo: [
                    NSLocalizedDescriptionKey: lastAction.failureDescription ?? lastAction.message
                ]
            )
        }
    }

    static func register() throws {
        let daemon = service
        logger.notice("management register requested currentStatus=\(statusName(daemon.status), privacy: .public)")
        // Skip only when approval is already pending — calling register() during
        // that window is incorrect. For .enabled we still call through so that a
        // PKG upgrade that replaced the binary gets its launchd job hash refreshed
        // (Apple docs: register() on an already-enabled service is non-destructive
        // and does not show the approval dialog again).
        if daemon.status == .requiresApproval {
            logger.notice("management register skipped because approval is pending currentStatus=\(statusName(daemon.status), privacy: .public)")
            return
        }

        try registerService()
    }

    private static func registerService() throws {
        do {
            logger.notice("SMAppService.register starting plist=\(plistName, privacy: .public)")
            try service.register()
            logger.notice("SMAppService.register returned status=\(statusName(service.status), privacy: .public)")
        } catch {
            logger.error("SMAppService.register threw status=\(statusName(service.status), privacy: .public) error=\(error.localizedDescription, privacy: .public)")
            switch service.status {
            case .enabled, .requiresApproval:
                logger.notice("SMAppService.register error treated as acceptable status=\(statusName(service.status), privacy: .public)")
                return
            default:
                throw error
            }
        }
    }

    static func refreshRegistration() throws {
        let daemon = service
        logger.notice("management refresh requested currentStatus=\(statusName(daemon.status), privacy: .public)")
        switch daemon.status {
        case .enabled:
            logger.notice("SMAppService.unregister before refresh starting")
            try daemon.unregister()
            logger.notice("SMAppService.unregister before refresh returned status=\(statusName(service.status), privacy: .public)")
        case .requiresApproval:
            logger.notice("management refresh skipped because approval is pending")
            return
        default:
            break
        }

        try registerService()
    }

    static func unregister() throws {
        let daemon = service
        logger.notice("management unregister requested currentStatus=\(statusName(daemon.status), privacy: .public)")
        switch daemon.status {
        case .notFound, .notRegistered:
            logger.notice("management unregister skipped currentStatus=\(statusName(daemon.status), privacy: .public)")
            return
        default:
            try daemon.unregister()
            logger.notice("management unregister returned status=\(statusName(service.status), privacy: .public)")
        }
    }

    static func printSnapshot(_ snapshot: StatusSnapshot = snapshot()) {
        print("Vixen Management: \(snapshot.description)")
        if let detail = snapshot.detail {
            print("  \(detail)")
        }
        if let lastAction = snapshot.lastActionStatus {
            print("  last \(lastAction.button): \(lastAction.ok ? "ok" : "failed")")
            if !lastAction.message.isEmpty {
                print("    \(lastAction.message)")
            }
        }
    }

    static func fireQueue(_ name: String) throws -> URL {
        logger.notice("firing management queue name=\(name, privacy: .public)")
        let trigger = queueRoot
            .appendingPathComponent(name, isDirectory: true)
            .appendingPathComponent("trigger-\(getpid())-\(UUID().uuidString)")
        let fd = trigger.path.withCString {
            Darwin.open($0, O_CREAT | O_EXCL | O_WRONLY, mode_t(S_IRUSR | S_IWUSR))
        }
        guard fd >= 0 else {
            let code = Int(errno)
            logger.error("failed to create management trigger name=\(name, privacy: .public) path=\(trigger.path, privacy: .public) errno=\(code, privacy: .public) message=\(String(cString: strerror(Int32(code))), privacy: .public)")
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: code,
                userInfo: [NSLocalizedDescriptionKey: String(cString: strerror(Int32(code)))]
            )
        }
        Darwin.close(fd)
        logger.notice("created management trigger name=\(name, privacy: .public) path=\(trigger.path, privacy: .public)")
        return trigger
    }

    private static func runHelper(arguments: [String]) -> (
        status: Int32, stdout: String, stderr: String
    ) {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("running bundled helper path=\(helperURL.path, privacy: .public) arguments=\(arguments.joined(separator: " "), privacy: .public)")
        let process = Process()
        process.executableURL = helperURL
        process.arguments = arguments

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            logger.error("bundled helper failed to start arguments=\(arguments.joined(separator: " "), privacy: .public) error=\(error.localizedDescription, privacy: .public)")
            return (127, "", error.localizedDescription)
        }

        process.waitUntilExit()
        let stdoutText =
            String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let stderrText =
            String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        logger.notice("bundled helper finished in \(elapsedMilliseconds(since: started), privacy: .public)ms status=\(process.terminationStatus, privacy: .public) stdout=\(stdoutText.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public) stderr=\(stderrText.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
        return (
            process.terminationStatus,
            stdoutText,
            stderrText
        )
    }

    private static func inspectLaunchd() -> LaunchdStatus {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("launchd inspection started label=\(label, privacy: .public)")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = ["print", "system/\(label)"]

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            logger.error("launchd inspection failed to start: \(error.localizedDescription, privacy: .public)")
            return LaunchdStatus(
                jobState: nil,
                lastExitCode: nil,
                commandError: "launchctl unavailable: \(error.localizedDescription)"
            )
        }

        process.waitUntilExit()

        let output = String(
            data: stdout.fileHandleForReading.readDataToEndOfFile(),
            encoding: .utf8
        ) ?? ""
        let errorOutput = String(
            data: stderr.fileHandleForReading.readDataToEndOfFile(),
            encoding: .utf8
        )?
            .trimmingCharacters(in: .whitespacesAndNewlines)

        if process.terminationStatus != 0 {
            let detail =
                errorOutput?.isEmpty == false ? errorOutput! : "exit \(process.terminationStatus)"
            logger.error("launchd inspection failed in \(elapsedMilliseconds(since: started), privacy: .public)ms detail=\(detail, privacy: .public)")
            return LaunchdStatus(
                jobState: nil,
                lastExitCode: nil,
                commandError: "launchctl print failed: \(detail)"
            )
        }

        var jobState: String?
        var lastExitCode: String?
        for rawLine in output.split(whereSeparator: \.isNewline) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if let value = value(in: line, after: "job state = ") {
                jobState = value
            } else if let value = value(in: line, after: "last exit code = ") {
                lastExitCode = value
            }
        }

        logger.notice("launchd inspection finished in \(elapsedMilliseconds(since: started), privacy: .public)ms jobState=\(jobState ?? "none", privacy: .public) lastExitCode=\(lastExitCode ?? "none", privacy: .public)")
        return LaunchdStatus(
            jobState: jobState,
            lastExitCode: lastExitCode,
            commandError: nil
        )
    }

    private static func value(in line: String, after prefix: String) -> String? {
        guard line.hasPrefix(prefix) else { return nil }
        return String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }

    private static func readLastActionStatus() -> LastActionStatus? {
        guard let data = try? Data(contentsOf: lastActionURL) else {
            logger.notice("management last-action missing path=\(lastActionURL.path, privacy: .public)")
            return nil
        }
        guard
            let object = try? JSONSerialization.jsonObject(with: data),
            let dictionary = object as? [String: Any],
            let button = dictionary["button"] as? String,
            let ok = dictionary["ok"] as? Bool
        else {
            logger.error("management last-action unreadable path=\(lastActionURL.path, privacy: .public)")
            return nil
        }
        let message = dictionary["message"] as? String ?? ""
        logger.notice("management last-action read button=\(button, privacy: .public) ok=\(ok, privacy: .public) message=\(message, privacy: .public)")
        return LastActionStatus(button: button, ok: ok, message: message)
    }

    private static func readRepairStatus() -> RepairStatus? {
        guard let data = try? Data(contentsOf: repairStatusURL) else {
            return nil
        }
        guard
            let object = try? JSONSerialization.jsonObject(with: data),
            let dictionary = object as? [String: Any],
            let active = dictionary["active"] as? Bool,
            let step = dictionary["step"] as? String,
            let state = dictionary["state"] as? String
        else {
            logger.error("management repair-status unreadable path=\(repairStatusURL.path, privacy: .public)")
            return nil
        }
        let message = dictionary["message"] as? String ?? ""
        let updatedUnixMillis =
            (dictionary["updated_unix_millis"] as? NSNumber)?.uint64Value ?? 0
        logger.notice("management repair-status read active=\(active, privacy: .public) step=\(step, privacy: .public) state=\(state, privacy: .public) updatedUnixMillis=\(updatedUnixMillis, privacy: .public) message=\(message, privacy: .public)")
        return RepairStatus(
            active: active,
            step: step,
            state: state,
            message: message,
            updatedUnixMillis: updatedUnixMillis
        )
    }

    private static func statusName(_ status: SMAppService.Status) -> String {
        switch status {
        case .enabled:
            "enabled"
        case .requiresApproval:
            "requiresApproval"
        case .notRegistered:
            "notRegistered"
        case .notFound:
            "notFound"
        @unknown default:
            "unknown"
        }
    }

    private static func elapsedMilliseconds(since started: UInt64) -> Int {
        let elapsed = DispatchTime.now().uptimeNanoseconds - started
        return Int(elapsed / 1_000_000)
    }

    private static func unixMillis() -> UInt64 {
        UInt64((Date().timeIntervalSince1970 * 1000).rounded(.down))
    }
}

private enum ManagementCommand {
    static func run(_ arguments: [String]) -> Int32 {
        guard let command = arguments.first else {
            ManagementService.logger.error("management CLI invoked without command")
            printUsage(to: stderr)
            return 64
        }
        ManagementService.logger.notice("management CLI command started command=\(command, privacy: .public) arguments=\(arguments.joined(separator: " "), privacy: .public)")

        do {
            switch command {
            case "register":
                try ManagementService.register()
                ManagementService.printSnapshot()
            case "refresh":
                try ManagementService.refreshRegistration()
                ManagementService.printSnapshot()
            case "unregister":
                try ManagementService.unregister()
                ManagementService.printSnapshot()
            case "repair-app-records":
                try ManagementService.repairAppRecords()
                print("Vixen Management: repaired app registration")
            case "repair-extension":
                try ManagementService.repairExtension()
                print("Vixen Management: repaired extension registration")
            case "repair":
                let lastAction = try ManagementService.runQueuedButtonSync("mount")
                try ManagementService.requireQueuedActionSucceeded(lastAction, name: "mount")
                print("Vixen Management: mounted VixenFS")
                ManagementService.printSnapshot()
            case "uninstall":
                try ManagementService.uninstallVixenFSSync()
                print("Vixen Management: uninstalled VixenFS setup")
            case "status":
                ManagementService.printSnapshot()
            case "mount", "remount":
                let lastAction = try ManagementService.runQueuedButtonSync(command)
                try ManagementService.requireQueuedActionSucceeded(lastAction, name: command)
                print("Vixen Management: \(command) completed")
                ManagementService.printSnapshot()
            case "unmount", "kill-fskitd":
                _ = try ManagementService.runQueuedButtonSync(command)
                print("Vixen Management: queued \(command)")
                ManagementService.printSnapshot()
            case "help", "--help", "-h":
                printUsage(to: stdout)
            default:
                ManagementService.logger.error("management CLI unknown command=\(command, privacy: .public)")
                printUsage(to: stderr)
                return 64
            }
            ManagementService.logger.notice("management CLI command succeeded command=\(command, privacy: .public)")
            return 0
        } catch {
            ManagementService.logger.error("management CLI command failed command=\(command, privacy: .public) error=\(error.localizedDescription, privacy: .public)")
            fputs("Vixen Management \(command) failed: \(error.localizedDescription)\n", stderr)
            return 1
        }
    }

    private static func printUsage(to stream: UnsafeMutablePointer<FILE>) {
        fputs(
            """
            Usage: Vixen management register|refresh|status|unregister|uninstall|repair|repair-app-records|repair-extension|mount|unmount|remount|kill-fskitd

            """,
            stream
        )
    }

}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    private let logger = Logger(subsystem: "eu.bearcove.vixen", category: "DoctorUI")
    @State private var extensionSnapshot = FSKitModuleSnapshot.pending
    @State private var managementSnapshot = ManagementService.StatusSnapshot(
        helperAvailable: false,
        helperDetail: "Not checked",
        serviceStatus: .notFound,
        launchdStatus: nil,
        lastActionStatus: nil
    )
    @State private var managementError: String? = nil
    @State private var isConfiguringManagement = false
    @State private var didRequestManagementSetup = false
    @State private var doctorSnapshot = DoctorSnapshot.pending
    @State private var isCheckingDoctor = false
    @State private var isAutoRepairingDoctor = false
    @State private var doctorProgressText: String? = nil
    @State private var repairProgressText: String? = nil
    @State private var uninstallDialog: UninstallDialog? = nil
    @State private var uninstallProgressText: String? = nil
    @State private var isUninstalling = false
    @State private var demoToggleOn = false
    @State private var cursorBob = false

    private let requiredMajorVersion = 15
    private let foxSize: CGFloat = 96
    private let brandOrange = Color(hex: 0xE88602)
    private let baseBg = Color(light: 0xFFFFFF, dark: 0x1E2020)
    private let sectionBg = Color(light: 0xF0F0F0, dark: 0x252626)
    private let sectionRowBg = Color(light: 0xFFFFFF, dark: 0x2E2E2E)

    private var isSupportedOS: Bool {
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= requiredMajorVersion
    }
    private var isManagementConfigured: Bool {
        managementSnapshot.isReady
    }
    private var isExtensionEnabled: Bool {
        extensionSnapshot.enabled
    }
    private var needsExtensionEnablement: Bool {
        extensionSnapshot.error == nil && (!extensionSnapshot.found || !extensionSnapshot.enabled)
    }
    private var shouldUseDoctorFlow: Bool {
        isManagementConfigured && !needsExtensionEnablement
    }

    private var contentMinHeight: CGFloat {
        if shouldUseDoctorFlow {
            let extraRows = max(0, doctorSnapshot.problemRows.count - 1)
            return doctorSnapshot.problemRows.isEmpty ? 500 : 620 + CGFloat(extraRows * 56)
        }
        return 500
    }

    var body: some View {
        Group {
            if !isSupportedOS {
                unsupportedOSView
            } else if needsExtensionEnablement {
                setupView
            } else if !isManagementConfigured {
                managementSetupView
            } else {
                completeView
            }
        }
        .padding(36)
        .frame(width: 720)
        .frame(minHeight: contentMinHeight)
        .background(baseBg)
        .task {
            guard isSupportedOS else { return }
            await pollExtensionStatus()
        }
        .task {
            guard isSupportedOS else { return }
            await pollManagementStatus()
        }
        .task {
            guard isSupportedOS else { return }
            await animateToggleDemo()
        }
        .task {
            guard isSupportedOS else { return }
            await pollDoctorStatus()
        }
        .onAppear {
            cursorBob = true
        }
        .onReceive(
            NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
        ) { _ in
            guard isSupportedOS else { return }
            Task {
                await checkExtensionOnce()
                refreshManagementStatus()
                if shouldUseDoctorFlow {
                    await refreshDoctorStatus()
                }
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .vixenUninstallRequested)) { _ in
            uninstallDialog = .confirm
        }
        .alert(item: $uninstallDialog) { dialog in
            switch dialog {
            case .confirm:
                return Alert(
                    title: Text("Uninstall Vixen?"),
                    message: Text(
                        "This unmounts VixenFS, removes Vixen's privileged management state, removes the installed app, and quits Vixen."
                    ),
                    primaryButton: .destructive(Text("Uninstall")) {
                        Task { await performUninstall() }
                    },
                    secondaryButton: .cancel()
                )
            case .completed:
                return Alert(
                    title: Text("Vixen removed"),
                    message: Text("VixenFS setup and the installed app were removed."),
                    dismissButton: .default(Text("OK"))
                )
            case .failed(let message):
                return Alert(
                    title: Text("Uninstall failed"),
                    message: Text(message),
                    dismissButton: .default(Text("OK"))
                )
            }
        }
        .overlay(alignment: .bottom) {
            if let uninstallProgressText {
                HStack(spacing: 10) {
                    ProgressView()
                        .controlSize(.small)
                        .tint(brandOrange)
                    Text(uninstallProgressText)
                        .font(.system(size: 12, weight: .medium))
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(
                    sectionBg,
                    in: RoundedRectangle(cornerRadius: 10, style: .continuous)
                )
                .padding(.bottom, 8)
            }
        }
    }

    // MARK: - Unsupported OS

    private var unsupportedOSView: some View {
        VStack(spacing: 24) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 128, height: 128)

            Text("macOS \(requiredMajorVersion) Required")
                .font(.system(size: 24, weight: .bold))

            Text("Vixen needs FSKit features available in macOS \(requiredMajorVersion) and later.")
                .font(.system(size: 15))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            Button {
                if let url = URL(string: "https://app.vixen.rs/") {
                    NSWorkspace.shared.open(url)
                }
            } label: {
                Label("Learn More", systemImage: "arrow.up.right")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 28)
                    .padding(.vertical, 12)
                    .background(
                        brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
        }
    }

    // MARK: - Setup (Almost there)

    private var setupView: some View {
        HStack(alignment: .center, spacing: 32) {
            // Left column: fox (fixed width)
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: foxSize, height: foxSize)

            // Center column: content (takes all remaining space)
            VStack(spacing: 16) {
                Text("Almost there")
                    .font(.system(size: 24, weight: .bold))

                Text(
                    "Vixen uses a virtual file system to give your builds a hermetic, reproducible view of the world. Enable VixenFS in System Settings to get started."
                )
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

                settingsPreview
                    .onTapGesture { openExtensionSettings() }
                    .onHover { hovering in
                        if hovering {
                            NSCursor.pointingHand.push()
                        } else {
                            NSCursor.pop()
                        }
                    }

                if let error = extensionSnapshot.error {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.yellow)
                        Text(error)
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                    }
                }

                Button(action: openExtensionSettings) {
                    Text("Open Settings")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 28)
                        .padding(.vertical, 12)
                        .background(
                            brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                }
                .buttonStyle(.plain)
            }
            .frame(maxWidth: .infinity)

            // Right column: balance (fixed width, matches fox)
            Spacer()
                .frame(width: foxSize)
        }
    }

    // MARK: - Extension status

    private func checkExtensionOnce() async {
        logger.notice("extension status check started")
        guard #available(macOS 15.4, *) else {
            extensionSnapshot = FSKitModuleSnapshot(
                found: false,
                enabled: false,
                urlPath: nil,
                error: "VixenFS extension status requires macOS 15.4 or later."
            )
            logger.error("extension status check unavailable on this macOS version")
            return
        }

        do {
            let extensions = try await FSClient.shared.installedExtensions
            let found = extensions.first { $0.bundleIdentifier == "eu.bearcove.vixenfs" }
            let urlPath = found?.url.path
            extensionSnapshot = FSKitModuleSnapshot(
                found: found != nil,
                enabled: found?.isEnabled ?? false,
                urlPath: urlPath,
                error: nil
            )
            logger.notice("extension status check finished found=\(found != nil, privacy: .public) enabled=\(extensionSnapshot.enabled, privacy: .public) url=\(urlPath ?? "none", privacy: .public)")
        } catch {
            extensionSnapshot = FSKitModuleSnapshot(
                found: false,
                enabled: false,
                urlPath: nil,
                error: error.localizedDescription
            )
            logger.error("extension status check failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func pollExtensionStatus() async {
        logger.notice("extension status polling started")
        while !Task.isCancelled {
            await checkExtensionOnce()
            try? await Task.sleep(for: .seconds(4))
        }
        logger.notice("extension status polling stopped")
    }

    private func animateToggleDemo() async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(1.0))
            withAnimation(.easeInOut(duration: 0.3)) { demoToggleOn = true }
            try? await Task.sleep(for: .seconds(2.0))
            withAnimation(.easeInOut(duration: 0.3)) { demoToggleOn = false }
        }
    }

    private func openExtensionSettings() {
        logger.notice("opening extension settings")
        let targets = [
            "x-apple.systempreferences:com.apple.ExtensionsPreferences?extensionPointIdentifier=com.apple.fskit.fsmodule",
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        ]
        for target in targets {
            if let url = URL(string: target), NSWorkspace.shared.open(url) {
                logger.notice("opened extension settings target=\(target, privacy: .public)")
                return
            }
        }
        logger.notice("falling back to opening System Settings app")
        NSWorkspace.shared.open(URL(fileURLWithPath: "/System/Applications/System Settings.app"))
    }

    // MARK: - Management daemon

    private var managementSetupView: some View {
        HStack(alignment: .center, spacing: 32) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: foxSize, height: foxSize)

            VStack(spacing: 16) {
                Text("One more step")
                    .font(.system(size: 24, weight: .bold))

                Text(
                    "Allow Vixen's background management item so it can mount and recover VixenFS when privileged macOS filesystem work is needed."
                )
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

                managementSetupPanel
            }
            .frame(maxWidth: .infinity)

            Spacer()
                .frame(width: foxSize)
        }
        .task {
            guard !didRequestManagementSetup else { return }
            didRequestManagementSetup = true
            logger.notice("management setup view appeared; configuring management daemon")
            configureManagementDaemon()
        }
    }

    private var managementStatusText: String {
        managementSnapshot.description
    }

    private var managementDetailText: String? {
        managementError ?? managementSnapshot.detail
    }

    private var managementButtonTitle: String {
        if isManagementConfigured {
            return "Check"
        }
        switch managementSnapshot.serviceStatus {
        case .enabled:
            return "Retry"
        case .requiresApproval:
            return "Open Settings"
        default:
            return "Configure"
        }
    }

    private var managementSetupPanel: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: isManagementConfigured ? "checkmark.shield.fill" : "lock.shield")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(isManagementConfigured ? Color.green : brandOrange)
                .frame(width: 28)

            VStack(alignment: .leading, spacing: 3) {
                Text("Management Daemon")
                    .font(.system(size: 13, weight: .semibold))
                Text(managementStatusText)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                if let managementDetailText {
                    Text(managementDetailText)
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            Spacer()

            Button(action: managementButtonAction) {
                Text(managementButtonTitle)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 8)
                    .background(
                        brandOrange,
                        in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                    )
            }
            .buttonStyle(.plain)
            .disabled(isConfiguringManagement)
        }
        .padding(12)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(sectionBg)
        )
    }

    private func managementButtonAction() {
        logger.notice("management button action configured=\(isManagementConfigured, privacy: .public) service=\(managementStatusText, privacy: .public)")
        if isManagementConfigured {
            Task { await refreshDoctorStatus() }
        } else if managementSnapshot.serviceStatus == .requiresApproval {
            openLoginItemsSettings()
        } else if managementSnapshot.serviceStatus == .enabled {
            refreshManagementDaemon()
        } else {
            configureManagementDaemon()
        }
    }

    private func refreshManagementStatus() {
        let snapshot = ManagementService.snapshot()
        managementSnapshot = snapshot
        logger.notice("management status refreshed ready=\(snapshot.isReady, privacy: .public) description=\(snapshot.description, privacy: .public) detail=\(snapshot.detail ?? "none", privacy: .public)")
    }

    @MainActor
    private func refreshDoctorStatus() async {
        guard !isCheckingDoctor else {
            logger.notice("doctor ui refresh skipped because a check is already running")
            return
        }
        let started = DispatchTime.now().uptimeNanoseconds
        isCheckingDoctor = true
        doctorProgressText = "Checking local setup"
        defer {
            isCheckingDoctor = false
            doctorProgressText = nil
        }
        await checkExtensionOnce()
        logger.notice("doctor ui refresh started extensionEnabled=\(isExtensionEnabled, privacy: .public) managementReady=\(isManagementConfigured, privacy: .public)")
        let firstSnapshot = await DoctorService.check(
            extensionSnapshot: extensionSnapshot
        )
        logger.notice("doctor first snapshot ready=\(firstSnapshot.isReady, privacy: .public) failures=\(firstSnapshot.rows.filter { $0.state == .failed }.count, privacy: .public) warnings=\(firstSnapshot.rows.filter { $0.state == .warning }.count, privacy: .public) canSafeRepair=\(firstSnapshot.canAttemptSafeMountRepair, privacy: .public) needsFskitRestart=\(firstSnapshot.needsFskitRestart, privacy: .public)")

        doctorSnapshot = firstSnapshot
        logger.notice("doctor ui accepted snapshot without mutation")
        refreshManagementStatus()
        logger.notice("doctor ui refresh finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
    }

    private func pollManagementStatus() async {
        logger.notice("management status polling started")
        while !Task.isCancelled {
            refreshManagementStatus()
            try? await Task.sleep(for: .seconds(12))
        }
        logger.notice("management status polling stopped")
    }

    private func pollDoctorStatus() async {
        logger.notice("doctor polling started")
        while !Task.isCancelled {
            if shouldUseDoctorFlow && !isAutoRepairingDoctor {
                logger.notice("doctor polling triggering refresh")
                await refreshDoctorStatus()
            } else {
                logger.notice("doctor polling skipped found=\(extensionSnapshot.found, privacy: .public) extensionEnabled=\(isExtensionEnabled, privacy: .public) managementReady=\(isManagementConfigured, privacy: .public) repairing=\(isAutoRepairingDoctor, privacy: .public)")
            }
            try? await Task.sleep(for: .seconds(30))
        }
        logger.notice("doctor polling stopped")
    }

    @MainActor
    private func performDoctorRepair() async {
        guard !isAutoRepairingDoctor else {
            logger.notice("doctor repair skipped because repair is already running")
            return
        }

        isAutoRepairingDoctor = true
        repairProgressText = "Starting repair"
        defer {
            isAutoRepairingDoctor = false
            repairProgressText = nil
        }
        logger.notice("doctor repair started")

        do {
            try await ManagementService.repairVixenFS { message in
                Task { @MainActor in
                    repairProgressText = message
                }
            }
            managementError = nil
        } catch {
            managementError = error.localizedDescription
            logger.error("doctor repair failed: \(error.localizedDescription, privacy: .public)")
        }

        try? await Task.sleep(for: .milliseconds(750))
        await checkExtensionOnce()
        doctorSnapshot = await DoctorService.check(
            extensionSnapshot: extensionSnapshot
        )
        refreshManagementStatus()
        logger.notice("doctor repair finished ready=\(doctorSnapshot.isReady, privacy: .public) failures=\(doctorSnapshot.rows.filter { $0.state == .failed }.count, privacy: .public) warnings=\(doctorSnapshot.rows.filter { $0.state == .warning }.count, privacy: .public)")
    }

    @MainActor
    private func performUninstall() async {
        guard !isUninstalling else {
            logger.notice("uninstall skipped because uninstall is already running")
            return
        }
        isUninstalling = true
        uninstallProgressText = "Starting uninstall"
        logger.notice("uninstall started from UI")
        defer {
            isUninstalling = false
            uninstallProgressText = nil
        }

        do {
            try await ManagementService.uninstallVixenFS { message in
                Task { @MainActor in
                    uninstallProgressText = message
                }
            }
            logger.notice("uninstall finished successfully; terminating app")
            NSApplication.shared.terminate(nil)
        } catch {
            logger.error("uninstall failed: \(error.localizedDescription, privacy: .public)")
            uninstallDialog = .failed(error.localizedDescription)
            refreshManagementStatus()
        }
    }

    private func elapsedMilliseconds(since started: UInt64) -> Int {
        let elapsed = DispatchTime.now().uptimeNanoseconds - started
        return Int(elapsed / 1_000_000)
    }

    private func configureManagementDaemon() {
        logger.notice("configure management daemon started")
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
            logger.notice("configure management daemon finished configured=\(isManagementConfigured, privacy: .public) error=\(managementError ?? "none", privacy: .public)")
        }

        do {
            try ManagementService.register()
            managementError = nil
            refreshManagementStatus()
            if managementSnapshot.serviceStatus == .requiresApproval {
                logger.notice("configure management daemon requires approval; opening settings")
                openLoginItemsSettings()
            }
        } catch {
            refreshManagementStatus()
            if isManagementConfigured || managementSnapshot.serviceStatus == .requiresApproval {
                managementError = nil
            } else {
                managementError = error.localizedDescription
            }
            logger.error("configure management daemon failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func refreshManagementDaemon() {
        logger.notice("refresh management daemon started")
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
            logger.notice("refresh management daemon finished configured=\(isManagementConfigured, privacy: .public) error=\(managementError ?? "none", privacy: .public)")
        }

        do {
            try ManagementService.refreshRegistration()
            managementError = nil
            refreshManagementStatus()
            if managementSnapshot.serviceStatus == .requiresApproval {
                logger.notice("refresh management daemon requires approval; opening settings")
                openLoginItemsSettings()
            }
        } catch {
            refreshManagementStatus()
            if isManagementConfigured || managementSnapshot.serviceStatus == .requiresApproval {
                managementError = nil
            } else {
                managementError = error.localizedDescription
            }
            logger.error("refresh management daemon failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    private func openLoginItemsSettings() {
        let targets = [
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
            "x-apple.systempreferences:com.apple.ExtensionsPreferences",
        ]
        for target in targets {
            if let url = URL(string: target), NSWorkspace.shared.open(url) {
                return
            }
        }
        NSWorkspace.shared.open(URL(fileURLWithPath: "/System/Applications/System Settings.app"))
    }

    // MARK: - Settings preview

    private func settingsDiskIcon(filled: Bool) -> some View {
        Image(systemName: filled ? "externaldrive.fill" : "externaldrive")
            .font(.system(size: 14, weight: .medium))
            .foregroundStyle(.white)
            .frame(width: 32, height: 32)
            .background(Color(nsColor: .systemGray))
            .clipShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
    }

    private var settingsPreview: some View {
        VStack(spacing: 0) {
            HStack(spacing: 5) {
                Image(systemName: "gearshape.fill")
                    .font(.system(size: 9))
                Text("System Settings")
                    .font(.system(size: 11, weight: .medium))
                Image(systemName: "chevron.right")
                    .font(.system(size: 7, weight: .bold))
                Text("General")
                    .font(.system(size: 11, weight: .medium))
                Image(systemName: "chevron.right")
                    .font(.system(size: 7, weight: .bold))
                Text("Login Items & Extensions")
                    .font(.system(size: 11, weight: .medium))
            }
            .foregroundStyle(.tertiary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)

            Divider()
                .opacity(0.5)

            VStack(spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    settingsDiskIcon(filled: false)
                    VStack(alignment: .leading, spacing: 3) {
                        Text("File System Extensions")
                            .font(.system(size: 13, weight: .semibold))
                        Text(
                            "File System extensions allow you to extend the file system functionality of macOS without requiring kernel-level access."
                        )
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(sectionRowBg)
                )

                ZStack(alignment: .trailing) {
                    HStack(alignment: .center, spacing: 12) {
                        settingsDiskIcon(filled: true)
                        Text("VixenFS")
                            .font(.system(size: 13, weight: .medium))
                        Spacer()
                        MockToggle(isOn: isExtensionEnabled || demoToggleOn)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                    .background(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(sectionRowBg)
                    )

                    Image(systemName: "cursorarrow.fill")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(
                            colorScheme == .dark
                                ? Color.white.opacity(0.85)
                                : Color.black.opacity(0.7)
                        )
                        .offset(
                            x: cursorBob ? -8 : -28,
                            y: cursorBob ? -4 : 8
                        )
                        .animation(
                            .easeInOut(duration: 1.25).repeatForever(autoreverses: true),
                            value: cursorBob
                        )
                        .padding(.trailing, 14)
                }
            }
            .padding(10)
            .opacity(0.85)
        }
        .background(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .fill(sectionBg)
        )
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(brandOrange.opacity(0.25), lineWidth: 1)
        )
    }

    // MARK: - Doctor

    private var doctorPanel: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .top, spacing: 10) {
                Group {
                    if isCheckingDoctor || isAutoRepairingDoctor {
                        ProgressView()
                            .controlSize(.small)
                            .tint(brandOrange)
                    } else {
                        Image(systemName: doctorSnapshot.hasFailure ? "stethoscope" : "checkmark.shield.fill")
                            .font(.system(size: 17, weight: .semibold))
                            .foregroundStyle(doctorSnapshot.hasFailure ? brandOrange : Color.green)
                    }
                }
                .frame(width: 28, height: 28)

                VStack(alignment: .leading, spacing: 2) {
                    Text("System Check")
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                    Text(doctorSummaryText)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                Spacer(minLength: 0)
            }

            HStack(spacing: 10) {
                Spacer(minLength: 0)

                if let doctorPrimaryActionTitle {
                    Button(action: doctorPrimaryAction) {
                        Label(doctorPrimaryActionTitle, systemImage: "wrench.and.screwdriver.fill")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(.white)
                            .lineLimit(1)
                            .padding(.horizontal, 12)
                            .frame(minWidth: 150, minHeight: 30)
                            .background(
                                brandOrange,
                                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                            )
                    }
                    .buttonStyle(.plain)
                    .disabled(isConfiguringManagement || isCheckingDoctor || isAutoRepairingDoctor)
                }

                Button(action: { Task { await refreshDoctorStatus() } }) {
                    Label("Check Again", systemImage: "arrow.clockwise")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(brandOrange)
                        .lineLimit(1)
                        .padding(.horizontal, 12)
                        .frame(minWidth: 132, minHeight: 30)
                        .background(
                            brandOrange.opacity(0.12),
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                }
                .buttonStyle(.plain)
                .disabled(isCheckingDoctor || isAutoRepairingDoctor)
            }

            if let activeProgressText {
                Text(activeProgressText)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if !doctorSnapshot.problemRows.isEmpty {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.circle.fill")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(brandOrange)

                        Text("What needs attention")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(.primary)

                        Spacer()
                    }

                    VStack(spacing: 9) {
                        ForEach(doctorSnapshot.problemRows) { row in
                            DoctorRowView(row: row, brandOrange: brandOrange)
                        }
                    }
                }
                .padding(.top, 2)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, minHeight: 132, alignment: .top)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(sectionBg)
        )
    }

    private var doctorPrimaryActionTitle: String? {
        if doctorSnapshot.hasFailure {
            return "Repair VixenFS"
        }
        return nil
    }

    private func doctorPrimaryAction() {
        Task { await performDoctorRepair() }
    }

    private var doctorSummaryText: String {
        if isAutoRepairingDoctor {
            return "Repairing VixenFS"
        }
        if isCheckingDoctor {
            return "Checking"
        }
        let failed = doctorSnapshot.rows.filter { $0.state == .failed }.count
        let warnings = doctorSnapshot.rows.filter { $0.state == .warning }.count
        if failed > 0 {
            return "\(failed) issue\(failed == 1 ? "" : "s")"
        }
        if warnings > 0 {
            return "\(warnings) warning\(warnings == 1 ? "" : "s")"
        }
        if doctorSnapshot.isReady {
            return "Ready"
        }
        return "Not checked"
    }

    private var activeProgressText: String? {
        if isAutoRepairingDoctor {
            return repairProgressText
        }
        if isCheckingDoctor {
            return doctorProgressText
        }
        return nil
    }

    private var completeTitle: String {
        if isCheckingDoctor || isAutoRepairingDoctor || (!doctorSnapshot.isReady && !doctorSnapshot.hasFailure) {
            return "Checking Vixen"
        }
        return doctorSnapshot.hasFailure ? "Needs attention" : "You're all set"
    }

    private var completeDescription: String {
        if !doctorSnapshot.isReady && !doctorSnapshot.hasFailure {
            return "Vixen is checking the local filesystem setup."
        }
        if doctorSnapshot.hasFailure {
            return "Vixen found local filesystem setup issues. Repair VixenFS runs the approved management helper once and reports progress here."
        }
        return "VixenFS is enabled. Head to the docs to install the CLI and run your first build."
    }

    // MARK: - Complete view

    private var completeView: some View {
        HStack(alignment: .center, spacing: 32) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: foxSize, height: foxSize)

            VStack(spacing: 20) {
                Text(completeTitle)
                    .font(.system(size: 24, weight: .bold))

                Text(completeDescription)
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

                if doctorSnapshot.isReady {
                    Button {
                        if let url = URL(string: "https://app.vixen.rs/") {
                            NSWorkspace.shared.open(url)
                        }
                    } label: {
                        Label("Get Started", systemImage: "arrow.up.right")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 28)
                            .padding(.vertical, 12)
                            .background(
                                brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }

                doctorPanel
            }
            .frame(maxWidth: .infinity)

            Spacer()
                .frame(width: foxSize)
        }
        .onAppear {
            refreshManagementStatus()
            Task { await refreshDoctorStatus() }
        }
    }
}

private enum UninstallDialog: Identifiable {
    case confirm
    case completed
    case failed(String)

    var id: String {
        switch self {
        case .confirm:
            "confirm"
        case .completed:
            "completed"
        case .failed(let message):
            "failed-\(message)"
        }
    }
}

private struct DoctorRowView: View {
    let row: DoctorRow
    let brandOrange: Color

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: iconName)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(iconColor)
                .frame(width: 18, height: 18)

            VStack(alignment: .leading, spacing: 2) {
                Text(row.title)
                    .font(.system(size: 12, weight: .medium))
                if let detail = row.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                        .lineLimit(nil)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var iconName: String {
        switch row.state {
        case .pending:
            "circle"
        case .checking:
            "arrow.triangle.2.circlepath"
        case .ok:
            "checkmark.circle.fill"
        case .warning:
            "exclamationmark.triangle.fill"
        case .failed:
            "xmark.circle.fill"
        }
    }

    private var iconColor: Color {
        switch row.state {
        case .pending:
            Color.secondary
        case .checking:
            brandOrange
        case .ok:
            Color.green
        case .warning:
            Color.yellow
        case .failed:
            Color.red
        }
    }
}

private struct MockToggle: View {
    let isOn: Bool

    var body: some View {
        Capsule()
            .fill(isOn ? Color.accentColor : Color(nsColor: .tertiaryLabelColor).opacity(0.4))
            .frame(width: 38, height: 22)
            .overlay(alignment: isOn ? .trailing : .leading) {
                Circle()
                    .fill(.white)
                    .shadow(color: .black.opacity(0.15), radius: 0.5, y: 0.5)
                    .frame(width: 18, height: 18)
                    .padding(.horizontal, 2)
            }
            .animation(.easeInOut(duration: 0.3), value: isOn)
    }
}

private enum SparkleBootstrap {
    static func makeUpdaterController() -> SPUStandardUpdaterController? {
        guard let info = Bundle.main.infoDictionary else { return nil }
        let feedURL = (info["SUFeedURL"] as? String)?.trimmingCharacters(
            in: .whitespacesAndNewlines)
        let publicKey = (info["SUPublicEDKey"] as? String)?.trimmingCharacters(
            in: .whitespacesAndNewlines)

        guard let feedURL, !feedURL.isEmpty, let publicKey, !publicKey.isEmpty else {
            return nil
        }

        return SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: nil
        )
    }
}

extension Color {
    fileprivate init(hex: UInt32) {
        let red = Double((hex >> 16) & 0xFF) / 255.0
        let green = Double((hex >> 8) & 0xFF) / 255.0
        let blue = Double(hex & 0xFF) / 255.0
        self.init(red: red, green: green, blue: blue)
    }

    fileprivate init(light: UInt32, dark: UInt32) {
        self.init(
            nsColor: NSColor(name: nil) { appearance in
                let hex =
                    appearance.bestMatch(from: [.darkAqua, .vibrantDark]) != nil ? dark : light
                return NSColor(
                    red: CGFloat((hex >> 16) & 0xFF) / 255.0,
                    green: CGFloat((hex >> 8) & 0xFF) / 255.0,
                    blue: CGFloat(hex & 0xFF) / 255.0,
                    alpha: 1
                )
            })
    }
}
