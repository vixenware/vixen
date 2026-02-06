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
        guard let appBundle = Bundle.main.bundleURL as URL?,
              let extensionURL = Bundle.main.builtInPlugInsURL?.appendingPathComponent("VixenFS Extension.appex") else {
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
    var body: some View {
        VStack(spacing: 20) {
            // Use the app icon (cat) instead of generic SF Symbol
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 128, height: 128)

            Text("Vixen")
                .font(.largeTitle)
                .fontWeight(.bold)

            Text("FSKit File System Extension")
                .font(.headline)
                .foregroundColor(.secondary)

            Divider()
                .padding(.horizontal, 40)

            VStack(alignment: .leading, spacing: 12) {
                Text("To enable the file system extension:")
                    .font(.subheadline)
                    .fontWeight(.medium)

                Text("Go to Login Items & Extensions and enable \"Vixen\" under File System Extensions")
                    .font(.subheadline)

                Button(action: openLoginItemsSettings) {
                    HStack {
                        Image(systemName: "gear")
                        Text("Open Login Items Settings")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            }
            .padding()
            .background(Color.secondary.opacity(0.1))
            .cornerRadius(8)

            Spacer()

            Text("Server: 127.0.0.1:10001")
                .font(.caption)
                .foregroundColor(.secondary)
        }
        .padding(40)
        .frame(minWidth: 400, minHeight: 400)
    }

    private func openLoginItemsSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }
}
