import Darwin
import Foundation
import OSLog

enum DoctorState: Sendable {
    case pending
    case checking
    case ok
    case warning
    case failed
}

struct DoctorRow: Identifiable, Sendable {
    let id: String
    let title: String
    let state: DoctorState
    let detail: String?
}

struct FSKitModuleSnapshot: Sendable {
    let found: Bool
    let enabled: Bool
    let urlPath: String?
    let error: String?

    static let pending = FSKitModuleSnapshot(
        found: false,
        enabled: false,
        urlPath: nil,
        error: nil
    )
}

/// Live backend/daemon facts, parsed from the VixenFS `status.json` payload.
/// This is the process the FSKit mount is connected to; on macOS it stands in
/// for "the daemon" in the Daemon & Health surface.
struct BackendStatus: Sendable {
    let connected: Bool
    let extensionPid: Int?
    let uptimeSeconds: Double?
    let socketPath: String?
    let appGroupPath: String?
    let bundleVersion: String?
    let connectedAt: String?
    let disconnectReason: String?
}

struct DoctorSnapshot: Sendable {
    let rows: [DoctorRow]
    let backend: BackendStatus?

    static let pending = DoctorSnapshot(
        rows: [
            DoctorRow(id: "extension", title: "File System Extension", state: .pending, detail: nil),
            DoctorRow(id: "management", title: "Management Helper", state: .pending, detail: nil),
            DoctorRow(id: "mount", title: "VixenFS Mount", state: .pending, detail: nil),
            DoctorRow(id: "status", title: "Status File", state: .pending, detail: nil),
            DoctorRow(id: "backend", title: "Backend Connection", state: .pending, detail: nil),
        ],
        backend: nil
    )

    var hasFailure: Bool {
        rows.contains { $0.state == .failed || $0.state == .warning }
    }

    var problemRows: [DoctorRow] {
        rows.filter { $0.state == .failed || $0.state == .warning }
    }

    var isReady: Bool {
        rows.allSatisfy { $0.state == .ok }
    }

    /// Is the VixenFS volume currently mounted? Used to re-arm auto-mount.
    var mountIsUp: Bool {
        row(id: "mount")?.state == .ok
    }

    var canAttemptSafeMountRepair: Bool {
        row(id: "extension")?.state == .ok
            && row(id: "management")?.state == .ok
            && row(id: "mount")?.state == .failed
    }

    var needsFskitRestart: Bool {
        let staleFskitState = problemRows.contains { row in
            guard let detail = row.detail else { return false }
            return detail.localizedCaseInsensitiveContains("Module eu.bearcove.vixenfs is disabled")
                || detail.localizedCaseInsensitiveContains("Unable to invoke task")
        }
        return staleFskitState
            && row(id: "extension")?.state == .ok
            && row(id: "management")?.state == .ok
    }

    private func row(id: String) -> DoctorRow? {
        rows.first { $0.id == id }
    }
}

enum DoctorService {
    private static let mountPoint = "/Volumes/Vixen/vfs"
    private static let statusPath = "\(mountPoint)/.vixenfs/status.json"
    private static let logger = Logger(subsystem: "eu.bearcove.vixen", category: "Doctor")
    private static let installedExtensionPath =
        "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex"
    private static var managementHelperPath: String {
        Bundle.main.bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("MacOS", isDirectory: true)
            .appendingPathComponent("vixen-management", isDirectory: false)
            .path
    }

    static func check(extensionSnapshot: FSKitModuleSnapshot) async -> DoctorSnapshot {
        await Task.detached(priority: .utility) {
            checkSync(extensionSnapshot: extensionSnapshot)
        }.value
    }

