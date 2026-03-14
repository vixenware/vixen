import CryptoKit
import FSKit
import Foundation
import RoamRuntime
import os

enum VixenFSConstants {
    static let subsystem = "eu.bearcove.vixenfs"
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"
    static let shmBootstrapMaxAttempts = 1
    static let shmBootstrapRetryDelayNs: UInt64 = 0
}

func vixenPOSIXCode(for error: Error) -> Int32 {
    if let posix = error as? POSIXError {
        return posix.code.rawValue
    }

    let nsError = error as NSError
    if nsError.domain == NSPOSIXErrorDomain {
        return Int32(nsError.code)
    }

    if error is CancellationError {
        return POSIXError.ECANCELED.rawValue
    }

    if let err = error as? ConnectionError {
        switch err {
        case .connectionClosed:
            return POSIXError.ENOTCONN.rawValue
        case .timeout:
            return POSIXError.ETIMEDOUT.rawValue
        case .transportError:
            return POSIXError.EIO.rawValue
        case .goodbye:
            return POSIXError.ECONNRESET.rawValue
        case .protocolViolation, .handshakeFailed:
            return POSIXError.EPROTO.rawValue
        }
    }

    if let err = error as? TransportError {
        switch err {
        case .connectionClosed:
            return POSIXError.ENOTCONN.rawValue
        case .wouldBlock:
            return POSIXError.EAGAIN.rawValue
        case .frameDecoding, .frameEncoding, .protocolViolation:
            return POSIXError.EPROTO.rawValue
        case .transportIO:
            return POSIXError.EIO.rawValue
        }
    }

    if let err = error as? RoamError {
        switch err {
        case .unknownMethod, .notImplemented:
            return POSIXError.ENOSYS.rawValue
        case .decodeError, .encodeError:
            return POSIXError.EPROTO.rawValue
        case .connectionClosed:
            return POSIXError.ENOTCONN.rawValue
        case .timeout:
            return POSIXError.ETIMEDOUT.rawValue
        case .cancelled:
            return POSIXError.ECANCELED.rawValue
        }
    }

    if let err = error as? ShmBootstrapError {
        switch err {
        case .invalidSid, .invalidSocketPath, .invalidRequestEncoding:
            return POSIXError.EINVAL.rawValue
        case .unsupportedPlatform:
            return POSIXError.ENOTSUP.rawValue
        case .socketCreateFailed(let errno), .connectFailed(let errno), .writeFailed(let errno),
            .readFailed(let errno):
            return errno
        case .eof:
            return POSIXError.EPIPE.rawValue
        case .invalidResponseMagic, .protocolError, .missingFileDescriptor:
            return POSIXError.EPROTO.rawValue
        }
    }

    if let err = error as? ShmRegionError {
        switch err {
        case .invalidSize, .invalidOffset, .resizeShrinkUnsupported:
            return POSIXError.EINVAL.rawValue
        case .createFailed(let errno), .openFailed(let errno), .statFailed(let errno),
            .mmapFailed(let errno), .munmapFailed(let errno), .ftruncateFailed(let errno),
            .fchmodFailed(let errno):
            return errno
        case .emptyFile:
            return POSIXError.ENOENT.rawValue
        }
    }

    return POSIXError.EIO.rawValue
}

func vixenMappedFSError(_ error: Error, operation: StaticString, logger: Logger) -> any Error {
    let code = vixenPOSIXCode(for: error)
    if code == POSIXError.ETIMEDOUT.rawValue {
        logger.error(
            "\(operation): timeout mapped to errno=\(code) (\(String(describing: error), privacy: .public))"
        )
        NSLog("VixenFS: timeout in \(operation)")
    }
    logger.error(
        "\(operation): mapped \(String(describing: error), privacy: .public) to errno=\(code)"
    )
    return fs_errorForPOSIXError(code)
}

extension VfsClient: @unchecked Sendable {}

actor LifecycleTrace {
    static let shared = LifecycleTrace()

    private var sequence: UInt64 = 0

    func mark(_ logger: Logger, event: String, details: String = "") {
        sequence += 1
        if details.isEmpty {
            logger.notice("lifecycle[\(self.sequence)] \(event, privacy: .public)")
        } else {
            logger.notice(
                "lifecycle[\(self.sequence)] \(event, privacy: .public) \(details, privacy: .public)"
            )
        }
    }
}

