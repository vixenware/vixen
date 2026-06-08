import AppKit
import Darwin
@preconcurrency import FSKit
import OSLog
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
        .windowResizability(.automatic)
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

/// Thin façade over the bundled `vixen-management` binary. There is no privileged
/// daemon anymore: mount/unmount run as the logged-in user (the user-owned device node
/// is what FSKit needs), and the only privileged step — creating and chowning the
/// `/Volumes/Vixen/vfs` mountpoint — is done once via a single admin prompt that runs
/// `vixen-management setup-mountpoint` as root. See FSKIT-REPAIR.md.
enum ManagementService {
    fileprivate static let logger = Logger(subsystem: "eu.bearcove.vixen", category: "Management")
    static let mountPoint = "/Volumes/Vixen/vfs"

    private static var helperURL: URL {
        Bundle.main.bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("MacOS", isDirectory: true)
            .appendingPathComponent("vixen-management", isDirectory: false)
    }

    /// Last mount/unmount result, surfaced in the doctor's mount row. Guarded by a lock
    /// because it is read from the doctor's detached check task and written from repair.
    private static let lastActionLock = NSLock()
    nonisolated(unsafe) private static var lastActionStorage: LastActionStatus?

    private static func setLastAction(_ value: LastActionStatus?) {
        lastActionLock.lock()
        lastActionStorage = value
        lastActionLock.unlock()
    }

    private static func currentLastAction() -> LastActionStatus? {
        lastActionLock.lock()
        defer { lastActionLock.unlock() }
        return lastActionStorage
    }

    struct StatusSnapshot {
        let helperAvailable: Bool
        let helperDetail: String?
        /// Does `/Volumes/Vixen/vfs` exist and is it owned by the current user? This is
        /// the only setup the app gates on; everything else is passwordless.
        let mountpointReady: Bool
        let lastActionStatus: LastActionStatus?

        var isReady: Bool {
            helperAvailable && mountpointReady
        }

        var description: String {
            guard helperAvailable else {
                return helperDetail ?? "Helper missing from app bundle"
            }
            return mountpointReady ? "Ready" : "Mountpoint not set up"
        }

        var backgroundItemDescription: String? {
            guard helperAvailable else { return nil }
            return mountpointReady ? "Mountpoint ready" : "Mountpoint needs first-run setup"
        }

