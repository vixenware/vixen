import FSKit
import Foundation
import os

actor ReadTelemetry {
    static let shared = ReadTelemetry()

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

/// FSKit Volume implementation that delegates to the VFS client.
final class Volume: FSVolume {

    private let log = Logger(subsystem: VixenFS.subsystem, category: "Volume")

    /// Item cache: itemId -> Item, protected by lock for thread safety.
    /// Volume is a class (not an actor) because it subclasses FSVolume, so we need
    /// explicit synchronization. FSKit may call operations concurrently.
    /// We use NSLock because Item doesn't conform to Sendable (ObjC base class).
    private let _itemsLock = NSLock()
    private var _items: [UInt64: Item] = [:]

    /// Root item ID (always 1 in our VFS protocol)
    private let rootId: UInt64 = 1

    init() {
        let volumeId = FSVolume.Identifier(uuid: UUID())
        let volumeName = FSFileName(string: "Vixen")
        log.notice("Volume init")
        super.init(volumeID: volumeId, volumeName: volumeName)
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

    private func getItem(_ fsItem: FSItem, fn: StaticString = #function) throws -> Item {
        guard let item = fsItem as? Item else {
            log.error("\(fn): unexpected FSItem type")
            throw fs_errorForPOSIXError(POSIXError.EINVAL.rawValue)
        }
        return item
    }

    // MARK: - VFS Client Access

    private func getClient() async throws -> VfsClient {
        try await VfsConnection.shared.getClient()
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
        log.debug("volumeStatistics")
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
        await ReadTelemetry.shared.logSummary(log, reason: "unmount")
        await ReadTelemetry.shared.reset()
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
        let result = try await getClient().getAttributes(itemId: rootId)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
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
        await ReadTelemetry.shared.logSummary(log, reason: "deactivate")
        await ReadTelemetry.shared.reset()
        removeAllCachedItems()
        await LifecycleTrace.shared.mark(log, event: "volume.deactivate.done")
    }

    func attributes(
        _ desiredAttributes: FSItem.GetAttributesRequest,
        of fsItem: FSItem
    ) async throws -> FSItem.Attributes {
        let item = try getItem(fsItem)
        return item.cachedAttributes
    }

    func setAttributes(
        _ newAttributes: FSItem.SetAttributesRequest,
        on fsItem: FSItem
    ) async throws -> FSItem.Attributes {
        let item = try getItem(fsItem)
        log.debug("setAttributes: itemId=\(item.itemId)")

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

        let itemId = item.itemId
        let client = try await getClient()
        let result = try await client.setAttributes(itemId: itemId, params: params)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
        }

        newAttributes.consumedAttributes = consumed

        // Refresh attributes from server
        let attrsResult = try await client.getAttributes(itemId: itemId)

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

        let dirId = dirItem.itemId
        let client = try await getClient()
        let result = try await client.lookup(parentId: dirId, name: nameStr)

        if result.error != 0 {
            log.debug("lookupItem: not found (error=\(result.error))")
            throw fs_errorForPOSIXError(result.error)
        }

        // Get full attributes for the found item
        let attrsResult = try await client.getAttributes(itemId: result.itemId)

        let fsAttrs = FSItem.Attributes.fromVfs(
            itemId: result.itemId,
            parentId: dirId,
            itemType: result.itemType,
            size: attrsResult.attrs.size,
            modifiedTime: attrsResult.attrs.modifiedTime,
            createdTime: attrsResult.attrs.createdTime,
            mode: attrsResult.attrs.mode
        )

        // Check if we already have this item cached
        if let existing = getCachedItem(result.itemId) {
            existing.updateAttributes(fsAttrs)
            return (existing, existing.name)
        }

        let item = Item(itemId: result.itemId, name: nameStr, attributes: fsAttrs)
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

        let vfsType: ItemType = (type == .directory) ? .directory : .file
        let dirId = dirItem.itemId
        let client = try await getClient()
        let result = try await client.create(parentId: dirId, name: nameStr, itemType: vfsType)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
        }

        // Get attributes for new item
        let attrsResult = try await client.getAttributes(itemId: result.itemId)

        let fsAttrs = FSItem.Attributes.fromVfs(
            itemId: result.itemId,
            parentId: dirId,
            itemType: vfsType,
            size: attrsResult.attrs.size,
            modifiedTime: attrsResult.attrs.modifiedTime,
            createdTime: attrsResult.attrs.createdTime,
            mode: attrsResult.attrs.mode
        )

        let item = Item(itemId: result.itemId, name: nameStr, attributes: fsAttrs)
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

        let itemId = item.itemId
        let result = try await getClient().delete(itemId: itemId)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
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
        let item = try getItem(fsItem)
        let destDir = try getItem(destinationDirectory)
        let destName = destinationName.string ?? ""
        log.debug("renameItem: itemId=\(item.itemId) -> \(destName) in \(destDir.itemId)")

        let itemId = item.itemId
        let destDirId = destDir.itemId
        let result = try await getClient().rename(
            itemId: itemId, newParentId: destDirId, newName: destName)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
        }

        return destinationName
    }

    func enumerateDirectory(
        _ directory: FSItem,
        startingAt cookie: FSDirectoryCookie,
        verifier: FSDirectoryVerifier,
        attributes: FSItem.GetAttributesRequest?,
        packer: FSDirectoryEntryPacker
    ) async throws -> FSDirectoryVerifier {
        let dirItem = try getItem(directory)
        log.debug("enumerateDirectory: itemId=\(dirItem.itemId) cookie=\(cookie.rawValue)")

        let dirId = dirItem.itemId
        let cursor = cookie.rawValue
        let result = try await getClient().readDir(itemId: dirId, cursor: cursor)

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
        }

        var nextCookie: UInt64 = 1
        for entry in result.entries {
            let entryAttrs = FSItem.Attributes.fromVfs(
                itemId: entry.itemId,
                parentId: dirId,
                itemType: entry.itemType,
                size: entry.size,
                modifiedTime: entry.modifiedTime,
                createdTime: entry.createdTime,
                mode: entry.mode
            )

            let entryItem = Item(itemId: entry.itemId, name: entry.name, attributes: entryAttrs)
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

        return FSDirectoryVerifier(result.nextCursor)
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

        let startNs = DispatchTime.now().uptimeNanoseconds
        let requestedBytes = max(length, 0)
        do {
            let itemId = item.itemId

            let result = try await getClient().read(
                itemId: itemId,
                offset: UInt64(offset),
                len: UInt64(requestedBytes)
            )

            if result.error != 0 {
                throw fs_errorForPOSIXError(result.error)
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
            await ReadTelemetry.shared.record(
                requestedBytes: requestedBytes,
                returnedBytes: copyLen,
                latencyNs: elapsedNs,
                isError: false
            )
            return copyLen
        } catch {
            let elapsedNs = DispatchTime.now().uptimeNanoseconds &- startNs
            await ReadTelemetry.shared.record(
                requestedBytes: requestedBytes,
                returnedBytes: 0,
                latencyNs: elapsedNs,
                isError: true
            )
            throw error
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

        let itemId = item.itemId

        let result = try await getClient().write(
            itemId: itemId, offset: UInt64(offset), data: Data(bytes))

        if result.error != 0 {
            throw fs_errorForPOSIXError(result.error)
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
