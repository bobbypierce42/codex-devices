import Foundation
import XCTest
@testable import DeviceServices

final class RegistryTests: XCTestCase {
    func testCrossDotRepeatedIDsPreserveIndependentGrantsAndUnknownFields() throws {
        let devices = try fixtureCapture().devices()
        XCTAssertEqual(devices.count, 1)
        XCTAssertEqual(devices[0].occurrences.count, 2)
        XCTAssertNotEqual(devices[0].occurrences[0].record["grants"], devices[0].occurrences[1].record["grants"])
        XCTAssertEqual(devices[0].occurrences[0].record["unknown"]["preserved"], .bool(true))
    }
    func testGlobalConflictDuplicateAndThreadDriftRefuse() throws {
        XCTAssertThrowsError(try fixtureCapture(secondName: "Wrong").devices())
        XCTAssertThrowsError(try fixtureCapture(duplicateID: "env-studio").devices())
        XCTAssertThrowsError(try fixtureCapture(rootThread: "moved-thread").devices())
        XCTAssertThrowsError(try fixtureCapture(nextCursor: .string("more")).devices())
    }
    func testExactCompleteCaptureAndAllowedNameOnlyRequest() async throws {
        let fixture = try fixtureCapture()
        let replies = [HTTPResult(status: 200, data: Data(fixture.rootResponse.utf8))] + fixture.scopes.map { HTTPResult(status: 200, data: Data($0.computersResponse.utf8)) } + [.init(status: 200, data: Data("{}".utf8))]
        let http = HTTPFixture(replies)
        let client = RegistryClient(transport: { try await http.send($0) })
        let captured = try await client.capture(account: fixtureAccount())
        XCTAssertEqual(captured.rootResponse, fixture.rootResponse)
        XCTAssertEqual(captured.scopes.map(\.computersResponse), fixture.scopes.map(\.computersResponse))
        try await client.rename(account: fixtureAccount(), installationID: fixtureInstallation, newName: "Studio")
        let requests = await http.requests
        XCTAssertEqual(requests.count, 4)
        XCTAssertEqual(requests[0].headers["ChatGPT-Account-Id"], "invented-account")
        XCTAssertEqual(requests[0].headers["User-Agent"], "CodexDevices/0.1")
        XCTAssertEqual(requests[0].url.absoluteString, "https://chatgpt.com/backend-api/tbo?limit=25&include_room_preview=false")
        XCTAssertEqual(requests[1].url.path, "/backend-api/tbo/dot~one/computers")
        XCTAssertEqual(URLComponents(url: requests[1].url, resolvingAgainstBaseURL: false)?.queryItems, [URLQueryItem(name: "expected_thread_id", value: "thread-one")])
        XCTAssertEqual(requests[3].method, "POST")
        XCTAssertEqual(requests[3].url.path, "/backend-api/flora/cca/executor")
        XCTAssertEqual(try JSONValue.parse(requests[3].body!), .object(["installation_id": .string(fixtureInstallation), "custom_display_name": .string("Studio")]))
    }
    func testErrorsAreRedactedAndNoPaginationGuesses() async throws {
        for status in [401, 403, 302, 500] {
            let http = HTTPFixture([.init(status: status, data: Data("secret body must not appear".utf8))])
            let client = RegistryClient(transport: { try await http.send($0) })
            do { _ = try await client.capture(account: fixtureAccount()); XCTFail("Expected refusal") }
            catch { XCTAssertFalse(error.localizedDescription.contains("secret")) }
            let count = await http.count(); XCTAssertEqual(count, 1)
        }
        let http = HTTPFixture([.init(status: 200, data: Data(#"{"items":[],"cursor":"more"}"#.utf8))])
        await assertServiceError(.invalidResponse) { _ = try await RegistryClient(transport: { try await http.send($0) }).capture(account: fixtureAccount()) }
        let redirect = HTTPFixture([.init(status: 200, data: Data(), redirected: true)])
        await assertServiceError(.refused) { _ = try await RegistryClient(transport: { try await redirect.send($0) }).capture(account: fixtureAccount()) }
    }
    func testOversizeTimeoutCancellationAndInvalidToken() async throws {
        let http = HTTPFixture([.init(status: 200, data: Data(repeating: 32, count: 4 * 1024 * 1024 + 1))])
        await assertServiceError(.oversized) { _ = try await RegistryClient(transport: { try await http.send($0) }).capture(account: fixtureAccount()) }
        await assertServiceError(.timeout) { _ = try await RegistryClient(transport: { _ in throw ServiceError.timeout }).capture(account: fixtureAccount()) }
        await assertServiceError(.canceled) { _ = try await RegistryClient(transport: { _ in throw CancellationError() }).capture(account: fixtureAccount()) }
        XCTAssertThrowsError(try AccountSession(token: "raw-secret")) { XCTAssertEqual($0 as? ServiceError, .authentication) }
        let account = try fixtureAccount()
        XCTAssertEqual(account.accountIDHash, DeviceServices.hash("invented-account"))
        XCTAssertFalse(account.accountIDHash.contains("invented-account"))
    }
    func testNativeTransportRefusesUnapprovedOriginWithoutNetwork() async {
        await assertServiceError(.refused) {
            _ = try await NativeHTTP.send(HTTPRequest(url: URL(string: "https://example.invalid/backend-api/tbo")!, method: "GET", headers: [:], body: nil))
        }
    }

    func testAppServerReadOnlyHandshakeAndMetadataHash() async throws {
        let root = try temporaryDirectory(), script = root.appendingPathComponent("fixture-server"), log = root.appendingPathComponent("requests.jsonl")
        let token = try fixtureAccount().token
        let source = """
        #!/bin/sh
        IFS= read -r line; printf '%s\\n' "$line" >> '\(log.path)'
        printf '%s\\n' '{"id":1,"result":{}}'
        IFS= read -r line; printf '%s\\n' "$line" >> '\(log.path)'
        IFS= read -r line; printf '%s\\n' "$line" >> '\(log.path)'
        printf '%s\\n' '{"id":2,"result":{"account":{"type":"chatgpt"}}}'
        IFS= read -r line; printf '%s\\n' "$line" >> '\(log.path)'
        printf '%s\\n' '{"id":3,"result":{"authMethod":"chatgptAuthTokens","authToken":"\(token)"}}'
        """
        try source.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
        let session = try await AppServerSession(executable: script).account()
        XCTAssertEqual(session.accountIDHash, try fixtureAccount().accountIDHash)
        let requests = try String(contentsOf: log, encoding: .utf8).split(separator: "\n").map { try JSONValue.parse(Data($0.utf8)) }
        XCTAssertEqual(requests.map { $0["method"].string! }, ["initialize", "initialized", "account/read", "getAuthStatus"])
        XCTAssertEqual(requests[0]["params"]["capabilities"]["explicitGatewayOauth"], .bool(true))
        XCTAssertEqual(requests[2]["params"]["refreshToken"], .bool(false))
        XCTAssertEqual(requests[3]["params"], .object(["includeToken": .bool(true), "refreshToken": .bool(false)]))
        XCTAssertFalse(try String(contentsOf: log, encoding: .utf8).contains(token))
    }
    func testAppServerUnexpectedRequestAndDeadlineAreRedacted() async throws {
        let root = try temporaryDirectory(), script = root.appendingPathComponent("fixture-server")
        try "#!/bin/sh\nprintf '%s\\n' '{\"id\":99,\"method\":\"login-secret\"}'\n".write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
        await assertServiceError(.refused) { _ = try await AppServerSession(executable: script).account() }
        try "#!/bin/sh\n/bin/sleep 1\n".write(to: script, atomically: true, encoding: .utf8)
        await assertServiceError(.timeout) { _ = try await AppServerSession(executable: script, timeout: 0.05).account() }
    }
    func testNullableCloudRecordRemainsInCompleteCapture() throws {
        let root = #"{"items":[{"id":"dot~one","active_root_thread_id":"thread-one","display_name":"One"}],"cursor":null}"#
        let body = #"{"thread_id":"thread-one","computers":[{"environment_id":"cloud-one","installation_id":null,"kind":"openai_cloud","computer_type":null,"display_name":null,"status":null}],"next_cursor":null}"#
        let capture = RegistryCapture(accountIDHash: try fixtureAccount().accountIDHash, rootResponse: root, scopes: [.init(dotID: "dot~one", threadID: "thread-one", computersResponse: body)])
        let devices = try capture.devices()
        XCTAssertEqual(devices.count, 1); XCTAssertEqual(devices[0].kind, "openai_cloud")
        XCTAssertEqual(devices[0].occurrences[0].record["installation_id"], .null)
    }
    func testEngineMinimalNonzeroErrorIsRedactedRefusal() async {
        let engine = EngineClient(executable: URL(fileURLWithPath: "/fixture/engine"), runner: { request in
            XCTAssertEqual(request.arguments, ["desktop"])
            return ProcessResult(status: 5, output: Data(#"{"schema_version":1,"op":"unknown","status":"refused","reason_code":"integrity"}"#.utf8))
        })
        await assertServiceError(.refused) { _ = try await engine.send(.object(["op": .string("status")])) }
    }
    func testAppServerEarlyClosedInputReturnsRedactedError() async throws {
        let root = try temporaryDirectory(), script = root.appendingPathComponent("early-close-server")
        let source = """
        #!/bin/sh
        exec 0<&-
        /bin/sleep 0.05
        printf '%s\\n' '{"id":1,"result":{}}'
        /bin/sleep 0.1
        """
        try source.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
        await assertServiceError(.unavailable) { _ = try await AppServerSession(executable: script).account() }
    }
}
