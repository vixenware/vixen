import FSKit
import Foundation
import os

actor ReadTelemetry {
    private var readCount: UInt64 = 0
    private var errorCount: UInt64 = 0
    private var totalRequestedBytes: UInt64 = 0
    private var totalBytes: UInt64 = 0
    private var totalLatencyNs: UInt64 = 0
    private var minRequestedBytes: UInt64 = .max
    private var maxRequestedBytes: UInt64 = 0
    private var minBytes: UInt64 = .max
    private var maxBytes: UInt64 = 0
    private var minLatencyNs: UInt64 = .max
    private var maxLatencyNs: UInt64 = 0

    func record(requestedBytes: Int, returnedBytes: Int, latencyNs: UInt64, isError: Bool) {
        readCount += 1
        if isError {
            errorCount += 1
        }

        let requested = UInt64(max(0, requestedBytes))
        totalRequestedBytes += requested
        minRequestedBytes = min(minRequestedBytes, requested)
        maxRequestedBytes = max(maxRequestedBytes, requested)

        let returned = UInt64(max(0, returnedBytes))
        totalBytes += returned
        minBytes = min(minBytes, returned)
        maxBytes = max(maxBytes, returned)

        totalLatencyNs += latencyNs
        minLatencyNs = min(minLatencyNs, latencyNs)
        maxLatencyNs = max(maxLatencyNs, latencyNs)
    }

    func logSummary(_ logger: Logger, reason: String) {
        guard readCount > 0 else {
            logger.notice("read_stats reason=\(reason, privacy: .public) reads=0")
            return
        }

        let avgBytes = Double(totalBytes) / Double(readCount)
        let avgRequestedBytes = Double(totalRequestedBytes) / Double(readCount)
        let avgLatencyNs = Double(totalLatencyNs) / Double(readCount)
        logger.notice(
            """
            read_stats reason=\(reason, privacy: .public) \
            reads=\(self.readCount) \
            errors=\(self.errorCount) \
            requested_bytes_min=\(self.minRequestedBytes) \
            requested_bytes_max=\(self.maxRequestedBytes) \
            requested_bytes_avg=\(avgRequestedBytes) \
            bytes_min=\(self.minBytes) \
            bytes_max=\(self.maxBytes) \
            bytes_avg=\(avgBytes) \
            latency_ns_min=\(self.minLatencyNs) \
            latency_ns_max=\(self.maxLatencyNs) \
            latency_ns_avg=\(avgLatencyNs)
            """
        )
    }

    func toJSON() -> Data {
        var json = "{\n"
        json += "  \"read_count\": \(readCount),\n"
        json += "  \"error_count\": \(errorCount),\n"
        json += "  \"requested_bytes\": {\n"
        json += "    \"total\": \(totalRequestedBytes),\n"
        if readCount > 0 {
            json += "    \"min\": \(minRequestedBytes),\n"
            json += "    \"max\": \(maxRequestedBytes),\n"
            json += "    \"avg\": \(Double(totalRequestedBytes) / Double(readCount))\n"
        } else {
            json += "    \"min\": 0,\n"
            json += "    \"max\": 0,\n"
            json += "    \"avg\": 0\n"
        }
        json += "  },\n"
        json += "  \"returned_bytes\": {\n"
        json += "    \"total\": \(totalBytes),\n"
        if readCount > 0 {
            json += "    \"min\": \(minBytes),\n"
            json += "    \"max\": \(maxBytes),\n"
            json += "    \"avg\": \(Double(totalBytes) / Double(readCount))\n"
        } else {
            json += "    \"min\": 0,\n"
            json += "    \"max\": 0,\n"
            json += "    \"avg\": 0\n"
        }
        json += "  },\n"
        json += "  \"latency_ns\": {\n"
        json += "    \"total\": \(totalLatencyNs),\n"
        if readCount > 0 {
            json += "    \"min\": \(minLatencyNs),\n"
            json += "    \"max\": \(maxLatencyNs),\n"
            json += "    \"avg\": \(Double(totalLatencyNs) / Double(readCount))\n"
        } else {
            json += "    \"min\": 0,\n"
            json += "    \"max\": 0,\n"
            json += "    \"avg\": 0\n"
        }
        json += "  }\n"
        json += "}\n"
        return Data(json.utf8)
    }

    func reset() {
        readCount = 0
        errorCount = 0
        totalRequestedBytes = 0
        totalBytes = 0
        totalLatencyNs = 0
        minRequestedBytes = .max
        maxRequestedBytes = 0
        minBytes = .max
        maxBytes = 0
        minLatencyNs = .max
        maxLatencyNs = 0
    }
}

struct RoutedPrefix {
    let prefix: String
    let sid: String
}

struct RoutedRouteMap {
    static func build(from routes: [RoutedPrefix]) -> [String: String] {
        var map: [String: String] = [:]
        for route in routes {
            // Deterministic winner when multiple sessions publish the same prefix.
            if map[route.prefix] == nil {
                map[route.prefix] = route.sid
            }
        }
        return map
    }

    static func removingSid(_ sid: String, from map: [String: String]) -> [String: String] {
        map.filter { _, ownerSID in ownerSID != sid }
    }
}

/// FSKit Volume implementation that delegates to the VFS client.
final class Volume: FSVolume, @unchecked Sendable {

    private let log = Logger(subsystem: VixenFSConstants.subsystem, category: "Volume")
    private let readTelemetry = ReadTelemetry()

    /// Item cache: itemId -> Item, protected by lock for thread safety.
    /// Volume is a class (not an actor) because it subclasses FSVolume, so we need
    /// explicit synchronization. FSKit may call operations concurrently.
    /// We use NSLock because Item doesn't conform to Sendable (ObjC base class).
    private let _itemsLock = NSLock()
    private var _items: [UInt64: Item] = [:]

    /// Stable synthetic IDs for dynamically routed session prefixes.
    private let _prefixIDLock = NSLock()
    private var _prefixIDs: [String: UInt64] = [:]
    private var _nextPrefixID: UInt64 = 0xFFFF_FFFF_FFFD_0000

