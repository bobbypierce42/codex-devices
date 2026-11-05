import Foundation
import XCTest
@testable import DeviceServices

@MainActor final class RegistryWorkflowTests: XCTestCase {
    private func setup() throws -> (RegistryWorkflowFixture, URL, RegistryWorkflow) {
        let engine = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("target/release/codex-devices")
        guard FileManager.default.isExecutableFile(atPath: engine.path) else { throw ServiceError.unavailable }
        let fixture = RegistryWorkflowFixture(engine: engine), parent = try temporaryDirectory().appendingPathComponent("workflow", isDirectory: true)
        return (fixture, parent, RegistryWorkflow(providers: fixture.providers, artifactParent: parent))
    }
    func testAuditNoopUnknownCloudAndCollisionNeverPrepareOrPOST() async throws {
        let (fixture, _, model) = try setup()
        await model.refresh()
        XCTAssertEqual(model.phase, .ready); XCTAssertEqual(model.records.count, 1)
        XCTAssertTrue(model.records[0].isRenameable); XCTAssertEqual(model.records[0].scopeCount, 2)
        await model.prepare(environmentID: "env-studio", newName: "Old Studio")
        await model.prepare(environmentID: "unknown", newName: "Studio")
        await model.applyReviewed(planDigest: String(repeating: "a", count: 64))
        for variant in [RegistryWorkflowFixture.Variant.cloud, .collision] {
            await fixture.setVariant(variant); await model.refresh()
            XCTAssertTrue(model.records.allSatisfy { !$0.isRenameable })
            await model.prepare(environmentID: model.records[0].environmentID, newName: "Studio")
        }
        let events = await fixture.events(); XCTAssertFalse(events.contains("prepare")); XCTAssertFalse(events.contains("POST"))
    }
    func testPreparedReviewShowsExactPlanAndPrivateBackupBeforeApproval() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        XCTAssertEqual(model.phase, .review); XCTAssertFalse(model.isBusy)
        let receipt = try XCTUnwrap(model.receipt), directory = try XCTUnwrap(model.operationDirectory)
        XCTAssertEqual(receipt.records[0].oldName, "Old Studio"); XCTAssertEqual(receipt.records[0].newName, "Studio")
        XCTAssertEqual(receipt.records[0].installationIDHash, DeviceServices.hash(fixtureInstallation))
        let before = try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("engine/before.json")))
        let submitted = await fixture.submittedCapture(); XCTAssertEqual(before, submitted)
        let reference = try JSONValue.parse(Data(contentsOf: parent.appendingPathComponent("last-review.json")))
        XCTAssertEqual(reference["operation_basename"].string, directory.lastPathComponent)
        XCTAssertEqual(reference["plan_sha256"].string, receipt.planSHA256)
        guard case .object(let fields) = reference else { return XCTFail("Reference must be a bounded object") }
        XCTAssertEqual(Set(fields.keys), ["operation_basename", "plan_sha256"])
        XCTAssertEqual((try FileManager.default.attributesOfItem(atPath: parent.appendingPathComponent("last-review.json").path)[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        let events = await fixture.events(); XCTAssertFalse(events.contains("POST"))
        await model.applyReviewed(planDigest: String(repeating: "0", count: 64))
        XCTAssertEqual(model.phase, .review)
        let after = await fixture.events(); XCTAssertFalse(after.contains("POST"))
    }
    func testApplyUsesRunnerIntentWitnessAndFreshReadbackOnce() async throws {
        let (fixture, _, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        let digest = try XCTUnwrap(model.receipt).planSHA256
        await model.applyReviewed(planDigest: digest)
        XCTAssertEqual(model.phase, .verified); XCTAssertEqual(model.records[0].displayName, "Studio")
        await model.applyReviewed(planDigest: digest); await model.verifyPending()
        XCTAssertEqual(model.phase, .verified)
        let events = await fixture.events(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
        let intent = try XCTUnwrap(events.firstIndex(of: "intent")), post = try XCTUnwrap(events.firstIndex(of: "POST")), reconcile = try XCTUnwrap(events.firstIndex(of: "reconcile"))
        XCTAssertLessThan(intent, post); XCTAssertLessThan(post, reconcile)
    }
    func testRestartAfterVerifiedOperationAllowsAnotherReview() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        await model.applyReviewed(planDigest: try XCTUnwrap(model.receipt).planSHA256)
        XCTAssertEqual(model.phase, .verified)
        let restarted = RegistryWorkflow(providers: fixture.providers, artifactParent: parent)
        await restarted.refresh()
        XCTAssertEqual(restarted.phase, .ready)
        await restarted.prepare(environmentID: "env-studio", newName: "Another Studio")
        XCTAssertEqual(restarted.phase, .review)
        let events = await fixture.events(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
    func testRestartRestoresPreparedReviewWithoutApplying() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        let restarted = RegistryWorkflow(providers: fixture.providers, artifactParent: parent)
        await restarted.refresh()
        XCTAssertEqual(restarted.phase, .review); XCTAssertEqual(restarted.receipt?.planSHA256, model.receipt?.planSHA256)
        let events = await fixture.events(); XCTAssertFalse(events.contains("POST"))
    }
    func testRestartAfterUnknownResponseRemainsReadbackOnlyAndUncertain() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        await fixture.setLostReply()
        let digest = try XCTUnwrap(model.receipt).planSHA256
        await model.applyReviewed(planDigest: digest); XCTAssertEqual(model.phase, .uncertain)
        let restarted = RegistryWorkflow(providers: fixture.providers, artifactParent: parent)
        await restarted.refresh(); XCTAssertEqual(restarted.phase, .uncertain)
        await restarted.applyReviewed(planDigest: digest); await restarted.verifyPending()
        XCTAssertEqual(restarted.phase, .uncertain)
        let events = await fixture.events(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
    func testMissingResponseWitnessCannotBecomeVerifiedOnRestart() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        try await fixture.recordIntentWithoutResponse(directory: XCTUnwrap(model.operationDirectory), digest: XCTUnwrap(model.receipt).planSHA256)
        let restarted = RegistryWorkflow(providers: fixture.providers, artifactParent: parent)
        await restarted.refresh(); XCTAssertEqual(restarted.phase, .uncertain)
        await restarted.verifyPending(); XCTAssertEqual(restarted.phase, .uncertain)
        let events = await fixture.events(); XCTAssertFalse(events.contains("POST"))
    }
    func testUnavailableEngineAfterPOSTCannotOpenAnotherReview() async throws {
        let (fixture, parent, model) = try setup()
        await model.refresh(); await model.prepare(environmentID: "env-studio", newName: "Studio")
        await fixture.failEngineAfterPOST()
        await model.applyReviewed(planDigest: try XCTUnwrap(model.receipt).planSHA256)
        XCTAssertEqual(model.phase, .uncertain)
        await model.prepare(environmentID: "env-studio", newName: "Another label")
        let restarted = RegistryWorkflow(providers: fixture.providers, artifactParent: parent)
        await restarted.refresh(); XCTAssertEqual(restarted.phase, .uncertain)
        await restarted.prepare(environmentID: "env-studio", newName: "Another label")
        let events = await fixture.events()
        XCTAssertEqual(events.filter { $0 == "prepare" }.count, 1); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
    func testUnsafeSavedReferencesRefuseWithoutEngineOrPOST() async throws {
        for variant in 0..<3 {
            let (fixture, parent, model) = try setup()
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let reference = parent.appendingPathComponent("last-review.json")
            let data = try JSONValue.object(["operation_basename": .string(variant == 0 ? "../../outside" : UUID().uuidString), "plan_sha256": .string(String(repeating: "a", count: 64))]).data()
            if variant == 2 {
                let target = parent.appendingPathComponent("invented-reference.json")
                try data.write(to: target); try FileManager.default.createSymbolicLink(at: reference, withDestinationURL: target)
            } else {
                try data.write(to: reference)
                try FileManager.default.setAttributes([.posixPermissions: variant == 1 ? 0o644 : 0o600], ofItemAtPath: reference.path)
            }
            await model.refresh(); XCTAssertEqual(model.phase, .uncertain)
            XCTAssertTrue(model.message.contains("Only readback is allowed"))
            await model.prepare(environmentID: "env-studio", newName: "Studio")
            let events = await fixture.events(); XCTAssertEqual(events, ["account", "capture"])
            XCTAssertFalse(events.contains("prepare")); XCTAssertFalse(events.contains("POST"))
        }
    }
    func testNetworkDiscoveryOnlyNeverCallsAccountOrEngineAndKeepsAliasRules() async {
        let providers = ServiceProviders(discover: { _ in .init(candidates: [fixtureHost], failures: []) }, inspect: { candidate, _ in PeerInspection(candidate: candidate, computerName: "Studio", installationIDHash: DeviceServices.hash(fixtureInstallation), hostKeyFingerprint: "SHA256:fixture") }, account: { XCTFail("Discovery must not authenticate"); throw ServiceError.unavailable }, capture: { _ in XCTFail(); throw ServiceError.unavailable }, rename: { _, _, _ in XCTFail() }, engine: { _ in XCTFail(); throw ServiceError.unavailable })
        let network = RepairCoordinator(providers: providers, artifactParent: URL(fileURLWithPath: "/private/tmp/codex-devices-network-unused"))
        await network.discoverOnly(); XCTAssertEqual(network.candidates, [fixtureHost]); XCTAssertTrue(network.devices.isEmpty)
        await network.inspect(fixtureHost); await network.inspect(.init(host: "alias.example"))
        XCTAssertNotNil(network.matchingInspection(installationIDHash: DeviceServices.hash(fixtureInstallation)))
        await network.discoverOnly(); XCTAssertTrue(network.inspections.isEmpty)
    }
}

private actor RegistryWorkflowFixture {
    enum Variant { case personal, cloud, collision }
    private let engine: EngineClient
    private var name = "Old Studio", lostReply = false, engineFailsAfterPOST = false
    private var variant: Variant = .personal
    private var log = [String](), prepared: JSONValue?
    init(engine: URL) { self.engine = EngineClient(executable: engine) }
    nonisolated var providers: RegistryCommandProviders {
        RegistryCommandProviders(account: { try await self.account() }, capture: { try await self.capture($0) }, rename: { _, _, name, id in try await self.rename(name, id) }, engine: { try await self.send($0) })
    }
    func events() -> [String] { log }
    func submittedCapture() -> JSONValue? { prepared }
    func setVariant(_ value: Variant) { variant = value }
    func setLostReply() { lostReply = true }
    func failEngineAfterPOST() { engineFailsAfterPOST = true }
    private func account() throws -> AccountSession { log.append("account"); return try fixtureAccount() }
    private func capture(_ account: AccountSession) throws -> RegistryCapture {
        log.append("capture")
        let capture = try fixtureCapture(name: name, duplicateID: variant == .collision ? "other-env" : nil)
        if variant != .cloud { return capture }
        let scopes = try capture.scopes.map { scope -> RegistryCapture.Scope in
            let cloud: JSONValue = .object(["environment_id": .string("cloud-record"), "kind": .string("openai_cloud"), "display_name": .string("Cloud"), "computer_type": .null, "installation_id": .null, "status": .null])
            let response: JSONValue = .object(["thread_id": .string(scope.threadID), "computers": .array([cloud]), "next_cursor": .null])
            return .init(dotID: scope.dotID, threadID: scope.threadID, computersResponse: String(data: try response.data(), encoding: .utf8)!)
        }
        return RegistryCapture(accountIDHash: account.accountIDHash, rootResponse: capture.rootResponse, scopes: scopes)
    }
    private func rename(_ newName: String, _ id: String) throws -> RegistryRenameReceipt {
        log.append("POST"); name = newName
        if lostReply { throw ServiceError.timeout }
        return .init(executorID: id, customDisplayName: newName)
    }
    private func send(_ request: JSONValue) async throws -> OperationReceipt {
        let op = request["op"].string!
        if engineFailsAfterPOST && log.contains("POST") { throw ServiceError.unavailable }
        if op == "prepare" { prepared = request["capture"] }
        if op == "intent" {
            let directory = URL(fileURLWithPath: request["operation_directory"].string!).deletingLastPathComponent()
            let witness = try JSONValue.parse(Data(contentsOf: directory.appendingPathComponent("registry-response.json")))
            XCTAssertEqual(witness["outcome"], .string("unknown"))
        }
        log.append(op); return try await engine.send(request)
    }
    func recordIntentWithoutResponse(directory: URL, digest: String) async throws {
        _ = try await engine.send(.object(["schema_version": .number(1), "op": .string("intent"), "operation_directory": .string(directory.appendingPathComponent("engine").path), "plan_sha256": .string(digest), "capture": try fixtureCapture().json, "environment_id": .string("env-studio"), "peers": .array([])]))
        name = "Studio"
    }
}
