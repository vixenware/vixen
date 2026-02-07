import AVKit
import AppKit
import Combine
import Foundation
import SwiftUI

@main
struct VixenApp: App {
    @StateObject private var setupModel = SetupViewModel()

    init() {
        registerExtension()
    }

    var body: some Scene {
        WindowGroup {
            SetupView()
                .environmentObject(setupModel)
        }
        .commands {
            CommandGroup(after: .help) {
                Divider()
                Button("Export Diagnostics…") {
                    setupModel.exportDiagnosticsFromMenu()
                }
            }
        }
    }

    private func registerExtension() {
        guard
            let extensionURL = Bundle.main.builtInPlugInsURL?.appendingPathComponent(
                "VixenFS Extension.appex")
        else {
            return
        }
        guard FileManager.default.fileExists(atPath: extensionURL.path) else {
            return
        }

        let result = SystemProbe.runProcess("/usr/bin/pluginkit", ["-a", extensionURL.path])
        if result.exitCode != 0 {
            print(
                "Extension registration returned status: \(result.exitCode) stderr=\(result.stderr)"
            )
        }
    }
}

private enum CheckStatus: Equatable {
    case unknown
    case ok
    case needsAction(reason: String)

    var isComplete: Bool {
        if case .ok = self {
            return true
        }
        return false
    }

    var requiresAction: Bool {
        if case .needsAction = self {
            return true
        }
        return false
    }
}

private struct SetupCheck: Identifiable, Equatable {
    enum Kind: String, CaseIterable {
        case appRunning
        case osVersion
        case vfsPermissions
    }

    let kind: Kind
    let title: String
    let subtitle: String
    let status: CheckStatus

    var id: Kind { kind }

    var primaryActionTitle: String? {
        guard status.requiresAction else {
            return nil
        }

        switch kind {
        case .appRunning:
            return nil
        case .osVersion:
            return nil
        case .vfsPermissions:
            return "Open Settings"
        }
    }
}

private struct ProcessResult {
    let exitCode: Int32
    let stdout: String
    let stderr: String
}

@MainActor
private final class SetupViewModel: ObservableObject {
    @Published var checks: [SetupCheck] = [
        SetupCheck(
            kind: .appRunning,
            title: "Vixen app running",
            subtitle: "Checking app status…",
            status: .unknown
        ),
        SetupCheck(
            kind: .osVersion,
            title: "macOS >26.0",
            subtitle: "Checking macOS version…",
            status: .unknown
        ),
        SetupCheck(
            kind: .vfsPermissions,
            title: "Enable filesystem extension",
            subtitle: "Checking file system extension status…",
            status: .unknown
        ),
    ]

    @Published var lastChecked: Date?
    @Published var isChecking = false

    private var refreshTask: Task<Void, Never>?
    private var timerCancellable: AnyCancellable?
    private var appActiveObserver: NSObjectProtocol?

    func startAutoRefresh() {
        guard timerCancellable == nil else {
            return
        }

        refresh()

        appActiveObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.refresh()
            }
        }

        timerCancellable = Timer.publish(every: 45, on: .main, in: .common)
            .autoconnect()
            .sink { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.refresh()
                }
            }
    }

    func stopAutoRefresh() {
        timerCancellable?.cancel()
        timerCancellable = nil

        if let observer = appActiveObserver {
            NotificationCenter.default.removeObserver(observer)
            appActiveObserver = nil
        }

        refreshTask?.cancel()
        refreshTask = nil
    }

    func refresh() {
        guard !isChecking else {
            return
        }

        refreshTask?.cancel()
        isChecking = true

        refreshTask = Task {
            defer {
                isChecking = false
                if !Task.isCancelled {
                    lastChecked = Date()
                }
            }

            let nextChecks = await Task.detached(priority: .userInitiated) {
                SystemProbe.collectChecks()
            }.value

            guard !Task.isCancelled else {
                return
            }

            checks = nextChecks
        }
    }

    func exportDiagnosticsFromMenu() {
        let snapshot = checks.isEmpty ? SystemProbe.collectChecks() : checks

        do {
            let path = try SystemProbe.exportDiagnostics(checks: snapshot)
            NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
        } catch {
            NSSound.beep()
            print("Failed to export diagnostics: \(error.localizedDescription)")
        }
    }
}