    /// Stable synthetic IDs for routed backend items keyed by SID + backend item ID.
    private let _routedItemIDLock = NSLock()
    private var _routedItemIDs: [String: UInt64] = [:]
    private var _nextRoutedItemID: UInt64 = 0xFFFF_FFFF_FFFC_0000

    /// Root item ID (always 1 in our VFS protocol)
    private let rootId: UInt64 = 1
    private static let mutableAttributesTTLNs: UInt64 = 2_000_000_000

    // Virtual items served entirely from the Swift side (never hit vx-vfsd).
    // IDs chosen high to avoid collision with vx-vfsd's allocator (starts at 2).
    private static let vixenfsDirId: UInt64 = 0xFFFF_FFFF_FFFE_0001
    private static let statsJsonId: UInt64 = 0xFFFF_FFFF_FFFE_0002
    private static let healthJsonId: UInt64 = 0xFFFF_FFFF_FFFE_0003
    private static let vixenfsDirName = ".vixenfs"
    private static let statsJsonName = "stats.json"
    private static let healthJsonName = "health.json"
    private static let volumeStatsLogIntervalNs: UInt64 = 2_000_000_000
    private static let volumeStatsLogLock = NSLock()
    nonisolated(unsafe) private static var volumeStatsLastLogNs: UInt64 = 0
    nonisolated(unsafe) private static var volumeStatsSuppressed: UInt64 = 0

    private func isVirtualItem(_ itemId: UInt64) -> Bool {
        itemId == Volume.vixenfsDirId || itemId == Volume.statsJsonId
            || itemId == Volume.healthJsonId
    }

    private func vixenfsDirAttributes() -> FSItem.Attributes {
        FSItem.Attributes.fromVfs(
            itemId: Volume.vixenfsDirId,
            parentId: rootId,
            itemType: .directory,
            size: 0,
            modifiedTime: 0,
            createdTime: 0,
            mode: 0o755
        )
    }

    private func statsJsonAttributes() async -> FSItem.Attributes {
        let data = await readTelemetry.toJSON()
        return FSItem.Attributes.fromVfs(
            itemId: Volume.statsJsonId,
            parentId: Volume.vixenfsDirId,
            itemType: .file,
            size: UInt64(data.count),
            modifiedTime: 0,
            createdTime: 0,
            mode: 0o444
        )
    }

    private func healthJsonAttributes() async -> FSItem.Attributes {
        let data = await ConnectionHealth.shared.toJSON()
        return FSItem.Attributes.fromVfs(
            itemId: Volume.healthJsonId,
            parentId: Volume.vixenfsDirId,
            itemType: .file,
            size: UInt64(data.count),
            modifiedTime: 0,
            createdTime: 0,
            mode: 0o444
        )
    }

    private let sessionDeathObserverID = UUID()

    init() {
        let volumeId = FSVolume.Identifier(uuid: UUID())
        let volumeName = FSFileName(string: "Vixen")
        log.notice("Volume init")
        super.init(volumeID: volumeId, volumeName: volumeName)

        let observerID = sessionDeathObserverID
        let owner = self
        Task {
            await VfsConnection.shared.observeSessionDeath(id: observerID) { sid in
                owner.purgeItems(forSid: sid)
            }
        }
    }

    deinit {
        let observerID = sessionDeathObserverID
        Task {
            await VfsConnection.shared.removeSessionDeathObserver(observerID)
        }
    }

    // MARK: - Item Cache

    private func cacheItem(_ item: Item) {
        _itemsLock.lock()
        defer { _itemsLock.unlock() }
        _items[item.itemId] = item
    }

    private func getCachedItem(_ itemId: UInt64) -> Item? {
        _itemsLock.lock()
        defer { _itemsLock.unlock() }
        return _items[itemId]
    }

    private func removeCachedItem(_ itemId: UInt64) {
        _itemsLock.lock()
        defer { _itemsLock.unlock() }
        _items.removeValue(forKey: itemId)
    }

    private func removeAllCachedItems() {
        _itemsLock.lock()
        defer { _itemsLock.unlock() }
        _items.removeAll()
    }

    private func getItem(_ fsItem: FSItem?, fn: StaticString = #function) throws -> Item {
        guard let fsItem else {
            log.error("\(fn): fsItem is nil (likely stale FSKit object during teardown)")
            throw fs_errorForPOSIXError(POSIXError.ESTALE.rawValue)
        }
        guard let item = fsItem as? Item else {
            log.error("\(fn): unexpected FSItem instance; expected Volume.Item")
            throw fs_errorForPOSIXError(POSIXError.EINVAL.rawValue)
        }
        return item
    }

    // MARK: - VFS Client Access

    private func withMappedClientCall<T>(
        operation: StaticString = #function,
        sid: String? = nil,
        _ body: (VfsClient) async throws -> T
    ) async throws -> T {
        let operationName = String(describing: operation)
        let requestID = await SwiftmoireClient.shared.beginRequest(operation: operationName)
        let spanID = moireRequestContext.spanID(fromRequestID: requestID)
        return try await moireRequestContext.$spanID.withValue(spanID) {
            let client: VfsClient
            do {
                client = try await VfsConnection.shared.getClient(for: sid)
            } catch {
                let errno = vixenPOSIXCode(for: error)
                if errno == POSIXError.ETIMEDOUT.rawValue {
                    let span = spanID ?? "none"
                    log.error(
                        "\(operation): timeout request_id=\(requestID, privacy: .public) span_id=\(span, privacy: .public) phase=getClient"
                    )
                }
                await SwiftmoireClient.shared.finishRequest(
                    requestID: requestID, operation: operationName, errno: errno)
                throw vixenMappedFSError(error, operation: operation, logger: log)
            }
            do {
                let value = try await body(client)
                await SwiftmoireClient.shared.finishRequest(
                    requestID: requestID, operation: operationName, errno: nil)
                return value
            } catch {
                let errno = vixenPOSIXCode(for: error)
                if errno == POSIXError.ETIMEDOUT.rawValue {
                    let span = spanID ?? "none"
                    log.error(
                        "\(operation): timeout request_id=\(requestID, privacy: .public) span_id=\(span, privacy: .public) phase=rpc"
                    )
                }
                await SwiftmoireClient.shared.finishRequest(
                    requestID: requestID, operation: operationName, errno: errno)
                throw vixenMappedFSError(error, operation: operation, logger: log)
            }
        }
    }

