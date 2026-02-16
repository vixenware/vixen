import Foundation

enum PeepsRequestContext {
    @TaskLocal static var spanID: String?

    static func spanID(fromRequestID requestID: String) -> String? {
        let prefix = "request:"
        guard requestID.hasPrefix(prefix) else {
            return nil
        }
        let span = String(requestID.dropFirst(prefix.count))
        return span.isEmpty ? nil : span
    }
}