struct SetupView: View {
    @EnvironmentObject private var vm: SetupViewModel
    @State private var expandedRow: SetupCheck.Kind?

    var body: some View {
        ScrollView(.vertical, showsIndicators: true) {
            VStack(alignment: .leading, spacing: 14) {
                header
                onboardingSurface
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .frame(minWidth: 1120, minHeight: 680)
        .onAppear { vm.startAutoRefresh() }
        .onDisappear { vm.stopAutoRefresh() }
    }

    private var header: some View {
        HStack(alignment: .top) {
            Text("Vixen — Setup")
                .font(.system(size: 26, weight: .bold))

            Spacer()

            AutoRefreshPill(isChecking: vm.isChecking, lastChecked: vm.lastChecked) {
                vm.refresh()
            }
        }
    }

    private var onboardingSurface: some View {
        let completed = vm.checks.filter(\.status.isComplete).count
        let total = vm.checks.count
        let progress = total == 0 ? 0.0 : Double(completed) / Double(total)

        return HStack(alignment: .top, spacing: 24) {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("\(completed) of \(total) steps complete")
                        .font(.system(size: 20, weight: .bold))

                    ProgressView(value: progress)
                        .controlSize(.small)
                        .tint(.blue)
                }
                .padding(.horizontal, 14)
                .padding(.top, 6)

                ForEach(vm.checks) { check in
                    SetupCheckRow(
                        check: check,
                        isExpanded: expandedRow == check.kind,
                        onToggleExpand: { toggleExpand(check.kind) },
                        onPrimaryAction: { handlePrimaryAction(for: check.kind) }
                    )
                    .padding(.horizontal, 14)
                }
            }
            .padding(.vertical, 2)
            .frame(maxWidth: .infinity, alignment: .leading)

            walkthroughPanel
                .frame(width: 420)
        }
    }

    private var walkthroughPanel: some View {
        let videoWidth: CGFloat = 396
        let videoHeight: CGFloat = videoWidth * (764.0 / 1068.0)

        return VStack(alignment: .leading, spacing: 10) {
            Text("Walkthrough")
                .font(.system(size: 15, weight: .bold))
            Text("Follow this video while enabling VixenFS Extension.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            Group {
                if let url = Bundle.main.url(forResource: "vfs-setup", withExtension: "mp4") {
                    LoopingAVPlayerView(url: url)
                } else {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(.quaternary.opacity(0.5))
                        .overlay(
                            Text("Walkthrough video will appear here")
                                .foregroundStyle(.secondary)
                                .font(.system(size: 12, weight: .semibold))
                        )
                }
            }
            .frame(width: videoWidth, height: videoHeight)
            .clipShape(RoundedRectangle(cornerRadius: 12))
        }
        .padding(.top, 2)
    }

    private func toggleExpand(_ kind: SetupCheck.Kind) {
        expandedRow = expandedRow == kind ? nil : kind
    }

    private func handlePrimaryAction(for kind: SetupCheck.Kind) {
        switch kind {
        case .appRunning:
            break
        case .osVersion:
            break
        case .vfsPermissions:
            expandedRow = .vfsPermissions
            SystemProbe.openFileSystemExtensionsSettings()
        }
    }
}

private struct AutoRefreshPill: View {
    let isChecking: Bool
    let lastChecked: Date?
    let onManualRun: () -> Void

    @State private var isHovered = false