    private func failPOSIX(
        _ code: Int32,
        operation: StaticString = #function,
        details: String = ""
    ) throws -> Never {
        let message = String(cString: strerror(code))
        if details.isEmpty {
            log.error("\(operation): failing with errno=\(code) (\(message, privacy: .public))")
        } else {
            log.error(
                "\(operation): failing with errno=\(code) (\(message, privacy: .public)) \(details, privacy: .public)"
            )
        }
        throw fs_errorForPOSIXError(code)
    }

    private func logOpStart(_ op: StaticString, details: String = "") {
        let opName = String(describing: op)
        if details.isEmpty {
            log.notice("op.start \(opName, privacy: .public)")
        } else {
            log.notice("op.start \(opName, privacy: .public) \(details, privacy: .public)")
        }
    }

    private func logOpSuccess(_ op: StaticString, details: String = "") {
        let opName = String(describing: op)
        if details.isEmpty {
            log.notice("op.success \(opName, privacy: .public)")
        } else {
            log.notice("op.success \(opName, privacy: .public) \(details, privacy: .public)")
        }
    }

    private func logOpFailure(_ op: StaticString, error: Error, details: String = "") {
        let opName = String(describing: op)
        let errno = vixenPOSIXCode(for: error)
        if details.isEmpty {
            log.error(
                "op.failure \(opName, privacy: .public) errno=\(errno) error=\(String(describing: error), privacy: .public)"
            )
        } else {
            log.error(
                "op.failure \(opName, privacy: .public) errno=\(errno) \(details, privacy: .public) error=\(String(describing: error), privacy: .public)"
            )
        }
    }

    private func ensureConnectionAvailable(
        operation: StaticString = #function,
        sid: String? = nil
    ) async throws {
        do {
            _ = try await VfsConnection.shared.getClient(for: sid)
        } catch {
            let code = vixenPOSIXCode(for: error)
            log.error(
                "\(operation): connection unavailable errno=\(code) error=\(String(describing: error), privacy: .public)"
            )
            throw fs_errorForPOSIXError(code)
        }
    }

    private func sanitizePrefix(_ prefix: String) -> String? {
        let trimmed = prefix.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        guard !trimmed.contains("/") else { return nil }
        guard trimmed != "." && trimmed != ".." else { return nil }
        return trimmed
    }

    private func discoveredRoutedPrefixes() async -> [RoutedPrefix] {
        let sids = await VfsConnection.shared.candidateSessionIDs()
        var out: [RoutedPrefix] = []

        for sid in sids {
            do {
                let result = try await withMappedClientCall(sid: sid) {
                    try await $0.readDir(itemId: rootId, cookie: 0, verifier: 0)
                }
                guard result.error == 0 else {
                    continue
                }

                for entry in result.entries {
                    guard entry.itemType == .directory else {
                        continue
                    }
                    guard entry.name != Volume.vixenfsDirName else {
                        continue
                    }
                    guard let sanitized = sanitizePrefix(entry.name) else {
                        continue
                    }
                    out.append(RoutedPrefix(prefix: sanitized, sid: sid))
                }
            } catch {
                // If this SID cannot be queried, treat it as having no visible routed prefixes.
                continue
            }
        }

        out.sort { lhs, rhs in
            if lhs.prefix == rhs.prefix {
                return lhs.sid < rhs.sid
            }
            return lhs.prefix < rhs.prefix
        }
        return out
    }

    private func currentRouteMap() async -> [String: String] {
        RoutedRouteMap.build(from: await discoveredRoutedPrefixes())
    }

    private func sidForPrefix(_ prefix: String) async -> String? {
        await currentRouteMap()[prefix]
    }

    private func routedPrefixItemID(prefix: String, sid: String) -> UInt64 {
        let key = "\(sid):\(prefix)"
        _prefixIDLock.lock()
        defer { _prefixIDLock.unlock() }
        if let existing = _prefixIDs[key] {
            return existing
        }
        _nextPrefixID &+= 1
        let allocated = _nextPrefixID
        _prefixIDs[key] = allocated
        return allocated
    }

    private func routedBackendItemID(sid: String, backendItemID: UInt64) -> UInt64 {
        let key = "\(sid):\(backendItemID)"
        _routedItemIDLock.lock()
        defer { _routedItemIDLock.unlock() }
        if let existing = _routedItemIDs[key] {
            return existing
        }
        _nextRoutedItemID &+= 1
        let allocated = _nextRoutedItemID
        _routedItemIDs[key] = allocated
        return allocated
    }

    private func localItemID(for sid: String?, backendItemID: UInt64) -> UInt64 {
        guard let sid else {
            return backendItemID
        }
        return routedBackendItemID(sid: sid, backendItemID: backendItemID)
    }

    private func routedPrefixAttributes(itemID: UInt64) -> FSItem.Attributes {
        FSItem.Attributes.fromVfs(
            itemId: itemID,
            parentId: rootId,
            itemType: .directory,
            size: 0,
            modifiedTime: 0,
            createdTime: 0,
            mode: 0o755
        )
    }

    private func purgeItems(forSid sid: String) {
        _itemsLock.lock()
        _items = _items.filter { _, item in
            guard let itemSID = item.sid else { return true }
            return itemSID != sid
        }
        _itemsLock.unlock()

        _prefixIDLock.lock()
        _prefixIDs = _prefixIDs.filter { key, _ in
            !key.hasPrefix("\(sid):")
        }
        _prefixIDLock.unlock()

        _routedItemIDLock.lock()
        _routedItemIDs = _routedItemIDs.filter { key, _ in
            !key.hasPrefix("\(sid):")
        }
        _routedItemIDLock.unlock()
    }

    private static func isImmutableFile(_ attrs: FSItem.Attributes) -> Bool {
        guard attrs.type == .file else {
            return false
        }
        return (attrs.mode & UInt32(0o222)) == 0
    }
}

