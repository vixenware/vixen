import AppKit
import Darwin
@preconcurrency import FSKit
import ServiceManagement
import Sparkle
import SwiftUI

@main
enum VixenMain {
    static func main() {
        let arguments = Array(CommandLine.arguments.dropFirst())
        if arguments.first == "management" {
            Darwin.exit(ManagementCommand.run(Array(arguments.dropFirst())))
        }
        VixenApp.main()
    }
}

struct VixenApp: App {
    private let updaterController = SparkleBootstrap.makeUpdaterController()

    init() {
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

private enum ManagementService {
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
        let serviceStatus: SMAppService.Status
        let launchdStatus: LaunchdStatus?
        let lastActionStatus: LastActionStatus?

        var isReady: Bool {
            serviceStatus == .enabled && launchdStatus?.failureDescription == nil
        }

        var description: String {
            switch serviceStatus {
            case .enabled:
                if launchdStatus?.failureDescription != nil {
                    return "Launch failed"
                }
                return "Ready"
            case .requiresApproval:
                return "Needs approval"
            case .notRegistered:
                return "Not registered"
            case .notFound:
                return "Missing from app bundle"
            @unknown default:
                return "Unknown"
            }
        }

        var detail: String? {
            guard serviceStatus == .enabled else { return nil }
            return launchdStatus?.failureDescription ?? lastActionStatus?.failureDescription
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
        let serviceStatus = status
        let launchdStatus = serviceStatus == .enabled ? inspectLaunchd() : nil
        let lastActionStatus = serviceStatus == .enabled ? readLastActionStatus() : nil
        return StatusSnapshot(
            serviceStatus: serviceStatus,
            launchdStatus: launchdStatus,
            lastActionStatus: lastActionStatus
        )
    }

    static func repairExtensionAsync() {
        DispatchQueue.global(qos: .utility).async {
            do {
                try repairExtension()
            } catch {
                NSLog("Vixen Management repair-extension failed: \(error.localizedDescription)")
            }
        }
    }

    static func repairExtension() throws {
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
    }

    static func register() throws {
        let daemon = service
        switch daemon.status {
        case .enabled, .requiresApproval:
            return
        default:
            break
        }

        try registerService()
    }

    private static func registerService() throws {
        do {
            try service.register()
        } catch {
            switch service.status {
            case .enabled, .requiresApproval:
                return
            default:
                throw error
            }
        }
    }

    static func refreshRegistration() throws {
        let daemon = service
        switch daemon.status {
        case .enabled:
            try daemon.unregister()
        case .requiresApproval:
            return
        default:
            break
        }

        try registerService()
    }

    static func unregister() throws {
        let daemon = service
        switch daemon.status {
        case .notFound, .notRegistered:
            return
        default:
            try daemon.unregister()
        }
    }

    static func printSnapshot(_ snapshot: StatusSnapshot = snapshot()) {
        print("Vixen Management: \(snapshot.description)")
        if let detail = snapshot.detail {
            print("  \(detail)")
        }
    }

    static func fireQueue(_ name: String) throws -> URL {
        let trigger = queueRoot
            .appendingPathComponent(name, isDirectory: true)
            .appendingPathComponent("trigger-\(getpid())-\(UUID().uuidString)")
        let fd = trigger.path.withCString {
            Darwin.open($0, O_CREAT | O_EXCL | O_WRONLY, mode_t(S_IRUSR | S_IWUSR))
        }
        guard fd >= 0 else {
            let code = Int(errno)
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: code,
                userInfo: [NSLocalizedDescriptionKey: String(cString: strerror(Int32(code)))]
            )
        }
        Darwin.close(fd)
        return trigger
    }

    private static func runHelper(arguments: [String]) -> (
        status: Int32, stdout: String, stderr: String
    ) {
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
            return (127, "", error.localizedDescription)
        }

        process.waitUntilExit()
        return (
            process.terminationStatus,
            String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? "",
            String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        )
    }

    private static func inspectLaunchd() -> LaunchdStatus {
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
        guard let data = try? Data(contentsOf: lastActionURL) else { return nil }
        guard
            let object = try? JSONSerialization.jsonObject(with: data),
            let dictionary = object as? [String: Any],
            let button = dictionary["button"] as? String,
            let ok = dictionary["ok"] as? Bool
        else {
            return nil
        }
        let message = dictionary["message"] as? String ?? ""
        return LastActionStatus(button: button, ok: ok, message: message)
    }
}

