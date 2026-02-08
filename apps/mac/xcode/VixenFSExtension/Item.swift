import FSKit
import Foundation

/// Wrapper around FSItem that holds our VFS item state.
/// Mutable fields are protected by a lock since Item instances are shared
/// between Volume's cache and FSKit, which may access them concurrently.
final class Item: FSItem {
    /// VFS item ID (unique identifier)
    let itemId: UInt64

    /// File name (immutable after init — set once during lookup/create)
    let name: FSFileName

    /// Cached attributes, protected by lock for concurrent access.
    /// We use NSLock instead of OSAllocatedUnfairLock because FSItem.Attributes
    /// is an ObjC class that doesn't conform to Sendable.
    private let _lock = NSLock()
    private var _cachedAttributes: FSItem.Attributes

    var cachedAttributes: FSItem.Attributes {
        _lock.lock()
        defer { _lock.unlock() }
        return _cachedAttributes
    }

    init(itemId: UInt64, name: String, attributes: FSItem.Attributes) {
        self.itemId = itemId
        self.name = FSFileName(string: name)
        self._cachedAttributes = attributes
    }

    func updateAttributes(_ attrs: FSItem.Attributes) {
        _lock.lock()
        defer { _lock.unlock() }
        _cachedAttributes = attrs
    }
}

// MARK: - Attribute Helpers

extension FSItem.Attributes {
    /// Create attributes from VFS response using ItemType enum
    static func fromVfs(
        itemId: UInt64,
        parentId: UInt64,
        itemType: ItemType,
        size: UInt64,
        modifiedTime: UInt64,
        createdTime: UInt64,
        mode: UInt32
    ) -> FSItem.Attributes {
        let attrs = FSItem.Attributes()

        // Set file ID
        if let id = FSItem.Identifier(rawValue: itemId) {
            attrs.fileID = id
        }

        // Set parent ID
        if let pid = FSItem.Identifier(rawValue: parentId) {
            attrs.parentID = pid
        }

        // Set type
        switch itemType {
        case .file:
            attrs.type = .file
        case .directory:
            attrs.type = .directory
        case .symlink:
            attrs.type = .symlink
        }

        // Set size and allocated size
        attrs.size = size
        attrs.allocSize = size

        // Set flags (st_flags — 0 for normal items)
        attrs.flags = 0

        // Set times
        attrs.modifyTime = timespec(tv_sec: Int(modifiedTime), tv_nsec: 0)
        attrs.birthTime = timespec(tv_sec: Int(createdTime), tv_nsec: 0)
        attrs.changeTime = timespec(tv_sec: Int(modifiedTime), tv_nsec: 0)
        attrs.accessTime = timespec(tv_sec: Int(modifiedTime), tv_nsec: 0)

        // Use mode from VFS (e.g., 0o755 for executable, 0o644 for regular)
        attrs.mode = mode

        // Default uid/gid
        attrs.uid = 501
        attrs.gid = 20

        // Link count
        attrs.linkCount = 1

        return attrs
    }
}
