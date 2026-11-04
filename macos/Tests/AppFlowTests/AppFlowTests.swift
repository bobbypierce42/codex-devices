import XCTest
@testable import DeviceServices

@MainActor
final class AppFlowTests: XCTestCase {
    func testSharedMatchKeepsVerifiedAliasesSelectableAndBlocksCopiedIdentity() async throws {
        let repository = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let engine = EngineClient(executable: repository.appendingPathComponent("target/release/codex-devices"))
        let first = NativeBridgeFixture.candidate
        let alias = HostCandidate(host: "studio-tail.example.invalid", name: "Invented Studio", source: "fixture")
        for sameVerifiedKey in [true, false] {
            let parent = URL(fileURLWithPath: "/private/tmp/codex-devices-alias-flow-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let fixture = NativeBridgeFixture()
            var providers = fixture.providers(engine: engine)
            let inspect = providers.inspect
            providers.discover = { _ in DiscoveryResult(candidates: [first, alias], failures: []) }
            providers.inspect = { candidate, user in
                let peer = try await inspect(candidate, user)
                return PeerInspection(candidate: peer.candidate, computerName: peer.computerName,
                                      installationIDHash: peer.installationIDHash,
                                      hostKeyFingerprint: sameVerifiedKey ? peer.hostKeyFingerprint : "SHA256:\(candidate.host)")
            }
            let coordinator = RepairCoordinator(providers: providers, artifactParent: parent)
            await coordinator.scan()
            await coordinator.inspect(first)
            await coordinator.inspect(alias)
            let device = try XCTUnwrap(coordinator.devices.first { $0.id == "invented-selected" })
            XCTAssertEqual(coordinator.inspections.count, 2)
            XCTAssertEqual(coordinator.matchingInspection(for: device) != nil, sameVerifiedKey)
            await coordinator.prepare([ReviewSelection(environmentID: device.id, newName: "Invented Studio")])
            if sameVerifiedKey {
                XCTAssertEqual(coordinator.phase, .review, coordinator.message)
                XCTAssertEqual(coordinator.receipt?.records.count, 1)
            } else {
                XCTAssertNotEqual(coordinator.phase, .review)
                let events = await fixture.events
                XCTAssertTrue(events.isEmpty, "A copied installation on distinct SSH hosts cannot reach the engine")
            }
        }
    }

    func testRealEngineCreatesPrivateBackupAndVerifiesAllOccurrences() async throws {
        let repository = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        let engineURL = repository.appendingPathComponent("target/release/codex-devices")
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: engineURL.path), "Build the locked release Rust engine before this integration check")
        let parent = URL(fileURLWithPath: "/private/tmp/codex-devices-app-flow-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let fixture = NativeBridgeFixture()
        let engine = EngineClient(executable: engineURL)
        let coordinator = RepairCoordinator(providers: fixture.providers(engine: engine), artifactParent: parent)
        let before = try await fixture.capture()

        await coordinator.scan()
        XCTAssertEqual(coordinator.phase, .ready)
        XCTAssertEqual(coordinator.devices.count, 3)
        XCTAssertEqual(coordinator.devices.first { $0.id == "invented-selected" }?.occurrences.count, 2)
        await coordinator.inspect(NativeBridgeFixture.candidate)
        await coordinator.prepare([ReviewSelection(environmentID: "invented-selected", newName: "Invented Studio")])
        XCTAssertEqual(coordinator.phase, .review, coordinator.message)
        let receipt = try XCTUnwrap(coordinator.receipt)
        let directory = URL(fileURLWithPath: receipt.operationDirectory)
        XCTAssertEqual(directory.deletingLastPathComponent().pathComponents, parent.pathComponents)
        let backup = directory.appendingPathComponent("before.json")
        let saved = try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: backup))
        let submittedCapture = await fixture.preparedCapture
        let submitted = try XCTUnwrap(submittedCapture)
        XCTAssertEqual(saved, submitted)
        let folderAttributes = try FileManager.default.attributesOfItem(atPath: directory.path)
        let backupAttributes = try FileManager.default.attributesOfItem(atPath: backup.path)
        XCTAssertEqual((folderAttributes[.posixPermissions] as? NSNumber)?.intValue, 0o700)
        XCTAssertEqual((backupAttributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)

        await coordinator.applyReviewed(planDigest: receipt.planSHA256)
        XCTAssertEqual(coordinator.phase, .verified, coordinator.message)
        let events = await fixture.events
        XCTAssertEqual(events, ["prepare", "preflight", "intent", "POST", "reconcile"])
        let after = try await fixture.capture()
        let beforeDevices = try before.devices(), afterDevices = try after.devices()
        XCTAssertEqual(afterDevices.map(\.installationIDHash), beforeDevices.map(\.installationIDHash))
        XCTAssertEqual(afterDevices.map(\.environmentID), beforeDevices.map(\.environmentID))
        XCTAssertEqual(afterDevices.first { $0.id == "invented-unselected" }, beforeDevices.first { $0.id == "invented-unselected" })
        XCTAssertEqual(afterDevices.first { $0.id == "invented-cloud" }, beforeDevices.first { $0.id == "invented-cloud" })
        let occurrences = try XCTUnwrap(afterDevices.first { $0.id == "invented-selected" }).occurrences
        XCTAssertTrue(occurrences.allSatisfy { $0.record["display_name"].string == "Invented Studio" })
        XCTAssertEqual(occurrences.map { $0.record["can_connect"] }, [.bool(true), .bool(false)])
        XCTAssertEqual(occurrences.map { $0.record["attached"] }, [.bool(true), .bool(false)])
        XCTAssertTrue(FileManager.default.fileExists(atPath: directory.appendingPathComponent("journal.jsonl").path))
    }

    func testSyntheticScanInspectReviewRepairPreservesIdentities() async throws {
        let coordinator = RepairCoordinator.demo()
        XCTAssertEqual(coordinator.phase, .idle)
        await coordinator.scan()
        XCTAssertEqual(coordinator.phase, .ready)
        let candidate = try XCTUnwrap(coordinator.candidates.first)
        await coordinator.inspect(candidate)
        let inspection = try XCTUnwrap(coordinator.inspections.first)
        let device = try XCTUnwrap(coordinator.devices.first { $0.installationIDHash == inspection.installationIDHash })
        let identities = Dictionary(uniqueKeysWithValues: coordinator.devices.map { ($0.id, $0.installationIDHash) })
        XCTAssertNotEqual(device.displayName, inspection.computerName)

        await coordinator.prepare([ReviewSelection(environmentID: device.id, newName: inspection.computerName)])
        XCTAssertEqual(coordinator.phase, .review)
        let receipt = try XCTUnwrap(coordinator.receipt)
        XCTAssertFalse(receipt.planSHA256.isEmpty)
        XCTAssertEqual(receipt.records.count, 1)
        XCTAssertEqual(receipt.records.first?.oldName, device.displayName)
        XCTAssertEqual(receipt.records.first?.newName, inspection.computerName)
        XCTAssertTrue(receipt.operationDirectory.hasPrefix("/tmp/codex-devices-synthetic-demo/operation-"))

        await coordinator.applyReviewed(planDigest: receipt.planSHA256)
        XCTAssertEqual(coordinator.phase, .verified)
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: coordinator.devices.map { ($0.id, $0.installationIDHash) }), identities)
    }

    func testUninspectedSelectionCannotReachReview() async throws {
        let coordinator = RepairCoordinator.demo()
        await coordinator.scan()
        let device = try XCTUnwrap(coordinator.devices.first)
        await coordinator.prepare([ReviewSelection(environmentID: device.id, newName: "Invented field Mac")])
        XCTAssertNotEqual(coordinator.phase, .review)
        XCTAssertNotEqual(coordinator.phase, .verified)
    }

    func testDifferentDigestCannotApplyReviewedChanges() async throws {
        let coordinator = RepairCoordinator.demo()
        await coordinator.scan()
        let candidate = try XCTUnwrap(coordinator.candidates.first)
        await coordinator.inspect(candidate)
        let inspection = try XCTUnwrap(coordinator.inspections.first)
        let device = try XCTUnwrap(coordinator.devices.first { $0.installationIDHash == inspection.installationIDHash })
        await coordinator.prepare([ReviewSelection(environmentID: device.id, newName: inspection.computerName)])
        XCTAssertEqual(coordinator.phase, .review)
        await coordinator.applyReviewed(planDigest: String(repeating: "0", count: 64))
        XCTAssertNotEqual(coordinator.phase, .verified)
        XCTAssertEqual(coordinator.devices.first { $0.id == device.id }?.displayName, device.displayName)
    }

    func testAskpassRefusesNonSSHParentWithoutOutput() throws {
        let products = Bundle(for: AppFlowTests.self).bundleURL.deletingLastPathComponent()
        let helper = products.appendingPathComponent("SSHAskpass")
        guard FileManager.default.isExecutableFile(atPath: helper.path) else {
            throw XCTSkip("Build SSHAskpass before checking its invocation boundary")
        }
        for environment in [[:], [
            "CODEX_DEVICES_ASKPASS_SESSION": UUID().uuidString,
            "CODEX_DEVICES_ASKPASS_DEADLINE": String(Int(Date().timeIntervalSince1970 + 30))
        ]] {
            let process = Process()
            let output = Pipe()
            let errors = Pipe()
            process.executableURL = helper
            process.environment = environment
            process.standardOutput = output
            process.standardError = errors
            try process.run()
            process.waitUntilExit()
            XCTAssertEqual(process.terminationStatus, 1)
            XCTAssertTrue(output.fileHandleForReading.readDataToEndOfFile().isEmpty)
            XCTAssertTrue(errors.fileHandleForReading.readDataToEndOfFile().isEmpty)
        }
    }
}

