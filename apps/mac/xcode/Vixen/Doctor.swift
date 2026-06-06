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
        DoctorRow(id: "management", title: "Management Service", state: .pending, detail: nil),
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

    private func row(id: String) -> DoctorRow? {
        rows.first { $0.id == id }
    }
}

enum DoctorService {
    private static let mountPoint = "/Volumes/Vixen/vfs"
    private static let statusPath = "\(mountPoint)/.vixenfs/status.json"
    private static let logger = Logger(subsystem: "eu.bearcove.vixen", category: "Doctor")

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
            logger.info("doctor check finished in \(elapsedMs, privacy: .public)ms with \(problemCount, privacy: .public) problem rows")
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
        rows.append(timed("backend row") {
            checkBackend(status)
        })

        return DoctorSnapshot(rows: rows)
    }

    private static func timed<T>(_ name: String, _ body: () -> T) -> T {
        let started = DispatchTime.now().uptimeNanoseconds
        let value = body()
        logger.info("doctor step \(name, privacy: .public) finished in \(elapsedMilliseconds(since: started), privacy: .public)ms")
        return value
    }

    private static func elapsedMilliseconds(since started: UInt64) -> Int {
        let elapsed = DispatchTime.now().uptimeNanoseconds - started
        return Int(elapsed / 1_000_000)
    }

    private static func checkRegistration() -> DoctorRow {
        do {
            try ManagementService.repairExtension()
            return DoctorRow(
                id: "registration",
                title: "App Registration",
                state: .ok,
                detail: "LaunchServices and PlugInKit point at /Applications/Vixen.app"
            )
        } catch {
            return DoctorRow(
                id: "registration",
                title: "App Registration",
                state: .failed,
                detail: error.localizedDescription
            )
        }
    }

    private static func checkExtension(extensionEnabled: Bool, extensionError: String?) -> DoctorRow {
        if let extensionError {
            return DoctorRow(
                id: "extension",
                title: "File System Extension",
                state: .failed,
                detail: extensionError
            )
        }

        return DoctorRow(
            id: "extension",
            title: "File System Extension",
            state: extensionEnabled ? .ok : .failed,
            detail: extensionEnabled ? "VixenFS is enabled" : "VixenFS is disabled"
        )
    }

    private static func checkManagement(_ snapshot: ManagementService.StatusSnapshot) -> DoctorRow {
        DoctorRow(
            id: "management",
            title: "Management Service",
            state: snapshot.isReady ? .ok : .failed,
            detail: snapshot.isReady ? "Ready" : snapshot.description
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
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .failed,
                detail: "No status payload"
            )
        }

        let connected = payload["connected"] as? Bool ?? false
        if connected {
            return DoctorRow(
                id: "backend",
                title: "Backend Connection",
                state: .ok,
                detail: "VFS backend is connected"
            )
        }

        let reason = (payload["disconnect_reason"] as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return DoctorRow(
            id: "backend",
            title: "Backend Connection",
            state: .failed,
            detail: reason?.isEmpty == false ? reason : "VFS backend is not connected"
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
        logger.info("doctor process \(executable, privacy: .public) finished in \(elapsedMilliseconds(since: started), privacy: .public)ms status=\(statusText, privacy: .public) timedOut=\(timedOut, privacy: .public)")
    }
}
