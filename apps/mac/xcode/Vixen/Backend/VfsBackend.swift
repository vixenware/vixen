import Foundation
import os

#if canImport(VoxRuntime)
    import VoxRuntime
#endif

/// A tiny in-memory VFS served to the VixenFS extension over `vixenfs.sock`.
///
/// The extension binds the socket and calls the `Vfs` service on whoever connects; this
/// app is that backend. This is intentionally minimal — just enough to prove the whole
/// path end to end: a read-only root with a couple of files. Real content (the CAS-backed
/// VFS) is the `vx` daemon's job; this makes "Backend Connection" green and shows files in
/// `/Volumes/Vixen/vfs` without an external daemon.
private let vfsBackendLog = Logger(subsystem: "eu.bearcove.vixen", category: "VfsBackend")

enum VfsBackendError: Error, CustomStringConvertible {
    case connectTimeout
    var description: String { "connect/handshake timed out" }
}

/// Log to both os_log and a plain file (`~/Library/Logs/Vixen/vixen-backend.log`).
/// os_log isn't reliably readable in every environment; the file always is.
func backendLog(_ message: String) {
    vfsBackendLog.notice("\(message, privacy: .public)")
    let path = (NSHomeDirectory() as NSString)
        .appendingPathComponent("Library/Logs/Vixen/vixen-backend.log")
    let dir = (path as NSString).deletingLastPathComponent
    try? FileManager.default.createDirectory(
        atPath: dir, withIntermediateDirectories: true)
    let stamp = ISO8601DateFormatter().string(from: Date())
    guard let data = "\(stamp) \(message)\n".data(using: .utf8) else { return }
    if let fh = FileHandle(forWritingAtPath: path) {
        defer { try? fh.close() }
        _ = try? fh.seekToEnd()
        try? fh.write(contentsOf: data)
    } else {
        try? data.write(to: URL(fileURLWithPath: path))
    }
}

// Errno-ish codes (0 = OK). Mutations are refused read-only.
private let E_OK: Int32 = 0
private let E_NOENT: Int32 = 2
private let E_ROFS: Int32 = 30
private let E_NOTDIR: Int32 = 20

/// Fixed timestamp for the demo tree (2026-01-01T00:00:00Z), so attributes are stable.
private let demoTime: UInt64 = 1_767_225_600

private struct Node {
    let id: UInt64
    let type: ItemType
    let mode: UInt32
    /// Directory children (name -> id); empty for files.
    let children: [(name: String, id: UInt64)]
    /// File contents; empty for directories.
    let data: Data
}

final class VfsBackend: VfsHandler {
    private let nodes: [UInt64: Node]

    init() {
        let readme = Data("""
        # VixenFS

        This volume is served by the Vixen app's in-app VFS backend (pure Swift, vox 0.8.2).
        If you can read this, the FSKit extension ↔ backend connection is live.
        """.utf8)
        let hello = Data("Hello from the Vixen Swift VFS backend!\n".utf8)

        let root = Node(
            id: 1, type: .directory, mode: 0o755,
            children: [("README.md", 2), ("hello.txt", 3)], data: Data())
        let readmeNode = Node(id: 2, type: .file, mode: 0o644, children: [], data: readme)
        let helloNode = Node(id: 3, type: .file, mode: 0o644, children: [], data: hello)
        self.nodes = [1: root, 2: readmeNode, 3: helloNode]
    }

    private func attrs(_ node: Node) -> ItemAttributes {
        ItemAttributes(
            itemId: node.id, itemType: node.type, size: UInt64(node.data.count),
            modifiedTime: demoTime, createdTime: demoTime, mode: node.mode)
    }

    private func missingAttrs(_ id: UInt64) -> ItemAttributes {
        ItemAttributes(
            itemId: id, itemType: .file, size: 0, modifiedTime: demoTime,
            createdTime: demoTime, mode: 0)
    }

    // MARK: - VfsHandler (read side)

    func lookup(parentId: UInt64, name: String) async throws -> LookupResult {
        guard let parent = nodes[parentId] else {
            return LookupResult(attrs: missingAttrs(0), error: E_NOENT)
        }
        guard parent.type == .directory else {
            return LookupResult(attrs: missingAttrs(0), error: E_NOTDIR)
        }
        guard let childId = parent.children.first(where: { $0.name == name })?.id,
            let child = nodes[childId]
        else {
            return LookupResult(attrs: missingAttrs(0), error: E_NOENT)
        }
        return LookupResult(attrs: attrs(child), error: E_OK)
    }

    func getAttributes(itemId: UInt64) async throws -> GetAttributesResult {
        guard let node = nodes[itemId] else {
            return GetAttributesResult(attrs: missingAttrs(itemId), error: E_NOENT)
        }
        return GetAttributesResult(attrs: attrs(node), error: E_OK)
    }

