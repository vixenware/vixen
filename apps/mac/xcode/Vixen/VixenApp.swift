import AppKit
@preconcurrency import FSKit
import SwiftUI

@main
struct VixenApp: App {
    init() {
        registerExtension()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
        .windowResizability(.contentSize)
    }

    private func registerExtension() {
        guard
            let extensionURL = Bundle.main.builtInPlugInsURL?
                .appendingPathComponent("VixenFS Extension.appex")
        else { return }
        guard FileManager.default.fileExists(atPath: extensionURL.path) else { return }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        process.arguments = ["-a", extensionURL.path]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
        } catch {
            print("Failed to register extension: \(error)")
        }
    }
}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.scenePhase) private var scenePhase
    @State private var isExtensionEnabled = false
    @State private var extensionError: String? = nil
    @State private var demoToggleOn = false
    @State private var cursorBob = false

    private let requiredMajorVersion = 26
    private let stepCircleSize: CGFloat = 28
    private let brandOrange = Color(hex: 0xE88602)
    private let brandOrangeMuted = Color(hex: 0xF6D9B6)
    private let baseBg = Color(light: 0xFFFFFF, dark: 0x1E2020)
    private let sectionBg = Color(light: 0xF0F0F0, dark: 0x252626)
    private let sectionRowBg = Color(light: 0xFFFFFF, dark: 0x2E2E2E)

    private var isSupportedOS: Bool {
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= requiredMajorVersion
    }
    private var allComplete: Bool {
        isSupportedOS && isExtensionEnabled
    }

    var body: some View {
        Group {
            if !isSupportedOS {
                unsupportedOSView
            } else {
                onboardingView
            }
        }
        .padding(40)
        .frame(width: 640)
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
        .onChange(of: scenePhase) { _, newPhase in
            if newPhase == .active && isSupportedOS {
                Task { await checkExtensionOnce() }
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
                .font(.system(size: 24, weight: .semibold, design: .rounded))

            Text("Vixen needs FSKit features available in macOS \(requiredMajorVersion) and later.")
                .font(.system(size: 15))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            Button {
                if let url = URL(string: "https://github.com/bearcove/vixen/issues/65") {
                    NSWorkspace.shared.open(url)
                }
            } label: {
                Label("Learn More", systemImage: "arrow.up.right")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 28)
                    .padding(.vertical, 12)
                    .background(brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
        }
    }

    // MARK: - Onboarding

    private var onboardingView: some View {
        HStack(alignment: .top, spacing: 48) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 128, height: 128)

            if allComplete {
                completeView
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                VStack(alignment: .leading, spacing: 20) {
                    stepsView
                    settingsPreview

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
                        Label("Open Settings", systemImage: "gearshape.fill")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 28)
                            .padding(.vertical, 12)
                            .background(brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                    .buttonStyle(.plain)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    // MARK: - Extension status

    private func checkExtensionOnce() async {
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

    // MARK: - Steps

    private var stepsView: some View {
        VStack(alignment: .leading, spacing: 0) {
            stepRow(title: "Vixen app", isComplete: true) {
                Text("An Apple requirement, also, look, a fox!")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
            stepRow(title: "macOS \(requiredMajorVersion).0+", isComplete: isSupportedOS) {
                Text("We need the latest FSKit.")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
            stepRow(title: "VixenFS extension", isComplete: isExtensionEnabled) {
                Text("Truly hermetic builds with a VFS")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
        }
        .background(alignment: .topLeading) {
            Rectangle()
                .fill(Color(nsColor: .separatorColor).opacity(0.5))
                .frame(width: 1)
                .padding(.top, 24)
                .padding(.bottom, 24)
                .padding(.leading, stepCircleSize / 2)
        }
    }

    private func stepRow<S: View>(
        title: String,
        isComplete: Bool,
        @ViewBuilder subtitle: () -> S
    ) -> some View {
        HStack(alignment: .center, spacing: 14) {
            ZStack {
                Circle()
                    .fill(isComplete ? brandOrange : baseBg)
                if !isComplete {
                    Circle().stroke(brandOrangeMuted, lineWidth: 2)
                }
                if isComplete {
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(.white)
                }
            }
            .frame(width: stepCircleSize, height: stepCircleSize)

            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
                subtitle()
            }
        }
        .padding(.vertical, 10)
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
            // Foreign chrome: signals this is a System Settings preview
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

            // Muted to read as illustration, not interactive UI
            VStack(spacing: 8) {
                HStack(alignment: .top, spacing: 12) {
                    settingsDiskIcon(filled: false)
                    VStack(alignment: .leading, spacing: 3) {
                        Text("File System Extensions")
                            .font(.system(size: 13, weight: .semibold))
                        Text("File System extensions allow you to extend the file system functionality of macOS without requiring kernel-level access.")
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
                        Text("VixenFS Extension")
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
        VStack(alignment: .leading, spacing: 20) {
            Text("All the boxes are checked")
                .font(.system(size: 24, weight: .semibold, design: .rounded))

            VStack(alignment: .leading, spacing: 10) {
                miniCheck("Vixen app")
                miniCheck("macOS \(requiredMajorVersion).0+")
                miniCheck("VixenFS extension enabled")
            }

            Text("You're ready to build with Vixen")
                .font(.system(size: 15))
                .foregroundStyle(.secondary)

            Button {
                if let url = URL(string: "https://vixen.rs/") {
                    NSWorkspace.shared.open(url)
                }
            } label: {
                Label("Get Started", systemImage: "arrow.up.right")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 28)
                    .padding(.vertical, 12)
                    .background(brandOrange, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
        }
    }

    private func miniCheck(_ label: String) -> some View {
        HStack(spacing: 10) {
            ZStack {
                Circle()
                    .fill(brandOrange)
                    .frame(width: 20, height: 20)
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .bold))
                    .foregroundStyle(.white)
            }
            Text(label)
                .font(.system(size: 15))
                .foregroundStyle(.secondary)
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

private extension Color {
    init(hex: UInt32) {
        let red = Double((hex >> 16) & 0xFF) / 255.0
        let green = Double((hex >> 8) & 0xFF) / 255.0
        let blue = Double(hex & 0xFF) / 255.0
        self.init(red: red, green: green, blue: blue)
    }

    init(light: UInt32, dark: UInt32) {
        self.init(nsColor: NSColor(name: nil) { appearance in
            let hex = appearance.bestMatch(from: [.darkAqua, .vibrantDark]) != nil ? dark : light
            return NSColor(
                red: CGFloat((hex >> 16) & 0xFF) / 255.0,
                green: CGFloat((hex >> 8) & 0xFF) / 255.0,
                blue: CGFloat(hex & 0xFF) / 255.0,
                alpha: 1
            )
        })
    }
}
