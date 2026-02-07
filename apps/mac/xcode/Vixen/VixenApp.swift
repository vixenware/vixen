import SwiftUI
import AppKit

@main
struct VixenApp: App {
    init() {
        // Register the embedded extension on app launch
        registerExtension()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }

    private func registerExtension() {
        guard let extensionURL = Bundle.main.builtInPlugInsURL?.appendingPathComponent("VixenFS Extension.appex") else {
            print("Could not find extension bundle")
            return
        }

        // Check if extension exists
        guard FileManager.default.fileExists(atPath: extensionURL.path) else {
            print("Extension not found at: \(extensionURL.path)")
            return
        }

        // Use pluginkit to register the extension
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        task.arguments = ["-a", extensionURL.path]

        do {
            try task.run()
            task.waitUntilExit()
            if task.terminationStatus == 0 {
                print("Extension registered successfully")
            } else {
                print("Extension registration returned status: \(task.terminationStatus)")
            }
        } catch {
            print("Failed to register extension: \(error)")
        }
    }
}

struct ContentView: View {
    @Environment(\.colorScheme) private var colorScheme
    @State private var isExtensionEnabled = false
    @State private var demoPointerOn = false

    private let requiredMajorVersion = 26

    private var isAppRunning: Bool { true }
    private var isSupportedOS: Bool {
        ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= requiredMajorVersion
    }
    private var completedSteps: Int {
        [isAppRunning, isSupportedOS, isExtensionEnabled].filter { $0 }.count
    }
    private let brandOrange = Color(hex: 0xE88602)
    private let brandOrangeMuted = Color(hex: 0xF6D9B6)

    var body: some View {
        ZStack {
            LinearGradient(
                colors: [
                    Color(nsColor: .windowBackgroundColor),
                    brandOrange.opacity(colorScheme == .dark ? 0.14 : 0.05)
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            .ignoresSafeArea()

            VStack(spacing: 18) {
                HStack(spacing: 10) {
                    Image(nsImage: NSApp.applicationIconImage)
                        .resizable()
                        .frame(width: 36, height: 36)

                    Text("Welcome to Vixen Setup")
                        .font(.system(size: 30, weight: .regular, design: .rounded))
                        .foregroundStyle(.primary)
                        .multilineTextAlignment(.center)
                }

                HStack(alignment: .top, spacing: 16) {
                    setupCard
                    walkthroughCard
                }
                .frame(maxWidth: .infinity, alignment: .center)
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 20)
        }
        .frame(minWidth: 900, minHeight: 560)
        .onAppear {
            refreshExtensionStatus()
            withAnimation(.easeInOut(duration: 1.25).repeatForever(autoreverses: true)) {
                demoPointerOn = true
            }
        }
        .onReceive(Timer.publish(every: 4, on: .main, in: .common).autoconnect()) { _ in
            refreshExtensionStatus()
        }
    }

    private func openLoginItemsSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences?extensionPointIdentifier=com.apple.fskit.fsmodule") {
            NSWorkspace.shared.open(url)
        }
    }

    private func refreshExtensionStatus() {
        let task = Process()
        let output = Pipe()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        task.arguments = ["-m", "-i", "eu.bearcove.vixenfs"]
        task.standardOutput = output
        task.standardError = Pipe()

        do {
            try task.run()
            task.waitUntilExit()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            let statusOutput = String(decoding: data, as: UTF8.self)
            isExtensionEnabled = parseExtensionEnabled(statusOutput)
        } catch {
            isExtensionEnabled = false
        }
    }

    private func parseExtensionEnabled(_ output: String) -> Bool {
        let row = output
            .split(separator: "\n")
            .first { $0.contains("eu.bearcove.vixenfs") }

        guard let row else {
            return false
        }

        let normalized = row.trimmingCharacters(in: .whitespaces)
        if normalized.hasPrefix("+") {
            return true
        }
        if normalized.localizedCaseInsensitiveContains("enabled") {
            return true
        }

        return false
    }

    private var setupCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("\(completedSteps) of 3 steps complete")
                .font(.system(size: 24, weight: .semibold, design: .rounded))
                .foregroundStyle(.primary)

            SetupStepRow(
                title: "Vixen app",
                subtitle: "You made it!",
                isComplete: isAppRunning,
                showsConnector: true,
                connectorColor: isSupportedOS ? brandOrange : brandOrangeMuted
            )

            SetupStepRow(
                title: "macOS \(requiredMajorVersion).0+",
                subtitle: "We need the latest FSKit features",
                isComplete: isSupportedOS,
                showsConnector: true,
                connectorColor: isExtensionEnabled ? brandOrange : brandOrangeMuted
            )