// MARK: - FSVolume.Operations

extension Volume: FSVolume.Operations {
    /// Keep block size page-aligned, but advertise a larger preferred I/O size
    /// so FSKit can batch reads for CAS-backed files.
    private static let reportedBlockSize: Int = 4096
    private static let reportedPreferredIOSize: Int = 262_144  // 256 KiB

    var supportedVolumeCapabilities: FSVolume.SupportedCapabilities {
        let caps = FSVolume.SupportedCapabilities()
        caps.supportsSymbolicLinks = false  // TODO: enable when implemented
        caps.supportsHardLinks = false
        caps.supportsPersistentObjectIDs = true
        caps.supports64BitObjectIDs = true
        caps.caseFormat = .sensitive
        return caps
    }

    var volumeStatistics: FSStatFSResult {
        let nowNs = DispatchTime.now().uptimeNanoseconds
        var shouldLog = false
        var suppressed = UInt64(0)
        Volume.volumeStatsLogLock.lock()
        if nowNs &- Volume.volumeStatsLastLogNs >= Volume.volumeStatsLogIntervalNs {
            shouldLog = true
            suppressed = Volume.volumeStatsSuppressed
            Volume.volumeStatsSuppressed = 0
            Volume.volumeStatsLastLogNs = nowNs
        } else {
            Volume.volumeStatsSuppressed &+= 1
        }
        Volume.volumeStatsLogLock.unlock()
        if shouldLog {
            log.debug("volumeStatistics suppressed_since_last=\(suppressed)")
        }
        // TODO: query the VFS backend for real stats. These are placeholder values
        // so that tools like `df` don't error out. The numbers are meaningless.
        let stats = FSStatFSResult(fileSystemTypeName: "vixenfs")
        stats.blockSize = Volume.reportedBlockSize
        stats.ioSize = Volume.reportedPreferredIOSize
        stats.totalBlocks = 1_000_000
        stats.freeBlocks = 500_000
        stats.availableBlocks = 500_000
        stats.totalFiles = 100_000
        stats.freeFiles = 50_000
        return stats
    }

    func mount(options: FSTaskOptions) async throws {
        log.debug("mount")
        await LifecycleTrace.shared.mark(log, event: "volume.mount.begin")
        // Connection is already established in Bridge.loadResource
        await LifecycleTrace.shared.mark(log, event: "volume.mount.done")
    }

    func unmount() async {
        log.debug("unmount")
        await LifecycleTrace.shared.mark(log, event: "volume.unmount.begin")
        await readTelemetry.logSummary(log, reason: "unmount")
        await readTelemetry.reset()
        removeAllCachedItems()
        await LifecycleTrace.shared.mark(log, event: "volume.unmount.done")
    }

    func synchronize(flags: FSSyncFlags) async throws {
        log.debug("synchronize")
        // Our VFS is already synchronized (memory-based for now)
    }

    func activate(options: FSTaskOptions) async throws -> FSItem {
        log.debug("activate - returning root item")
        await LifecycleTrace.shared.mark(log, event: "volume.activate.begin")

        let rootId = self.rootId
        let result = try await withMappedClientCall {
            try await $0.getAttributes(itemId: rootId)
        }

        if result.error != 0 {
            try failPOSIX(result.error, details: "itemId=\(rootId)")
        }

        let rootAttrs = FSItem.Attributes.fromVfs(
            itemId: result.attrs.itemId,
            parentId: rootId,
            itemType: result.attrs.itemType,
            size: result.attrs.size,
            modifiedTime: result.attrs.modifiedTime,
            createdTime: result.attrs.createdTime,
            mode: result.attrs.mode
        )

        let root = Item(itemId: rootId, name: "", attributes: rootAttrs)
        cacheItem(root)
        await LifecycleTrace.shared.mark(
            log, event: "volume.activate.done", details: "rootId=\(rootId)")
        return root
    }

    func deactivate(options: FSDeactivateOptions = []) async throws {
        log.debug("deactivate")
        await LifecycleTrace.shared.mark(
            log, event: "volume.deactivate.begin", details: "options=\(options.rawValue)")
        await readTelemetry.logSummary(log, reason: "deactivate")
        await readTelemetry.reset()
        removeAllCachedItems()
        await LifecycleTrace.shared.mark(log, event: "volume.deactivate.done")
    }

    func attributes(
        _ desiredAttributes: FSItem.GetAttributesRequest,
        of fsItem: FSItem
    ) async throws -> FSItem.Attributes {
        let op: StaticString = #function
        let item = try getItem(fsItem, fn: op)
        logOpStart(op, details: "itemId=\(item.itemId)")
        do {
            // Virtual items: always fresh from Swift side
            if item.itemId == Volume.vixenfsDirId {
                let attrs = vixenfsDirAttributes()
                item.updateAttributes(attrs)
                logOpSuccess(op, details: "itemId=\(item.itemId) source=virtual")
                return attrs
            }
            if item.itemId == Volume.statsJsonId {
                let attrs = await statsJsonAttributes()
                item.updateAttributes(attrs)
                logOpSuccess(op, details: "itemId=\(item.itemId) source=virtual")
                return attrs
            }
            if item.itemId == Volume.healthJsonId {
                let attrs = await healthJsonAttributes()
                item.updateAttributes(attrs)
                logOpSuccess(op, details: "itemId=\(item.itemId) source=virtual")
                return attrs
            }

            // Avoid serving stale cached attributes when the underlying transport is terminal.
            try await ensureConnectionAvailable(operation: op, sid: item.sid)

            let nowNs = DispatchTime.now().uptimeNanoseconds
            let snapshot = item.attributesSnapshot()
            let cachedAttrs = snapshot.attrs

            if Volume.isImmutableFile(cachedAttrs) {
                logOpSuccess(op, details: "itemId=\(item.itemId) source=immutable-cache")
                return cachedAttrs
            }

            if nowNs &- snapshot.lastValidationNs < Volume.mutableAttributesTTLNs {
                logOpSuccess(op, details: "itemId=\(item.itemId) source=ttl-cache")
                return cachedAttrs
            }

            let result = try await withMappedClientCall(sid: item.sid) {
                try await $0.getAttributes(itemId: item.backendItemId)
            }
            guard result.error == 0 else {
                log.debug(
                    "attributes: revalidate failed for itemId=\(item.itemId) error=\(result.error); using cached"
                )
                logOpSuccess(
                    op, details: "itemId=\(item.itemId) source=cache-after-revalidate-failure")
                return cachedAttrs
            }

            let refreshedAttrs = FSItem.Attributes.fromVfs(
                itemId: result.attrs.itemId,
                parentId: cachedAttrs.parentID.rawValue,
                itemType: result.attrs.itemType,
                size: result.attrs.size,
                modifiedTime: result.attrs.modifiedTime,
                createdTime: result.attrs.createdTime,
                mode: result.attrs.mode
            )
            item.updateAttributes(refreshedAttrs, validatedAtNs: nowNs)
            logOpSuccess(op, details: "itemId=\(item.itemId) source=rpc-revalidated")
            return refreshedAttrs
        } catch {
            logOpFailure(op, error: error, details: "itemId=\(item.itemId)")
            throw error
        }
    }

