import Darwin
import Foundation
import os

private enum PeepsWire {
    static let maxFrameSize = 128 * 1024 * 1024
}

private struct PeepsSnapshotRequest: Decodable {
    let type: String
    let snapshotID: Int64
    let timeoutMs: Int64

    enum CodingKeys: String, CodingKey {
        case type
        case snapshotID = "snapshot_id"
        case timeoutMs = "timeout_ms"
    }
}

private struct PeepsGraphReply: Encodable {
    let type: String
    let snapshotID: Int64
    let process: String
    let pid: UInt32
    let graph: PeepsGraphSnapshot?

    enum CodingKeys: String, CodingKey {
        case type
        case snapshotID = "snapshot_id"
        case process
        case pid
        case graph
    }
}

private struct PeepsGraphSnapshot: Encodable {
    let processName: String
    let procKey: String
    let nodes: [PeepsGraphNode]
    let edges: [PeepsGraphEdge]

    enum CodingKeys: String, CodingKey {
        case processName = "process_name"
        case procKey = "proc_key"
        case nodes
        case edges
    }
}

private struct PeepsGraphNode: Encodable {
    let id: String
    let kind: String
    let label: String?
    let attrsJSON: String

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case label
        case attrsJSON = "attrs_json"
    }
}

private struct PeepsGraphEdge: Encodable {
    let src: String
    let dst: String
    let kind: String
    let attrsJSON: String

    enum CodingKeys: String, CodingKey {
        case src
        case dst
        case kind
        case attrsJSON = "attrs_json"
    }
}

private struct PeepsRequestAttrs: Encodable {
    let spanID: String
    let operation: String
    let startedMs: Int64

    enum CodingKeys: String, CodingKey {
        case spanID = "span_id"
        case operation
        case startedMs = "started_ms"
    }
}

private struct PeepsSyscallAttrs: Encodable {
    let operation: String
    let phase: String
    let tsMs: Int64
    let errno: Int32?
    let requestID: String?

    enum CodingKeys: String, CodingKey {
        case operation
        case phase
        case tsMs = "ts_ms"
        case errno
        case requestID = "request_id"
    }
}

private actor SwiftPeepsGraphState {
    private struct InFlightRequest {
        let spanID: String
        let id: String
        let operation: String
        let startedMs: Int64
    }

    private struct SyscallEvent {
        let id: String
        let requestID: String?
        let operation: String
        let phase: String
        let tsMs: Int64
        let errno: Int32?
    }

    private var inFlight: [String: InFlightRequest] = [:]
    private var inFlightSyscalls: [String: SyscallEvent] = [:]

    func beginRequest(operation: String) -> String {
        let spanID = newULID()
        let requestID = "request:\(spanID)"
        let startedMs = Self.nowMs()
        inFlight[requestID] = InFlightRequest(
            spanID: spanID, id: requestID, operation: operation, startedMs: startedMs)
        inFlightSyscalls[requestID] = SyscallEvent(
            id: "syscall:\(newULID())",
            requestID: requestID,
            operation: operation,
            phase: "begin",
            tsMs: startedMs,
            errno: nil
        )
        return requestID
    }

    func finishRequest(requestID: String, operation: String, errno: Int32?) {
        _ = operation
        _ = errno
        inFlightSyscalls.removeValue(forKey: requestID)
        inFlight.removeValue(forKey: requestID)
    }

    func snapshot(processName: String, pid: UInt32) -> PeepsGraphSnapshot {
        var nodes: [PeepsGraphNode] = []
        var edges: [PeepsGraphEdge] = []

        for req in inFlight.values {
            let attrs = encodeAttrs(
                PeepsRequestAttrs(
                    spanID: req.spanID,
                    operation: req.operation,
                    startedMs: req.startedMs
                ))
            nodes.append(
                PeepsGraphNode(
                    id: req.id,
                    kind: "Request",
                    label: req.operation,
                    attrsJSON: attrs
                )
            )
        }

        for syscall in inFlightSyscalls.values {
            let attrs = encodeAttrs(
                PeepsSyscallAttrs(
                    operation: syscall.operation,
                    phase: syscall.phase,
                    tsMs: syscall.tsMs,
                    errno: syscall.errno,
                    requestID: syscall.requestID
                )
            )
            nodes.append(
                PeepsGraphNode(
                    id: syscall.id,
                    kind: "Syscall",
                    label: syscall.operation,
                    attrsJSON: attrs
                )
            )
            if let requestID = syscall.requestID {
                edges.append(
                    PeepsGraphEdge(
                        src: requestID,
                        dst: syscall.id,
                        kind: "Touches",
                        attrsJSON: "{}"
                    )
                )
                edges.append(
                    PeepsGraphEdge(
                        src: syscall.id,
                        dst: requestID,
                        kind: "Touches",
                        attrsJSON: "{}"
                    )
                )
                if syscall.phase == "begin" {
                    edges.append(
                        PeepsGraphEdge(
                            src: requestID,
                            dst: syscall.id,
                            kind: "Needs",
                            attrsJSON: "{}"
                        )
                    )
                }
            }
        }

        return PeepsGraphSnapshot(
            processName: processName,
            procKey: canonicalProcKey(processName: processName, pid: pid),
            nodes: nodes,
            edges: edges
        )
    }

    private func encodeAttrs<T: Encodable>(_ attrs: T) -> String {
        let encoder = JSONEncoder()
        if let data = try? encoder.encode(attrs), let str = String(data: data, encoding: .utf8) {
            return str
        }
        return "{}"
    }

    private static func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }
}