    private static func checkSync(extensionSnapshot: FSKitModuleSnapshot) -> DoctorSnapshot {
        let started = DispatchTime.now().uptimeNanoseconds
        var rows: [DoctorRow] = []
        defer {
            let elapsedMs = elapsedMilliseconds(since: started)
            let problemCount = rows.filter { $0.state == .failed || $0.state == .warning }.count
            logger.notice("doctor check finished in \(elapsedMs, privacy: .public)ms with \(problemCount, privacy: .public) problem rows")
        }

        rows.append(timed("extension") {
            checkExtension(extensionSnapshot)
        })

        let management = timed("management snapshot") {
            ManagementService.snapshot()
        }
        rows.append(timed("management row") {
            checkManagement(management)
        })

        let mount = timed("mount table") {
            checkMount(lastAction: management.lastActionStatus)
        }
        rows.append(mount.row)

        let status = timed("status payload") {
            mount.isMounted ? readStatusPayload() : StatusProbe.skipped("Mount missing")
        }
        rows.append(status.row)
        rows.append(timed("backend probe") {
            checkBackend()
        })

        return DoctorSnapshot(rows: rows, backend: backendStatus(from: status.payload))
    }

    private static func backendStatus(from payload: [String: Any]?) -> BackendStatus? {
        guard let payload else { return nil }
        return BackendStatus(
            connected: payload["connected"] as? Bool ?? false,
            extensionPid: (payload["extension_pid"] as? NSNumber)?.intValue,
            uptimeSeconds: (payload["uptime_seconds"] as? NSNumber)?.doubleValue,
            socketPath: trimmedString(payload["socket_path"]),
            appGroupPath: trimmedString(payload["app_group_path"]),
            bundleVersion: trimmedString(payload["bundle_version"]),
            connectedAt: trimmedString(payload["connected_at"]),
            disconnectReason: trimmedString(payload["disconnect_reason"])
        )
    }

    private static func trimmedString(_ value: Any?) -> String? {
        guard let string = value as? String else { return nil }
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    private static func timed<T>(_ name: String, _ body: () -> T) -> T {
        let started = DispatchTime.now().uptimeNanoseconds
        let value = body()
        logger.notice("doctor step \(name, privacy: .public) finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
        return value
    }

    private static func elapsedMilliseconds(since started: UInt64) -> Int {
        let elapsed = DispatchTime.now().uptimeNanoseconds - started
        return Int(elapsed / 1_000_000)
    }

    private static func checkExtension(_ snapshot: FSKitModuleSnapshot) -> DoctorRow {
        if let error = snapshot.error {
            logger.error("doctor extension failed error=\(error, privacy: .public)")
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .failed,
                detail: error
            )
        }

        guard snapshot.found else {
            logger.error("doctor extension failed found=false")
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .failed,
                detail: "FSKit does not list VixenFS as an installed module"
            )
        }

        guard snapshot.enabled else {
            logger.notice("doctor extension verdict found=true enabled=false url=\(snapshot.urlPath ?? "none", privacy: .public)")
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .failed,
                detail: "VixenFS is installed but disabled"
            )
        }