    func setAttributes(
        _ newAttributes: FSItem.SetAttributesRequest,
        on fsItem: FSItem
    ) async throws -> FSItem.Attributes {
        let item = try getItem(fsItem)
        log.debug("setAttributes: itemId=\(item.itemId)")

        // Virtual items are read-only
        if isVirtualItem(item.itemId) {
            return item.cachedAttributes
        }

        // Only forward attributes the kernel actually requested to change.
        // Reading unset fields returns zero, which would clobber real values
        // (e.g., setting mode=0 on a freshly-created directory).
        var consumed: FSItem.Attribute = []
        let params = SetAttributesParams(
            mode: newAttributes.isValid(.mode)
                ? {
                    consumed.insert(.mode)
                    return UInt32(newAttributes.mode)
                }() : nil,
            modifiedTime: newAttributes.isValid(.modifyTime)
                ? {
                    consumed.insert(.modifyTime)
                    return UInt64(newAttributes.modifyTime.tv_sec)
                }() : nil,
            size: newAttributes.isValid(.size)
                ? {
                    consumed.insert(.size)
                    return UInt64(newAttributes.size)
                }() : nil
        )

        let itemID = item.backendItemId
        let result = try await withMappedClientCall(sid: item.sid) {
            try await $0.setAttributes(itemId: itemID, params: params)
        }

        if result.error != 0 {
            try failPOSIX(result.error, details: "itemId=\(itemID)")
        }

        newAttributes.consumedAttributes = consumed

        // Refresh attributes from server
        let attrsResult = try await withMappedClientCall(sid: item.sid) {
            try await $0.getAttributes(itemId: itemID)
        }

        if attrsResult.error == 0 {
            let cachedAttrs = item.cachedAttributes
            let fsAttrs = FSItem.Attributes.fromVfs(
                itemId: attrsResult.attrs.itemId,
                parentId: cachedAttrs.parentID.rawValue,
                itemType: attrsResult.attrs.itemType,
                size: attrsResult.attrs.size,
                modifiedTime: attrsResult.attrs.modifiedTime,
                createdTime: attrsResult.attrs.createdTime,
                mode: attrsResult.attrs.mode
            )
            item.updateAttributes(fsAttrs)
        }

        return item.cachedAttributes
    }

    func lookupItem(
        named name: FSFileName,
        inDirectory directory: FSItem
    ) async throws -> (FSItem, FSFileName) {
        let dirItem = try getItem(directory)
        let nameStr = name.string ?? ""
        log.debug("lookupItem: name=\(nameStr) in dir=\(dirItem.itemId)")

        // Virtual .vixenfs directory under root
        if dirItem.itemId == rootId && nameStr == Volume.vixenfsDirName {
            let attrs = vixenfsDirAttributes()
            if let existing = getCachedItem(Volume.vixenfsDirId) {
                existing.updateAttributes(attrs)
                return (existing, existing.name)
            }
            let item = Item(
                itemId: Volume.vixenfsDirId, name: Volume.vixenfsDirName, attributes: attrs)
            cacheItem(item)
            return (item, item.name)
        }

        // Virtual stats.json under .vixenfs
        if dirItem.itemId == Volume.vixenfsDirId && nameStr == Volume.statsJsonName {
            let attrs = await statsJsonAttributes()
            if let existing = getCachedItem(Volume.statsJsonId) {
                existing.updateAttributes(attrs)
                return (existing, existing.name)
            }
            let item = Item(
                itemId: Volume.statsJsonId, name: Volume.statsJsonName, attributes: attrs)
            cacheItem(item)
            return (item, item.name)
        }

        // Virtual health.json under .vixenfs
        if dirItem.itemId == Volume.vixenfsDirId && nameStr == Volume.healthJsonName {
            let attrs = await healthJsonAttributes()
            if let existing = getCachedItem(Volume.healthJsonId) {
                existing.updateAttributes(attrs)
                return (existing, existing.name)
            }
            let item = Item(
                itemId: Volume.healthJsonId, name: Volume.healthJsonName, attributes: attrs)
            cacheItem(item)
            return (item, item.name)
        }

        // Items under .vixenfs that aren't known virtual files → ENOENT
        if dirItem.itemId == Volume.vixenfsDirId {
            throw fs_errorForPOSIXError(POSIXError.ENOENT.rawValue)
        }

        // Strict per-session routed prefix lookup at root.
        if dirItem.itemId == rootId {
            guard let sid = await sidForPrefix(nameStr) else {
                throw fs_errorForPOSIXError(POSIXError.ENOENT.rawValue)
            }
            let routedID = routedPrefixItemID(prefix: nameStr, sid: sid)
            let routedAttrs = routedPrefixAttributes(itemID: routedID)
            if let existing = getCachedItem(routedID) {
                existing.updateAttributes(routedAttrs)
                return (existing, existing.name)
            }
            let routedItem = Item(
                itemId: routedID,
                name: nameStr,
                attributes: routedAttrs,
                prefix: nameStr,
                sid: sid,
                backendItemId: rootId
            )
            cacheItem(routedItem)
            return (routedItem, routedItem.name)
        }

        let dirSID = dirItem.sid
        let dirBackendID = dirItem.backendItemId
        let result = try await withMappedClientCall(sid: dirSID) {
            try await $0.lookup(parentId: dirBackendID, name: nameStr)
        }

        if result.error != 0 {
            log.debug("lookupItem: not found (error=\(result.error))")
            try failPOSIX(result.error, details: "parentId=\(dirBackendID) name=\(nameStr)")
        }

        let attrs = result.attrs
        let localItemID = localItemID(for: dirSID, backendItemID: attrs.itemId)

        let fsAttrs = FSItem.Attributes.fromVfs(
            itemId: localItemID,
            parentId: dirItem.itemId,
            itemType: attrs.itemType,
            size: attrs.size,
            modifiedTime: attrs.modifiedTime,
            createdTime: attrs.createdTime,
            mode: attrs.mode
        )

        // Check if we already have this item cached
        if let existing = getCachedItem(localItemID) {
            existing.updateAttributes(fsAttrs)
            return (existing, existing.name)
        }

        let item = Item(
            itemId: localItemID,
            name: nameStr,
            attributes: fsAttrs,
            prefix: dirItem.prefix,
            sid: dirSID,
            backendItemId: attrs.itemId
        )
        cacheItem(item)
        return (item, item.name)
    }

