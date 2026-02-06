// Vixen: Pure Swift VFS client using RoamRuntime
import Foundation
import RoamRuntime

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

func main() async {
    print("Vixen: Pure Swift VFS client via roam-runtime")
    print("================================================\n")

    do {
        // Connect to VFS server and establish roam session
        print("Connecting to VFS server at 127.0.0.1:10001...")
        let transport = try await connect(host: "127.0.0.1", port: 10001)
        let (handle, driver) = try await establishInitiator(
            transport: transport,
            dispatcher: NoopDispatcher()
        )
        let driverTask = Task {
            try await driver.run()
        }
        let client = VfsClient(connection: handle)
        print("Connected!\n")

        // Ping
        print("--- ping ---")
        let pong = try await client.ping()
        print("Response: \"\(pong)\"\n")

        // Read root directory
        print("--- readDir (/) ---")
        let root = try await client.readDir(itemId: 1, cursor: 0)
        for entry in root.entries {
            let icon = entry.itemType == .directory ? "📁" : "📄"
            print("  \(icon) \(entry.name)")
        }
        print()

        // Lookup and read hello.txt
        print("--- read hello.txt ---")
        let lookup = try await client.lookup(parentId: 1, name: "hello.txt")
        let content = try await client.read(itemId: lookup.itemId, offset: 0, len: 1024)
        if let text = String(data: content.data, encoding: .utf8) {
            print("Content: \"\(text.trimmingCharacters(in: .newlines))\"")
        }
        print()

        // Create, write, read, delete a test file
        print("--- create/write/read/delete test ---")
        let created = try await client.create(parentId: 1, name: "swift-test.txt", itemType: .file)
        print("Created file id=\(created.itemId)")

        let testData = Data("Hello from pure Swift!".utf8)
        let written = try await client.write(itemId: created.itemId, offset: 0, data: testData)
        print("Wrote \(written.bytesWritten) bytes")

        let readBack = try await client.read(itemId: created.itemId, offset: 0, len: 1024)
        if let text = String(data: readBack.data, encoding: .utf8) {
            print("Read back: \"\(text)\"")
        }

        let deleted = try await client.delete(itemId: created.itemId)
        print("Deleted (error=\(deleted.error))")
        print()

        driverTask.cancel()

        print("================================================")
        print("All operations successful! Pure Swift VFS works.")

    } catch {
        print("ERROR: \(error)")
    }
}

// Run async main
Task {
    await main()
    exit(0)
}
RunLoop.main.run()