final class NoopDispatcher: ServiceDispatcher, @unchecked Sendable {
    // Compatibility with older RoamRuntime protocol shape (no channels argument).
    func preregister(
        methodId: UInt64,
        payload: [UInt8],
        registry: ChannelRegistry
    ) async {
        _ = methodId
        _ = payload
        _ = registry
    }

    // Compatibility with newer RoamRuntime protocol shape (channels argument).
    func preregister(
        methodId: UInt64,
        payload: [UInt8],
        channels: [UInt64],
        registry: ChannelRegistry
    ) async {
        _ = channels
    }

    // Compatibility with older RoamRuntime protocol shape (no channels argument).
    func dispatch(
        methodId: UInt64,
        payload: [UInt8],
        requestId: UInt64,
        registry: ChannelRegistry,
        taskTx: @escaping @Sendable (TaskMessage) -> Void
    ) async {
        _ = methodId
        _ = payload
        _ = registry
        taskTx(.response(requestId: requestId, payload: encodeUnknownMethodError()))
    }

    // Compatibility with newer RoamRuntime protocol shape (channels argument).
    func dispatch(
        methodId: UInt64,
        payload: [UInt8],
        channels: [UInt64],
        requestId: UInt64,
        registry: ChannelRegistry,
        taskTx: @escaping @Sendable (TaskMessage) -> Void
    ) async {
        _ = channels
        taskTx(.response(requestId: requestId, payload: encodeUnknownMethodError()))
    }
}

actor ConnectionHealth {
    static let shared = ConnectionHealth()

    private var everConnected = false
    private var currentlyConnected = false
    private var connectedAt: Date?
    private var disconnectedAt: Date?
    private var disconnectReason: String?

    func recordConnected() {
        everConnected = true
        currentlyConnected = true
        connectedAt = Date()
        disconnectedAt = nil
        disconnectReason = nil
    }

    func recordDisconnected(reason: String) {
        currentlyConnected = false
        disconnectedAt = Date()
        disconnectReason = reason
    }

    func toJSON() -> Data {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]

        var json = "{\n"
        json += "  \"connected\": \(currentlyConnected),\n"
        json += "  \"ever_connected\": \(everConnected),\n"

        if let appGroupPath = resolveAppGroupPath() {
            json += "  \"app_group_path\": \"\(appGroupPath)\",\n"
        } else {
            json += "  \"app_group_path\": null,\n"
        }

        if let t = connectedAt {
            json += "  \"connected_at\": \"\(iso.string(from: t))\",\n"
        } else {
            json += "  \"connected_at\": null,\n"
        }

        if let t = disconnectedAt {
            json += "  \"disconnected_at\": \"\(iso.string(from: t))\",\n"
        } else {
            json += "  \"disconnected_at\": null,\n"
        }

        if currentlyConnected, let start = connectedAt {
            let seconds = Date().timeIntervalSince(start)
            json += "  \"uptime_seconds\": \(String(format: "%.3f", seconds)),\n"
        } else if let start = connectedAt, let end = disconnectedAt {
            let seconds = end.timeIntervalSince(start)
            json += "  \"uptime_seconds\": \(String(format: "%.3f", seconds)),\n"
        } else {
            json += "  \"uptime_seconds\": null,\n"
        }

        if let reason = disconnectReason {
            json += "  \"disconnect_reason\": \"\(reason)\"\n"
        } else {
            json += "  \"disconnect_reason\": null\n"
        }

        json += "}\n"
        return Data(json.utf8)
    }

    private func resolveAppGroupPath() -> String? {
        FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: VixenFSConstants.appGroupIdentifier
        )?.path
    }
}