    func reclaimItem(_ fsItem: FSItem) async throws {
        let item = try getItem(fsItem)
        log.debug("reclaimItem: itemId=\(item.itemId)")
        removeCachedItem(item.itemId)
    }

    func readSymbolicLink(_ fsItem: FSItem) async throws -> FSFileName {
        let item = try getItem(fsItem)
        log.debug("readSymbolicLink: itemId=\(item.itemId)")
        // TODO: implement symlink reading
        throw fs_errorForPOSIXError(POSIXError.ENOTSUP.rawValue)
    }

    func createItem(
        named name: FSFileName,
        type: FSItem.ItemType,
        inDirectory directory: FSItem,
        attributes newAttributes: FSItem.SetAttributesRequest
    ) async throws -> (FSItem, FSFileName) {
        let dirItem = try getItem(directory)
        let nameStr = name.string ?? ""
        log.debug("createItem: name=\(nameStr) in dir=\(dirItem.itemId)")

        // Cannot create items under .vixenfs
        if dirItem.itemId == Volume.vixenfsDirId {
            throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
        }

        if dirItem.itemId == rootId {
            throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
        }

        let vfsType: ItemType = (type == .directory) ? .directory : .file
        let dirSID = dirItem.sid
        let dirBackendID = dirItem.backendItemId
        let result = try await withMappedClientCall(sid: dirSID) {
            try await $0.create(parentId: dirBackendID, name: nameStr, itemType: vfsType)
        }

        if result.error != 0 {
            try failPOSIX(result.error, details: "parentId=\(dirBackendID) name=\(nameStr)")
        }

        // Get attributes for new item
        let attrsResult = try await withMappedClientCall(sid: dirSID) {
            try await $0.getAttributes(itemId: result.itemId)
        }

        let localItemID = localItemID(for: dirSID, backendItemID: result.itemId)

        let fsAttrs = FSItem.Attributes.fromVfs(
            itemId: localItemID,
            parentId: dirItem.itemId,
            itemType: vfsType,
            size: attrsResult.attrs.size,
            modifiedTime: attrsResult.attrs.modifiedTime,
            createdTime: attrsResult.attrs.createdTime,
            mode: attrsResult.attrs.mode
        )

        let item = Item(
            itemId: localItemID,
            name: nameStr,
            attributes: fsAttrs,
            prefix: dirItem.prefix,
            sid: dirSID,
            backendItemId: result.itemId
        )
        cacheItem(item)
        return (item, item.name)
    }

    func createSymbolicLink(
        named name: FSFileName,
        inDirectory directory: FSItem,
        attributes newAttributes: FSItem.SetAttributesRequest,
        linkContents contents: FSFileName
    ) async throws -> (FSItem, FSFileName) {
        log.debug("createSymbolicLink: not supported")
        throw fs_errorForPOSIXError(POSIXError.ENOTSUP.rawValue)
    }

    func createLink(
        to item: FSItem,
        named name: FSFileName,
        inDirectory directory: FSItem
    ) async throws -> FSFileName {
        log.debug("createLink: not supported (hard links)")
        throw fs_errorForPOSIXError(POSIXError.ENOTSUP.rawValue)
    }

    func removeItem(
        _ fsItem: FSItem,
        named name: FSFileName,
        fromDirectory directory: FSItem
    ) async throws {
        let item = try getItem(fsItem)
        log.debug("removeItem: itemId=\(item.itemId)")

        // Virtual items cannot be deleted
        if isVirtualItem(item.itemId) {
            throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
        }

        let itemID = item.backendItemId
        let result = try await withMappedClientCall(sid: item.sid) {
            try await $0.delete(itemId: itemID)
        }

        if result.error != 0 {
            try failPOSIX(result.error, details: "itemId=\(itemID)")
        }

        removeCachedItem(item.itemId)
    }

