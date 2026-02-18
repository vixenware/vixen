import Darwin
import Foundation
import os

private enum PeepsWire {
    static let maxFrameSize = 128 * 1024 * 1024
}

private struct PeepsSnapshotRequest: Decodable {
    let snapshotID: Int64
    let timeoutMs: Int64

    enum CodingKeys: String, CodingKey {
        case snapshotID = "snapshot_id"
        case timeoutMs = "timeout_ms"
    }
}

private struct PeepsCutRequest: Decodable {
    let cutID: String

    enum CodingKeys: String, CodingKey {
        case cutID = "cut_id"
    }
}

private struct PeepsServerMessage: Decodable {
    let snapshotRequest: PeepsSnapshotRequest?
    let cutRequest: PeepsCutRequest?

    enum CodingKeys: String, CodingKey {
        case snapshotRequest = "snapshot_request"
        case cutRequest = "cut_request"
    }
}

private struct PeepsHandshake: Encodable {
    let processName: String
    let pid: UInt32

    enum CodingKeys: String, CodingKey {
        case processName = "process_name"
        case pid
    }
}

private struct PeepsClientHandshakeMessage: Encodable {
    let handshake: PeepsHandshake
}

private struct PeepsSnapshotReply: Encodable {
    let snapshotID: Int64
    let ptimeNowMs: UInt64
    let snapshot: PeepsSnapshot

    enum CodingKeys: String, CodingKey {
        case snapshotID = "snapshot_id"
        case ptimeNowMs = "ptime_now_ms"
        case snapshot
    }
}

private struct PeepsClientSnapshotReplyMessage: Encodable {
    let snapshotReply: PeepsSnapshotReply

    enum CodingKeys: String, CodingKey {
        case snapshotReply = "snapshot_reply"
    }
}

private struct PeepsCursor: Encodable {
    let streamID: String
    let nextSeqNo: UInt64

    enum CodingKeys: String, CodingKey {
        case streamID = "stream_id"
        case nextSeqNo = "next_seq_no"
    }
}

private struct PeepsCutAck: Encodable {
    let cutID: String
    let cursor: PeepsCursor

    enum CodingKeys: String, CodingKey {
        case cutID = "cut_id"
        case cursor
    }
}

private struct PeepsClientCutAckMessage: Encodable {
    let cutAck: PeepsCutAck

    enum CodingKeys: String, CodingKey {
        case cutAck = "cut_ack"
    }
}

private struct PeepsSnapshot: Encodable {
    let entities: [PeepsEntity]
    let scopes: [PeepsScope]
    let edges: [PeepsEdge]
    let events: [PeepsEvent]
}

private struct PeepsEntity: Encodable {
    let id: String
    let birth: UInt64
    let source: String
    let krate: String?
    let name: String
    let body: PeepsEntityBody
    let meta: [String: String]
}

private enum PeepsEntityBody: Encodable {
    case request(method: String, argsPreview: String)
    case fileOp(op: String, path: String)

    private struct RequestBody: Encodable {
        let method: String
        let argsPreview: String

        enum CodingKeys: String, CodingKey {
            case method
            case argsPreview = "args_preview"
        }
    }

    private struct FileOpBody: Encodable {
        let op: String
        let path: String
    }

    private enum CodingKeys: String, CodingKey {
        case request
        case fileOp = "file_op"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .request(method, argsPreview):
            try container.encode(RequestBody(method: method, argsPreview: argsPreview), forKey: .request)
        case let .fileOp(op, path):
            try container.encode(FileOpBody(op: op, path: path), forKey: .fileOp)
        }
    }
}

private struct PeepsScope: Encodable {}
private struct PeepsEvent: Encodable {}

private struct PeepsEdge: Encodable {
    let src: String
    let dst: String
    let kind: String
    let meta: [String: String]
}

