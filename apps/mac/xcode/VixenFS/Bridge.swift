import CryptoKit
import FSKit
import Foundation
import VoxRuntime
import os

enum VixenFSConstants {
    static let subsystem = "eu.bearcove.vixenfs"
    static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"
    static let vfsSocketName = "vixenfs.sock"
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

    if let err = error as? VoxError {
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
        case .indeterminate:
            return POSIXError.EIO.rawValue
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
    func preregister(methodId: UInt64, payload: [UInt8], registry: ChannelRegistry) async {
        _ = methodId
        _ = payload
        _ = registry
    }

    func dispatch(
        methodId: UInt64,
        payload: [UInt8],
        requestId: UInt64,
        registry: ChannelRegistry,
        schemaSendTracker: SchemaSendTracker,
        taskTx: @escaping @Sendable (TaskMessage) -> Void
    ) async {
        _ = methodId
        _ = payload
        _ = registry
        _ = schemaSendTracker
        taskTx(.response(requestId: requestId, payload: encodeUnknownMethodError()))
    }
}

extension VfsClient: ExpectedRootClient {
    public static let voxServiceName = "Vfs"
}

struct ConnectionHealthSnapshot: Codable {
    let connected: Bool
    let everConnected: Bool
    let appGroupPath: String?
    let connectedAt: String?
    let disconnectedAt: String?
    let uptimeSeconds: Double?
    let disconnectReason: String?

    enum CodingKeys: String, CodingKey {
        case connected
        case everConnected = "ever_connected"
        case appGroupPath = "app_group_path"
        case connectedAt = "connected_at"
        case disconnectedAt = "disconnected_at"
        case uptimeSeconds = "uptime_seconds"
        case disconnectReason = "disconnect_reason"
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

    func snapshot() -> ConnectionHealthSnapshot {
        let connectedAtString = connectedAt.map(Self.iso8601String)
        let disconnectedAtString = disconnectedAt.map(Self.iso8601String)

        let uptimeSeconds: Double?
        if currentlyConnected, let start = connectedAt {
            uptimeSeconds = Date().timeIntervalSince(start)
        } else if let start = connectedAt, let end = disconnectedAt {
            uptimeSeconds = end.timeIntervalSince(start)
        } else {
            uptimeSeconds = nil
        }

        return ConnectionHealthSnapshot(
            connected: currentlyConnected,
            everConnected: everConnected,
            appGroupPath: resolveAppGroupPath(),
            connectedAt: connectedAtString,
            disconnectedAt: disconnectedAtString,
            uptimeSeconds: uptimeSeconds,
            disconnectReason: disconnectReason
        )
    }

    func toJSON() -> Data {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            var data = try encoder.encode(snapshot())
            data.append(0x0A)
            return data
        } catch {
            return Data("{}\n".utf8)
        }
    }