    func renameItem(
        _ fsItem: FSItem,
        inDirectory sourceDirectory: FSItem,
        named sourceName: FSFileName,
        to destinationName: FSFileName,
        inDirectory destinationDirectory: FSItem,
        overItem: FSItem?
    ) async throws -> FSFileName {
        let op: StaticString = #function
        let item = try getItem(fsItem, fn: op)
        let destDir = try getItem(destinationDirectory, fn: op)
        let destName = destinationName.string ?? ""
        log.debug("renameItem: itemId=\(item.itemId) -> \(destName) in \(destDir.itemId)")
        logOpStart(
            op,
            details:
                "itemId=\(item.itemId) newParentId=\(destDir.itemId) newName=\(destName)")
        do {
            // Virtual items cannot be renamed
            if isVirtualItem(item.itemId) || isVirtualItem(destDir.itemId) {
                throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
            }

            if item.itemId == rootId || destDir.itemId == rootId {
                throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
            }

            guard item.sid == destDir.sid else {
                throw fs_errorForPOSIXError(POSIXError.EXDEV.rawValue)
            }

            try await ensureConnectionAvailable(operation: op, sid: item.sid)

            let itemID = item.backendItemId
            let destDirID = destDir.backendItemId
            let result = try await withMappedClientCall(sid: item.sid) {
                try await $0.rename(itemId: itemID, newParentId: destDirID, newName: destName)
            }

            if result.error != 0 {
                try failPOSIX(
                    result.error,
                    details: "itemId=\(itemID) newParentId=\(destDirID) newName=\(destName)")
            }

            logOpSuccess(
                op,
                details:
                    "itemId=\(item.itemId) newParentId=\(destDir.itemId) newName=\(destName)")
            return destinationName
        } catch {
            logOpFailure(
                op,
                error: error,
                details:
                    "itemId=\(item.itemId) newParentId=\(destDir.itemId) newName=\(destName)")
            throw error
        }
    }

    func enumerateDirectory(
        _ directory: FSItem,
        startingAt cookie: FSDirectoryCookie,
        verifier: FSDirectoryVerifier,
        attributes: FSItem.GetAttributesRequest?,
        packer: FSDirectoryEntryPacker
    ) async throws -> FSDirectoryVerifier {
        let op: StaticString = #function
        let dirItem = try getItem(directory, fn: op)
        let cookieValue = cookie.rawValue
        log.debug("enumerateDirectory: itemId=\(dirItem.itemId) cookie=\(cookieValue)")
        logOpStart(op, details: "itemId=\(dirItem.itemId) cookie=\(cookieValue)")
        do {
            // Virtual .vixenfs directory: list virtual files
            if dirItem.itemId == Volume.vixenfsDirId {
                if cookieValue == 0 {
                    let statsAttrs = await statsJsonAttributes()
                    let statsItem = Item(
                        itemId: Volume.statsJsonId, name: Volume.statsJsonName,
                        attributes: statsAttrs)
                    cacheItem(statsItem)
                    let shouldContinue = packer.packEntry(
                        name: FSFileName(string: Volume.statsJsonName),
                        itemType: .file,
                        itemID: statsAttrs.fileID,
                        nextCookie: FSDirectoryCookie(1),
                        attributes: attributes != nil ? statsAttrs : nil
                    )
                    if shouldContinue {
                        let healthAttrs = await healthJsonAttributes()
                        let healthItem = Item(
                            itemId: Volume.healthJsonId, name: Volume.healthJsonName,
                            attributes: healthAttrs)
                        cacheItem(healthItem)
                        _ = packer.packEntry(
                            name: FSFileName(string: Volume.healthJsonName),
                            itemType: .file,
                            itemID: healthAttrs.fileID,
                            nextCookie: FSDirectoryCookie(2),
                            attributes: attributes != nil ? healthAttrs : nil
                        )
                    }
                }
                if cookieValue == 1 {
                    let healthAttrs = await healthJsonAttributes()
                    let healthItem = Item(
                        itemId: Volume.healthJsonId, name: Volume.healthJsonName,
                        attributes: healthAttrs)
                    cacheItem(healthItem)
                    _ = packer.packEntry(
                        name: FSFileName(string: Volume.healthJsonName),
                        itemType: .file,
                        itemID: healthAttrs.fileID,
                        nextCookie: FSDirectoryCookie(2),
                        attributes: attributes != nil ? healthAttrs : nil
                    )
                }
                logOpSuccess(op, details: "itemId=\(dirItem.itemId) source=virtual")
                return FSDirectoryVerifier(0)
            }

            if dirItem.itemId == rootId {
                let sortedRoutes = (await currentRouteMap()).sorted { lhs, rhs in
                    lhs.key < rhs.key
                }
                var index = Int(cookieValue)
                while index < sortedRoutes.count {
                    let (prefix, sid) = sortedRoutes[index]
                    let routedID = routedPrefixItemID(prefix: prefix, sid: sid)
                    let routedAttrs = routedPrefixAttributes(itemID: routedID)
                    if let existing = getCachedItem(routedID) {
                        existing.updateAttributes(routedAttrs)
                    } else {
                        let routedItem = Item(
                            itemId: routedID,
                            name: prefix,
                            attributes: routedAttrs,
                            prefix: prefix,
                            sid: sid,
                            backendItemId: rootId
                        )
                        cacheItem(routedItem)
                    }

                    let shouldContinue = packer.packEntry(
                        name: FSFileName(string: prefix),
                        itemType: .directory,
                        itemID: routedAttrs.fileID,
                        nextCookie: FSDirectoryCookie(UInt64(index + 1)),
                        attributes: attributes != nil ? routedAttrs : nil
                    )
                    if !shouldContinue {
                        break
                    }
                    index += 1
                }
                logOpSuccess(op, details: "itemId=\(dirItem.itemId) source=session-prefixes")
                return FSDirectoryVerifier(0)
            }

            let dirSID = dirItem.sid
            try await ensureConnectionAvailable(operation: op, sid: dirSID)

            let dirBackendID = dirItem.backendItemId
            let verifierValue = verifier.rawValue
            let result = try await withMappedClientCall(sid: dirSID) {
                try await $0.readDir(itemId: dirBackendID, cookie: cookieValue, verifier: verifierValue)
            }

            if result.error != 0 {
                try failPOSIX(
                    result.error,
                    details:
                        "itemId=\(dirBackendID) cookie=\(cookieValue) verifier=\(verifierValue)")
            }

            var nextCookie: UInt64 = cookieValue + 1
            for entry in result.entries {
                let localItemID = localItemID(for: dirSID, backendItemID: entry.itemId)

                let entryAttrs = FSItem.Attributes.fromVfs(
                    itemId: localItemID,
                    parentId: dirItem.itemId,
                    itemType: entry.itemType,
                    size: entry.size,
                    modifiedTime: entry.modifiedTime,
                    createdTime: entry.createdTime,
                    mode: entry.mode
                )

                let entryItem = Item(
                    itemId: localItemID,
                    name: entry.name,
                    attributes: entryAttrs,
                    prefix: dirItem.prefix,
                    sid: dirSID,
                    backendItemId: entry.itemId
                )
                cacheItem(entryItem)

                let shouldContinue = packer.packEntry(
                    name: FSFileName(string: entry.name),
                    itemType: entryAttrs.type,
                    itemID: entryAttrs.fileID,
                    nextCookie: FSDirectoryCookie(nextCookie),
                    attributes: attributes != nil ? entryAttrs : nil
                )

                if !shouldContinue {
                    break
                }
                nextCookie += 1
            }

            logOpSuccess(
                op,
                details:
                    "itemId=\(dirItem.itemId) entries=\(result.entries.count) verifier=\(result.verifier)"
            )
            return FSDirectoryVerifier(result.verifier)
        } catch {
            logOpFailure(
                op, error: error, details: "itemId=\(dirItem.itemId) cookie=\(cookieValue)")
            throw error
        }
    }
}