            SetupStepRow(
                title: "VixenFS extension",
                subtitle: nil,
                isComplete: isExtensionEnabled,
                showsConnector: false,
                connectorColor: .clear
            ) {
                HStack(alignment: .center, spacing: 8) {
                    Button(action: openLoginItemsSettings) {
                        Label("Open settings", systemImage: "gearshape.fill")
                            .font(.system(size: 13, weight: .semibold))
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.regular)
                    .tint(Color.accentColor)

                    Text("then toggle it on.")
                        .font(.system(size: 15, weight: .regular, design: .rounded))
                        .foregroundStyle(.secondary)
                }
                .padding(.top, 2)
            }
        }
        .padding(.vertical, 16)
        .padding(.horizontal, 18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
        )
        .shadow(color: Color.black.opacity(colorScheme == .dark ? 0.2 : 0.08), radius: 8, y: 4)
    }

    private var walkthroughCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Walkthrough")
                .font(.system(size: 24, weight: .semibold, design: .rounded))
                .foregroundStyle(.primary)

            ZStack(alignment: .bottomTrailing) {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color(nsColor: .windowBackgroundColor))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
                    )

                VStack(spacing: 8) {
                    HStack(alignment: .top, spacing: 10) {
                        Image(systemName: "externaldrive.badge.timemachine")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(.secondary)
                            .frame(width: 28, height: 28)
                            .background(Color(nsColor: .quaternaryLabelColor).opacity(0.18))
                            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))

                        VStack(alignment: .leading, spacing: 2) {
                            Text("File System Extensions")
                                .font(.system(size: 16, weight: .semibold))
                                .foregroundStyle(.primary)
                            Text("Enable VixenFS in this list.")
                                .font(.system(size: 13))
                                .foregroundStyle(.secondary)
                        }

                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(Color(nsColor: .controlBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))

                    HStack(spacing: 10) {
                        Image(nsImage: NSApp.applicationIconImage)
                            .resizable()
                            .frame(width: 24, height: 24)
                            .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))

                        VStack(alignment: .leading, spacing: 2) {
                            Text("VixenFS Extension")
                                .font(.system(size: 16, weight: .medium))
                                .foregroundStyle(.primary)
                            Text("Toggle this on")
                                .font(.system(size: 12))
                                .foregroundStyle(.secondary)
                        }

                        Spacer(minLength: 0)

                        WalkthroughToggle(isOn: isExtensionEnabled || demoPointerOn)
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .background(Color(nsColor: .controlBackgroundColor))
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))

                    Divider()

                    HStack(spacing: 10) {
                        Image(systemName: "externaldrive")
                            .font(.system(size: 13))
                            .foregroundStyle(.secondary)
                            .frame(width: 24, height: 24)
                        Text("fs-kitty")
                            .font(.system(size: 15))
                            .foregroundStyle(.secondary)
                        Spacer()
                        WalkthroughToggle(isOn: false)
                            .opacity(0.45)
                    }
                    .padding(.horizontal, 12)
                    .padding(.bottom, 4)
                }
                .padding(10)

                Image(systemName: "cursorarrow.fill")
                    .font(.system(size: 16, weight: .bold))
                    .foregroundStyle(colorScheme == .dark ? Color.white.opacity(0.92) : Color.black.opacity(0.78))
                    .offset(x: demoPointerOn ? -22 : -54, y: demoPointerOn ? -28 : -10)
                    .animation(.easeInOut(duration: 1.25), value: demoPointerOn)
                    .padding(.trailing, 16)
                    .padding(.bottom, 14)
            }
            .frame(height: 240)
        }
        .padding(.vertical, 16)
        .padding(.horizontal, 16)
        .frame(width: 360, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
        )
        .shadow(color: Color.black.opacity(colorScheme == .dark ? 0.2 : 0.08), radius: 8, y: 4)
    }
}

private struct WalkthroughToggle: View {
    let isOn: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 12, style: .continuous)
            .fill(isOn ? Color.accentColor : Color(nsColor: .tertiaryLabelColor).opacity(0.35))
            .frame(width: 46, height: 24)
            .overlay(alignment: isOn ? .trailing : .leading) {
                Circle()
                    .fill(Color.white)
                    .frame(width: 20, height: 20)
                    .padding(.horizontal, 2)
            }
    }
}

private struct SetupStepRow<ExtraContent: View>: View {
    let title: String
    let subtitle: String?
    let isComplete: Bool
    let showsConnector: Bool
    let connectorColor: Color
    let extraContent: ExtraContent

    init(
        title: String,
        subtitle: String?,
        isComplete: Bool,
        showsConnector: Bool,
        connectorColor: Color,
        @ViewBuilder extraContent: () -> ExtraContent = { EmptyView() }
    ) {
        self.title = title
        self.subtitle = subtitle
        self.isComplete = isComplete
        self.showsConnector = showsConnector
        self.connectorColor = connectorColor
        self.extraContent = extraContent()
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(spacing: 0) {
                ZStack {
                    Circle()
                        .fill(isComplete ? Color(hex: 0xE88602) : .clear)
                        .overlay {
                            Circle()
                                .stroke(
                                    isComplete ? Color(hex: 0xE88602) : Color(hex: 0xD9A981),
                                    lineWidth: isComplete ? 0 : 2
                                )
                        }
                    if isComplete {
                        Image(systemName: "checkmark")
                            .font(.system(size: 12, weight: .bold))
                            .foregroundStyle(.white)
                    }
                }
                .frame(width: 22, height: 22)

                if showsConnector {
                    RoundedRectangle(cornerRadius: 1, style: .continuous)
                        .fill(connectorColor)
                        .frame(width: 2, height: 52)
                        .padding(.top, 4)
                }
            }

            VStack(alignment: .leading, spacing: 5) {
                Text(title)
                    .font(.system(size: 24, weight: .semibold, design: .rounded))
                    .foregroundStyle(.primary)

                if let subtitle, !subtitle.isEmpty {
                    Text(subtitle)
                        .font(.system(size: 15, weight: .regular, design: .rounded))
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }

                extraContent
            }
            .padding(.top, 1)
        }
    }
}

private extension Color {
    init(hex: UInt32) {
        let red = Double((hex >> 16) & 0xFF) / 255.0
        let green = Double((hex >> 8) & 0xFF) / 255.0
        let blue = Double(hex & 0xFF) / 255.0
        self.init(red: red, green: green, blue: blue)
    }
}
