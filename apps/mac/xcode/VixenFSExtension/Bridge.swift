import FSKit
import Foundation
import RoamRuntime
import os

enum VixenFS {
    static let subsystem = "eu.bearcove.vixenfs"
    static let maxPayloadSize: UInt32 = 256 * 1024 * 1024  // 256 MiB
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
    func preregister(methodId: UInt64, payload: [UInt8], registry: ChannelRegistry) async {}

    func dispatch(
        methodId: UInt64,
        payload: [UInt8],
        requestId: UInt64,
        registry: ChannelRegistry,
        taskTx: @escaping @Sendable (TaskMessage) -> Void
    ) async {
        taskTx(.response(requestId: requestId, payload: encodeUnknownMethodError()))
    }
}

/// Global VFS client shared across the extension
actor VfsConnection {
    static let shared = VfsConnection()

    private var client: VfsClient?
    private var driverTask: Task<Void, Never>?
    private var disconnectRequested = false
    private let log = Logger(subsystem: VixenFS.subsystem, category: "VfsConnection")

    func connect(url: URL) async throws {
        if client != nil {
            log.info("Already connected")
            return
        }
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.begin", details: "url=\(url.absoluteString)")

        guard let host = url.host else {
            throw NSError(
                domain: "VFS", code: -1,
                userInfo: [NSLocalizedDescriptionKey: "Missing host in URL: \(url)"])
        }

        let port = url.port ?? 10001
        log.info("Connecting to \(host):\(port)")

        let maxPayloadSize = VixenFS.maxPayloadSize

        let transport = try await RoamRuntime.connect(host: host, port: port)
        let (handle, driver) = try await establishInitiator(
            transport: transport,
            dispatcher: NoopDispatcher(),
            maxPayloadSize: maxPayloadSize
        )

        disconnectRequested = false
        let logger = log
        driverTask = Task { [weak self] in
            do {
                try await driver.run()
                logger.error("Roam driver exited cleanly — server is gone")
            } catch {
                logger.error("Roam driver exited with error: \(error.localizedDescription)")
            }
            await self?.handleDriverExit()
        }
        client = VfsClient(connection: handle, timeout: 5.0)
        log.info("Connected to \(url.absoluteString)")
        await LifecycleTrace.shared.mark(
            log, event: "vfs.connect.ready", details: "url=\(url.absoluteString)")
    }

    func disconnect() async {
        await LifecycleTrace.shared.mark(log, event: "vfs.disconnect")
        disconnectRequested = true
        driverTask?.cancel()
        driverTask = nil
        client = nil
    }

    func getClient() throws -> VfsClient {
        // TODO: can't we make bad states unrepresentable instead of.. this?
        guard let client = client else {
            throw NSError(
                domain: "VFS", code: -1, userInfo: [NSLocalizedDescriptionKey: "Not connected"])
        }
        return client
    }

    private func handleDriverExit() async {
        if disconnectRequested {
            log.info("VFS connection closed during normal unload")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.normal")
        } else {
            log.error("Connection to VFS server lost; waiting for FSKit to unload/deactivate")
            await LifecycleTrace.shared.mark(log, event: "vfs.driver.exit.unexpected")
        }
        client = nil
        driverTask = nil
    }
}

/// Wrapper to make FSKit callbacks Sendable for Swift 6 concurrency.
/// FSKit callbacks are thread-safe in practice.
/// TODO: they are CLEARLY not and that's why we've had crashes lol
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
    // TODO: is it???
    static let shared = Bridge()

    private let log = Logger(subsystem: VixenFS.subsystem, category: "Bridge")

    private override init() {
        log.info("Bridge initialized")
        super.init()
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

                let containerUUID = UUID()
                let result = FSProbeResult.usable(
                    name: "Vixen",
                    containerID: FSContainerIdentifier(uuid: containerUUID)
                )
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.probeResource.success",
                    details: resourceDescription)
                sendable.handler(result, nil)
            } catch {
                self.log.error("probeResource FAILED: \(error.localizedDescription)")
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.probeResource.failure",
                    details: "error=\(error.localizedDescription)")
                sendable.handler(nil, error)
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
                self.log.error("loadResource FAILED: \(error.localizedDescription)")
                await LifecycleTrace.shared.mark(
                    self.log, event: "bridge.loadResource.failure",
                    details: "error=\(error.localizedDescription)")
                sendable.handler(nil, fs_errorForPOSIXError(POSIXError.EIO.rawValue))
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
    /// Expected URL format: vixen://host:port or vixen://host (defaults to port 10001)
    private func extractVixenURL(from resource: FSResource) -> URL? {
        guard let urlResource = resource as? FSGenericURLResource else {
            return nil
        }

        let url = urlResource.url

        guard url.scheme == "vixen", url.host != nil else {
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
