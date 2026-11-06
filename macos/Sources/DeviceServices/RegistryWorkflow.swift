import Foundation
import Combine
import Darwin

public struct RegistryComputer: Identifiable, Sendable, Equatable {
    public var id: String { environmentID }
    public let environmentID: String
    public let displayName: String
    public let installationIDHash: String
    public let kind: String
    public let computerType: String
    public let status: String?
    public let isRenameable: Bool
    public let scopeCount: Int
}

@MainActor public final class RegistryWorkflow: ObservableObject {
    @Published public private(set) var records = [RegistryComputer]()
    @Published public private(set) var phase: RepairState = .idle
    @Published public private(set) var message = "Refresh to read your Codex computers."
    @Published public private(set) var receipt: OperationReceipt?
    @Published public private(set) var operationDirectory: URL?
    @Published public private(set) var isBusy = false
    public let isDemo: Bool
    private let runner: RegistryCommandRunner
    private let artifactParent: URL
    private var unresolvedReference = false

    public init(providers: RegistryCommandProviders, artifactParent: URL, isDemo: Bool = false) {
        self.runner = RegistryCommandRunner(providers: providers); self.artifactParent = artifactParent; self.isDemo = isDemo
    }
    public convenience init(engineURL: URL, artifactParent: URL) {
        self.init(providers: .native(engineURL: engineURL), artifactParent: artifactParent)
    }
    private func begin(_ phase: RepairState) -> Bool {
        guard !isBusy else { return false }; isBusy = true; self.phase = phase; return true
    }
    private func failure(_ error: Error) {
        phase = unresolvedReference || receipt?.records.contains(where: { $0.state != "pending" }) == true ? .uncertain : .failed
        message = unresolvedReference
            ? "A saved operation cannot be resolved safely. Only readback is allowed; its request will not be repeated."
            : (error as? ServiceError ?? .unavailable).localizedDescription
    }
    public func refresh() async {
        guard begin(.scanning) else { return }; defer { isBusy = false }
        do {
            try await loadRecords()
            unresolvedReference = true
            if let reference = try WorkflowReviewReference.read(parent: artifactParent) {
                let directory = artifactParent.appendingPathComponent(reference.basename, isDirectory: true)
                let saved = try await runner.review(operationDirectory: directory, planSHA256: reference.digest)
                receipt = saved; operationDirectory = directory
                if saved.status == "prepared", saved.records[0].state == "pending" {
                    unresolvedReference = false
                    phase = .review; message = "A saved review is ready. Check its names and backup before applying."
                } else if saved.status == "verified", saved.records[0].state == "verified",
                          (try? RegistryResponseEvidence.read(directory: directory, digest: reference.digest, environmentID: saved.records[0].environmentID)) == .accepted {
                    unresolvedReference = false
                    receipt = nil; operationDirectory = nil; phase = .ready
                    message = "Previous operation verified. Select a computer to review a different label."
                } else {
                    phase = .uncertain; message = "A prior operation needs fresh readback. Its request will not be repeated."
                }
            } else {
                unresolvedReference = false
                receipt = nil; operationDirectory = nil; phase = .ready
                message = isDemo ? "Synthetic account inventory. No live account was accessed." : "Account inventory loaded. Select a computer to review a different label."
            }
        } catch { failure(error) }
    }
    public func prepare(environmentID: String, newName: String) async {
        guard !isBusy else { return }
        guard !unresolvedReference, phase != .uncertain, let record = records.first(where: { $0.environmentID == environmentID }), record.isRenameable, record.displayName != newName else { message = ServiceError.refused.localizedDescription; return }
        guard begin(.preparing) else { return }; defer { isBusy = false }
        do {
            let basename = UUID().uuidString, directory = artifactParent.appendingPathComponent(basename, isDirectory: true)
            let command = try RegistryCommand.parse(["rename", "--id", environmentID, "--name", newName, "--out", directory.path])
            try SSHService.privateDirectory(artifactParent)
            let result = await runner.run(command)
            let prepared = try decodeReceipt(result, directory: directory, digest: nil)
            guard result.exitCode == 0, prepared.status == "prepared", prepared.records[0].state == "pending" else { throw ServiceError.refused }
            try WorkflowReviewReference.save(parent: artifactParent, basename: basename, digest: prepared.planSHA256)
            receipt = prepared; operationDirectory = directory; phase = .review
            message = isDemo ? "Synthetic backup prepared. Review the exact name change." : "Private backup prepared. Review the exact name change before applying."
        } catch { failure(error) }
    }
    public func applyReviewed(planDigest: String) async {
        guard !isBusy, phase == .review, let receipt, receipt.status == "prepared", receipt.records[0].state == "pending", receipt.planSHA256 == planDigest, let directory = operationDirectory else { message = ServiceError.refused.localizedDescription; return }
        guard begin(.applying) else { return }; defer { isBusy = false }
        unresolvedReference = true
        let result = await runner.run(arguments: ["apply", "--operation", directory.path, "--approve-plan-sha256", planDigest])
        await complete(result, directory: directory, digest: planDigest)
    }
    public func verifyPending() async {
        guard !isBusy, let receipt, let directory = operationDirectory else { return }
        guard begin(.applying) else { return }; defer { isBusy = false }
        let result = await runner.run(arguments: ["verify", "--operation", directory.path, "--expected-plan-sha256", receipt.planSHA256])
        await complete(result, directory: directory, digest: receipt.planSHA256)
    }
    private func complete(_ result: RegistryCommandResult, directory: URL, digest: String) async {
        do {
            if result.exitCode == 0 {
                let verified = try decodeReceipt(result, directory: directory, digest: digest)
                guard verified.status == "verified", verified.records[0].state == "verified" else { throw ServiceError.refused }
                receipt = verified; phase = .verified; unresolvedReference = false
                message = isDemo ? "Synthetic name change verified with the Rust engine." : "Name change verified by the response and fresh complete account readback."
                do { try await loadRecords() } catch { message += " The computer list could not be refreshed." }
            } else {
                receipt = try await runner.review(operationDirectory: directory, planSHA256: digest)
                if result.exitCode == 3, let receipt, receipt.status == "prepared", receipt.records[0].state == "pending",
                   try RegistryResponseEvidence.readIfPresent(directory: directory, digest: digest, environmentID: receipt.records[0].environmentID) == nil {
                    unresolvedReference = false; phase = .failed
                    message = "This attempt did not send a rename request. Account details may have changed; prepare a new backup and review."
                    return
                }
                if let receipt, receipt.status == "prepared", receipt.records[0].state == "pending",
                   (try? RegistryResponseEvidence.read(directory: directory, digest: digest, environmentID: receipt.records[0].environmentID)) == .unknown {
                    unresolvedReference = false; phase = .review
                    message = "No intent was recorded. Review the saved change before retrying."
                    return
                }
                phase = result.exitCode == 4 || receipt?.records[0].state != "pending" ? .uncertain : .failed
                unresolvedReference = phase == .uncertain
                message = result.json?["reason_code"].string == "response-unconfirmed" || result.json?["reason_code"].string == "response-mismatch"
                    ? "Response proof is incomplete. Readback cannot resolve this automatically; maintainer investigation is required."
                    : (phase == .uncertain ? ServiceError.uncertain.localizedDescription : ServiceError.refused.localizedDescription)
            }
        } catch {
            if result.exitCode == 4 { unresolvedReference = true }
            failure(error)
        }
    }
    private func decodeReceipt(_ result: RegistryCommandResult, directory: URL, digest: String?) throws -> OperationReceipt {
        guard let value = result.json?["receipt"], let decoded = try? JSONDecoder().decode(OperationReceipt.self, from: value.data()), decoded.evidenceMode == "registry-label", decoded.records.count == 1,
              decoded.operationDirectory == directory.appendingPathComponent("engine", isDirectory: true).path, isHash(decoded.planSHA256), digest == nil || decoded.planSHA256 == digest else { throw ServiceError.refused }
        return decoded
    }
    private func loadRecords() async throws {
        let result = await runner.run(arguments: ["audit"])
        guard result.exitCode == 0, result.json?["evidence_mode"].string == "registry-label", let rows = result.json?["records"].array else { throw ServiceError.unavailable }
        var fingerprints = [String: Int]()
        for row in rows { if let fingerprint = row["installation_id_sha256"].string { fingerprints[fingerprint, default: 0] += 1 } }
        records = try rows.map { row in
            guard let id = row["environment_id"].string, let name = row["display_name"].string, let fingerprint = row["installation_id_sha256"].string, let kind = row["kind"].string, let type = row["computer_type"].string, let scopes = row["scopes"].array else { throw ServiceError.invalidResponse }
            let status = row["status"].string
            return RegistryComputer(environmentID: id, displayName: name, installationIDHash: fingerprint, kind: kind, computerType: type, status: status,
                isRenameable: kind == "user" && type == "app_connection" && !name.isEmpty && isHash(fingerprint) && fingerprint != hash("") && fingerprints[fingerprint] == 1 && ["connected", "offline"].contains(status ?? ""), scopeCount: scopes.count)
        }
    }
    public static func demo(engineURL: URL) -> RegistryWorkflow {
        let fixture = WorkflowDemoRegistry(engineURL: engineURL)
        return RegistryWorkflow(providers: fixture.providers, artifactParent: URL(fileURLWithPath: "/private/tmp/codex-devices-workflow-demo-\(UUID().uuidString)"), isDemo: true)
    }
}

