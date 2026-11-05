import Foundation
import XCTest
@testable import DeviceServices

@MainActor final class RepairTests: XCTestCase {
    func coordinator(_ fixture: RepairFixture) -> RepairCoordinator {
        RepairCoordinator(providers: fixture.providers, artifactParent: URL(fileURLWithPath: "/private/tmp/codex-devices-fixture-unused"), isDemo: true)
    }
    func ready(_ fixture: RepairFixture) async -> RepairCoordinator {
        let model = coordinator(fixture)
        await model.scan(); await model.inspect(fixtureHost)
        await model.prepare([ReviewSelection(environmentID: "env-studio", newName: "Studio")])
        return model
    }
    func testIntentIsDurableBeforeOnePOSTAndFreshReadback() async throws {
        let fixture = RepairFixture(), model = await ready(fixture)
        XCTAssertEqual(model.phase, .review)
        let receipt = try XCTUnwrap(model.receipt)
        await model.applyReviewed(planDigest: receipt.planSHA256)
        XCTAssertEqual(model.phase, .verified)
        XCTAssertEqual(model.devices.first?.displayName, "Studio")
        XCTAssertEqual(model.devices.first?.installationIDHash, DeviceServices.hash(fixtureInstallation))
        let events = await fixture.log()
        XCTAssertEqual(events, ["prepare", "preflight", "intent", "POST", "reconcile"])
    }
    func testRefusedIntentNeverPOSTs() async throws {
        let fixture = RepairFixture(); await fixture.configure(intentRefused: true)
        let model = await ready(fixture)
        await model.applyReviewed(planDigest: try XCTUnwrap(model.receipt).planSHA256)
        let events = await fixture.log()
        XCTAssertFalse(events.contains("POST")); XCTAssertEqual(model.phase, .failed)
    }
    func testLostPOSTResponseReconcilesWithoutRetry() async throws {
        let fixture = RepairFixture(); await fixture.configure(ambiguous: true)
        let model = await ready(fixture), digest = try XCTUnwrap(model.receipt).planSHA256
        await model.applyReviewed(planDigest: digest)
        XCTAssertEqual(model.phase, .verified)
        await model.applyReviewed(planDigest: digest)
        await model.reconcilePending()
        let events = await fixture.log(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
    func testStaleReadbackRemainsUncertainAndNeverRetries() async throws {
        let fixture = RepairFixture(); await fixture.configure(ambiguous: true, stale: true)
        let model = await ready(fixture), digest = try XCTUnwrap(model.receipt).planSHA256
        await model.applyReviewed(planDigest: digest)
        XCTAssertEqual(model.phase, .uncertain)
        await model.applyReviewed(planDigest: digest)
        await model.reconcilePending()
        XCTAssertEqual(model.phase, .uncertain)
        let events = await fixture.log(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
    func testDistinctEnvironmentFingerprintCollisionBlocksPreparation() async {
        let fixture = RepairFixture(); await fixture.configure(duplicate: true)
        let model = await ready(fixture)
        XCTAssertEqual(model.phase, .failed)
        let events = await fixture.log(); XCTAssertTrue(events.isEmpty)
    }
    func testWrongDigestAndAccountDriftNeverPOST() async throws {
        let fixture = RepairFixture(), model = await ready(fixture)
        await model.applyReviewed(planDigest: String(repeating: "b", count: 64))
        XCTAssertEqual(model.phase, .review)
        await fixture.configure(accountDrift: true)
        await model.applyReviewed(planDigest: try XCTUnwrap(model.receipt).planSHA256)
        XCTAssertEqual(model.phase, .failed)
        let events = await fixture.log(); XCTAssertFalse(events.contains("POST"))
    }
    func testTwoPhysicalHostKeysWithCopiedIdentityBlockRepair() async {
        let fixture = RepairFixture()
        var providers = fixture.providers
        providers.inspect = { candidate, _ in PeerInspection(candidate: candidate, computerName: candidate.name, installationIDHash: DeviceServices.hash(fixtureInstallation), hostKeyFingerprint: "SHA256:\(candidate.host)") }
        let model = RepairCoordinator(providers: providers, artifactParent: URL(fileURLWithPath: "/private/tmp/codex-devices-fixture-unused"), isDemo: true)
        await model.scan(); await model.inspect(fixtureHost); await model.inspect(HostCandidate(host: "second.example"))
        await model.prepare([.init(environmentID: "env-studio", newName: "Studio")])
        XCTAssertEqual(model.phase, .failed)
        let events = await fixture.log(); XCTAssertTrue(events.isEmpty)
    }
    func testSameVerifiedHostKeyAliasesDoNotCreateCollision() async {
        let fixture = RepairFixture(), model = coordinator(fixture)
        await model.scan(); await model.inspect(fixtureHost); await model.inspect(HostCandidate(host: "alias.example"))
        XCTAssertNotNil(model.matchingInspection(for: model.devices[0]))
        await model.prepare([.init(environmentID: "env-studio", newName: "Studio")])
        XCTAssertEqual(model.phase, .review)
    }
    func testLocalAndNetworkAliasesShareVerifiedHostKey() async {
        let fixture = RepairFixture(), model = coordinator(fixture)
        await model.scan(); await model.inspect(HostCandidate(host: "localhost", isLocal: true)); await model.inspect(fixtureHost)
        XCTAssertNotNil(model.matchingInspection(for: model.devices[0]))
        await model.prepare([.init(environmentID: "env-studio", newName: "Studio")])
        XCTAssertEqual(model.phase, .review)
    }
    func testSameKeyDifferentSystemNamesAndUnknownAliasesStayAmbiguous() async {
        for unknown in [false, true] {
            let fixture = RepairFixture()
            var providers = fixture.providers
            providers.inspect = { candidate, _ in
                PeerInspection(candidate: candidate, computerName: unknown ? "Studio" : candidate.host, installationIDHash: DeviceServices.hash(fixtureInstallation), hostKeyFingerprint: unknown ? nil : "SHA256:fixture")
            }
            let model = RepairCoordinator(providers: providers, artifactParent: URL(fileURLWithPath: "/private/tmp/codex-devices-fixture-unused"), isDemo: true)
            await model.scan(); await model.inspect(fixtureHost); await model.inspect(HostCandidate(host: "alias.example"))
            XCTAssertNil(model.matchingInspection(for: model.devices[0]))
            await model.prepare([.init(environmentID: "env-studio", newName: "Studio")])
            XCTAssertEqual(model.phase, .failed)
            let events = await fixture.log(); XCTAssertTrue(events.isEmpty)
        }
    }
    func testRestartRestoresPrivateReferenceAndOnlyReadsBack() async throws {
        let root = try temporaryDirectory(), fixture = RepairFixture()
        await fixture.configure(ambiguous: true, stale: true)
        let first = RepairCoordinator(providers: fixture.providers, artifactParent: root)
        await first.scan(); await first.inspect(fixtureHost)
        await first.prepare([.init(environmentID: "env-studio", newName: "Studio")])
        await first.applyReviewed(planDigest: try XCTUnwrap(first.receipt).planSHA256)
        XCTAssertEqual(first.phase, .uncertain)
        let reference = root.appendingPathComponent("pending-operation.json")
        let attributes = try FileManager.default.attributesOfItem(atPath: reference.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        XCTAssertFalse(try String(contentsOf: reference, encoding: .utf8).contains(fixtureInstallation))
        await fixture.settleName("Studio")
        let restarted = RepairCoordinator(providers: fixture.providers, artifactParent: root)
        await restarted.scan()
        XCTAssertEqual(restarted.phase, .uncertain)
        await restarted.reconcilePending()
        XCTAssertEqual(restarted.phase, .verified)
        let events = await fixture.log(); XCTAssertEqual(events.filter { $0 == "POST" }.count, 1)
    }
}
