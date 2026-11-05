import Foundation
import XCTest
@testable import DeviceServices

let fixtureInstallation = "12345678-1234-1234-1234-123456789abc"
let fixtureHost = HostCandidate(host: "studio.example", name: "Studio")
func fixtureAccount(_ name: String = "invented-account") throws -> AccountSession {
    let payload = try JSONValue.object(["https://api.openai.com/auth": .object(["chatgpt_account_id": .string(name)])]).data().base64EncodedString()
    return try AccountSession(token: "invented.\(payload).signature")
}
func fixtureCapture(name: String = "Old Studio", secondName: String? = nil, environment: String = "env-studio", duplicateID: String? = nil, rootThread: String = "thread-one", nextCursor: JSONValue = .null) throws -> RegistryCapture {
    let rows = ["dot~one", "dot~two"].enumerated().map { index, dot -> RegistryCapture.Scope in
        var computers: [JSONValue] = [.object(["environment_id": .string(environment), "installation_id": .string(fixtureInstallation), "kind": .string("user"), "computer_type": .string("app_connection"), "display_name": .string(index == 1 ? secondName ?? name : name), "status": .string("connected"), "grants": .array([.string(index == 0 ? "owner" : "viewer")]), "attached": .bool(index == 0), "unknown": .object(["preserved": .bool(true)])])]
        if let duplicateID { computers.append(.object(["environment_id": .string(duplicateID), "installation_id": .string(fixtureInstallation), "kind": .string("user"), "computer_type": .string("app_connection"), "display_name": .string("Duplicate"), "status": .string("connected")])) }
        let body = JSONValue.object(["thread_id": .string(index == 0 ? "thread-one" : "thread-two"), "computers": .array(computers), "next_cursor": nextCursor])
        return .init(dotID: dot, threadID: index == 0 ? "thread-one" : "thread-two", computersResponse: String(data: try! body.data(), encoding: .utf8)!)
    }
    let root: JSONValue = .object(["items": .array([.object(["id": .string("dot~one"), "active_root_thread_id": .string(rootThread), "display_name": .string("One")]), .object(["id": .string("dot~two"), "active_root_thread_id": .string("thread-two"), "display_name": .string("Two")])]), "cursor": .null])
    return RegistryCapture(accountIDHash: try fixtureAccount().accountIDHash, rootResponse: String(data: try root.data(), encoding: .utf8)!, scopes: rows)
}
func temporaryDirectory() throws -> URL {
    let path = URL(fileURLWithPath: "/private/tmp/codex-devices-services-tests-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: path, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
    return path
}
func assertServiceError(_ expected: ServiceError, _ body: () async throws -> Void, file: StaticString = #filePath, line: UInt = #line) async {
    do { try await body(); XCTFail("Expected \(expected)", file: file, line: line) }
    catch { XCTAssertEqual(error as? ServiceError, expected, file: file, line: line) }
}
actor HTTPFixture {
    var requests = [HTTPRequest]()
    var replies: [HTTPResult]
    var error: ServiceError?
    init(_ replies: [HTTPResult], error: ServiceError? = nil) { self.replies = replies; self.error = error }
    func send(_ request: HTTPRequest) throws -> HTTPResult {
        requests.append(request)
        if let error { throw error }
        guard !replies.isEmpty else { throw ServiceError.unavailable }
        return replies.removeFirst()
    }
    func count() -> Int { requests.count }
}
actor RepairFixture {
    var name = "Old Studio"
    var events = [String]()
    var ambiguous = false
    var stale = false
    var intentRefused = false
    var accountDrift = false
    var calls = 0
    var duplicate = false
    var directory = ""
    var selectedName = "Studio"
    var verified = false
    var intent = false
    let digest = String(repeating: "a", count: 64)
    func configure(ambiguous: Bool = false, stale: Bool = false, intentRefused: Bool = false, accountDrift: Bool = false, duplicate: Bool = false) {
        self.ambiguous = ambiguous; self.stale = stale; self.intentRefused = intentRefused; self.accountDrift = accountDrift; self.duplicate = duplicate
    }
    nonisolated var providers: ServiceProviders {
        ServiceProviders(discover: { _ in DiscoveryResult(candidates: [fixtureHost], failures: []) }, inspect: { candidate, _ in PeerInspection(candidate: candidate, computerName: "Studio", installationIDHash: DeviceServices.hash(fixtureInstallation), hostKeyFingerprint: "SHA256:fixture") }, account: { try await self.account() }, capture: { _ in try await self.capture() }, rename: { _, _, name in try await self.rename(name) }, engine: { try await self.engine($0) })
    }
    func account() throws -> AccountSession { calls += 1; return try fixtureAccount(accountDrift && calls > 2 ? "different-account" : "invented-account") }
    func capture() throws -> RegistryCapture { try fixtureCapture(name: name, secondName: stale && intent ? "Old Studio" : nil, duplicateID: duplicate ? "other-env" : nil) }
    func rename(_ name: String) throws {
        events.append("POST"); if !stale { self.name = name }
        if ambiguous { throw ServiceError.timeout }
    }
    func log() -> [String] { events }
    func settleName(_ value: String) { name = value; stale = false }
    func engine(_ request: JSONValue) throws -> OperationReceipt {
        let op = request["op"].string ?? ""; events.append(op)
        var status = "refused"
        switch op {
        case "prepare": directory = request["operation_directory"].string ?? ""; selectedName = request["selections"].array?.first?["new_name"].string ?? ""; status = "prepared"
        case "preflight": status = "ready"
        case "intent": status = intentRefused || intent ? "refused" : "intent-recorded"; intent = true
        case "reconcile": verified = !stale && name == selectedName; status = verified ? "verified" : "uncertain"
        case "status": status = verified ? "verified" : "uncertain"
        default: break
        }
        return try JSONDecoder().decode(OperationReceipt.self, from: JSONValue.object(["schema_version": .number(1), "op": .string(op), "status": .string(status), "operation_directory": .string(directory), "plan_sha256": .string(digest), "reason_code": .string("fixture"), "records": .array([.object(["environment_id": .string("env-studio"), "installation_id_sha256": .string(DeviceServices.hash(fixtureInstallation)), "old_name": .string("Old Studio"), "new_name": .string(selectedName), "state": .string(verified ? "verified" : intent ? "intent-recorded" : "pending")])])]).data())
    }
}