private enum WorkflowReviewReference {
    struct Reference { let basename: String; let digest: String }
    private static let filename = "last-review.json"
    static func read(parent: URL) throws -> Reference? {
        var info = stat()
        if lstat(parent.path, &info) != 0 { guard errno == ENOENT else { throw ServiceError.refused }; return nil }
        let directory = try RegistryResponseEvidence.directoryDescriptor(parent); defer { close(directory) }
        let fd = openat(directory, filename, O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC)
        if fd < 0 { guard errno == ENOENT else { throw ServiceError.refused }; return nil }
        defer { close(fd) }; try RegistryResponseEvidence.validate(fd)
        var bytes = [UInt8](repeating: 0, count: 4097), data = Data()
        while true {
            let count = Darwin.read(fd, &bytes, bytes.count)
            if count < 0 && errno == EINTR { continue }; guard count >= 0 else { throw ServiceError.refused }
            if count == 0 { break }; guard data.count + count <= 4096 else { throw ServiceError.refused }; data.append(contentsOf: bytes.prefix(count))
        }
        let value = try JSONValue.parse(data)
        guard case .object(let fields) = value, Set(fields.keys) == ["operation_basename", "plan_sha256"], let basename = value["operation_basename"].string, UUID(uuidString: basename) != nil, !basename.contains("/"), let digest = value["plan_sha256"].string, isHash(digest) else { throw ServiceError.refused }
        return Reference(basename: basename, digest: digest)
    }
    static func save(parent: URL, basename: String, digest: String) throws {
        _ = try read(parent: parent)
        let directory = try RegistryResponseEvidence.directoryDescriptor(parent); defer { close(directory) }
        let temporary = "last-review-next-\(UUID().uuidString).json"
        let fd = openat(directory, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw ServiceError.refused }; defer { close(fd) }
        let data = try JSONValue.object(["operation_basename": .string(basename), "plan_sha256": .string(digest)]).data()
        var offset = 0
        while offset < data.count {
            let count = data.withUnsafeBytes { Darwin.write(fd, $0.baseAddress!.advanced(by: offset), data.count - offset) }
            if count < 0 && errno == EINTR { continue }; guard count > 0 else { throw ServiceError.unavailable }; offset += count
        }
        guard fsync(fd) == 0, renameat(directory, temporary, directory, filename) == 0, fsync(directory) == 0 else { throw ServiceError.unavailable }
    }
}

