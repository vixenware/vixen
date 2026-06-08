import AppKit
import SwiftUI

// MARK: - Sections

/// The native app shell's top-level surfaces. The doctor/health checklist is no
/// longer the home screen — it lives inside `.daemon`, and the home is `.overview`.
enum ShellSection: String, CaseIterable, Identifiable, Hashable {
    case overview
    case builds
    case store
    case capabilities
    case daemon

    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .builds: "Builds"
        case .store: "Store"
        case .capabilities: "Capabilities"
        case .daemon: "Daemon & Health"
        }
    }

    var symbol: String {
        switch self {
        case .overview: "gauge.with.dots.needle.33percent"
        case .builds: "hammer"
        case .store: "internaldrive"
        case .capabilities: "sparkles"
        case .daemon: "stethoscope"
        }
    }
}

// MARK: - Shell

/// Native macOS shell: a source-list sidebar + detail surfaces, replacing the
/// old "doctor checklist as home" screen. Data that exists today (health, mount,
/// version) renders live; data that needs new daemon plumbing (store stats,
/// build history, advertised capabilities) renders an honest "coming online"
/// state until that backend lands.
struct VixenShell: View {
    let doctor: DoctorSnapshot
    let management: ManagementService.StatusSnapshot
    let ext: FSKitModuleSnapshot
    let isChecking: Bool
    let isRepairing: Bool
    let progress: String?
    let appVersion: String
    let brandOrange: Color
    let onRecheck: () -> Void
    let onRepair: () -> Void

    @State private var section: ShellSection? = .overview

    static let mountPoint = "/Volumes/Vixen/vfs"

    private var healthy: Bool { !doctor.hasFailure }

