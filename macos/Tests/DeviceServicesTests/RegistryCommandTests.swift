import Foundation
import XCTest
@testable import DeviceServices

final class RegistryCommandTests: XCTestCase {
    private func setupFixture() throws -> (RegistryCommandFixture, URL) {
        let root = try temporaryDirectory()
        let engine = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("target/release/codex-devices")
        guard FileManager.default.isExecutableFile(atPath: engine.path) else { throw ServiceError.unavailable }
        return (RegistryCommandFixture(engine: engine), root.appendingPathComponent("reviewed-operation", isDirectory: true))
    }
    private func prepare(_ fixture: RegistryCommandFixture, _ directory: URL) async throws -> String {
        let prepared = await RegistryCommandRunner(providers: fixture.providers).run(arguments: ["rename", "--id", "env-studio", "--name", "Studio", "--out", directory.path])
        XCTAssertEqual(prepared.exitCode, 0, prepared.output)
        XCTAssertFalse(prepared.output.contains(fixtureInstallation))
        return try XCTUnwrap(prepared.json?["receipt"]["plan_sha256"].string)
    }
    func testInvalidArgumentsAndHelpNeverCallProviders() async {
        let providers = RegistryCommandProviders(account: { XCTFail("No auth for invalid input/help"); throw ServiceError.unavailable }, capture: { _ in XCTFail(); throw ServiceError.unavailable }, rename: { _, _, _, _ in XCTFail(); throw ServiceError.unavailable }, engine: { _ in XCTFail(); throw ServiceError.unavailable })
        let runner = RegistryCommandRunner(providers: providers)
        for arguments in [[], ["unknown"], ["audit", "--unknown", "x"], ["audit", "--engine", "relative"], ["audit", "--engine", "/one", "--engine", "/two"], ["rename", "--id", "env-studio", "--name", "Studio"], ["apply", "--operation", "/private/tmp/invented", "--approve-plan-sha256", "bad"]] {
            let result = await runner.run(arguments: arguments); XCTAssertEqual(result.exitCode, 2)
        }
        for arguments in [["--help"], ["audit", "--help"], ["help"]] {
            let result = await runner.run(arguments: arguments); XCTAssertEqual(result.exitCode, 0); XCTAssertTrue(result.output.contains("codex-devices-mac audit"))
        }
    }
    func testAuditReportsRegistryEvidenceWithoutRawInstallationOrCredentials() async throws {
        let (fixture, _) = try setupFixture()
        let result = await RegistryCommandRunner(providers: fixture.providers).run(arguments: ["audit"])
        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(result.json?["evidence_mode"].string, "registry-label")
        XCTAssertEqual(result.json?["fresh_peer_inspection"], .bool(false))
        XCTAssertEqual(result.json?["records"].array?.count, 1)
        XCTAssertEqual(result.json?["records"].array?.first?["scopes"].array?.count, 2)
        XCTAssertFalse(result.output.contains(fixtureInstallation))
        let events = await fixture.events(); XCTAssertEqual(events, ["account", "capture"])
    }
    func testRealEnginePrepareBacksUpExactCaptureAndApplyRequiresDurableIntent() async throws {
        let (fixture, directory) = try setupFixture(), digest = try await prepare(fixture, directory)
        let before = try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("engine/before.json")))
        let submitted = await fixture.submittedCapture()
        XCTAssertEqual(before, submitted)
        let wrapperMode = try FileManager.default.attributesOfItem(atPath: directory.path)[.posixPermissions] as? NSNumber
        XCTAssertEqual(wrapperMode?.intValue, 0o700)
        let applied = await RegistryCommandRunner(providers: fixture.providers).run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
        XCTAssertEqual(applied.exitCode, 0, applied.output)
        XCTAssertEqual(applied.json?["receipt"]["status"].string, "verified")
        let events = await fixture.events()
        XCTAssertEqual(events, ["account", "capture", "prepare", "status", "account", "capture", "preflight", "witness-before-intent", "intent", "POST", "account", "capture", "reconcile"])
        let witness = try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("registry-response.json")))
        XCTAssertEqual(witness["outcome"], .string("accepted")); XCTAssertEqual(witness["plan_sha256"], .string(digest))
        XCTAssertFalse(String(data: try witness.data(), encoding: .utf8)!.contains(fixtureInstallation))
    }
    func testWrongDigestAndAccountDriftPreventPOST() async throws {
        let (fixture, directory) = try setupFixture(), digest = try await prepare(fixture, directory)
        let runner = RegistryCommandRunner(providers: fixture.providers)
        let wrong = await runner.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", String(repeating: "0", count: 64)])
        XCTAssertEqual(wrong.exitCode, 3)
        await fixture.setDrift(true)
        let drifted = await runner.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
        XCTAssertEqual(drifted.exitCode, 3)
        let events = await fixture.events(); XCTAssertFalse(events.contains("POST")); XCTAssertFalse(events.contains("intent"))
    }
    func testRestartNeverRepeatsWriteAndVerifyIsFreshReadOnly() async throws {
        let (fixture, directory) = try setupFixture(), digest = try await prepare(fixture, directory)
        let first = await RegistryCommandRunner(providers: fixture.providers).run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
        XCTAssertEqual(first.exitCode, 0, first.output)
        let restarted = RegistryCommandRunner(providers: fixture.providers)
        let repeated = await restarted.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
        XCTAssertEqual(repeated.exitCode, 3)
        let verified = await restarted.run(arguments: ["verify", "--operation", directory.path, "--expected-plan-sha256", digest])
        XCTAssertEqual(verified.exitCode, 0, verified.output)
        let events = await fixture.events(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
        XCTAssertEqual(Array(events.suffix(4)), ["status", "account", "capture", "reconcile"])
    }
    func testMismatchedOrLostResponseStaysUncertainDespiteMatchingFreshReadback() async throws {
        for behavior in [RegistryCommandFixture.Reply.mismatch, .lost] {
            let (fixture, directory) = try setupFixture(), digest = try await prepare(fixture, directory)
            await fixture.setReply(behavior)
            let runner = RegistryCommandRunner(providers: fixture.providers)
            let applied = await runner.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
            XCTAssertEqual(applied.exitCode, 4); XCTAssertEqual(applied.json?["status"], .string("uncertain"))
            let verified = await runner.run(arguments: ["verify", "--operation", directory.path, "--expected-plan-sha256", digest])
            XCTAssertEqual(verified.exitCode, 4); XCTAssertEqual(verified.json?["status"], .string("uncertain"))
            let repeated = await runner.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", digest])
            XCTAssertEqual(repeated.exitCode, 3)
            let events = await fixture.events(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
        }
    }
    func testExistingWrapperRefusesBeforeAuthenticationAndMissingWitnessCannotVerify() async throws {
        let (fixture, directory) = try setupFixture(), digest = try await prepare(fixture, directory)
        let previous = await fixture.events()
        let runner = RegistryCommandRunner(providers: fixture.providers)
        let repeatPrepare = await runner.run(arguments: ["rename", "--id", "env-studio", "--name", "Studio", "--out", directory.path])
        XCTAssertEqual(repeatPrepare.exitCode, 3)
        let after = await fixture.events(); XCTAssertEqual(previous, after)
        // Simulate a prior intent from a different process; no HTTP request is made.
        try await fixture.recordIntentWithoutResponse(directory: directory, digest: digest)
        let verified = await runner.run(arguments: ["verify", "--operation", directory.path, "--expected-plan-sha256", digest])
        XCTAssertEqual(verified.exitCode, 4); XCTAssertEqual(verified.json?["reason_code"], .string("response-unconfirmed"))
        let events = await fixture.events(); XCTAssertFalse(events.contains("POST"))
    }
}

private actor RegistryCommandFixture {
    enum Reply { case accepted, mismatch, lost }
    private let engine: EngineClient
    private var log = [String]()
    private var captureAtPrepare: JSONValue?
    private var name = "Old Studio"
    private var drift = false
    private var reply: Reply = .accepted
    init(engine: URL) { self.engine = EngineClient(executable: engine) }
    nonisolated var providers: RegistryCommandProviders {
        RegistryCommandProviders(account: { try await self.account() }, capture: { try await self.capture($0) }, rename: { try await self.rename($0, $1, $2, $3) }, engine: { try await self.send($0) })
    }
    func setDrift(_ value: Bool) { drift = value }
    func setReply(_ value: Reply) { reply = value }
    func events() -> [String] { log }
    func submittedCapture() -> JSONValue? { captureAtPrepare }
    private func account() throws -> AccountSession { log.append("account"); return try fixtureAccount(drift ? "different-account" : "invented-account") }
    private func capture(_ account: AccountSession) throws -> RegistryCapture {
        log.append("capture"); let capture = try fixtureCapture(name: name)
        return RegistryCapture(accountIDHash: account.accountIDHash, rootResponse: capture.rootResponse, scopes: capture.scopes)
    }
    private func rename(_ account: AccountSession, _ installation: String, _ newName: String, _ environment: String) throws -> RegistryRenameReceipt {
        log.append("POST")
        XCTAssertEqual(installation, fixtureInstallation); XCTAssertEqual(environment, "env-studio"); XCTAssertEqual(newName, "Studio")
        name = newName
        if reply == .lost { throw ServiceError.timeout }
        return .init(executorID: reply == .mismatch ? "different-executor" : environment, customDisplayName: newName)
    }
    private func send(_ request: JSONValue) async throws -> OperationReceipt {
        let op = request["op"].string ?? "unknown"
        if op == "prepare" { captureAtPrepare = request["capture"] }
        if op == "intent" {
            let directory = URL(fileURLWithPath: request["operation_directory"].string!).deletingLastPathComponent()
            let witness = try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("registry-response.json")))
            XCTAssertEqual(witness["outcome"], .string("unknown")); XCTAssertEqual(witness["environment_id"], .string("env-studio")); XCTAssertEqual(witness["plan_sha256"], request["plan_sha256"])
            log.append("witness-before-intent")
        }
        log.append(op)
        return try await engine.send(request)
    }
    func recordIntentWithoutResponse(directory: URL, digest: String) async throws {
        let account = try fixtureAccount(), capture = try fixtureCapture()
        _ = try await engine.send(.object(["schema_version": .number(1), "op": .string("intent"), "operation_directory": .string(directory.appendingPathComponent("engine").path), "plan_sha256": .string(digest), "capture": RegistryCapture(accountIDHash: account.accountIDHash, rootResponse: capture.rootResponse, scopes: capture.scopes).json, "peers": .array([]), "environment_id": .string("env-studio")]))
        name = "Studio"
    }
}