    func readDir(itemId: UInt64, cookie: UInt64, verifier: UInt64) async throws -> ReadDirResult {
        guard let node = nodes[itemId], node.type == .directory else {
            return ReadDirResult(entries: [], nextCookie: 0, verifier: 0, error: E_NOTDIR)
        }
        // Single-page enumeration: everything on cookie 0, then we're done.
        if cookie != 0 {
            return ReadDirResult(entries: [], nextCookie: 0, verifier: 0, error: E_OK)
        }
        let entries = node.children.compactMap { child -> DirEntry? in
            guard let c = nodes[child.id] else { return nil }
            return DirEntry(
                name: child.name, itemId: c.id, itemType: c.type, size: UInt64(c.data.count),
                modifiedTime: demoTime, createdTime: demoTime, mode: c.mode)
        }
        return ReadDirResult(entries: entries, nextCookie: 0, verifier: 0, error: E_OK)
    }

    func read(itemId: UInt64, offset: UInt64, len: UInt64) async throws -> ReadResult {
        guard let node = nodes[itemId], node.type == .file else {
            return ReadResult(data: Data(), error: E_NOENT)
        }
        let total = UInt64(node.data.count)
        guard offset < total else { return ReadResult(data: Data(), error: E_OK) }
        let start = Int(offset)
        let end = Int(min(total, offset + len))
        return ReadResult(data: node.data.subdata(in: start..<end), error: E_OK)
    }

    // MARK: - VfsHandler (write side) — read-only volume

    func write(itemId: UInt64, offset: UInt64, data: Data) async throws -> WriteResult {
        WriteResult(bytesWritten: 0, error: E_ROFS)
    }
    func create(parentId: UInt64, name: String, itemType: ItemType, mode: UInt32) async throws
        -> CreateResult
    {
        CreateResult(itemId: 0, error: E_ROFS)
    }
    func delete(itemId: UInt64) async throws -> VfsResult { VfsResult(error: E_ROFS) }
    func rename(itemId: UInt64, newParentId: UInt64, newName: String) async throws -> VfsResult {
        VfsResult(error: E_ROFS)
    }
    func setAttributes(itemId: UInt64, params: SetAttributesParams) async throws -> VfsResult {
        VfsResult(error: E_ROFS)
    }
    func ping() async throws -> String { "vixen-swift-backend" }
}

/// Connects to the extension's `vixenfs.sock` and serves `VfsBackend`, reconnecting
/// whenever the session drops (e.g. the volume is remounted). Idempotent to start.
final class VfsBackendService: @unchecked Sendable {
    static let shared = VfsBackendService()

    private let lock = NSLock()
    private var started = false

    private init() {}

    /// Socket the extension advertises — the app-group container's `vixenfs.sock`,
    /// the same path the extension binds (RustFSKitBootstrap).
    /// The app-group container shared with the FSKit extension (same identifier its
    /// `VixenFSConstants` uses; that type lives in the appex target, so it's duplicated
    /// here rather than imported).
    private static let appGroupIdentifier = "B2N6FSRTPV.vixenfs"

    private func socketPath() -> String? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: Self.appGroupIdentifier)?
            .appendingPathComponent("vixenfs.sock").path
    }

    func startIfNeeded() {
        backendLog("startIfNeeded called")
        lock.lock()
        let already = started
        started = true
        lock.unlock()
        guard !already else { return }
        Task.detached(priority: .utility) { await self.serveLoop() }
    }

    private func serveLoop() async {
        backendLog("serveLoop started")
        #if !canImport(VoxRuntime)
            backendLog("VoxRuntime not available at compile time — backend disabled")
        #endif
        while !Task.isCancelled {
            guard let path = socketPath() else {
                backendLog("no app-group container; cannot locate vixenfs.sock")
                try? await Task.sleep(for: .seconds(5))
                continue
            }
            #if canImport(VoxRuntime)
                do {
                    backendLog("connecting to \(path)")
                    // Bound the connect/handshake so a stall is observable + retried
                    // rather than dead-hanging forever.
                    let session = try await withThrowingTaskGroup(
                        of: VoxRuntime.Session.self
                    ) { group in
                        group.addTask {
                            try await VoxRuntime.Session.initiator(
                                UnixConnector(path: path),
                                dispatcher: VfsDispatcher(handler: VfsBackend()),
                                resumable: true)
                        }
                        group.addTask {
                            try await Task.sleep(for: .seconds(10))
                            throw VfsBackendError.connectTimeout
                        }
                        let established = try await group.next()!
                        group.cancelAll()
                        return established
                    }
                    backendLog("connected; serving")
                    try await session.run()
                    backendLog("session ended; will reconnect")
                } catch {
                    backendLog("connect failed: \(error)")
                }
            #endif
            // The socket only exists while the volume is mounted; retry until it's up.
            try? await Task.sleep(for: .seconds(2))
        }
    }
}