    private static func iso8601String(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
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

    private struct RootConnection {
        var client: VfsClient?
        var session: Session?
        var driverTask: Task<Void, Never>?
        var terminalError: NSError?
    }

    private var root = RootConnection(client: nil, session: nil, driverTask: nil, terminalError: nil)
    private var disconnectRequested = false
    private var debugEnabled = false
    private var listener: UnixListener?
    private var listenerTask: Task<Void, Never>?
    private let log = Logger(subsystem: VixenFSConstants.subsystem, category: "VfsConnection")
    private var disconnectObservers: [UUID: @Sendable () -> Void] = [:]

    func connect(url: URL) async throws {
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.begin", details: "url=\(url.absoluteString)")

        guard let mountOptions = parseMountOptions(from: url) else {
            throw POSIXError(.EINVAL)
        }

        debugEnabled = mountOptions.debug
        disconnectRequested = false

        await SwiftmoireClient.shared.configure(
            dashboardAddr: mountOptions.moireDashboard,
            debugFrames: mountOptions.debug
        )

        if listener == nil {
            try await startListener()
        }

        if debugEnabled {
            log.notice("mount debug enabled")
        }

        log.info("Ready to accept VFS peers on \(url.absoluteString)")
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.ready", details: "url=\(url.absoluteString)")
    }

    func disconnect() async {
        await LifecycleTrace.shared.mark(log, event: "vfs.disconnect")
        disconnectRequested = true
        debugEnabled = false
        await SwiftmoireClient.shared.configure(dashboardAddr: nil, debugFrames: false)

        root.session?.handle.shutdown()
        root.driverTask?.cancel()
        root = RootConnection(client: nil, session: nil, driverTask: nil, terminalError: nil)
        notifyDisconnect()
        listenerTask?.cancel()
        listenerTask = nil
        if let listener {
            try? await listener.close()
        }
        self.listener = nil
        await ConnectionHealth.shared.recordDisconnected(reason: "requested")
    }

    func getClient() async throws -> VfsClient {
        if let client = root.client {
            return client
        }

        if let terminalError = root.terminalError {
            log.notice(
                "Refusing reconnect for terminal root peer: \(terminalError.localizedDescription, privacy: .public)"
            )
            throw terminalError
        }

        log.notice("No live root VFS peer")
        throw POSIXError(.ENOTCONN)
    }

    func observeDisconnect(id: UUID, _ observer: @escaping @Sendable () -> Void) {
        disconnectObservers[id] = observer
    }

    func removeDisconnectObserver(_ id: UUID) {
        disconnectObservers.removeValue(forKey: id)
    }

    private func handleDriverExit(reason: String) async {
        if disconnectRequested {
            log.info("VFS root peer connection closed during normal unload")
            NSLog("VixenFS: accepted Vox connection closed during unload")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.normal")
            root = RootConnection(client: nil, session: nil, driverTask: nil, terminalError: nil)
        } else {
            root = RootConnection(
                client: nil,
                session: nil,
                driverTask: nil,
                terminalError: NSError(
                domain: NSPOSIXErrorDomain,
                code: Int(POSIXError.ECONNRESET.rawValue),
                userInfo: [NSLocalizedDescriptionKey: reason]
            )
            )
            log.error(
                "Connection to root VFS peer lost; waiting for FSKit to unload/deactivate"
            )
            NSLog("VixenFS: accepted Vox connection dropped unexpectedly (driver_exit)")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.unexpected")
        }

        if root.client == nil {
            await ConnectionHealth.shared.recordDisconnected(reason: reason)
        }

        notifyDisconnect()
    }

    private func notifyDisconnect() {
        for observer in disconnectObservers.values {
            observer()
        }
    }

    private func startListener() async throws {
        let socketPath = try listenerSocketPath()
        debugLog("binding VFS listener path=\(socketPath)")
        let listener = try await UnixListener.bind(unixPath: socketPath)
        self.listener = listener
        self.listenerTask = Task { [weak self] in
            for await link in listener.acceptConnections() {
                guard let self else { break }
                self.log.notice("listener accepted raw VFS peer")
                await self.acceptIncoming(link: link)
            }
            self?.log.notice("listener accept loop ended")
        }
        log.notice("Listening for VFS peers on \(socketPath, privacy: .public)")
    }

    private func acceptIncoming(link: NIOFrameLink) async {
        do {
            log.notice("beginning Session.acceptFreshLink for raw VFS peer")
            let session = try await Session.acceptFreshLink(
                link,
                conduit: .bare,
                expecting: VfsClient.self,
                dispatcher: NoopDispatcher(),
                resumable: false
            )
            log.notice("Session.acceptFreshLink completed for VFS peer")

            let logger = log
            let driverTask = Task { [weak self] in
                do {
                    try await session.run()
                    logger.error("Accepted root Vox driver exited cleanly")
                    await self?.handleDriverExit(reason: "driver_exit_clean")
                } catch {
                    logger.error(
                        "Accepted root Vox driver exited with error: \(String(describing: error), privacy: .public)"
                    )
                    await self?.handleDriverExit(reason: "driver_exit_error:\(String(describing: error))")
                }
            }

            if let existing = root.session {
                existing.handle.shutdown()
                root.driverTask?.cancel()
            }

            root = RootConnection(
                client: VfsClient(connection: session.connection, timeout: 28.0),
                session: session,
                driverTask: driverTask,
                terminalError: nil
            )
            await ConnectionHealth.shared.recordConnected()
            log.notice("Accepted root VFS peer")
        } catch {
            log.error(
                "failed to accept VFS peer: \(String(describing: error), privacy: .public)"
            )
        }
    }

    private func listenerSocketPath() throws -> String {
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

        let socketURL = appGroupURL.appendingPathComponent(
            VixenFSConstants.vfsSocketName,
            isDirectory: false
        )
        let socketPath = socketURL.path
        debugLog("appGroupPath=\(appGroupURL.path) len=\(appGroupURL.path.count)")
        debugLog("socketPath=\(socketPath) len=\(socketPath.count)")
        logPathState(path: socketPath, label: "socketPath")
        return socketPath
    }
    private struct MountOptions {
        let debug: Bool
        let moireDashboard: String?
    }

    private func parseMountOptions(from url: URL) -> MountOptions? {
        guard
            let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
            components.scheme == "vixen",
            components.host == "session"
        else {
            return nil
        }
        let items = components.queryItems ?? []

        // Hard cut: sid=... bootstrap URLs are no longer accepted.
        if items.contains(where: { $0.name == "sid" }) {
            return nil
        }

        let debugValue = items.first(where: { $0.name == "debug" })?.value?.lowercased()
        let debug = debugValue == "1" || debugValue == "true" || debugValue == "yes"
        let moireDashboard = items.first(where: { $0.name == "moire_dashboard" })?.value
        return MountOptions(debug: debug, moireDashboard: moireDashboard)
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
    /// Expected URL format: vixen://session[?debug=1][&moire_dashboard=...]
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