    var body: some View {
        HStack(spacing: 10) {
            if isChecking {
                ProgressView()
                    .controlSize(.small)
            }

            Text(statusText)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.secondary)

            if isHovered && !isChecking {
                Button("Run again", action: onManualRun)
                    .buttonStyle(.plain)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(.quaternary.opacity(0.22), in: Capsule())
        .onHover { hovering in
            isHovered = hovering
        }
    }

    private var statusText: String {
        if isChecking {
            if let lastChecked {
                return "Checking… • last run \(Self.timeString(lastChecked))"
            }
            return "Checking…"
        }

        if let lastChecked {
            return "Up to date • last run \(Self.timeString(lastChecked))"
        }
        return "Up to date"
    }

    private static func timeString(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}

private struct CheckAssetIcon: View {
    let isDone: Bool

    var body: some View {
        Image(isDone ? "check_filled" : "check_empty")
            .resizable()
            .frame(width: 22, height: 22)
            .accessibilityHidden(true)
    }
}

private struct SetupCheckRow: View {
    let check: SetupCheck
    let isExpanded: Bool
    let onToggleExpand: () -> Void
    let onPrimaryAction: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center, spacing: 10) {
                CheckAssetIcon(isDone: check.status.isComplete)

                VStack(alignment: .leading, spacing: 6) {
                    Text(check.title)
                        .font(.system(size: 16, weight: .semibold))
                }

                Spacer()

                if let actionTitle = check.primaryActionTitle, !shouldShowInlineHelp {
                    Button {
                        onPrimaryAction()
                    } label: {
                        if check.kind == .vfsPermissions {
                            Label(actionTitle, systemImage: "gearshape")
                        } else {
                            Text(actionTitle)
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.regular)
                    .font(.system(size: 13, weight: .semibold))
                }
            }

            if shouldShowInlineHelp {
                inlineHelp
                    .padding(.leading, 32)
                    .padding(.top, 2)
            }
        }
        .padding(.vertical, 8)
        .contentShape(Rectangle())
        .onTapGesture {
            if canExpand {
                onToggleExpand()
            }
        }
        .animation(.easeInOut(duration: 0.18), value: isExpanded)
    }

    private var canExpand: Bool {
        check.kind == .vfsPermissions
    }

    private var shouldShowInlineHelp: Bool {
        canExpand && (check.status.requiresAction || isExpanded)
    }

    @ViewBuilder
    private var inlineHelp: some View {
        switch check.kind {
        case .vfsPermissions:
            VFSInlineHelp(onOpenSettings: onPrimaryAction)
        case .appRunning, .osVersion:
            EmptyView()
        }
    }
}

private struct VFSInlineHelp: View {
    let onOpenSettings: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("How to enable it")
                .font(.system(size: 13, weight: .bold))

            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("• Click")
                Button {
                    onOpenSettings()
                } label: {
                    Label("Open Settings", systemImage: "gearshape")
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
                Text("then enable \"VixenFS Extension\".")
            }
            .foregroundStyle(.secondary)
            .font(.system(size: 12))
        }
    }
}

private struct LoopingAVPlayerView: NSViewRepresentable {
    let url: URL

    final class Coordinator {
        let player = AVQueuePlayer()
        var looper: AVPlayerLooper?
        var currentURL: URL?
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView(frame: .zero)
        view.controlsStyle = .none
        view.videoGravity = .resizeAspect
        view.player = context.coordinator.player
        configureLoop(on: context.coordinator)
        return view
    }

    func updateNSView(_ nsView: AVPlayerView, context: Context) {
        nsView.player = context.coordinator.player
        configureLoop(on: context.coordinator)
    }

    private func configureLoop(on coordinator: Coordinator) {
        guard coordinator.currentURL != url else {
            return
        }
        coordinator.currentURL = url

        coordinator.player.removeAllItems()
        let item = AVPlayerItem(url: url)
        coordinator.looper = AVPlayerLooper(player: coordinator.player, templateItem: item)
        coordinator.player.play()
    }
}

private enum SystemProbe {
    static func runProcess(_ executable: String, _ arguments: [String]) -> ProcessResult {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: executable)
        task.arguments = arguments

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        task.standardOutput = stdoutPipe
        task.standardError = stderrPipe

        do {
            try task.run()
        } catch {
            return ProcessResult(exitCode: -1, stdout: "", stderr: String(describing: error))
        }

        task.waitUntilExit()

        let stdoutData = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
        let stderrData = stderrPipe.fileHandleForReading.readDataToEndOfFile()