        var detail: String? {
            guard helperAvailable else { return helperDetail }
            return backgroundItemDescription
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

    static func snapshot() -> StatusSnapshot {
        let started = DispatchTime.now().uptimeNanoseconds
        let helper = helperStatus()
        let mountpointReady = helper.available && isMountpointReady()
        let snapshot = StatusSnapshot(
            helperAvailable: helper.available,
            helperDetail: helper.detail,
            mountpointReady: mountpointReady,
            lastActionStatus: currentLastAction()
        )
        logger.notice("management snapshot finished in \(elapsedMilliseconds(since: started), privacy: .public)ms helperAvailable=\(helper.available, privacy: .public) mountpointReady=\(mountpointReady, privacy: .public) ready=\(snapshot.isReady, privacy: .public)")
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

    /// `/Volumes/Vixen/vfs` exists and is owned by us. When the volume is mounted the
    /// mountpoint reports the volume's root ownership, so treat a live mount as ready too.
    static func isMountpointReady() -> Bool {
        var st = stat()
        guard stat(mountPoint, &st) == 0 else { return false }
        guard (st.st_mode & S_IFMT) == S_IFDIR else { return false }
        return st.st_uid == getuid() || isVixenMounted()
    }

    private static func isVixenMounted() -> Bool {
        let result = runProcess("/sbin/mount", arguments: [])
        guard result.status == 0 else { return false }
        return result.stdout.split(whereSeparator: \.isNewline).contains { line in
            line.contains(" on \(mountPoint) ") && line.localizedCaseInsensitiveContains("vixenfs")
        }
    }

    static func repairExtension() throws {
        try runHelperChecked(["repair-extension"], label: "repair-extension")
    }

    static func repairAppRecords() throws {
        try runHelperChecked(["repair-app-records"], label: "repair-app-records")
    }

    /// Ensure the volume is mounted: make the mountpoint exist+owned (one-time admin
    /// prompt), then run the user-side mount. No password unless the mountpoint must be
    /// (re)created.
    static func repairVixenFS(progress: @escaping @Sendable (String) -> Void) async throws {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("VixenFS repair requested from app")
        progress("Preparing mountpoint")
        try ensureMountpoint()
        progress("Mounting VixenFS")
        let result = runHelper(arguments: ["mount"])
        recordAction("mount", result: result)
        try requireHelperSucceeded(result, name: "mount")
        progress("VixenFS mounted")
        logger.notice("VixenFS repair finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
    }

    static func uninstallVixenFS(progress: @escaping @Sendable (String) -> Void) async throws {
        logger.notice("VixenFS uninstall requested from app")
        progress("Unmounting VixenFS")
        let result = runHelper(arguments: ["uninstall"])
        recordAction("uninstall", result: result)
        try requireHelperSucceeded(result, name: "uninstall")
        progress("VixenFS setup removed")
    }

    /// The single privileged operation, via one admin prompt. No-op when the mountpoint
    /// is already owned by us, so it normally prompts at most once in the app's lifetime.
    static func ensureMountpoint() throws {
        if isMountpointReady() {
            logger.notice("mountpoint already set up; no admin prompt needed")
            return
        }
        logger.notice("mountpoint not set up; requesting one-time admin authorization")
        let helperPath = helperURL.path
        // Run the privileged helper through AppleScript so macOS shows the standard admin
        // password dialog. `quoted form of` keeps a path with spaces intact.
        let command = "do shell script quoted form of \"\(appleScriptEscaped(helperPath))\" & \" setup-mountpoint\" with administrator privileges"
        try runAppleScriptPrivileged(command)
        guard isMountpointReady() else {
            throw NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(EIO),
                userInfo: [NSLocalizedDescriptionKey: "Mountpoint setup did not take effect"]
            )
        }
        logger.notice("mountpoint set up via admin prompt")
    }

    private static func appleScriptEscaped(_ value: String) -> String {
        value.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
    }

    private static func runAppleScriptPrivileged(_ source: String) throws {
        let run: () throws -> Void = {
            var errorInfo: NSDictionary?
            guard let script = NSAppleScript(source: source) else {
                throw NSError(
                    domain: NSPOSIXErrorDomain,
                    code: Int(EINVAL),
                    userInfo: [NSLocalizedDescriptionKey: "Could not build authorization script"]
                )
            }
            script.executeAndReturnError(&errorInfo)
            if let errorInfo {
                let message = (errorInfo[NSAppleScript.errorMessage] as? String)
                    ?? "Authorization was cancelled"
                logger.error("privileged AppleScript failed: \(message, privacy: .public)")
                throw NSError(
                    domain: NSPOSIXErrorDomain,
                    code: Int(EPERM),
                    userInfo: [NSLocalizedDescriptionKey: message]
                )
            }
        }
        // NSAppleScript must run on the main thread.
        if Thread.isMainThread {
            try run()
        } else {
            var thrown: Error?
            DispatchQueue.main.sync {
                do { try run() } catch { thrown = error }
            }
            if let thrown { throw thrown }
        }
    }

    private static func recordAction(
        _ button: String,
        result: (status: Int32, stdout: String, stderr: String)
    ) {
        let message = (result.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? result.stdout
            : result.stderr).trimmingCharacters(in: .whitespacesAndNewlines)
        setLastAction(LastActionStatus(button: button, ok: result.status == 0, message: message))
    }

    private static func runHelperChecked(_ arguments: [String], label: String) throws {
        logger.notice("\(label, privacy: .public) started helper=\(helperURL.path, privacy: .public)")
        let result = runHelper(arguments: arguments)
        try requireHelperSucceeded(result, name: label)
        logger.notice("\(label, privacy: .public) succeeded stdout=\(result.stdout.trimmingCharacters(in: .whitespacesAndNewlines), privacy: .public)")
    }

    private static func requireHelperSucceeded(
        _ result: (status: Int32, stdout: String, stderr: String),
        name: String
    ) throws {
        guard result.status != 0 else { return }
        let detail = result.stderr.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
            : result.stderr.trimmingCharacters(in: .whitespacesAndNewlines)
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

    private static func runHelper(arguments: [String]) -> (
        status: Int32, stdout: String, stderr: String
    ) {
        runProcess(helperURL.path, arguments: arguments)
    }

    private static func runProcess(_ executable: String, arguments: [String]) -> (
        status: Int32, stdout: String, stderr: String
    ) {
        let started = DispatchTime.now().uptimeNanoseconds
        logger.notice("running \(executable, privacy: .public) arguments=\(arguments.joined(separator: " "), privacy: .public)")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        do {
            try process.run()
        } catch {
            logger.error("\(executable, privacy: .public) failed to start error=\(error.localizedDescription, privacy: .public)")
            return (127, "", error.localizedDescription)
        }

        process.waitUntilExit()
        let stdoutText =
            String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let stderrText =
            String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        logger.notice("\(executable, privacy: .public) finished in \(elapsedMilliseconds(since: started), privacy: .public)ms status=\(process.terminationStatus, privacy: .public)")
        return (process.terminationStatus, stdoutText, stderrText)
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
            case "repair-app-records":
                try ManagementService.repairAppRecords()
                print("Vixen Management: repaired app registration")
            case "repair-extension":
                try ManagementService.repairExtension()
                print("Vixen Management: repaired extension registration")
            case "setup-mountpoint":
                try ManagementService.ensureMountpoint()
                print("Vixen Management: mountpoint ready")
            case "status":
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
            Usage: Vixen management status|setup-mountpoint|repair-app-records|repair-extension
            (mount/unmount/uninstall run as the user via the bundled vixen-management binary.)

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
        mountpointReady: false,
        lastActionStatus: nil
    )
    @State private var managementError: String? = nil
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
    /// The bundled `vixen-management` binary is present. (The mountpoint, the only
    /// privileged setup, is created lazily by the Repair button — not a gate to the UI.)
    private var isManagementConfigured: Bool {
        managementSnapshot.helperAvailable
    }
    private var isExtensionEnabled: Bool {
        extensionSnapshot.enabled
    }
    private var needsExtensionEnablement: Bool {
        extensionSnapshot.error == nil && (!extensionSnapshot.found || !extensionSnapshot.enabled)
    }
    private var shouldUseDoctorFlow: Bool {
        !needsExtensionEnablement
    }

    private var contentMinHeight: CGFloat {
        if shouldUseDoctorFlow {
            let extraRows = max(0, doctorSnapshot.problemRows.count - 1)
            return doctorSnapshot.problemRows.isEmpty ? 500 : 620 + CGFloat(extraRows * 56)
        }
        return 500
    }

    private var appVersionString: String {
        (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "—"
    }

    @ViewBuilder
    private var rootContent: some View {
        if isSupportedOS && !needsExtensionEnablement {
            VixenShell(
                doctor: doctorSnapshot,
                management: managementSnapshot,
                ext: extensionSnapshot,
                isChecking: isCheckingDoctor,
                isRepairing: isAutoRepairingDoctor,
                progress: activeProgressText,
                appVersion: appVersionString,
                brandOrange: brandOrange,
                onRecheck: { Task { await refreshDoctorStatus() } },
                onRepair: { Task { await performDoctorRepair() } }
            )
            .onAppear {
                refreshManagementStatus()
                Task { await refreshDoctorStatus() }
            }
        } else {
            Group {
                if !isSupportedOS {
                    unsupportedOSView
                } else {
                    setupView
                }
            }
            .padding(36)
            .frame(width: 720)
            .frame(minHeight: contentMinHeight)
        }
    }

    var body: some View {
        rootContent
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

    // MARK: - Management helper

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
                    .disabled(isCheckingDoctor || isAutoRepairingDoctor)
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