/// Global VFS client shared across the extension
actor VfsConnection {
    static let shared = VfsConnection()

    private struct PeerConnection {
        var client: VfsClient?
        var driverTask: Task<Void, Never>?
        var terminalError: NSError?
    }

    private var peers: [String: PeerConnection] = [:]
    private var defaultSid: String?
    private var disconnectRequested = false
    private var debugEnabled = false
    private let log = Logger(subsystem: VixenFSConstants.subsystem, category: "VfsConnection")
    private var sessionDeathObservers: [UUID: @Sendable (String) -> Void] = [:]

    func connect(url: URL) async throws {
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.begin", details: "url=\(url.absoluteString)")

        guard let mountOptions = parseMountOptions(from: url) else {
            throw POSIXError(.EINVAL)
        }

        defaultSid = mountOptions.sid
        debugEnabled = mountOptions.debug
        disconnectRequested = false

        await SwiftmoireClient.shared.configure(
            dashboardAddr: mountOptions.moireDashboard,
            debugFrames: mountOptions.debug
        )

        if debugEnabled {
            log.notice("mount debug enabled for sid=\(mountOptions.sid, privacy: .public)")
        }

        _ = try await getClient(for: mountOptions.sid)
        log.info("Connected to \(url.absoluteString)")
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.ready", details: "url=\(url.absoluteString)")
    }

    func disconnect() async {
        await LifecycleTrace.shared.mark(log, event: "vfs.disconnect")
        disconnectRequested = true
        debugEnabled = false
        await SwiftmoireClient.shared.configure(dashboardAddr: nil, debugFrames: false)

        for (sid, peer) in peers {
            peer.driverTask?.cancel()
            notifySessionDeath(sid: sid)
        }
        peers.removeAll()
        await ConnectionHealth.shared.recordDisconnected(reason: "requested")
    }

    func getClient(for sid: String? = nil) async throws -> VfsClient {
        let targetSid: String
        if let sid {
            targetSid = sid
        } else if let defaultSid {
            targetSid = defaultSid
        } else {
            throw POSIXError(.ENOTCONN)
        }

        if let existing = peers[targetSid], let client = existing.client {
            return client
        }

        if let existing = peers[targetSid], let terminalError = existing.terminalError {
            throw terminalError
        }

        log.info("Connecting over SHM sid=\(targetSid, privacy: .public)")
        let client = try await establishClient(for: targetSid)
        await ConnectionHealth.shared.recordConnected()
        return client
    }

    func observeSessionDeath(id: UUID, _ observer: @escaping @Sendable (String) -> Void) {
        sessionDeathObservers[id] = observer
    }

    func removeSessionDeathObserver(_ id: UUID) {
        sessionDeathObservers.removeValue(forKey: id)
    }

    func liveSessionIDs() -> [String] {
        peers
            .filter { _, peer in peer.client != nil }
            .map { sid, _ in sid }
            .sorted()
    }

    func candidateSessionIDs() -> [String] {
        var sids = Set<String>()
        if let defaultSid {
            sids.insert(defaultSid)
        }
        for sid in peers.keys {
            sids.insert(sid)
        }
        for sid in bootstrapSessionIDs() {
            sids.insert(sid)
        }
        return sids.sorted()
    }

    private func bootstrapSessionIDs() -> [String] {
        guard
            let appGroupURL = FileManager.default.containerURL(
                forSecurityApplicationGroupIdentifier: VixenFSConstants.appGroupIdentifier)
        else {
            return []
        }
        let sessionsURL = appGroupURL.appendingPathComponent("sessions", isDirectory: true)
        guard
            let entries = try? FileManager.default.contentsOfDirectory(
                at: sessionsURL,
                includingPropertiesForKeys: [.isDirectoryKey],
                options: [.skipsHiddenFiles])
        else {
            return []
        }

        var sids: [String] = []
        for entry in entries {
            let sid = entry.lastPathComponent
            guard isValidSid(sid) else {
                continue
            }
            var isDir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: entry.path, isDirectory: &isDir), isDir.boolValue
            else {
                continue
            }
            sids.append(sid)
        }
        return sids
    }

    private func establishClient(for sid: String) async throws -> VfsClient {
        let ticket = try await requestTicket(sid: sid)
        debugLog("bootstrap control socket established sid=\(sid)")
        log.info(
            "SHM bootstrap ticket sid=\(sid, privacy: .public) hubPath=\(ticket.hubPath, privacy: .public) peerId=\(ticket.peerId, privacy: .public)"
        )
        debugLog("ticket doorbellFd=\(ticket.doorbellFd) hubPathLen=\(ticket.hubPath.count)")
        logPathState(path: ticket.hubPath, label: "hubPath")

        let transport: ShmGuestTransport
        do {
            transport = try ShmGuestTransport.attach(ticket: ticket)
        } catch {
            log.error(
                "SHM attach failed sid=\(sid, privacy: .public): \(self.describe(error: error), privacy: .public)"
            )
            throw error
        }

        let (handle, driver) = try await establishShmGuest(
            transport: transport,
            dispatcher: NoopDispatcher()
        )

        let logger = log
        let driverTask = Task { [weak self] in
            do {
                try await driver.run()
                logger.error("Roam driver exited cleanly — server is gone")
                NSLog("VixenFS: Roam SHM driver exited cleanly (server gone)")
                await self?.handleDriverExit(sid: sid, reason: "driver_exit_clean")
            } catch {
                logger.error(
                    "Roam driver exited with error: \(String(describing: error), privacy: .public)")
                NSLog("VixenFS: Roam SHM driver exited with error: \(String(describing: error))")
                await self?.handleDriverExit(
                    sid: sid,
                    reason: "driver_exit_error:\(String(describing: error))"
                )
            }
        }

        peers[sid] = PeerConnection(
            client: VfsClient(connection: handle, timeout: 28.0),
            driverTask: driverTask,
            terminalError: nil
        )
        return peers[sid]!.client!
    }

    private func handleDriverExit(sid: String, reason: String) async {
        var peer = peers[sid] ?? PeerConnection(client: nil, driverTask: nil, terminalError: nil)
        peer.client = nil
        peer.driverTask = nil

        if disconnectRequested {
            log.info("VFS connection closed during normal unload sid=\(sid, privacy: .public)")
            NSLog("VixenFS: SHM connection closed during unload")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.normal")
            peers.removeValue(forKey: sid)
        } else {
            peer.terminalError = NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(POSIXError.ECONNRESET.rawValue),
                userInfo: [NSLocalizedDescriptionKey: reason]
            )
            peers[sid] = peer
            log.error(
                "Connection to VFS server lost for sid=\(sid, privacy: .public); waiting for FSKit to unload/deactivate"
            )
            NSLog("VixenFS: SHM connection dropped unexpectedly (driver_exit)")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.unexpected")
        }

        if peers.values.allSatisfy({ $0.client == nil }) {
            await ConnectionHealth.shared.recordDisconnected(reason: reason)
        }

        notifySessionDeath(sid: sid)
    }

    private func notifySessionDeath(sid: String) {
        for observer in sessionDeathObservers.values {
            observer(sid)
        }
    }

    private func requestTicket(sid: String) async throws -> ShmBootstrapTicket {
        guard
            let appGroupURL = FileManager.default.containerURL(
                forSecurityApplicationGroupIdentifier: VixenFSConstants.appGroupIdentifier)
        else {
            throw NSError(
                domain: "VFS", code: -1,
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "Failed to resolve App Group container for \(VixenFSConstants.appGroupIdentifier)"
                ])
        }

        let controlSockURL = appGroupURL.appendingPathComponent("c-\(sid).sock", isDirectory: false)
        let controlSockPath = controlSockURL.path
        debugLog("appGroupPath=\(appGroupURL.path) len=\(appGroupURL.path.count)")
        debugLog("controlSockPath=\(controlSockPath) len=\(controlSockPath.count)")
        logPathState(path: controlSockPath, label: "controlSockPath")

        do {
            return try requestShmBootstrapTicket(controlSocketPath: controlSockPath, sid: sid)
        } catch {
            log.error(
                "SHM bootstrap attempt 1 failed for sid=\(sid, privacy: .public): \(self.describe(error: error), privacy: .public)"
            )
            throw NSError(
                domain: "VFS", code: -1,
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "Failed SHM bootstrap for sid=\(sid): \(self.describe(error: error))"
                ])
        }
    }

    private func describe(error: Error) -> String {
        if let err = error as? ShmRegionError {
            switch err {
            case .invalidSize:
                return "ShmRegionError.invalidSize"
            case .invalidOffset:
                return "ShmRegionError.invalidOffset"
            case .createFailed(let errno):
                return "ShmRegionError.createFailed(errno=\(errno))"
            case .openFailed(let errno):
                return "ShmRegionError.openFailed(errno=\(errno))"
            case .statFailed(let errno):
                return "ShmRegionError.statFailed(errno=\(errno))"
            case .mmapFailed(let errno):
                return "ShmRegionError.mmapFailed(errno=\(errno))"
            case .munmapFailed(let errno):
                return "ShmRegionError.munmapFailed(errno=\(errno))"
            case .ftruncateFailed(let errno):
                return "ShmRegionError.ftruncateFailed(errno=\(errno))"
            case .fchmodFailed(let errno):
                return "ShmRegionError.fchmodFailed(errno=\(errno))"
            case .resizeShrinkUnsupported:
                return "ShmRegionError.resizeShrinkUnsupported"
            case .emptyFile:
                return "ShmRegionError.emptyFile"
            }
        }
        if let err = error as? ShmBootstrapError {
            switch err {
            case .invalidSid:
                return "ShmBootstrapError.invalidSid"
            case .unsupportedPlatform:
                return "ShmBootstrapError.unsupportedPlatform"
            case .invalidSocketPath:
                return "ShmBootstrapError.invalidSocketPath"
            case .socketCreateFailed(let errno):
                return "ShmBootstrapError.socketCreateFailed(errno=\(errno))"
            case .connectFailed(let errno):
                return "ShmBootstrapError.connectFailed(errno=\(errno))"
            case .writeFailed(let errno):
                return "ShmBootstrapError.writeFailed(errno=\(errno))"
            case .readFailed(let errno):
                return "ShmBootstrapError.readFailed(errno=\(errno))"
            case .eof:
                return "ShmBootstrapError.eof"
            case .invalidRequestEncoding:
                return "ShmBootstrapError.invalidRequestEncoding"
            case .invalidResponseMagic:
                return "ShmBootstrapError.invalidResponseMagic"
            case .protocolError(let message):
                return "ShmBootstrapError.protocolError(\(message))"
            case .missingFileDescriptor:
                return "ShmBootstrapError.missingFileDescriptor"
            }
        }
        return String(describing: error)
    }

    private struct MountOptions {
        let sid: String
        let debug: Bool
        let moireDashboard: String?
    }

    private func parseMountOptions(from url: URL) -> MountOptions? {
        guard
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
            let items = components.queryItems,
            let sid = items.first(where: { $0.name == "sid" })?.value,
            isValidSid(sid)
        else {
            return nil
        }
        let debugValue = items.first(where: { $0.name == "debug" })?.value?.lowercased()
        let debug = debugValue == "1" || debugValue == "true" || debugValue == "yes"
        let moireDashboard = items.first(where: { $0.name == "moire_dashboard" })?.value
        return MountOptions(sid: sid, debug: debug, moireDashboard: moireDashboard)
    }

    private func isValidSid(_ sid: String) -> Bool {
        if sid.count == 32 {
            return sid.unicodeScalars.allSatisfy {
                CharacterSet(charactersIn: "0123456789abcdefABCDEF").contains($0)
            }
        }
        if sid.count == 36 {
            let chars = Array(sid)
            let hyphenPositions: Set<Int> = [8, 13, 18, 23]
            for i in 0..<chars.count {
                if hyphenPositions.contains(i) {
                    if chars[i] != "-" {
                        return false
                    }
                    continue
                }
                if !chars[i].isHexDigit {
                    return false
                }
            }
            return true
        }
        return false
    }

    private func debugLog(_ message: String) {
        guard debugEnabled else { return }
        log.notice("debug: \(message, privacy: .public)")
    }

    private func logPathState(path: String, label: String) {
        guard debugEnabled else { return }
        var isDir: ObjCBool = false
        let exists = FileManager.default.fileExists(atPath: path, isDirectory: &isDir)
        if exists {
            let attrs = (try? FileManager.default.attributesOfItem(atPath: path)) ?? [:]
            let fileSize = attrs[.size] as? NSNumber
            debugLog(
                "\(label) exists=1 isDir=\(isDir.boolValue ? 1 : 0) size=\(fileSize?.int64Value ?? -1)"
            )
        } else {
            debugLog("\(label) exists=0")
        }
    }
}