// MARK: - FSVolume.ReadWriteOperations

extension Volume: FSVolume.ReadWriteOperations {
    func read(
        from fsItem: FSItem,
        at offset: off_t,
        length: Int,
        into buffer: FSMutableFileDataBuffer
    ) async throws -> Int {
        let item = try getItem(fsItem)
        log.debug("read: itemId=\(item.itemId) offset=\(offset) length=\(length)")

        // Virtual files under .vixenfs: serve directly
        if item.itemId == Volume.statsJsonId || item.itemId == Volume.healthJsonId {
            let data: Data
            if item.itemId == Volume.statsJsonId {
                data = await readTelemetry.toJSON()
            } else {
                data = await ConnectionHealth.shared.toJSON()
            }
            let start = min(Int(offset), data.count)
            let end = min(start + max(length, 0), data.count)
            let slice = data[start..<end]
            let copyLen = min(buffer.length, slice.count)
            if copyLen > 0 {
                slice.withUnsafeBytes { src in
                    buffer.withUnsafeMutableBytes { dst in
                        dst.copyBytes(
                            from: UnsafeRawBufferPointer(start: src.baseAddress!, count: copyLen))
                    }
                }
            }
            return copyLen
        }

        let startNs = DispatchTime.now().uptimeNanoseconds
        let requestedBytes = max(length, 0)
        do {
            let itemID = item.backendItemId

            let result = try await withMappedClientCall(sid: item.sid) {
                try await $0.read(
                    itemId: itemID,
                    offset: UInt64(offset),
                    len: UInt64(requestedBytes)
                )
            }

            if result.error != 0 {
                try failPOSIX(
                    result.error,
                    details: "itemId=\(itemID) offset=\(offset) length=\(requestedBytes)")
            }

            let copyLen = min(buffer.length, result.data.count)
            if copyLen > 0 {
                result.data.withUnsafeBytes { src in
                    buffer.withUnsafeMutableBytes { dst in
                        dst.copyBytes(
                            from: UnsafeRawBufferPointer(
                                start: src.baseAddress!,
                                count: copyLen
                            ))
                    }
                }
            }

            let elapsedNs = DispatchTime.now().uptimeNanoseconds &- startNs
            await readTelemetry.record(
                requestedBytes: requestedBytes,
                returnedBytes: copyLen,
                latencyNs: elapsedNs,
                isError: false
            )
            return copyLen
        } catch {
            let elapsedNs = DispatchTime.now().uptimeNanoseconds &- startNs
            await readTelemetry.record(
                requestedBytes: requestedBytes,
                returnedBytes: 0,
                latencyNs: elapsedNs,
                isError: true
            )
            throw vixenMappedFSError(error, operation: #function, logger: log)
        }
    }

    func write(
        contents: Data,
        to fsItem: FSItem,
        at offset: off_t
    ) async throws -> Int {
        // IMPORTANT: Copy data immediately before any async suspension point.
        // FSKit may free the underlying NSData buffer, causing heap corruption
        // if we access it after an await.
        let bytes = [UInt8](contents)

        let item = try getItem(fsItem)
        log.debug("write: itemId=\(item.itemId) offset=\(offset) length=\(bytes.count)")

        // Virtual items are read-only
        if isVirtualItem(item.itemId) {
            throw fs_errorForPOSIXError(POSIXError.EACCES.rawValue)
        }

        let itemID = item.backendItemId

        let result = try await withMappedClientCall(sid: item.sid) {
            try await $0.write(itemId: itemID, offset: UInt64(offset), data: Data(bytes))
        }

        if result.error != 0 {
            try failPOSIX(
                result.error,
                details: "itemId=\(itemID) offset=\(offset) length=\(bytes.count)")
        }

        return Int(result.bytesWritten)
    }
}

// MARK: - FSVolume.PathConfOperations

extension Volume: FSVolume.PathConfOperations {
    var maximumLinkCount: Int { 1 }
    var maximumNameLength: Int { 255 }
    var restrictsOwnershipChanges: Bool { true }
    var truncatesLongNames: Bool { false }
    var maximumXattrSize: Int { 0 }
    var maximumXattrSizeInBits: Int { 0 }
    var maximumFileSize: UInt64 { UInt64.max }
    var maximumFileSizeInBits: Int { 64 }
}

// MARK: - FSVolume.OpenCloseOperations

extension Volume: FSVolume.OpenCloseOperations {
    var isOpenCloseInhibited: Bool {
        get { true }  // We don't track open/close state
        set {}
    }

    func openItem(_ item: FSItem, modes: FSVolume.OpenModes) async throws {
        // No-op: we don't track open state
    }

    func closeItem(_ item: FSItem, modes: FSVolume.OpenModes) async throws {
        // No-op: we don't track open state
    }
}

// MARK: - FSVolume.AccessCheckOperations

extension Volume: FSVolume.AccessCheckOperations {
    var isAccessCheckInhibited: Bool {
        get { true }  // Allow all access for now
        set {}
    }

    func checkAccess(
        to theItem: FSItem,
        requestedAccess access: FSVolume.AccessMask
    ) async throws -> Bool {
        return true  // Allow all access
    }
}
