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

struct DoctorSnapshot: Sendable {
    let rows: [DoctorRow]

    static let pending = DoctorSnapshot(rows: [
        DoctorRow(id: "registration", title: "App Registration", state: .pending, detail: nil),
        DoctorRow(id: "extension", title: "File System Extension", state: .pending, detail: nil),
        DoctorRow(id: "management", title: "Management Helper", state: .pending, detail: nil),
        DoctorRow(id: "mount", title: "VixenFS Mount", state: .pending, detail: nil),
        DoctorRow(id: "status", title: "Status File", state: .pending, detail: nil),
        DoctorRow(id: "backend", title: "Backend Connection", state: .pending, detail: nil),
    ])

    var hasFailure: Bool {
        rows.contains { $0.state == .failed || $0.state == .warning }
    }

    var problemRows: [DoctorRow] {
        rows.filter { $0.state == .failed || $0.state == .warning }
    }

    var isReady: Bool {
        rows.allSatisfy { $0.state == .ok }
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
            && row(id: "registration")?.state == .ok
            && row(id: "extension")?.state == .ok
            && row(id: "management")?.state == .ok
    }

    var needsUserSessionRegistrationRepair: Bool {
        guard let registration = row(id: "registration") else { return false }
        guard registration.state == .warning || registration.state == .failed else { return false }
        let detail = registration.detail ?? ""
        return detail.localizedCaseInsensitiveContains("Stale VixenFS registrations")
            || detail.localizedCaseInsensitiveContains("PlugInKit points at")
    }

    func withRegistrationRepairFailure(_ error: String) -> DoctorSnapshot {
        DoctorSnapshot(
            rows: rows.map { row in
                guard row.id == "registration" else { return row }
                let existing = row.detail?.trimmingCharacters(in: .whitespacesAndNewlines)
                let detail: String
                if let existing, !existing.isEmpty {
                    detail = "\(existing)\nAutomatic cleanup failed: \(error)"
                } else {
                    detail = "Automatic cleanup failed: \(error)"
                }
                return DoctorRow(
                    id: row.id,
                    title: row.title,
                    state: .warning,
                    detail: detail
                )
            }
        )
    }

    private func row(id: String) -> DoctorRow? {
        rows.first { $0.id == id }
    }
}

enum DoctorService {
    private static let mountPoint = "/Volumes/Vixen/vfs"
    private static let statusPath = "\(mountPoint)/.vixenfs/status.json"
    private static let logger = Logger(subsystem: "eu.bearcove.vixen", category: "Doctor")
    private static let installedAppPath = "/Applications/Vixen.app"
    private static let installedExtensionPath =
        "/Applications/Vixen.app/Contents/Extensions/VixenFS.appex"
    private static let extensionBundleIdentifier = "eu.bearcove.vixenfs"
    private static var managementHelperPath: String {
        Bundle.main.bundleURL
            .appendingPathComponent("Contents", isDirectory: true)
            .appendingPathComponent("MacOS", isDirectory: true)
            .appendingPathComponent("vixen-management", isDirectory: false)
            .path
    }

    static func check(extensionEnabled: Bool, extensionError: String?) async -> DoctorSnapshot {
        await Task.detached(priority: .utility) {
            checkSync(extensionEnabled: extensionEnabled, extensionError: extensionError)
        }.value
    }