/// Wrapper to make FSKit callbacks Sendable for Swift 6 concurrency.
/// FSKit callbacks are thread-safe in practice.
struct SendableReplyHandler<T>: @unchecked Sendable {
    let handler: (T?, (any Error)?) -> Void
}

struct SendableVoidReplyHandler: @unchecked Sendable {
    let handler: ((any Error)?) -> Void
}

/// The bridge between FSKit and our VFS client.
/// Implements FSUnaryFileSystem which is the top-level FSKit protocol.
final class Bridge: FSUnaryFileSystem, FSUnaryFileSystemOperations, @unchecked Sendable {
    // FSKit manages concurrency - singleton is safe here
    static let shared = Bridge()

    private let log = Logger(subsystem: VixenFSConstants.subsystem, category: "Bridge")

    private override init() {
        log.info("Bridge initialized")
        super.init()
    }

    private static func deterministicContainerUUID(for url: URL) -> UUID {
        var bytes = [UInt8](SHA256.hash(data: Data(url.absoluteString.utf8)).prefix(16))
        bytes[6] = (bytes[6] & 0x0F) | 0x50
        bytes[8] = (bytes[8] & 0x3F) | 0x80
        return UUID(
            uuid: (
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15]
            ))
    }

    /// Called by FSKit to check if we can handle this resource
    func probeResource(
        resource: FSResource,
        replyHandler: @escaping (FSProbeResult?, (any Error)?) -> Void
    ) {
        log.info("probeResource")
        let resourceDescription = describe(resource: resource)

        // Extract server URL from resource
        guard let url = extractVixenURL(from: resource) else {
            log.error("probeResource: not a valid vixen:// URL resource")
            replyHandler(nil, fs_errorForPOSIXError(POSIXError.EINVAL.rawValue))
            return
        }

        let sendable = SendableReplyHandler(handler: replyHandler)
        Task {
            await LifecycleTrace.shared.mark(
                self.log, event: "bridge.probeResource.begin",
                details: resourceDescription)
            do {
                try await VfsConnection.shared.connect(url: url)

                let containerUUID = Bridge.deterministicContainerUUID(for: url)
                let result = FSProbeResult.usable(
                    name: "Vixen",
                    containerID: FSContainerIdentifier(uuid: containerUUID)
                )
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.probeResource.success",
                    details: resourceDescription)
                sendable.handler(result, nil)
            } catch {
                self.log.error(
                    "probeResource FAILED: \(String(describing: error), privacy: .public)")
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.probeResource.failure",
                    details: "error=\(String(describing: error))")
                sendable.handler(
                    nil,
                    vixenMappedFSError(error, operation: #function, logger: self.log)
                )
            }
        }
    }

    /// Called by FSKit to load the filesystem and get a volume
    func loadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler: @escaping (FSVolume?, (any Error)?) -> Void
    ) {
        log.info("loadResource")
        let resourceDescription = describe(resource: resource)

        // Extract server URL and options from resource
        guard let url = extractVixenURL(from: resource) else {
            log.error("loadResource: not a valid vixen:// URL resource")
            replyHandler(nil, fs_errorForPOSIXError(POSIXError.EINVAL.rawValue))
            return
        }
        let sendable = SendableReplyHandler(handler: replyHandler)
        Task {
            await LifecycleTrace.shared.mark(
                self.log, event: "bridge.loadResource.begin",
                details: resourceDescription)
            do {
                try await VfsConnection.shared.connect(url: url)

                let volume = Volume()
                self.containerStatus = .ready
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.loadResource.success",
                    details: resourceDescription)
                sendable.handler(volume, nil)
            } catch {
                self.log.error(
                    "loadResource FAILED: \(String(describing: error), privacy: .public)")
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.loadResource.failure",
                    details: "error=\(String(describing: error))")
                sendable.handler(
                    nil,
                    vixenMappedFSError(error, operation: #function, logger: self.log)
                )
            }
        }
    }

    /// Called when FSKit wants to unload the resource
    func unloadResource(
        resource: FSResource,
        options: FSTaskOptions,
        replyHandler reply: @escaping ((any Error)?) -> Void
    ) {
        log.info("unloadResource")
        let resourceDescription = describe(resource: resource)
        let sendable = SendableVoidReplyHandler(handler: reply)
        Task {
            await LifecycleTrace.shared.mark(
                self.log, event: "bridge.unloadResource.begin",
                details: resourceDescription)
            await VfsConnection.shared.disconnect()
            await LifecycleTrace.shared.mark(
                self.log, event: "bridge.unloadResource.done",
                details: resourceDescription)
            sendable.handler(nil)
        }
    }

    /// Called after loading completes
    func didFinishLoading() {
        log.info("didFinishLoading")
        Task {
            await LifecycleTrace.shared.mark(self.log, event: "bridge.didFinishLoading")
        }
    }

    // MARK: - URL Resource Handling

    /// Extract the vixen:// URL from an FSGenericURLResource.
    /// Expected URL format: vixen://session?sid=<sid>
    private func extractVixenURL(from resource: FSResource) -> URL? {
        guard let urlResource = resource as? FSGenericURLResource else {
            return nil
        }

        let url = urlResource.url

        guard url.scheme == "vixen" else {
            return nil
        }

        return url
    }

    private func describe(resource: FSResource) -> String {
        if let urlResource = resource as? FSGenericURLResource {
            return "url=\(urlResource.url.absoluteString)"
        }
        return "resourceType=\(String(describing: type(of: resource)))"
    }
}