private actor SwiftPeepsGraphState {
    private static let source = "/apps/mac/xcode/VixenFS/PeepsBridge.swift"
    private static let processStartUptimeNs = DispatchTime.now().uptimeNanoseconds

    static func ptimeNowMs() -> UInt64 {
        (DispatchTime.now().uptimeNanoseconds - processStartUptimeNs) / 1_000_000
    }

    private struct InFlightRequest {
        let spanID: String
        let id: String
        let operation: String
        let birthPtimeMs: UInt64
    }

    private struct SyscallEvent {
        let id: String
        let requestID: String?
        let operation: String
        let phase: String
        let birthPtimeMs: UInt64
        let errno: Int32?
    }

    private var inFlight: [String: InFlightRequest] = [:]
    private var inFlightSyscalls: [String: SyscallEvent] = [:]

    func beginRequest(operation: String) -> String {
        let spanID = newULID()
        let requestID = "request:\(spanID)"
        let birthPtimeMs = Self.ptimeNowMs()
        inFlight[requestID] = InFlightRequest(
            spanID: spanID,
            id: requestID,
            operation: operation,
            birthPtimeMs: birthPtimeMs
        )
        inFlightSyscalls[requestID] = SyscallEvent(
            id: "syscall:\(newULID())",
            requestID: requestID,
            operation: operation,
            phase: "begin",
            birthPtimeMs: birthPtimeMs,
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

    func snapshot() -> PeepsSnapshot {
        var entities: [PeepsEntity] = []
        var edges: [PeepsEdge] = []

        for req in inFlight.values {
            entities.append(
                PeepsEntity(
                    id: req.id,
                    birth: req.birthPtimeMs,
                    source: Self.source,
                    krate: nil,
                    name: req.operation,
                    body: .request(method: req.operation, argsPreview: "(swift bridge)"),
                    meta: ["span_id": req.spanID]
                )
            )
        }

        for syscall in inFlightSyscalls.values {
            var meta = ["phase": syscall.phase]
            if let errno = syscall.errno {
                meta["errno"] = String(errno)
            }
            entities.append(
                PeepsEntity(
                    id: syscall.id,
                    birth: syscall.birthPtimeMs,
                    source: Self.source,
                    krate: nil,
                    name: syscall.operation,
                    body: .fileOp(op: "other", path: syscall.operation),
                    meta: meta
                )
            )
            if let requestID = syscall.requestID {
                edges.append(PeepsEdge(src: requestID, dst: syscall.id, kind: "touches", meta: [:]))
                edges.append(PeepsEdge(src: syscall.id, dst: requestID, kind: "touches", meta: [:]))
                if syscall.phase == "begin" {
                    edges.append(PeepsEdge(src: requestID, dst: syscall.id, kind: "needs", meta: [:]))
                }
            }
        }

        return PeepsSnapshot(entities: entities, scopes: [], edges: edges, events: [])
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
        let handshakePayload = try JSONEncoder().encode(
            PeepsClientHandshakeMessage(
                handshake: PeepsHandshake(processName: processName, pid: UInt32(pid))
            )
        )
        if debugFrames, let handshakeStr = String(data: handshakePayload, encoding: .utf8) {
            log.notice("Swift peeps handshake payload=\(handshakeStr, privacy: .public)")
        }
        try writeFrame(fd: fd, payload: handshakePayload)

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

            let message: PeepsServerMessage
            do {
                message = try JSONDecoder().decode(PeepsServerMessage.self, from: payload)
            } catch {
                log.error("Swift peeps decode request failed: \(String(describing: error), privacy: .public)")
                continue
            }

            if let request = message.snapshotRequest {
                log.info(
                    "Swift peeps snapshot_request snapshot_id=\(request.snapshotID) timeout_ms=\(request.timeoutMs)"
                )

                let snapshot = await state.snapshot()
                log.info(
                    "Swift peeps snapshot_reply snapshot_id=\(request.snapshotID) entities=\(snapshot.entities.count) edges=\(snapshot.edges.count)"
                )

                let replyPayload = try JSONEncoder().encode(
                    PeepsClientSnapshotReplyMessage(
                        snapshotReply: PeepsSnapshotReply(
                            snapshotID: request.snapshotID,
                            ptimeNowMs: SwiftPeepsGraphState.ptimeNowMs(),
                            snapshot: snapshot
                        )
                    )
                )
                if debugFrames, let replyStr = String(data: replyPayload, encoding: .utf8) {
                    log.notice("Swift peeps reply payload=\(replyStr, privacy: .public)")
                }
                try writeFrame(fd: fd, payload: replyPayload)
                continue
            }

            if let cut = message.cutRequest {
                let streamID = "\(processName)-\(UInt32(pid))"
                let ackPayload = try JSONEncoder().encode(
                    PeepsClientCutAckMessage(
                        cutAck: PeepsCutAck(
                            cutID: cut.cutID,
                            cursor: PeepsCursor(streamID: streamID, nextSeqNo: 0)
                        )
                    )
                )
                if debugFrames, let ackStr = String(data: ackPayload, encoding: .utf8) {
                    log.notice("Swift peeps cut_ack payload=\(ackStr, privacy: .public)")
                }
                try writeFrame(fd: fd, payload: ackPayload)
                continue
            }
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
