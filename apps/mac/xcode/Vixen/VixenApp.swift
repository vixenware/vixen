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

        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        task.arguments = ["-a", extensionURL.path]
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice

        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            print("Failed to register extension: \(error)")
        }
    }
}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    @State private var isExtensionEnabled = false
    @State private var demoToggleOn = false
    @State private var cursorBob = false

    private let requiredMajorVersion = 26
    private let brandOrange = Color(hex: 0xE88602)
    private let brandOrangeMuted = Color(hex: 0xF6D9B6)
    private let settingsBg = Color(light: 0xEAE8E5, dark: 0x2A2A2A)
    private let settingsRowBg = Color(light: 0xFFFFFF, dark: 0x333333)

    private var isSupportedOS: Bool {
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= requiredMajorVersion
    }
    private var allComplete: Bool {
        isSupportedOS && isExtensionEnabled
    }

    private var background: Color {
        colorScheme == .dark
            ? Color(red: 0x1c / 255.0, green: 0x1c / 255.0, blue: 0x1c / 255.0)
            : Color(red: 0xf5 / 255.0, green: 0xf3 / 255.0, blue: 0xf0 / 255.0)
    }

    var body: some View {
        HStack(spacing: 64) {
            Image("fox_logo")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 128, height: 128)

            Group {
                if allComplete {
                    completeView
                } else {
                    setupView
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(40)
        .background(background)
        .task {
            await pollExtensionStatus()
        }
        .task {
            await animateToggleDemo()
        }
        .onAppear {
            cursorBob = true
        }
    }

    // MARK: - Extension status

    private func pollExtensionStatus() async {
        while !Task.isCancelled {
            do {
                let extensions = try await FSClient.shared.installedExtensions
                let found = extensions.first { $0.bundleIdentifier == "eu.bearcove.vixenfs" }
                isExtensionEnabled = found?.isEnabled ?? false
            } catch {
                print("FSClient error: \(error)")
            }
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

    // MARK: - Setup view

    private var setupView: some View {
        VStack(alignment: .leading, spacing: 24) {
            stepsView
            settingsMockup
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
    }

    private var stepsView: some View {
        VStack(alignment: .leading, spacing: 0) {
            stepRow(title: "Vixen app", isComplete: true) {
                Text("An Apple requirement, also, look, a fox!")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
            stepRow(title: "macOS \(requiredMajorVersion).0+", isComplete: isSupportedOS) {
                HStack(spacing: 0) {
                    Text("We need the latest FSKit. ")
                        .font(.system(size: 13))
                        .foregroundStyle(.secondary)
                    Link("Why?", destination: URL(string: "https://developer.apple.com/documentation/fskit/fspathurlresource")!)
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(brandOrange)
                }
            }
            stepRow(title: "VixenFS extension", isComplete: isExtensionEnabled) {
                Text("Truly hermetic builds with a VFS")
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
            }
        }
        .overlay(alignment: .topLeading) {
            Rectangle()
                .fill(Color(nsColor: .separatorColor).opacity(0.5))
                .frame(width: 1)
                .padding(.top, 24)
                .padding(.bottom, 24)
                .padding(.leading, 13.5)
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
                    .fill(isComplete ? brandOrange : background)
                if !isComplete {
                    Circle().stroke(brandOrangeMuted, lineWidth: 2)
                }
                if isComplete {
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(.white)
                }
            }
            .frame(width: 28, height: 28)

            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 17, weight: .semibold))
                subtitle()
            }
        }
        .padding(.vertical, 10)
    }

    // MARK: - Settings mockup

    private var diskIcon: some View {
        Image(systemName: "externaldrive.fill")
            .foregroundStyle(.white)
    }

    private var settingsMockup: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                diskIcon
                    .font(.system(size: 22, weight: .medium))
                    .frame(width: 48, height: 48)
                    .background(Color(nsColor: .systemGray))
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))

                VStack(alignment: .leading, spacing: 3) {
                    Text("File System Extensions")
                        .font(.system(size: 13, weight: .semibold))
                    Text("File System extensions allow you to extend the file system functionality of macOS without requiring kernel-level access.")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(14)

            ZStack(alignment: .trailing) {
                HStack(spacing: 10) {
                    diskIcon
                        .font(.system(size: 14, weight: .medium))
                        .frame(width: 32, height: 32)
                        .background(Color(nsColor: .systemGray))
                        .clipShape(RoundedRectangle(cornerRadius: 7, style: .continuous))

                    Text("VixenFS Extension")
                        .font(.system(size: 13, weight: .medium))

                    Spacer()

                    MockToggle(isOn: isExtensionEnabled || demoToggleOn)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(settingsRowBg)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))

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
            .padding(.horizontal, 10)
            .padding(.bottom, 10)
        }
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(settingsBg)
        )
        .frame(maxWidth: 340)
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
                if let url = URL(string: "https://github.com/bearcove/vixen") {
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
        RoundedRectangle(cornerRadius: 16, style: .continuous)
            .fill(isOn ? Color.accentColor : Color(nsColor: .tertiaryLabelColor).opacity(0.5))
            .frame(width: 50, height: 30)
            .overlay(alignment: isOn ? .trailing : .leading) {
                Circle()
                    .fill(.white)
                    .shadow(color: .black.opacity(0.15), radius: 1, y: 1)
                    .frame(width: 26, height: 26)
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