        return ProcessResult(
            exitCode: task.terminationStatus,
            stdout: String(data: stdoutData, encoding: .utf8) ?? "",
            stderr: String(data: stderrData, encoding: .utf8) ?? ""
        )
    }

    static func collectChecks() -> [SetupCheck] {
        [
            appRunningCheck(),
            osVersionCheck(),
            vfsPermissionsCheck(),
        ]
    }

    static func appRunningCheck() -> SetupCheck {
        SetupCheck(
            kind: .appRunning,
            title: "Vixen app running",
            subtitle: "Detected Vixen.app is open.",
            status: .ok
        )
    }

    static func osVersionCheck() -> SetupCheck {
        let os = ProcessInfo.processInfo.operatingSystemVersion
        let isSupported = os.majorVersion > 26 || (os.majorVersion == 26 && (os.minorVersion > 0 || os.patchVersion > 0))

        return SetupCheck(
            kind: .osVersion,
            title: "macOS >26.0",
            subtitle: "Detected macOS \(os.majorVersion).\(os.minorVersion).\(os.patchVersion).",
            status: isSupported ? .ok : .needsAction(reason: "Vixen requires a newer macOS version.")
        )
    }

    static func vfsPermissionsCheck() -> SetupCheck {
        let extensionIdentifier = "eu.bearcove.vixenfs"
        let result = runProcess("/usr/bin/pluginkit", ["-m", "-p", "com.apple.fskit.fsmodule"])

        guard result.exitCode == 0 else {
            return SetupCheck(
                kind: .vfsPermissions,
                title: "Enable filesystem extension",
                subtitle: "Required for the virtual file system.",
                status: .needsAction(reason: "Open settings and enable File System Extensions.")
            )
        }

        for rawLine in result.stdout.split(separator: "\n") {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            guard line.contains(extensionIdentifier) else {
                continue
            }

            if line.hasPrefix("+") {
                return SetupCheck(
                    kind: .vfsPermissions,
                    title: "Enable filesystem extension",
                    subtitle: "Vixen File System Extension is enabled.",
                    status: .ok
                )
            }

            return SetupCheck(
                kind: .vfsPermissions,
                title: "Enable filesystem extension",
                subtitle: "Required for the virtual file system.",
                status: .needsAction(reason: "Enable Vixen File System Extension in settings.")
            )
        }

        return SetupCheck(
            kind: .vfsPermissions,
            title: "Enable filesystem extension",
            subtitle: "Required for the virtual file system.",
            status: .needsAction(reason: "Enable Vixen File System Extension in settings.")
        )
    }

    static func openFileSystemExtensionsSettings() {
        let deepLinks = [
            "x-apple.systempreferences:com.apple.ExtensionsPreferences?extensionPointIdentifier=com.apple.fskit.fsmodule",
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Extensions",
        ]

        for target in deepLinks {
            if let url = URL(string: target), NSWorkspace.shared.open(url) {
                return
            }
        }

        NSWorkspace.shared.open(URL(fileURLWithPath: "/System/Applications/System Settings.app"))
    }

    static func diagnosticsReport(checks: [SetupCheck]) -> String {
        let formatter = ISO8601DateFormatter()
        var lines: [String] = []
        lines.append("Vixen diagnostics")
        lines.append("generated_at=\(formatter.string(from: Date()))")
        lines.append("app_bundle=\(Bundle.main.bundleIdentifier ?? "unknown")")
        lines.append("app_version=\(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "unknown")")
        lines.append("shell=\(ProcessInfo.processInfo.environment["SHELL"] ?? "")")
        lines.append("path=\(ProcessInfo.processInfo.environment["PATH"] ?? "")")
        lines.append("")
        lines.append("checks:")

        for check in checks {
            let state: String
            switch check.status {
            case .unknown:
                state = "unknown"
            case .ok:
                state = "ok"
            case .needsAction(let reason):
                state = "needs_action (\(reason))"
            }
            lines.append("- \(check.kind.rawValue): \(state) | \(check.subtitle)")
        }

        return lines.joined(separator: "\n")
    }

    static func exportDiagnostics(checks: [SetupCheck]) throws -> String {
        let logsDir = NSHomeDirectory().appending("/Library/Logs/Vixen")
        try FileManager.default.createDirectory(atPath: logsDir, withIntermediateDirectories: true)

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        let filePath = logsDir.appending("/diagnostics-\(formatter.string(from: Date())).txt")

        let report = diagnosticsReport(checks: checks)
        try report.write(toFile: filePath, atomically: true, encoding: .utf8)
        return filePath
    }
}