actor SwiftPeepsClient {
    static let shared = SwiftPeepsClient()

    private let log = Logger(subsystem: VixenFSConstants.subsystem, category: "SwiftPeeps")
    private let state = SwiftPeepsGraphState()
    private var loopTask: Task<Void, Never>?
    private var configuredAddr: String?
    private var debugFrames = false

    private let processName = "vixenfs-swift"
    private let pid = getpid()

    func configure(dashboardAddr: String?, debugFrames: Bool) {
        let trimmed = dashboardAddr?.trimmingCharacters(in: .whitespacesAndNewlines)
        let normalized = (trimmed?.isEmpty == false) ? trimmed : nil
        self.debugFrames = debugFrames

        if normalized == configuredAddr {
            return
        }

        loopTask?.cancel()
        loopTask = nil
        configuredAddr = normalized

        guard let addr = normalized else {
            log.info("Swift peeps pull loop disabled")
            return
        }

        log.info("Swift peeps pull loop enabled addr=\(addr, privacy: .public)")
        let state = self.state
        let processName = self.processName
        let pid = self.pid
        let logger = self.log
        let debugEnabled = self.debugFrames

        loopTask = Task.detached(priority: .background) {
            while !Task.isCancelled {
                do {
                    try await Self.runPullSession(
                        addr: addr,
                        state: state,
                        processName: processName,
                        pid: pid,
                        log: logger,
                        debugFrames: debugEnabled
                    )
                } catch {
                    logger.error("Swift peeps session error: \(String(describing: error), privacy: .public)")
                }

                if Task.isCancelled {
                    break
                }
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }

    func beginRequest(operation: String) async -> String {
        await state.beginRequest(operation: operation)
    }

    func finishRequest(requestID: String, operation: String, errno: Int32?) async {
        await state.finishRequest(requestID: requestID, operation: operation, errno: errno)
    }

    private static func runPullSession(
        addr: String,
        state: SwiftPeepsGraphState,
        processName: String,
        pid: Int32,
        log: Logger,
        debugFrames: Bool
    ) async throws {
        let (host, port) = try parseDashboardAddress(addr)
        let fd = try connectTCP(host: host, port: port)
        defer { _ = Darwin.close(fd) }

        log.info("Swift peeps connected host=\(host, privacy: .public) port=\(port, privacy: .public)")

        while !Task.isCancelled {
            guard let lenData = try readExact(fd: fd, count: 4) else {
                return
            }
            let frameLen = Int(
                UInt32(bigEndian: lenData.withUnsafeBytes { raw in
                    raw.load(as: UInt32.self)
                }))
            if frameLen <= 0 || frameLen > PeepsWire.maxFrameSize {
                throw NSError(
                    domain: "SwiftPeeps", code: 2,
                    userInfo: [NSLocalizedDescriptionKey: "invalid frame length \(frameLen)"])
            }
            guard let payload = try readExact(fd: fd, count: frameLen) else {
                return
            }

            if debugFrames, let payloadStr = String(data: payload, encoding: .utf8) {
                log.notice("Swift peeps request payload=\(payloadStr, privacy: .public)")
            }

            let request: PeepsSnapshotRequest
            do {
                request = try JSONDecoder().decode(PeepsSnapshotRequest.self, from: payload)
            } catch {
                log.error("Swift peeps decode request failed: \(String(describing: error), privacy: .public)")
                continue
            }

            guard request.type == "snapshot_request" else {
                continue
            }
            log.info(
                "Swift peeps snapshot_request snapshot_id=\(request.snapshotID) timeout_ms=\(request.timeoutMs)"
            )

            let graph = await state.snapshot(processName: processName, pid: UInt32(pid))
            log.info(
                "Swift peeps graph_reply snapshot_id=\(request.snapshotID) nodes=\(graph.nodes.count) edges=\(graph.edges.count)"
            )
            let reply = PeepsGraphReply(
                type: "graph_reply",
                snapshotID: request.snapshotID,
                process: processName,
                pid: UInt32(pid),
                graph: graph
            )
            let replyPayload = try JSONEncoder().encode(reply)
            if debugFrames, let replyStr = String(data: replyPayload, encoding: .utf8) {
                log.notice("Swift peeps reply payload=\(replyStr, privacy: .public)")
            }
            try writeFrame(fd: fd, payload: replyPayload)
        }
    }

    private static func parseDashboardAddress(_ raw: String) throws -> (String, String) {
        if raw.hasPrefix("[") {
            guard let close = raw.firstIndex(of: "]") else {
                throw NSError(domain: "SwiftPeeps", code: 3, userInfo: [NSLocalizedDescriptionKey: "invalid IPv6 dashboard address"])
            }
            let host = String(raw[raw.index(after: raw.startIndex)..<close])
            let remainder = raw[close...]
            guard remainder.hasPrefix("]:") else {
                throw NSError(domain: "SwiftPeeps", code: 3, userInfo: [NSLocalizedDescriptionKey: "missing port in dashboard address"])
            }
            let port = String(remainder.dropFirst(2))
            return (host, port)
        }

        guard let idx = raw.lastIndex(of: ":") else {
            throw NSError(domain: "SwiftPeeps", code: 3, userInfo: [NSLocalizedDescriptionKey: "missing port in dashboard address"])
        }
        let host = String(raw[..<idx])
        let port = String(raw[raw.index(after: idx)...])
        if host.isEmpty || port.isEmpty {
            throw NSError(domain: "SwiftPeeps", code: 3, userInfo: [NSLocalizedDescriptionKey: "invalid dashboard address"])
        }
        return (host, port)
    }

    private static func connectTCP(host: String, port: String) throws -> Int32 {
        var hints = addrinfo(
            ai_flags: 0,
            ai_family: AF_UNSPEC,
            ai_socktype: SOCK_STREAM,
            ai_protocol: IPPROTO_TCP,
            ai_addrlen: 0,
            ai_canonname: nil,
            ai_addr: nil,
            ai_next: nil
        )
        var result: UnsafeMutablePointer<addrinfo>?
        let rc = getaddrinfo(host, port, &hints, &result)
        guard rc == 0, let first = result else {
            throw NSError(domain: "SwiftPeeps", code: 4, userInfo: [NSLocalizedDescriptionKey: String(cString: gai_strerror(rc))])
        }
        defer { freeaddrinfo(first) }

        var ptr: UnsafeMutablePointer<addrinfo>? = first
        while let ai = ptr {
            let fd = socket(ai.pointee.ai_family, ai.pointee.ai_socktype, ai.pointee.ai_protocol)
            if fd >= 0 {
                var yes: Int32 = 1
                _ = withUnsafePointer(to: &yes) {
                    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, $0, socklen_t(MemoryLayout<Int32>.size))
                }
                if Darwin.connect(fd, ai.pointee.ai_addr, ai.pointee.ai_addrlen) == 0 {
                    return fd
                }
                _ = Darwin.close(fd)
            }
            ptr = ai.pointee.ai_next
        }

        throw NSError(domain: "SwiftPeeps", code: 5, userInfo: [NSLocalizedDescriptionKey: "failed to connect TCP socket"])
    }

    private static func readExact(fd: Int32, count: Int) throws -> Data? {
        var out = Data(count: count)
        var offset = 0
        while offset < count {
            let readCount = out.withUnsafeMutableBytes { rawBuf -> Int in
                guard let base = rawBuf.baseAddress else { return -1 }
                let ptr = base.advanced(by: offset)
                return Darwin.recv(fd, ptr, count - offset, 0)
            }
            if readCount == 0 {
                return offset == 0 ? nil : {
                    return nil
                }()
            }
            if readCount < 0 {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
            }
            offset += readCount
        }
        return out
    }

    private static func writeFrame(fd: Int32, payload: Data) throws {
        var len = UInt32(payload.count).bigEndian
        try withUnsafeBytes(of: &len) { lenBuf in
            try writeAll(fd: fd, buffer: lenBuf)
        }
        try payload.withUnsafeBytes { payloadBuf in
            try writeAll(fd: fd, buffer: payloadBuf)
        }
    }

    private static func writeAll(fd: Int32, buffer: UnsafeRawBufferPointer) throws {
        guard let base = buffer.baseAddress else {
            return
        }
        var offset = 0
        let total = buffer.count
        while offset < total {
            let written = Darwin.send(fd, base.advanced(by: offset), total - offset, 0)
            if written <= 0 {
                throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno))
            }
            offset += written
        }
    }

}

