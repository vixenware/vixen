import Foundation
import VoxRuntime

struct NoopDispatcher: ServiceDispatcher {
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

enum ExpectedVfsClient: ExpectedRootClient {
    static let voxServiceName = "Vfs"
}

@main
struct VoxAttachProbe {
    static func main() async throws {
        let args = Array(CommandLine.arguments.dropFirst())
        guard args.count == 2 else {
            fputs("usage: VoxAttachProbe <noop|vfs> /tmp/vox-attach-probe.sock\n", stderr)
            Foundation.exit(64)
        }

        let mode = args[0]
        let socketPath = args[1]
        print("listening \(mode) \(socketPath)")
        let listener = try await UnixListener.bind(unixPath: socketPath)
        defer {
            Task {
                try? await listener.close()
            }
        }

        for await link in listener.acceptConnections() {
            print("accepted raw link")
            do {
                let session: Session
                switch mode {
                case "noop":
                    session = try await Session.acceptFreshLink(
                        link,
                        conduit: .bare,
                        expecting: NoopRootClient.self,
                        dispatcher: NoopDispatcher(),
                        acceptConnections: false,
                        resumable: false
                    )
                case "vfs":
                    session = try await Session.acceptFreshLink(
                        link,
                        conduit: .bare,
                        expecting: ExpectedVfsClient.self,
                        dispatcher: NoopDispatcher(),
                        acceptConnections: false,
                        resumable: false
                    )
                default:
                    fputs("unknown mode: \(mode)\n", stderr)
                    Foundation.exit(64)
                }
                print("accepted session")
                for entry in session.peerMetadata {
                    print("metadata \(entry.key)=\(entry.value)")
                }
                try await session.run()
                print("driver exited cleanly")
            } catch {
                print("accept failed: \(error)")
            }
            break
        }
    }
}