        if let urlPath = snapshot.urlPath, urlPath != installedExtensionPath {
            logger.warning("doctor extension warning url=\(urlPath, privacy: .public) expected=\(installedExtensionPath, privacy: .public)")
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .warning,
                detail: "FSKit installed module points at \(urlPath)"
            )
        }

        logger.notice("doctor extension verdict found=true enabled=true url=\(snapshot.urlPath ?? "none", privacy: .public)")
        return DoctorRow(
            id: "extension",
            title: "File System Extension",
            state: .ok,
            detail: "FSKit lists VixenFS as installed and enabled"
        )
    }

    private static func checkManagement(_ snapshot: ManagementService.StatusSnapshot) -> DoctorRow {
        logger.notice("doctor management snapshot ready=\(snapshot.isReady, privacy: .public) description=\(snapshot.description, privacy: .public) detail=\(snapshot.detail ?? "none", privacy: .public) background=\(snapshot.backgroundItemDescription ?? "none", privacy: .public)")
        return DoctorRow(
            id: "management",
            title: "Management Helper",
            state: snapshot.isReady ? .ok : .failed,
            detail: snapshot.isReady
                ? snapshot.backgroundItemDescription ?? "Bundled helper is ready"
                : snapshot.description
        )
    }

    private struct MountProbe {
        let row: DoctorRow
        let isMounted: Bool
    }

    private static func checkMount(lastAction: ManagementService.LastActionStatus?) -> MountProbe {
        let result = runProcess("/sbin/mount", timeout: 2)
        guard result.status == 0 else {
            return MountProbe(
                row: DoctorRow(
                    id: "mount",
                    title: "VixenFS Mount",
                    state: .failed,
                    detail: result.failureDetail(defaultMessage: "mount table unavailable")
                ),
                isMounted: false
            )
        }

        let isMounted = result.stdout
            .split(whereSeparator: \.isNewline)
            .contains { line in
                line.contains(" on \(mountPoint) ") && line.localizedCaseInsensitiveContains("vixenfs")
            }

        let failedMountDetail: String?
        if let lastAction,
            !lastAction.ok,
            lastAction.button == "mount" || lastAction.button == "remount"
        {
            failedMountDetail = lastAction.failureDescription
        } else {
            failedMountDetail = nil
        }
        logger.notice("doctor mount check mounted=\(isMounted, privacy: .public) lastActionButton=\(lastAction?.button ?? "none", privacy: .public) lastActionOk=\(lastAction?.ok.description ?? "none", privacy: .public) failedDetail=\(failedMountDetail ?? "none", privacy: .public)")

        return MountProbe(
            row: DoctorRow(
                id: "mount",
                title: "VixenFS Mount",
                state: isMounted ? .ok : .failed,
                detail: isMounted ? mountPoint : failedMountDetail ?? "No vixenfs mount at \(mountPoint)"
            ),
            isMounted: isMounted
        )
    }

    private struct StatusProbe {
        let row: DoctorRow
        let payload: [String: Any]?

        static func skipped(_ detail: String) -> StatusProbe {
            StatusProbe(
                row: DoctorRow(id: "status", title: "Status File", state: .failed, detail: detail),
                payload: nil
            )
        }
    }

    private static func readStatusPayload() -> StatusProbe {
        let result = runProcess("/bin/cat", arguments: [statusPath], timeout: 1)
        guard result.status == 0 else {
            logger.error("doctor status read failed timedOut=\(result.timedOut, privacy: .public) detail=\(result.failureDetail(defaultMessage: "Could not read \(statusPath)"), privacy: .public)")
            return StatusProbe(
                row: DoctorRow(
                    id: "status",
                    title: "Status File",
                    state: .failed,
                    detail: result.timedOut
                        ? "Timed out reading \(statusPath)"
                        : result.failureDetail(defaultMessage: "Could not read \(statusPath)")
                ),
                payload: nil
            )
        }

        guard
            let data = result.stdout.data(using: .utf8),
            let object = try? JSONSerialization.jsonObject(with: data),
            let payload = object as? [String: Any]
        else {
            logger.error("doctor status read failed invalid JSON payload=\(result.stdout, privacy: .public)")
            return StatusProbe(
                row: DoctorRow(
                    id: "status",
                    title: "Status File",
                    state: .failed,
                    detail: "Status payload is not valid JSON"
                ),
                payload: nil
            )
        }

        let appGroupPath = (payload["app_group_path"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let hasAppGroup = appGroupPath?.isEmpty == false
        logger.notice("doctor status payload parsed hasAppGroup=\(hasAppGroup, privacy: .public) appGroupPath=\(appGroupPath ?? "none", privacy: .public) keys=\(payload.keys.sorted().joined(separator: ", "), privacy: .public)")

        return StatusProbe(
            row: DoctorRow(
                id: "status",
                title: "Status File",
                state: hasAppGroup ? .ok : .failed,
                detail: hasAppGroup ? appGroupPath : "Missing app_group_path"
            ),
            payload: payload
        )
    }

    /// Health = read the well-known file that Vixen.app's own prefix serves, straight
    /// through the mount, with a normal Swift file read. That exercises the whole chain
    /// (kernel → FSKit → vox socket → the app's prefix, looping back into this process)
    /// without a helper, an active probe, or a second prefix-provider — so unlike the old
    /// `vixen-management probe-vfs`, it can't displace the real prefix off the mount.
    private static func checkBackend() -> DoctorRow {
        // The app provides its tree under its claimed prefix (`/Volumes/Vixen/vfs/app/`),
        // not at the mount root — read the well-known file there.
        let path = "\(mountPoint)/\(vfsPrefix)/README.md"
        guard let data = readFileWithTimeout(path, timeout: 2), !data.isEmpty else {
            logger.error("doctor backend failed: couldn't read \(path, privacy: .public)")
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .failed,
                detail: "Couldn't read \(path)"
            )
        }
        logger.notice("doctor backend ok: read \(data.count) bytes from \(path, privacy: .public)")
        return DoctorRow(
            id: "backend",
            title: "Backend Connection",
            state: .ok,
            detail: "Read README.md (\(data.count) bytes) from the Vixen prefix"
        )
    }

    /// Read a file on a background queue with a bound, so a wedged mount can't hang the
    /// doctor (and the UI) indefinitely.
    private static func readFileWithTimeout(_ path: String, timeout: TimeInterval) -> Data? {
        let semaphore = DispatchSemaphore(value: 0)
        var result: Data?
        DispatchQueue.global(qos: .userInitiated).async {
            result = try? Data(contentsOf: URL(fileURLWithPath: path))
            semaphore.signal()
        }
        guard semaphore.wait(timeout: .now() + timeout) == .success else {
            return nil
        }
        return result
    }

    private struct CommandResult {
        let status: Int32?
        let stdout: String
        let stderr: String
        let timedOut: Bool

        func failureDetail(defaultMessage: String) -> String {
            if timedOut {
                return "Timed out"
            }
            let detail = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            if !detail.isEmpty {
                return detail
            }
            if let status {
                return "\(defaultMessage) (exit \(status))"
            }
            return defaultMessage
        }
    }

    private static func runProcess(
        _ executable: String,
        arguments: [String] = [],
        timeout: TimeInterval
    ) -> CommandResult {
        let started = DispatchTime.now().uptimeNanoseconds
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in
            done.signal()
        }

        do {
            try process.run()
        } catch {
            logProcess(executable, started: started, status: 127, timedOut: false)
            return CommandResult(
                status: 127,
                stdout: "",
                stderr: error.localizedDescription,
                timedOut: false
            )
        }

        if done.wait(timeout: .now() + timeout) == .timedOut {
            let pid = process.processIdentifier
            process.terminate()
            Darwin.kill(pid, SIGKILL)
            logProcess(executable, started: started, status: nil, timedOut: true)
            return CommandResult(status: nil, stdout: "", stderr: "", timedOut: true)
        }

        let stdoutText =
            String(data: stdout.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        let stderrText =
            String(data: stderr.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        logProcess(executable, started: started, status: process.terminationStatus, timedOut: false)
        return CommandResult(
            status: process.terminationStatus,
            stdout: stdoutText,
            stderr: stderrText,
            timedOut: false
        )
    }

    private static func logProcess(
        _ executable: String,
        started: UInt64,
        status: Int32?,
        timedOut: Bool
    ) {
        let statusText = status.map(String.init) ?? "none"
        logger.notice("doctor process \(executable, privacy: .public) finished in \(elapsedMilliseconds(since: started), privacy: .public)ms status=\(statusText, privacy: .public) timedOut=\(timedOut, privacy: .public)")
    }
}