private func canonicalProcKey(processName: String, pid: UInt32) -> String {
    let slug = sanitizeIDSegment(processName)
    return "\(slug)-\(pid)"
}

private func sanitizeIDSegment(_ raw: String) -> String {
    String(raw.map { ch in
        if ch.isASCII {
            if ch.isLowercase || ch.isNumber || ch == "." || ch == "_" || ch == "-" {
                return ch
            }
            if ch.isUppercase {
                return Character(ch.lowercased())
            }
        }
        return "-"
    })
}

private let ulidAlphabet: [Character] = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

private func newULID() -> String {
    var randomBytes = [UInt8](repeating: 0, count: 10)
    arc4random_buf(&randomBytes, randomBytes.count)

    let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
    var bytes = [UInt8](repeating: 0, count: 16)
    bytes[0] = UInt8((nowMs >> 40) & 0xFF)
    bytes[1] = UInt8((nowMs >> 32) & 0xFF)
    bytes[2] = UInt8((nowMs >> 24) & 0xFF)
    bytes[3] = UInt8((nowMs >> 16) & 0xFF)
    bytes[4] = UInt8((nowMs >> 8) & 0xFF)
    bytes[5] = UInt8(nowMs & 0xFF)
    for i in 0..<10 {
        bytes[6 + i] = randomBytes[i]
    }

    return encodeULIDBytes(bytes)
}

