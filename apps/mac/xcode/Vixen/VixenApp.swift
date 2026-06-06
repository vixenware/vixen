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

    init() {
        Logger(subsystem: "eu.bearcove.vixen", category: "App")
            .notice("VixenApp init; scheduling user-session extension repair")
        ManagementService.repairExtensionAsync()
    }

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
            }
        }
    }

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
            helperAvailable
        }

        var description: String {
            guard helperAvailable else {
                return helperDetail ?? "Helper missing from app bundle"
            }
            return "Ready"
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
            return "last \(button) failed: \(message)"
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

    static func repairExtensionAsync() {
        logger.notice("scheduling async repair-extension")
        DispatchQueue.global(qos: .utility).async {
            do {
                try repairExtension()
                logger.notice("async repair-extension succeeded")
            } catch {
                logger.error("async repair-extension failed: \(error.localizedDescription, privacy: .public)")
            }
        }
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

    static func runPrivilegedButton(_ name: String) throws {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("privileged management button requested name=\(name, privacy: .public) euid=\(Darwin.geteuid(), privacy: .public)")
        guard ["mount", "unmount", "remount", "kill-fskitd"].contains(name) else {
            logger.error("privileged management button rejected unknown name=\(name, privacy: .public)")
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(EINVAL),
                userInfo: [NSLocalizedDescriptionKey: "Unknown management button \(name)"]
            )
        }

        if Darwin.geteuid() == 0 {
            logger.notice("running privileged management button directly because caller is already root")
        } else {
            try repairExtension()
        }

        let result =
            Darwin.geteuid() == 0
            ? runHelper(arguments: [name])
            : runHelperWithAdministratorPrivileges(arguments: [name])
        guard result.status == 0 else {
            let detail =
                result.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
                : result.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            logger.error("privileged management button failed name=\(name, privacy: .public) status=\(result.status, privacy: .public) detail=\(detail, privacy: .public)")
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(result.status),
                userInfo: [
                    NSLocalizedDescriptionKey: detail.isEmpty
                        ? "vixen-management \(name) exited \(result.status)"
                        : detail
                ]
            )
        }
        logger.notice("privileged management button finished in \(elapsedMilliseconds(since: started), privacy: .public)ms name=\(name, privacy: .public) stdout=\(result.stdout.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
    }

    static func register() throws {
        let daemon = service
        logger.notice("management register requested currentStatus=\(statusName(daemon.status), privacy: .public)")
        switch daemon.status {
        case .enabled, .requiresApproval:
            logger.notice("management register skipped currentStatus=\(statusName(daemon.status), privacy: .public)")
            return
        default:
            break
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

    private static func runHelperWithAdministratorPrivileges(arguments: [String]) -> (
        status: Int32, stdout: String, stderr: String
    ) {
        let started = DispatchTime.now().uptimeNanoseconds
        let commandArguments = [
            "/usr/bin/env",
            "VIXEN_MANAGEMENT_LOG=info,vx_management=debug,vixen_management=debug",
            helperURL.path,
        ] + arguments
        logger.notice("requesting administrator privileges for helper command arguments=\(commandArguments.joined(separator: " "), privacy: .public)")

        let script = """
        on run argv
            set commandParts to {}
            repeat with rawArg in argv
                set end of commandParts to quoted form of (rawArg as text)
            end repeat
            set oldDelimiters to AppleScript's text item delimiters
            set AppleScript's text item delimiters to " "
            set commandText to commandParts as text
            set AppleScript's text item delimiters to oldDelimiters
            do shell script commandText with administrator privileges
        end run
        """

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        process.arguments = ["-e", script] + commandArguments

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            logger.error("administrator helper request failed to start arguments=\(commandArguments.joined(separator: " "), privacy: .public) error=\(error.localizedDescription, privacy: .public)")
            return (127, "", error.localizedDescription)
        }

        process.waitUntilExit()
        let stdoutText =
            String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let stderrText =
            String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        logger.notice("administrator helper request finished in \(elapsedMilliseconds(since: started), privacy: .public)ms status=\(process.terminationStatus, privacy: .public) stdout=\(stdoutText.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public) stderr=\(stderrText.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
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
                try ManagementService.repairExtension()
                try ManagementService.register()
                ManagementService.printSnapshot()
            case "refresh":
                try ManagementService.repairExtension()
                try ManagementService.refreshRegistration()
                ManagementService.printSnapshot()
            case "unregister":
                try ManagementService.unregister()
                ManagementService.printSnapshot()
            case "repair-extension":
                try ManagementService.repairExtension()
                print("Vixen Management: repaired extension registration")
            case "status":
                do {
                    try ManagementService.repairExtension()
                } catch {
                    fputs(
                        "Vixen Management: extension repair failed: \(error.localizedDescription)\n",
                        stderr
                    )
                }
                ManagementService.printSnapshot()
            case "mount", "remount":
                try ManagementService.runPrivilegedButton(command)
                print("Vixen Management: ran \(command)")
                ManagementService.printSnapshot()
            case "unmount", "kill-fskitd":
                try ManagementService.runPrivilegedButton(command)
                print("Vixen Management: ran \(command)")
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
            Usage: Vixen management register|refresh|status|unregister|repair-extension|mount|unmount|remount|kill-fskitd

            """,
            stream
        )
    }
}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    private let logger = Logger(subsystem: "eu.bearcove.vixen", category: "DoctorUI")
    @State private var isExtensionEnabled = false
    @State private var extensionError: String? = nil
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
    @State private var lastSafeRepairAt = Date.distantPast
    @State private var showFskitRestartConfirmation = false
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

    private var contentMinHeight: CGFloat {
        if isExtensionEnabled && isManagementConfigured {
            return doctorSnapshot.problemRows.isEmpty ? 500 : 560
        }
        return 500
    }

    var body: some View {
        Group {
            if !isSupportedOS {
                unsupportedOSView
            } else if !isExtensionEnabled {
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
        .alert("Restart FSKit?", isPresented: $showFskitRestartConfirmation) {
            Button("Restart FSKit", role: .destructive) {
                restartFskit()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Vixen has already repaired the app registration and retried the mount. Restarting FSKit can clear stale macOS filesystem-extension state, but it may interrupt other FSKit volumes."
            )
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
                if isExtensionEnabled {
                    refreshManagementStatus()
                    if isManagementConfigured {
                        await refreshDoctorStatus()
                    }
                }
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

                if let error = extensionError {
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
            isExtensionEnabled = false
            extensionError = "VixenFS extension status requires macOS 15.4 or later."
            logger.error("extension status check unavailable on this macOS version")
            return
        }

        do {
            let extensions = try await FSClient.shared.installedExtensions
            let found = extensions.first { $0.bundleIdentifier == "eu.bearcove.vixenfs" }
            isExtensionEnabled = found?.isEnabled ?? false
            extensionError = nil
            logger.notice("extension status check finished found=\(found != nil, privacy: .public) enabled=\(isExtensionEnabled, privacy: .public)")
        } catch {
            extensionError = error.localizedDescription
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
        try? ManagementService.repairExtension()
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
            Task { await refreshDoctorStatus(forceSafeRepair: true) }
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
    private func refreshDoctorStatus(forceSafeRepair: Bool = false) async {
        guard !isCheckingDoctor else {
            logger.notice("doctor ui refresh skipped because a check is already running")
            return
        }
        let started = DispatchTime.now().uptimeNanoseconds
        isCheckingDoctor = true
        defer { isCheckingDoctor = false }
        logger.notice("doctor ui refresh started forceSafeRepair=\(forceSafeRepair, privacy: .public) extensionEnabled=\(isExtensionEnabled, privacy: .public) managementReady=\(isManagementConfigured, privacy: .public)")
        let firstSnapshot = await DoctorService.check(
            extensionEnabled: isExtensionEnabled,
            extensionError: extensionError
        )
        logger.notice("doctor first snapshot ready=\(firstSnapshot.isReady, privacy: .public) failures=\(firstSnapshot.rows.filter { $0.state == .failed }.count, privacy: .public) warnings=\(firstSnapshot.rows.filter { $0.state == .warning }.count, privacy: .public) canSafeRepair=\(firstSnapshot.canAttemptSafeMountRepair, privacy: .public) needsFskitRestart=\(firstSnapshot.needsFskitRestart, privacy: .public)")

        let shouldRunSafeRepair =
            !firstSnapshot.needsFskitRestart
            && (firstSnapshot.canAttemptSafeMountRepair
                || (forceSafeRepair && firstSnapshot.hasFailure))

        if shouldRunSafeRepair {
            logger.notice("doctor ui running safe repair force=\(forceSafeRepair, privacy: .public)")
            await performSafeDoctorRepair(force: forceSafeRepair)
            doctorSnapshot = await DoctorService.check(
                extensionEnabled: isExtensionEnabled,
                extensionError: extensionError
            )
            logger.notice("doctor post-repair snapshot ready=\(doctorSnapshot.isReady, privacy: .public) failures=\(doctorSnapshot.rows.filter { $0.state == .failed }.count, privacy: .public) warnings=\(doctorSnapshot.rows.filter { $0.state == .warning }.count, privacy: .public) needsFskitRestart=\(doctorSnapshot.needsFskitRestart, privacy: .public)")
        } else {
            doctorSnapshot = firstSnapshot
            logger.notice("doctor ui accepted first snapshot without safe repair")
        }
        refreshManagementStatus()
        logger.notice("doctor ui refresh finished in \(elapsedMilliseconds(since: started), privacy: .public)ms forceSafeRepair=\(forceSafeRepair, privacy: .public)")
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
            if isExtensionEnabled && isManagementConfigured && !doctorSnapshot.needsFskitRestart {
                logger.notice("doctor polling triggering refresh")
                await refreshDoctorStatus()
            } else {
                logger.notice("doctor polling skipped extensionEnabled=\(isExtensionEnabled, privacy: .public) managementReady=\(isManagementConfigured, privacy: .public) needsFskitRestart=\(doctorSnapshot.needsFskitRestart, privacy: .public)")
            }
            try? await Task.sleep(for: .seconds(30))
        }
        logger.notice("doctor polling stopped")
    }

    @MainActor
    private func performSafeDoctorRepair(force: Bool) async {
        guard !isAutoRepairingDoctor else {
            logger.notice("safe doctor repair skipped because repair is already running")
            return
        }
        let now = Date()
        if !force && now.timeIntervalSince(lastSafeRepairAt) < 10 {
            logger.notice("safe doctor repair throttled force=\(force, privacy: .public)")
            return
        }

        isAutoRepairingDoctor = true
        lastSafeRepairAt = now
        defer { isAutoRepairingDoctor = false }
        logger.notice("safe doctor repair started force=\(force, privacy: .public)")

        await Task.detached(priority: .utility) {
            do {
                try ManagementService.runPrivilegedButton("mount")
            } catch {
                ManagementService.logger.error("safe doctor repair failed to run mount: \(error.localizedDescription, privacy: .public)")
            }
        }.value
        try? await Task.sleep(for: .seconds(1))
        logger.notice("safe doctor repair finished")
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
            try ManagementService.repairExtension()
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
            try ManagementService.repairExtension()
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

    private func restartFskit() {
        logger.notice("restart FSKit requested")
        Task {
            isConfiguringManagement = true
            defer {
                isConfiguringManagement = false
                refreshManagementStatus()
                logger.notice("restart FSKit task finished error=\(managementError ?? "none", privacy: .public)")
            }

            do {
                try ManagementService.runPrivilegedButton("kill-fskitd")
                logger.notice("restart FSKit ran kill-fskitd")
                try? await Task.sleep(for: .seconds(2))
                try ManagementService.runPrivilegedButton("mount")
                logger.notice("restart FSKit ran mount")
                managementError = nil
                await refreshDoctorStatus(forceSafeRepair: true)
            } catch {
                managementError = error.localizedDescription
                logger.error("restart FSKit failed: \(error.localizedDescription, privacy: .public)")
            }
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
            HStack(alignment: .center, spacing: 10) {
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
                    Text(doctorSummaryText)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }

                Spacer()
            }

            HStack(spacing: 10) {
                Spacer(minLength: 0)
                if let doctorPrimaryActionTitle {
                    Button(action: doctorPrimaryAction) {
                        Text(doctorPrimaryActionTitle)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 14)
                            .frame(minWidth: 118, minHeight: 30)
                            .background(
                                brandOrange,
                                in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                            )
                    }
                    .buttonStyle(.plain)
                    .disabled(isConfiguringManagement || isCheckingDoctor || isAutoRepairingDoctor)
                }

                Button(action: { Task { await refreshDoctorStatus() } }) {
                    Text("Check Again")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(brandOrange)
                        .padding(.horizontal, 14)
                        .frame(minWidth: 118, minHeight: 30)
                        .background(
                            brandOrange.opacity(0.12),
                            in: RoundedRectangle(cornerRadius: 8, style: .continuous)
                        )
                }
                .buttonStyle(.plain)
                .disabled(isCheckingDoctor || isAutoRepairingDoctor)
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
        .frame(maxWidth: .infinity, minHeight: 110, alignment: .top)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(sectionBg)
        )
    }

    private var doctorPrimaryActionTitle: String? {
        if doctorSnapshot.needsFskitRestart {
            return "Restart FSKit"
        }
        if doctorSnapshot.canAttemptSafeMountRepair {
            return "Mount VixenFS"
        }
        return nil
    }

    private func doctorPrimaryAction() {
        if doctorSnapshot.needsFskitRestart {
            showFskitRestartConfirmation = true
        } else if doctorSnapshot.canAttemptSafeMountRepair {
            Task { await refreshDoctorStatus(forceSafeRepair: true) }
        }
    }

    private var doctorSummaryText: String {
        if isAutoRepairingDoctor {
            return "Repairing safe state"
        }
        if isCheckingDoctor {
            return "Checking"
        }
        let failed = doctorSnapshot.rows.filter { $0.state == .failed }.count
        let warnings = doctorSnapshot.rows.filter { $0.state == .warning }.count
        if failed > 0 {
            if doctorSnapshot.needsFskitRestart {
                return "FSKit has stale state"
            }
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

    private var completeTitle: String {
        if isCheckingDoctor || isAutoRepairingDoctor || (!doctorSnapshot.isReady && !doctorSnapshot.hasFailure) {
            return "Checking Vixen"
        }
        return doctorSnapshot.hasFailure ? "Needs attention" : "You're all set"
    }

    private var completeDescription: String {
        if !doctorSnapshot.isReady && !doctorSnapshot.hasFailure {
            return "Vixen is checking the local filesystem setup and repairing safe state automatically."
        }
        if doctorSnapshot.hasFailure {
            if doctorSnapshot.needsFskitRestart {
                return "Vixen repaired the safe state it can reach. macOS still reports stale FSKit state."
            }
            return "Vixen is checking the local filesystem setup and repairing safe state automatically."
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