    var body: some View {
        NavigationSplitView {
            List(selection: $section) {
                Section("This Mac") {
                    ForEach(ShellSection.allCases) { item in
                        Label {
                            HStack {
                                Text(item.title)
                                if item == .daemon {
                                    Spacer()
                                    Circle()
                                        .fill(healthy ? Color.green : Color.red)
                                        .frame(width: 8, height: 8)
                                }
                            }
                        } icon: {
                            Image(systemName: item.symbol)
                        }
                        .tag(item)
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 210, max: 240)
            .safeAreaInset(edge: .bottom) { accountFooter }
        } detail: {
            detail
                .frame(minWidth: 560, minHeight: 480)
                .toolbar { toolbarContent }
        }
    }

    @ViewBuilder
    private var detail: some View {
        switch section ?? .overview {
        case .overview:
            OverviewSurface(
                doctor: doctor, appVersion: appVersion,
                brandOrange: brandOrange, healthy: healthy,
                onReveal: revealMount, onSelect: { section = $0 }
            )
        case .builds:
            ComingOnlineSurface(
                title: "Builds",
                symbol: "hammer",
                headline: "Build history isn't recorded yet",
                detail: "Every evaluation already produces a content-addressed output tree. Once the daemon persists its run firehose, this becomes a live + past build list — and you'll be able to reveal any past result in Finder.",
                brandOrange: brandOrange
            )
        case .store:
            ComingOnlineSurface(
                title: "Store",
                symbol: "internaldrive",
                headline: "Store metrics aren't exposed yet",
                detail: "The content-addressed store has the numbers (size on disk, chunk/node counts, dedup ratio) but no stats RPC. Adding one lights up usage and the disk-budget control here.",
                brandOrange: brandOrange
            )
        case .capabilities:
            ComingOnlineSurface(
                title: "Capabilities",
                symbol: "sparkles",
                headline: "Capabilities aren't advertised yet",
                detail: "This will list the services the daemon hosts, the toolchains it discovered, and what builds this machine can execute — sourced from the daemon's status RPC.",
                brandOrange: brandOrange
            )
        case .daemon:
            DaemonHealthSurface(
                doctor: doctor, management: management, ext: ext,
                isChecking: isChecking, isRepairing: isRepairing,
                progress: progress, brandOrange: brandOrange,
                onRecheck: onRecheck, onRepair: onRepair
            )
        }
    }

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Button {
                if let url = URL(string: "https://app.vixen.rs/") {
                    NSWorkspace.shared.open(url)
                }
            } label: {
                Label("Dashboard", systemImage: "arrow.up.right.square")
            }
            .help("Open the Vixen cloud dashboard")
        }
        ToolbarItem(placement: .primaryAction) {
            HStack(spacing: 6) {
                Circle()
                    .fill(healthy ? Color.green : Color.red)
                    .frame(width: 9, height: 9)
                Text(healthy ? "healthy" : "needs attention")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var accountFooter: some View {
        HStack(spacing: 9) {
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(LinearGradient(colors: [brandOrange, brandOrange.opacity(0.6)],
                                     startPoint: .topLeading, endPoint: .bottomTrailing))
                .frame(width: 26, height: 26)
                .overlay(Image(systemName: "person.fill").font(.system(size: 12)).foregroundStyle(.white))
            VStack(alignment: .leading, spacing: 1) {
                Text("Not signed in")
                    .font(.system(size: 12.5, weight: .semibold))
                Text("Sign in on app.vixen.rs")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func revealMount() {
        NSWorkspace.shared.selectFile(nil, inFileViewerRootedAtPath: VixenShell.mountPoint)
    }
}

// MARK: - Overview

private struct OverviewSurface: View {
    let doctor: DoctorSnapshot
    let appVersion: String
    let brandOrange: Color
    let healthy: Bool
    let onReveal: () -> Void
    let onSelect: (ShellSection) -> Void

    private func row(_ id: String) -> DoctorRow? { doctor.rows.first { $0.id == id } }
    private var mounted: Bool { row("mount")?.state == .ok }
    private var connected: Bool { row("backend")?.state == .ok }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 7) {
                    Text("THIS MAC")
                        .font(.system(size: 11, weight: .semibold))
                        .tracking(0.6)
                        .foregroundStyle(.tertiary)
                    HStack(spacing: 11) {
                        Circle()
                            .fill(healthy ? Color.green : Color.red)
                            .frame(width: 13, height: 13)
                            .shadow(color: (healthy ? Color.green : Color.red).opacity(0.6), radius: 5)
                        Text(healthy ? "Everything's running" : "Needs attention")
                            .font(.system(size: 24, weight: .bold))
                    }
                    Text(summary)
                        .font(.system(size: 12.5))
                        .foregroundStyle(.secondary)
                }

                LazyVGrid(columns: [GridItem(.flexible(), spacing: 16), GridItem(.flexible(), spacing: 16)], spacing: 16) {
                    card(title: "Store", select: .store) {
                        ComingOnlineMini(text: "Usage arrives with the store stats RPC.")
                    }
                    card(title: "Builds", select: .builds) {
                        ComingOnlineMini(text: "Run history arrives with the daemon firehose.")
                    }
                    card(title: "Connection", select: .daemon) {
                        VStack(spacing: 0) {
                            kv("mount", value: mounted ? "browsable" : "not mounted", ok: mounted)
                            kv("backend", value: connected ? "connected" : "offline", ok: connected)
                            kv("path", value: VixenShell.mountPoint, ok: nil)
                        }
                    }
                    card(title: "Updates", select: nil) {
                        VStack(spacing: 0) {
                            kv("version", value: "v\(appVersion)", ok: nil)
                            kv("channel", value: "stable · auto", ok: nil)
                        }
                    }
                }

                if mounted {
                    Button(action: onReveal) {
                        Label("Reveal store in Finder", systemImage: "folder")
                    }
                    .controlSize(.large)
                }
            }
            .padding(26)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .navigationTitle("Overview")
    }

    private var summary: String {
        var parts: [String] = []
        parts.append(connected ? "Connected to the VFS backend" : "Backend offline")
        parts.append(mounted ? "mounted at \(VixenShell.mountPoint)" : "not mounted")
        return parts.joined(separator: " · ")
    }

    private func card<Content: View>(title: String, select: ShellSection?, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(title.uppercased())
                    .font(.system(size: 11, weight: .semibold))
                    .tracking(0.5)
                    .foregroundStyle(.secondary)
                Spacer()
                if select != nil {
                    Image(systemName: "chevron.right").font(.system(size: 10, weight: .semibold)).foregroundStyle(.tertiary)
                }
            }
            content()
        }
        .padding(15)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 11, style: .continuous).fill(.quaternary.opacity(0.5)))
        .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5))
        .contentShape(Rectangle())
        .onTapGesture { if let select { onSelect(select) } }
    }

    private func kv(_ key: String, value: String, ok: Bool?) -> some View {
        HStack {
            Text(key).font(.system(size: 12.5)).foregroundStyle(.secondary)
            Spacer()
            HStack(spacing: 6) {
                if let ok {
                    Circle().fill(ok ? Color.green : Color.red).frame(width: 7, height: 7)
                }
                Text(value).font(.system(size: 12.5))
            }
        }
        .padding(.vertical, 5)
    }
}

// MARK: - Daemon & Health (the demoted doctor)

private struct DaemonHealthSurface: View {
    let doctor: DoctorSnapshot
    let management: ManagementService.StatusSnapshot
    let ext: FSKitModuleSnapshot
    let isChecking: Bool
    let isRepairing: Bool
    let progress: String?
    let brandOrange: Color
    let onRecheck: () -> Void
    let onRepair: () -> Void

