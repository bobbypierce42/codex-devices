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
    func testRootPagesRetainExactResponsesAndRejectRepeatedCursor() async throws {
        let first = #"{"items":[{"id":"dot~one","active_root_thread_id":"thread-one","display_name":"One"}],"cursor":"more"}"#
        let last = #"{"items":[{"id":"dot~two","active_root_thread_id":"thread-two","display_name":"Two"}],"cursor":null}"#
        let fixture = try fixtureCapture()
        let replies = [HTTPResult(status: 200, data: Data(first.utf8)), .init(status: 200, data: Data(last.utf8))] + fixture.scopes.map { HTTPResult(status: 200, data: Data($0.computersResponse.utf8)) }
        let http = HTTPFixture(replies)
        let capture = try await RegistryClient(transport: { try await http.send($0) }).capture(account: fixtureAccount())
        XCTAssertEqual(capture.rootResponse, first)
        XCTAssertEqual(try capture.devices().first?.occurrences.count, 2)
        XCTAssertEqual(capture.json["root_followup_pages"].array?.first?["response_utf8"].string, last)
        let engine = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("target/release/codex-devices")
        let directory = try temporaryDirectory().appendingPathComponent("engine")
        let receipt = try await EngineClient(executable: engine).send(.object(["schema_version": .number(1), "op": .string("prepare"), "operation_directory": .string(directory.path), "evidence_mode": .string("registry-label"), "capture": capture.json, "selections": .array([.object(["environment_id": .string("env-studio"), "new_name": .string("Studio")])]), "peers": .array([])]))
        XCTAssertEqual(receipt.status, "prepared")
        XCTAssertEqual(try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("before.json"))), capture.json)
        let requests = await http.requests
        XCTAssertEqual(URLComponents(url: requests[1].url, resolvingAgainstBaseURL: false)?.queryItems?.last, URLQueryItem(name: "cursor", value: "more"))
        let repeated = HTTPFixture([.init(status: 200, data: Data(first.utf8)), .init(status: 200, data: Data(first.utf8))])
        await assertServiceError(.invalidResponse) { _ = try await RegistryClient(transport: { try await repeated.send($0) }).capture(account: fixtureAccount()) }
    }
    func testComputerScopePaginationPreservesPagesAndRejectsLoops() async throws {
        let fixture = try fixtureCapture()
        let first = #"{"thread_id":"thread-one","computers":[],"next_cursor":"next"}"#
        let last = fixture.scopes[0].computersResponse
        let http = HTTPFixture([.init(status: 200, data: Data(fixture.rootResponse.utf8)), .init(status: 200, data: Data(first.utf8)), .init(status: 200, data: Data(last.utf8)), .init(status: 200, data: Data(fixture.scopes[1].computersResponse.utf8))])
        let capture = try await RegistryClient(transport: { try await http.send($0) }).capture(account: fixtureAccount())
        XCTAssertEqual(try capture.devices().first?.occurrences.count, 2)
        XCTAssertEqual(capture.json["scopes"].array?.first?["computer_followup_pages"].array?.first?["response_utf8"].string, last)
        let engine = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("target/release/codex-devices")
        let directory = try temporaryDirectory().appendingPathComponent("engine")
        let receipt = try await EngineClient(executable: engine).send(.object(["schema_version": .number(1), "op": .string("prepare"), "operation_directory": .string(directory.path), "evidence_mode": .string("registry-label"), "capture": capture.json, "selections": .array([.object(["environment_id": .string("env-studio"), "new_name": .string("Studio")])]), "peers": .array([])]))
        XCTAssertEqual(receipt.status, "prepared")
        XCTAssertEqual(try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("before.json"))), capture.json)
        let requests = await http.requests
        XCTAssertEqual(URLComponents(url: requests[2].url, resolvingAgainstBaseURL: false)?.queryItems?.last, URLQueryItem(name: "cursor", value: "next"))
        let repeated = HTTPFixture([.init(status: 200, data: Data(fixture.rootResponse.utf8)), .init(status: 200, data: Data(first.utf8)), .init(status: 200, data: Data(first.utf8))])
        await assertServiceError(.invalidResponse) { _ = try await RegistryClient(transport: { try await repeated.send($0) }).capture(account: fixtureAccount()) }
    }
    func testExactCompleteCaptureAndAllowedNameOnlyRequest() async throws {
        let fixture = try fixtureCapture()
        let replies = [HTTPResult(status: 200, data: Data(fixture.rootResponse.utf8))] + fixture.scopes.map { HTTPResult(status: 200, data: Data($0.computersResponse.utf8)) } + [.init(status: 200, data: Data(#"{"executor_id":"env-studio","custom_display_name":"Studio"}"#.utf8))]
        let http = HTTPFixture(replies)
        let client = RegistryClient(transport: { try await http.send($0) })
        let captured = try await client.capture(account: fixtureAccount())
        XCTAssertEqual(captured.rootResponse, fixture.rootResponse)
        XCTAssertEqual(captured.scopes.map(\.computersResponse), fixture.scopes.map(\.computersResponse))
        let renamed = try await client.rename(account: fixtureAccount(), installationID: fixtureInstallation, newName: "Studio", expectedEnvironmentID: "env-studio")
        XCTAssertTrue(renamed.validates(environmentID: "env-studio", newName: "Studio"))
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
    func testErrorsAreRedactedAndMissingPageRefuses() async throws {
        for status in [401, 403, 302, 500] {
            let http = HTTPFixture([.init(status: status, data: Data("secret body must not appear".utf8))])
            let client = RegistryClient(transport: { try await http.send($0) })
            do { _ = try await client.capture(account: fixtureAccount()); XCTFail("Expected refusal") }
            catch { XCTAssertFalse(error.localizedDescription.contains("secret")) }
            let count = await http.count(); XCTAssertEqual(count, 1)
        }
        let http = HTTPFixture([.init(status: 200, data: Data(#"{"items":[],"cursor":"more"}"#.utf8))])
        await assertServiceError(.unavailable) { _ = try await RegistryClient(transport: { try await http.send($0) }).capture(account: fixtureAccount()) }
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
    func testRenameResponseRequiresExactNameAndDirectOrEncodedExecutorID() async throws {
        let raw = "ccarenv_synthetic-executor"
        let encoded = "ccarenv_b64_" + Data(raw.utf8).base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        for expected in [raw, encoded] {
            let response: JSONValue = .object(["executor_id": .string(raw), "custom_display_name": .string("Studio"), "ignored_credential": .string("invented-not-retained")])
            let client = RegistryClient(transport: { _ in .init(status: 200, data: try response.data()) })
            let receipt = try await client.rename(account: fixtureAccount(), installationID: fixtureInstallation, newName: "Studio", expectedEnvironmentID: expected)
            XCTAssertEqual(receipt.executorID, raw)
            XCTAssertFalse(String(describing: receipt).contains("invented-not-retained"))
        }
        let mismatch: JSONValue = .object(["executor_id": .string("other-executor"), "custom_display_name": .string("Studio")])
        await assertServiceError(.drift) { _ = try await RegistryClient(transport: { _ in .init(status: 200, data: try mismatch.data()) }).rename(account: fixtureAccount(), installationID: fixtureInstallation, newName: "Studio", expectedEnvironmentID: encoded) }
        for response in [JSONValue.object(["executor_id": .string(raw), "custom_display_name": .string("Wrong")]), .object(["executor_id": .string(""), "custom_display_name": .string("Studio")]), .object([:])] {
            await assertServiceError(.invalidResponse) { _ = try await RegistryClient(transport: { _ in .init(status: 200, data: try response.data()) }).rename(account: fixtureAccount(), installationID: fixtureInstallation, newName: "Studio", expectedEnvironmentID: raw) }
        }
    }
}