private func encodeULIDBytes(_ bytes: [UInt8]) -> String {
    precondition(bytes.count == 16, "ULID must be 16 bytes")

    let v0 = (UInt32(bytes[0]) << 24) | (UInt32(bytes[1]) << 16) | (UInt32(bytes[2]) << 8)
        | UInt32(bytes[3])
    let v1 = (UInt32(bytes[4]) << 24) | (UInt32(bytes[5]) << 16) | (UInt32(bytes[6]) << 8)
        | UInt32(bytes[7])
    let v2 = (UInt32(bytes[8]) << 24) | (UInt32(bytes[9]) << 16) | (UInt32(bytes[10]) << 8)
        | UInt32(bytes[11])
    let v3 = (UInt32(bytes[12]) << 24) | (UInt32(bytes[13]) << 16) | (UInt32(bytes[14]) << 8)
        | UInt32(bytes[15])

    let i0 = Int((v0 >> 27) & 0x1F)
    let i1 = Int((v0 >> 22) & 0x1F)
    let i2 = Int((v0 >> 17) & 0x1F)
    let i3 = Int((v0 >> 12) & 0x1F)
    let i4 = Int((v0 >> 7) & 0x1F)
    let i5 = Int((v0 >> 2) & 0x1F)
    let i6 = Int(((v0 & 0x03) << 3) | ((v1 >> 29) & 0x07))
    let i7 = Int((v1 >> 24) & 0x1F)
    let i8 = Int((v1 >> 19) & 0x1F)
    let i9 = Int((v1 >> 14) & 0x1F)
    let i10 = Int((v1 >> 9) & 0x1F)
    let i11 = Int((v1 >> 4) & 0x1F)
    let i12 = Int(((v1 & 0x0F) << 1) | ((v2 >> 31) & 0x01))
    let i13 = Int((v2 >> 26) & 0x1F)
    let i14 = Int((v2 >> 21) & 0x1F)
    let i15 = Int((v2 >> 16) & 0x1F)
    let i16 = Int((v2 >> 11) & 0x1F)
    let i17 = Int((v2 >> 6) & 0x1F)
    let i18 = Int((v2 >> 1) & 0x1F)
    let i19 = Int(((v2 & 0x01) << 4) | ((v3 >> 28) & 0x0F))
    let i20 = Int((v3 >> 23) & 0x1F)
    let i21 = Int((v3 >> 18) & 0x1F)
    let i22 = Int((v3 >> 13) & 0x1F)
    let i23 = Int((v3 >> 8) & 0x1F)
    let i24 = Int((v3 >> 3) & 0x1F)
    let i25 = Int((v3 & 0x07) << 2)

    let indices = [
        i0, i1, i2, i3, i4, i5, i6, i7, i8, i9, i10, i11, i12,
        i13, i14, i15, i16, i17, i18, i19, i20, i21, i22, i23, i24, i25,
    ]
    return String(indices.map { ulidAlphabet[$0] })
}