    private var healthy: Bool { !doctor.hasFailure }
    private var okCount: Int { doctor.rows.filter { $0.state == .ok }.count }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 7) {
                    Text("LOCAL DAEMON")
                        .font(.system(size: 11, weight: .semibold)).tracking(0.6).foregroundStyle(.tertiary)
                    HStack(spacing: 11) {
                        if isChecking || isRepairing {
                            ProgressView().controlSize(.small)
                        } else {
                            Circle().fill(healthy ? Color.green : Color.red).frame(width: 13, height: 13)
                        }
                        Text(headline).font(.system(size: 22, weight: .bold))
                    }
                    Text("All five subsystems are checked here. When one drops, its row turns red and the sidebar dot follows — that's the only time this page asks for you.")
                        .font(.system(size: 12.5)).foregroundStyle(.secondary)
                    if let progress {
                        Text(progress).font(.system(size: 11.5)).foregroundStyle(.secondary)
                    }
                }

                // Health checks card
                VStack(alignment: .leading, spacing: 0) {
                    HStack {
                        Text("HEALTH CHECKS").font(.system(size: 11, weight: .semibold)).tracking(0.5).foregroundStyle(.secondary)
                        Spacer()
                        Text("\(okCount) / \(doctor.rows.count)").font(.system(size: 11, weight: .medium)).foregroundStyle(healthy ? .secondary : Color.red)
                    }
                    .padding(.bottom, 6)

                    ForEach(doctor.rows) { row in
                        HealthCheckRow(row: row, brandOrange: brandOrange,
                                       canRepair: row.id == "mount" && row.state == .failed,
                                       onRepair: onRepair)
                        if row.id != doctor.rows.last?.id {
                            Divider().opacity(0.4)
                        }
                    }
                }
                .padding(15)
                .background(RoundedRectangle(cornerRadius: 11, style: .continuous).fill(.quaternary.opacity(0.5)))
                .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).strokeBorder(healthy ? AnyShapeStyle(Color(nsColor: .separatorColor)) : AnyShapeStyle(Color.red.opacity(0.4)), lineWidth: 0.5))

                HStack(spacing: 10) {
                    if doctor.hasFailure {
                        Button(action: onRepair) {
                            Label("Repair VixenFS", systemImage: "wrench.and.screwdriver.fill")
                        }
                        .controlSize(.large)
                        .buttonStyle(.borderedProminent)
                        .tint(brandOrange)
                        .disabled(isChecking || isRepairing)
                    }
                    Button(action: onRecheck) {
                        Label("Re-check", systemImage: "arrow.clockwise")
                    }
                    .controlSize(.large)
                    .disabled(isChecking || isRepairing)
                }
            }
            .padding(26)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .navigationTitle("Daemon & Health")
    }

    private var headline: String {
        if isRepairing { return "Repairing…" }
        if isChecking { return "Checking…" }
        return healthy ? "Healthy" : "\(doctor.rows.count - okCount) issue\(doctor.rows.count - okCount == 1 ? "" : "s")"
    }
}

private struct HealthCheckRow: View {
    let row: DoctorRow
    let brandOrange: Color
    let canRepair: Bool
    let onRepair: () -> Void

    var body: some View {
        HStack(spacing: 11) {
            ZStack {
                Circle().fill(tint.opacity(0.16)).frame(width: 20, height: 20)
                Image(systemName: glyph).font(.system(size: 11, weight: .bold)).foregroundStyle(tint)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(row.title).font(.system(size: 13))
                if let detail = row.detail, !detail.isEmpty, row.state != .ok {
                    Text(detail).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(2)
                }
            }
            Spacer(minLength: 8)
            if canRepair {
                Button("Remount", action: onRepair).controlSize(.small).buttonStyle(.borderedProminent).tint(brandOrange)
            } else if row.state == .ok, let detail = row.detail, !detail.isEmpty {
                Text(detail).font(.system(size: 11.5)).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(.vertical, 8)
    }

    private var tint: Color {
        switch row.state {
        case .ok: .green
        case .warning: .yellow
        case .failed: .red
        case .checking: brandOrange
        case .pending: .secondary
        }
    }

    private var glyph: String {
        switch row.state {
        case .ok: "checkmark"
        case .warning: "exclamationmark"
        case .failed: "xmark"
        case .checking: "arrow.triangle.2.circlepath"
        case .pending: "circle"
        }
    }
}

// MARK: - Coming online placeholders

private struct ComingOnlineSurface: View {
    let title: String
    let symbol: String
    let headline: String
    let detail: String
    let brandOrange: Color

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: symbol)
                .font(.system(size: 34, weight: .regular))
                .foregroundStyle(brandOrange.opacity(0.8))
            Text(headline).font(.system(size: 18, weight: .semibold))
            Text(detail)
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            Text("COMING ONLINE")
                .font(.system(size: 10, weight: .bold)).tracking(1.2)
                .foregroundStyle(.tertiary)
                .padding(.top, 2)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(40)
        .navigationTitle(title)
    }
}

private struct ComingOnlineMini: View {
    let text: String
    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "hourglass").font(.system(size: 11)).foregroundStyle(.tertiary)
            Text(text).font(.system(size: 12)).foregroundStyle(.secondary)
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
    }
}