    private static func checkSync(extensionEnabled: Bool, extensionError: String?) -> DoctorSnapshot {
        let started = DispatchTime.now().uptimeNanoseconds
        var rows: [DoctorRow] = []
        defer {
            let elapsedMs = elapsedMilliseconds(since: started)
            let problemCount = rows.filter { $0.state == .failed || $0.state == .warning }.count
            logger.notice("doctor check finished in \(elapsedMs, privacy: .public)ms with \(problemCount, privacy: .public) problem rows")
        }

        rows.append(timed("registration") {
            checkRegistration()
        })
        rows.append(timed("extension") {
            checkExtension(extensionEnabled: extensionEnabled, extensionError: extensionError)
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
            checkBackend(status)
        })

        return DoctorSnapshot(rows: rows)
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

    private static func checkRegistration() -> DoctorRow {
        let result = runProcess(
            "/usr/bin/pluginkit",
            arguments: ["-mAvvv", "-i", extensionBundleIdentifier],
            timeout: 2
        )
        guard result.status == 0 else {
            return DoctorRow(
                id: "registration",
                title: "App Registration",
                state: .failed,
                detail: result.failureDetail(defaultMessage: "PlugInKit registration unavailable")
            )
        }

        let records = parsePlugInKitRecords(result.stdout)
        logger.notice("doctor registration records count=\(records.count, privacy: .public) paths=\(records.compactMap(\.path).joined(separator: ", "), privacy: .public)")
        let hasInstalledRegistration = records.contains { record in
            record.path == installedExtensionPath && record.parentBundle == installedAppPath
        }
        guard hasInstalledRegistration else {
            let registeredPaths = records.compactMap(\.path)
            logger.error("doctor registration failed installedRegistration=false paths=\(registeredPaths.joined(separator: ", "), privacy: .public)")
            return DoctorRow(
                id: "registration",
                title: "App Registration",
                state: .failed,
                detail: registeredPaths.isEmpty
                    ? "PlugInKit has no VixenFS registration"
                    : "PlugInKit points at \(registeredPaths.joined(separator: ", "))"
            )
        }

        let stalePaths = records
            .compactMap(\.path)
            .filter { $0 != installedExtensionPath }
        if !stalePaths.isEmpty {
            logger.warning("doctor registration warning stalePaths=\(stalePaths.joined(separator: ", "), privacy: .public)")
            return DoctorRow(
                id: "registration",
                title: "App Registration",
                state: .warning,
                detail: "Stale VixenFS registrations: \(stalePaths.joined(separator: ", "))"
            )
        }

        logger.notice("doctor registration ok installedPath=\(installedExtensionPath, privacy: .public)")
        return DoctorRow(
            id: "registration",
            title: "App Registration",
            state: .ok,
            detail: "PlugInKit points at /Applications/Vixen.app"
        )
    }

    private struct PlugInKitRecord {
        var path: String?
        var parentBundle: String?
    }

    private static func parsePlugInKitRecords(_ output: String) -> [PlugInKitRecord] {
        var records: [PlugInKitRecord] = []
        var current: PlugInKitRecord?

        func finishCurrent() {
            if let current {
                records.append(current)
            }
            current = nil
        }

        for rawLine in output.split(whereSeparator: \.isNewline) {
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.contains("\(extensionBundleIdentifier)(") {
                finishCurrent()
                current = PlugInKitRecord()
                continue
            }

            guard current != nil else { continue }
            if let value = value(in: line, after: "Path = ") {
                current?.path = value
            } else if let value = value(in: line, after: "Parent Bundle = ") {
                current?.parentBundle = value
            }
        }
        finishCurrent()

        return records
    }

    private static func value(in line: String, after prefix: String) -> String? {
        guard line.hasPrefix(prefix) else { return nil }
        return String(line.dropFirst(prefix.count)).trimmingCharacters(in: .whitespaces)
    }

    private static func checkExtension(extensionEnabled: Bool, extensionError: String?) -> DoctorRow {
        if let extensionError {
            logger.error("doctor extension failed error=\(extensionError, privacy: .public)")
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .failed,
                detail: extensionError
            )
        }

        logger.notice("doctor extension verdict enabled=\(extensionEnabled, privacy: .public)")
        return DoctorRow(
            id: "extension",
            title: "File System Extension",
            state: extensionEnabled ? .ok : .failed,
            detail: extensionEnabled ? "VixenFS is enabled" : "VixenFS is disabled"
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

    private static func checkBackend(_ status: StatusProbe) -> DoctorRow {
        guard let payload = status.payload else {
            logger.error("doctor backend failed because status payload is missing")
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .failed,
                detail: "No status payload"
            )
        }

        let connected = payload["connected"] as? Bool ?? false
        if connected {
            logger.notice("doctor backend ok connected=true")
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .ok,
                detail: "VFS backend is connected"
            )
        }

        let everConnected = payload["ever_connected"] as? Bool ?? false
        let reason = (payload["disconnect_reason"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        logger.notice("doctor backend idle connected=false everConnected=\(everConnected, privacy: .public) reason=\(reason ?? "none", privacy: .public); running active probe")
        let result = runProcess(
            managementHelperPath,
            arguments: ["probe-vfs"],
            timeout: 6
        )
        guard result.status == 0 else {
            logger.error("doctor backend probe failed timedOut=\(result.timedOut, privacy: .public) detail=\(result.failureDetail(defaultMessage: "VFS backend probe failed"), privacy: .public)")
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .failed,
                detail: result.failureDetail(defaultMessage: "VFS backend probe failed")
            )
        }

        let detail = result.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
        logger.notice("doctor backend probe ok stdout=\(detail, privacy: .public)")
        return DoctorRow(
            id: "backend",
            title: "Backend Connection",
            state: .ok,
            detail: detail.isEmpty ? "VFS probe read through VixenFS" : detail
        )
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
