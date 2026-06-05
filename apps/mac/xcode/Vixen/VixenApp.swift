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
        registerExtension()
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

    private func registerExtension() {
        // Happens automatically on app launch?
    }
}

private enum ManagementService {
    private static let plistName = "eu.bearcove.vixen.management.plist"
    private static let queueRoot = URL(
        fileURLWithPath: "/Library/Application Support/Vixen/management/queue",
        isDirectory: true
    )

    static var service: SMAppService {
        SMAppService.daemon(plistName: plistName)
    }

    static var status: SMAppService.Status {
        service.status
    }

    static func register() throws {
        let daemon = service
        switch daemon.status {
        case .enabled, .requiresApproval:
            return
        default:
            break
        }

        do {
            try daemon.register()
        } catch {
            switch service.status {
            case .enabled, .requiresApproval:
                return
            default:
                throw error
            }
        }
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

    static func statusDescription(_ status: SMAppService.Status = status) -> String {
        switch status {
        case .enabled:
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
                try ManagementService.register()
                print("Vixen Management: \(ManagementService.statusDescription())")
            case "unregister":
                try ManagementService.unregister()
                print("Vixen Management: \(ManagementService.statusDescription())")
            case "status":
                print("Vixen Management: \(ManagementService.statusDescription())")
            case "mount", "unmount", "remount", "kill-fskitd":
                let trigger = try ManagementService.fireQueue(command)
                print("Vixen Management: queued \(command) via \(trigger.path)")
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
            Usage: Vixen management register|status|unregister|mount|unmount|remount|kill-fskitd

            """,
            stream
        )
    }
}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    @State private var isExtensionEnabled = false
    @State private var extensionError: String? = nil
    @State private var managementStatus: SMAppService.Status = .notFound
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
        managementStatus == .enabled
    }
    private var allComplete: Bool {
        isSupportedOS && isExtensionEnabled
    }

    var body: some View {
        Group {
            if !isSupportedOS {
                unsupportedOSView
            } else if allComplete {
                completeView
            } else {
                setupView
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

    private var managementStatusText: String {
        ManagementService.statusDescription(managementStatus)
    }

    private var managementButtonTitle: String {
        if isManagementConfigured {
            return "Recover"
        }
        switch managementStatus {
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
                if let managementError {
                    Text(managementError)
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
        } else if managementStatus == .requiresApproval {
            openLoginItemsSettings()
        } else {
            configureManagementDaemon()
        }
    }

    private func refreshManagementStatus() {
        managementStatus = ManagementService.status
    }

    private func configureManagementDaemon() {
        isConfiguringManagement = true
        defer {
            isConfiguringManagement = false
            refreshManagementStatus()
        }

        do {
            try ManagementService.register()
            managementError = nil
        } catch {
            refreshManagementStatus()
            if isManagementConfigured || managementStatus == .requiresApproval {
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
            _ = try ManagementService.fireQueue("remount")
            managementError = "Recovery requested"
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
