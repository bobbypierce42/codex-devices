import Foundation
import CryptoKit

public enum ServiceError: String, Error, LocalizedError, Sendable {
    case invalidInput, unavailable, refused, timeout, canceled, oversized, invalidResponse, authentication, changedHostKey, trustRejected, drift, uncertain
    public var errorDescription: String? {
        switch self {
        case .invalidInput: "Check the host, username, and proposed name."
        case .unavailable: "This service is unavailable. Check its availability and try again."
        case .refused: "The operation could not be validated. No new request was authorized."
        case .timeout: "The operation timed out. Check readback before attempting another repair."
        case .canceled: "The operation was canceled."
        case .oversized: "The response exceeded the supported size limit."
        case .invalidResponse: "The response did not match the supported format."
        case .authentication: "An existing ChatGPT sign-in is required. Check the selected desktop app."
        case .changedHostKey: "This Mac's SSH host key changed. Inspection was blocked."
        case .trustRejected: "SSH trust was declined. The Mac was not inspected."
        case .drift: "The account, Mac identity, or registry changed. Review the current state."
        case .uncertain: "The repair outcome is uncertain. Check readback; the request will not be repeated."
        }
    }
}

public struct HostCandidate: Identifiable, Hashable, Sendable {
    public var id: String { host }
    public let host: String
    public let name: String
    public let source: String
    public let isLocal: Bool
    public init(host: String, name: String? = nil, source: String = "manual", isLocal: Bool = false) {
        self.host = host; self.name = name ?? host; self.source = source; self.isLocal = isLocal
    }
}
public struct PeerInspection: Sendable, Equatable {
    public let candidate: HostCandidate
    public let computerName: String
    public let installationIDHash: String
    public let hostKeyFingerprint: String?
    public init(candidate: HostCandidate, computerName: String, installationIDHash: String, hostKeyFingerprint: String? = nil) {
        self.candidate = candidate; self.computerName = computerName; self.installationIDHash = installationIDHash; self.hostKeyFingerprint = hostKeyFingerprint
    }
}
public struct DeviceOccurrence: Sendable, Equatable {
    public let dotID: String
    public let threadID: String
    public let record: JSONValue
}
public struct LogicalDevice: Identifiable, Sendable, Equatable {
    public var id: String { environmentID }
    public let environmentID: String
    public let displayName: String
    public let installationIDHash: String
    public let kind: String
    public let computerType: String
    public let occurrences: [DeviceOccurrence]
    let installationID: String
}
public struct ReviewSelection: Codable, Sendable, Equatable {
    public let environmentID: String
    public let newName: String
    public init(environmentID: String, newName: String) { self.environmentID = environmentID; self.newName = newName }
    enum CodingKeys: String, CodingKey { case environmentID = "environment_id", newName = "new_name" }
}
public struct OperationReceipt: Codable, Sendable, Equatable {
    public struct Record: Codable, Sendable, Equatable, Identifiable {
        public var id: String { environmentID }
        public let environmentID: String
        public let installationIDHash: String
        public let oldName: String
        public let newName: String
        public let state: String
        enum CodingKeys: String, CodingKey { case environmentID = "environment_id", installationIDHash = "installation_id_sha256", oldName = "old_name", newName = "new_name", state }
    }
    public let schemaVersion: Int
    public let op: String
    public let status: String
    public let operationDirectory: String
    public let planSHA256: String
    public let records: [Record]
    public let reasonCode: String
    public let evidenceMode: String?
    enum CodingKeys: String, CodingKey { case schemaVersion = "schema_version", op, status, operationDirectory = "operation_directory", planSHA256 = "plan_sha256", records, reasonCode = "reason_code", evidenceMode = "evidence_mode" }
}
public enum RepairState: String, Sendable { case idle, scanning, inspecting, ready, preparing, review, applying, verified, uncertain, failed }
public struct HostTrustChallenge: Sendable { public let host: String; public let fingerprint: String }
public struct SourceFailure: Sendable, Equatable { public let source: String; public let error: ServiceError }
public struct DiscoveryResult: Sendable { public let candidates: [HostCandidate]; public let failures: [SourceFailure] }

public enum JSONValue: Codable, Sendable, Equatable {
    case object([String: JSONValue]), array([JSONValue]), string(String), number(Double), bool(Bool), null
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let x = try? c.decode(Bool.self) { self = .bool(x) }
        else if let x = try? c.decode(String.self) { self = .string(x) }
        else if let x = try? c.decode([String: JSONValue].self) { self = .object(x) }
        else if let x = try? c.decode([JSONValue].self) { self = .array(x) }
        else { self = .number(try c.decode(Double.self)) }
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self { case .object(let v): try c.encode(v); case .array(let v): try c.encode(v); case .string(let v): try c.encode(v); case .number(let v): try c.encode(v); case .bool(let v): try c.encode(v); case .null: try c.encodeNil() }
    }
    public subscript(_ key: String) -> JSONValue { if case .object(let x) = self { return x[key] ?? .null }; return .null }
    public var string: String? { if case .string(let x) = self { return x }; return nil }
    public var array: [JSONValue]? { if case .array(let x) = self { return x }; return nil }
    static func parse(_ data: Data) throws -> JSONValue {
        guard data.count <= 4 * 1024 * 1024 else { throw ServiceError.oversized }
        do { return try JSONDecoder().decode(Self.self, from: data) } catch { throw ServiceError.invalidResponse }
    }
    func data() throws -> Data { try JSONEncoder().encode(self) }
}
func hash(_ string: String) -> String { SHA256.hash(data: Data(string.utf8)).map { String(format: "%02x", $0) }.joined() }
func isHash(_ value: String) -> Bool { value.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) } }