private actor NativeBridgeFixture {
    static let candidate = HostCandidate(host: "studio.example.invalid", name: "Invented Studio", source: "fixture")
    private var selectedName = "Invented Old Studio"
    private(set) var events = [String]()
    private(set) var preparedCapture: JSONValue?
    private let installation = "11111111-2222-3333-4444-555555555555"

    nonisolated func providers(engine: EngineClient) -> ServiceProviders {
        ServiceProviders(
            discover: { _ in DiscoveryResult(candidates: [Self.candidate], failures: []) },
            inspect: { candidate, _ in await self.inspect(candidate) },
            account: { try await self.account() },
            capture: { _ in try await self.capture() },
            rename: { _, installation, name in try await self.rename(installation, name) },
            engine: { request in
                await self.record(request)
                return try await engine.send(request)
            }
        )
    }

    private func record(_ request: JSONValue) {
        let operation = request["op"].string ?? "unknown"
        events.append(operation)
        if operation == "prepare" { preparedCapture = request["capture"] }
    }
    private func inspect(_ candidate: HostCandidate) -> PeerInspection {
        PeerInspection(candidate: candidate, computerName: "Invented Studio", installationIDHash: hash(installation), hostKeyFingerprint: "SHA256:invented-host-key")
    }
    private func account() throws -> AccountSession {
        let payload = try JSONValue.object(["https://api.openai.com/auth": .object(["chatgpt_account_id": .string("invented-app-flow-account")])]).data().base64EncodedString()
        return try AccountSession(token: "invented.\(payload).signature")
    }
    private func rename(_ selectedInstallation: String, _ name: String) throws {
        guard selectedInstallation == installation else { throw ServiceError.drift }
        events.append("POST")
        selectedName = name
    }
    func capture() throws -> RegistryCapture {
        let scopes = try ["invented-dot-one", "invented-dot-two"].enumerated().map { index, dot in
            let thread = "invented-thread-\(index)"
            let selected: JSONValue = .object([
                "environment_id": .string("invented-selected"), "installation_id": .string(installation),
                "kind": .string("user"), "computer_type": .string("app_connection"),
                "display_name": .string(selectedName), "status": .string("connected"),
                "can_connect": .bool(index == 0), "attached": .bool(index == 0)
            ])
            let unselected: JSONValue = .object([
                "environment_id": .string("invented-unselected"), "installation_id": .string("66666666-7777-8888-9999-000000000000"),
                "kind": .string("user"), "computer_type": .string("app_connection"),
                "display_name": .string("Invented Other Mac"), "status": .string("offline"), "can_connect": .bool(false)
            ])
            let cloud: JSONValue = .object([
                "environment_id": .string("invented-cloud"), "kind": .string("openai_cloud"),
                "display_name": .string("Invented Cloud"), "can_connect": .bool(false),
                "unknown_metadata": .object(["preserved": .bool(true)])
            ])
            let response: JSONValue = .object(["thread_id": .string(thread), "computers": .array([selected, unselected, cloud]), "next_cursor": .null])
            return RegistryCapture.Scope(dotID: dot, threadID: thread, computersResponse: String(decoding: try response.data(), as: UTF8.self))
        }
        let root: JSONValue = .object(["items": .array(scopes.map { .object(["id": .string($0.dotID), "active_root_thread_id": .string($0.threadID), "display_name": .string("Invented Dot")]) }), "cursor": .null])
        return RegistryCapture(accountIDHash: try account().accountIDHash, rootResponse: String(decoding: try root.data(), as: UTF8.self), scopes: scopes)
    }
}