private enum ManagementCommand {
    static func run(_ arguments: [String]) -> Int32 {
        guard let command = arguments.first else {
            printUsage(to: stderr)
            return 64
        }

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
                try ManagementService.repairExtension()
                let trigger = try ManagementService.fireQueue(command)
                print("Vixen Management: queued \(command) via \(trigger.path)")
                ManagementService.printSnapshot()
            case "unmount", "kill-fskitd":
                let trigger = try ManagementService.fireQueue(command)
                print("Vixen Management: queued \(command) via \(trigger.path)")
                ManagementService.printSnapshot()
            case "help", "--help", "-h":
                printUsage(to: stdout)
            default:
                printUsage(to: stderr)
                return 64
            }
            return 0
        } catch {
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
    @State private var isExtensionEnabled = false
    @State private var extensionError: String? = nil
    @State private var managementSnapshot = ManagementService.StatusSnapshot(
        serviceStatus: .notFound,
        launchdStatus: nil,
        lastActionStatus: nil
    )
    @State private var managementError: String? = nil
    @State private var isConfiguringManagement = false
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
        guard #available(macOS 15.4, *) else {
            isExtensionEnabled = false
            extensionError = "VixenFS extension status requires macOS 15.4 or later."
            return
        }

        do {
            let extensions = try await FSClient.shared.installedExtensions
            let found = extensions.first { $0.bundleIdentifier == "eu.bearcove.vixenfs" }
            isExtensionEnabled = found?.isEnabled ?? false
            extensionError = nil
        } catch {
            extensionError = error.localizedDescription
        }
    }

    private func pollExtensionStatus() async {
        while !Task.isCancelled {
            await checkExtensionOnce()
            try? await Task.sleep(for: .seconds(4))
        }
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
        try? ManagementService.repairExtension()
        let targets = [
            "x-apple.systempreferences:com.apple.ExtensionsPreferences?extensionPointIdentifier=com.apple.fskit.fsmodule",
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        ]
        for target in targets {
            if let url = URL(string: target), NSWorkspace.shared.open(url) {
                return
            }
        }
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
    }

    private var managementStatusText: String {
        managementSnapshot.description
    }

    private var managementDetailText: String? {
        managementError ?? managementSnapshot.detail
    }

    private var managementButtonTitle: String {
        if isManagementConfigured {
            return "Recover"
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
        if isManagementConfigured {
            requestManagementRecovery()
        } else if managementSnapshot.serviceStatus == .requiresApproval {
            openLoginItemsSettings()
        } else if managementSnapshot.serviceStatus == .enabled {
            refreshManagementDaemon()
        } else {
            configureManagementDaemon()
        }
    }

    private func refreshManagementStatus() {
        managementSnapshot = ManagementService.snapshot()
    }

    private func pollManagementStatus() async {
        while !Task.isCancelled {
            refreshManagementStatus()
            try? await Task.sleep(for: .seconds(4))
        }
    }

    private func configureManagementDaemon() {
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
        }

        do {
            try ManagementService.repairExtension()
            try ManagementService.register()
            managementError = nil
            refreshManagementStatus()
            if managementSnapshot.serviceStatus == .requiresApproval {
                openLoginItemsSettings()
            }
        } catch {
            refreshManagementStatus()
            if isManagementConfigured || managementSnapshot.serviceStatus == .requiresApproval {
                managementError = nil
            } else {
                managementError = error.localizedDescription
            }
        }
    }

    private func refreshManagementDaemon() {
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
        }

        do {
            try ManagementService.repairExtension()
            try ManagementService.refreshRegistration()
            managementError = nil
            refreshManagementStatus()
            if managementSnapshot.serviceStatus == .requiresApproval {
                openLoginItemsSettings()
            }
        } catch {
            refreshManagementStatus()
            if isManagementConfigured || managementSnapshot.serviceStatus == .requiresApproval {
                managementError = nil
            } else {
                managementError = error.localizedDescription
            }
        }
    }

    private func requestManagementRecovery() {
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
        }

        do {
            try ManagementService.repairExtension()
            _ = try ManagementService.fireQueue("remount")
            managementError = nil
            refreshManagementStatus()
        } catch {
            managementError = error.localizedDescription
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

    // MARK: - Complete view

    private var completeView: some View {
        HStack(alignment: .center, spacing: 32) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: foxSize, height: foxSize)

            VStack(spacing: 20) {
                Text("You're all set")
                    .font(.system(size: 24, weight: .bold))

                Text(
                    "VixenFS is enabled. Head to the docs to install the CLI and run your first build."
                )
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

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

                managementSetupPanel
            }
            .frame(maxWidth: .infinity)

            Spacer()
                .frame(width: foxSize)
        }
        .onAppear {
            refreshManagementStatus()
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