private actor WorkflowDemoRegistry {
    private let engine: EngineClient
    private var name = "Old Studio"
    private let installation = "12345678-1234-1234-1234-123456789abc"
    init(engineURL: URL) { engine = EngineClient(executable: engineURL) }
    nonisolated var providers: RegistryCommandProviders {
        RegistryCommandProviders(account: { try await self.account() }, capture: { _ in try await self.capture() }, rename: { _, _, name, id in await self.rename(name, id) }, engine: { try await self.engine.send($0) })
    }
    private func account() throws -> AccountSession {
        let metadata = Data(#"{"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#.utf8).base64EncodedString()
        return try AccountSession(token: "synthetic.\(metadata).synthetic")
    }
    private func capture() throws -> RegistryCapture {
        let root: JSONValue = .object(["items": .array([.object(["id": .string("synthetic~dot"), "active_root_thread_id": .string("synthetic-thread"), "display_name": .string("Demo")])]), "cursor": .null])
        let computers: JSONValue = .object(["thread_id": .string("synthetic-thread"), "next_cursor": .null, "computers": .array([.object(["environment_id": .string("synthetic-studio"), "installation_id": .string(installation), "kind": .string("user"), "computer_type": .string("app_connection"), "display_name": .string(name), "status": .string("connected")])])])
        return RegistryCapture(accountIDHash: hash("synthetic-account"), rootResponse: String(data: try root.data(), encoding: .utf8)!, scopes: [.init(dotID: "synthetic~dot", threadID: "synthetic-thread", computersResponse: String(data: try computers.data(), encoding: .utf8)!)])
    }
    private func rename(_ name: String, _ id: String) -> RegistryRenameReceipt { self.name = name; return .init(executorID: id, customDisplayName: name) }
}
