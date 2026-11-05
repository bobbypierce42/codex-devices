import Foundation
import Combine

public struct ServiceProviders: Sendable {
    public var discover: @Sendable (String) async -> DiscoveryResult
    public var inspect: @Sendable (HostCandidate, String) async throws -> PeerInspection
    public var account: AccountProvider
    public var capture: @Sendable (AccountSession) async throws -> RegistryCapture
    public var rename: @Sendable (AccountSession, String, String) async throws -> Void
    public var engine: @Sendable (JSONValue) async throws -> OperationReceipt
    public init(discover: @escaping @Sendable (String) async -> DiscoveryResult, inspect: @escaping @Sendable (HostCandidate, String) async throws -> PeerInspection, account: @escaping AccountProvider, capture: @escaping @Sendable (AccountSession) async throws -> RegistryCapture, rename: @escaping @Sendable (AccountSession, String, String) async throws -> Void, engine: @escaping @Sendable (JSONValue) async throws -> OperationReceipt) {
        self.discover = discover; self.inspect = inspect; self.account = account; self.capture = capture; self.rename = rename; self.engine = engine
    }
}
@MainActor public final class RepairCoordinator: ObservableObject {
    @Published public private(set) var candidates = [HostCandidate]()
    @Published public private(set) var devices = [LogicalDevice]()
    @Published public private(set) var inspections = [PeerInspection]()
    @Published public private(set) var phase: RepairState = .idle
    @Published public private(set) var message = "Ready to discover Macs."
    @Published public private(set) var receipt: OperationReceipt?
    @Published public var manualHost = ""
    @Published public var sshUser = ""
    @Published public private(set) var sourceFailures = [SourceFailure]()
    public let isDemo: Bool
    private let providers: ServiceProviders
    private let artifactParent: URL
    private var operationDirectory: URL?
    private var reviewedAccount: String?
    private var selections = [ReviewSelection]()
    private var operationPeers = [PeerInspection]()
    private var busy = false
    private var attempted = Set<String>()
    private var pendingReference: URL { artifactParent.appendingPathComponent("pending-operation.json") }
    public init(providers: ServiceProviders, artifactParent: URL, isDemo: Bool = false) {
        self.providers = providers; self.artifactParent = artifactParent; self.isDemo = isDemo
    }
    public convenience init(engineURL: URL, helperURL: URL, artifactParent: URL, trust: @escaping TrustDecision) {
        let discovery = DiscoveryService()
        let ssh = SSHService(helperURL: helperURL, trustDirectory: artifactParent.appendingPathComponent("ssh"), trust: trust)
        let registry = RegistryClient(), engine = EngineClient(executable: engineURL)
        self.init(providers: ServiceProviders(discover: { await discovery.scan(manualHost: $0) }, inspect: { try await ssh.inspect($0, user: $1) }, account: { try await AppServerSession(executable: AppServerSession.installedExecutable()).account() }, capture: { try await registry.capture(account: $0) }, rename: { try await registry.rename(account: $0, installationID: $1, newName: $2) }, engine: { try await engine.send($0) }), artifactParent: artifactParent)
    }
    private func start(_ state: RepairState) -> Bool {
        guard !busy else { return false }; busy = true; phase = state; return true
    }
    private func fail(_ error: Error) {
        phase = attempted.isEmpty ? .failed : .uncertain
        message = (error as? ServiceError ?? .unavailable).localizedDescription
    }
    public func scan() async {
        let completed = phase == .verified
        guard start(.scanning) else { return }; defer { busy = false }
        if completed { attempted = [] }
        inspections = []
        if attempted.isEmpty { receipt = nil; operationDirectory = nil; selections = []; operationPeers = []; reviewedAccount = nil }
        let result = await providers.discover(manualHost)
        candidates = result.candidates; sourceFailures = result.failures
        do {
            let account = try await providers.account()
            let capture = try await providers.capture(account)
            devices = try capture.devices()
            if try await restorePending(account: account) { return }
            phase = .ready
            message = result.failures.isEmpty ? "Discovery and account inventory complete." : "Inventory complete. Some discovery sources were unavailable."
        } catch { fail(error) }
    }
    public func discoverOnly() async {
        guard start(.scanning) else { return }; defer { busy = false }
        inspections = []; devices = []
        let result = await providers.discover(manualHost)
        candidates = result.candidates; sourceFailures = result.failures
        phase = .ready
        message = result.failures.isEmpty ? "Mac discovery complete." : "Discovery complete. Some sources were unavailable."
    }
    public func inspect(_ candidate: HostCandidate) async {
        guard start(.inspecting) else { return }; defer { busy = false }
        do {
            let peer = try await providers.inspect(candidate, sshUser)
            guard isHash(peer.installationIDHash) else { throw ServiceError.invalidResponse }
            inspections.removeAll { $0.candidate.id == candidate.id }; inspections.append(peer)
            phase = .ready; message = "Mac inspected. Match by installation fingerprint."
            if inspectionCollision(peer.installationIDHash) { message = "Identity collision: several inspected Macs share this installation fingerprint. Repair is blocked." }
        } catch { fail(error) }
    }
    private func inspectionCollision(_ fingerprint: String) -> Bool {
        let matches = inspections.filter { $0.installationIDHash == fingerprint }
        return matches.count > 1 && representativeInspection(for: fingerprint) == nil
    }
    private func representativeInspection(for fingerprint: String) -> PeerInspection? {
        let matches = inspections.filter { $0.installationIDHash == fingerprint }
        guard let first = matches.first else { return nil }
        guard matches.count > 1 else { return first }
        guard let key = first.hostKeyFingerprint, matches.allSatisfy({ $0.hostKeyFingerprint == key && $0.computerName == first.computerName }) else { return nil }
        return first
    }
    public func matchingInspection(for device: LogicalDevice) -> PeerInspection? {
        matchingInspection(for: device, in: devices)
    }
    public func matchingInspection(installationIDHash: String) -> PeerInspection? {
        representativeInspection(for: installationIDHash)
    }
    private func matchingInspection(for device: LogicalDevice, in devices: [LogicalDevice]) -> PeerInspection? {
        guard device.kind == "user", device.computerType == "app_connection", !device.installationID.isEmpty, !device.displayName.isEmpty,
              ["connected", "offline"].contains(device.occurrences.first?.record["status"].string ?? ""),
              devices.filter({ $0.installationIDHash == device.installationIDHash }).count == 1 else { return nil }
        return representativeInspection(for: device.installationIDHash)
    }
    private func saveReference(complete: Bool) throws {
        guard !isDemo, let receipt, let reviewedAccount else { return }
        try SSHService.privateDirectory(artifactParent)
        if FileManager.default.fileExists(atPath: pendingReference.path) { try SSHService.privateFile(pendingReference) }
        let data = try JSONValue.object(["operation_directory": .string(receipt.operationDirectory), "plan_sha256": .string(receipt.planSHA256), "account_id_sha256": .string(reviewedAccount), "complete": .bool(complete)]).data()
        try data.write(to: pendingReference, options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: pendingReference.path)
        let handle = try FileHandle(forWritingTo: pendingReference)
        defer { try? handle.close() }; try handle.synchronize()
    }
    private func restorePending(account: AccountSession) async throws -> Bool {
        guard !isDemo, FileManager.default.fileExists(atPath: pendingReference.path) else { return false }
        try SSHService.privateDirectory(artifactParent); try SSHService.privateFile(pendingReference)
        let reference = try JSONValue.parse(Data(contentsOf: pendingReference))
        if reference["complete"] == .bool(true) { return false }
        guard reference["complete"] == .bool(false), reference["account_id_sha256"].string == account.accountIDHash,
              let directory = reference["operation_directory"].string, let digest = reference["plan_sha256"].string, isHash(digest) else { throw ServiceError.drift }
        try await loadPending(directory: URL(fileURLWithPath: directory), digest: digest, account: account)
        return true
    }
    private func loadPending(directory: URL, digest: String, account: AccountSession) async throws {
        guard isHash(digest), directory.deletingLastPathComponent().standardizedFileURL.pathComponents == artifactParent.standardizedFileURL.pathComponents else { throw ServiceError.refused }
        let request: JSONValue = .object(["schema_version": .number(1), "op": .string("status"), "operation_directory": .string(directory.path), "plan_sha256": .string(digest)])
        let status = try await providers.engine(request)
        guard status.planSHA256 == digest, status.operationDirectory == directory.path else { throw ServiceError.drift }
        operationDirectory = directory; receipt = status; reviewedAccount = account.accountIDHash
        attempted = Set(status.records.filter { $0.state != "pending" }.map(\.environmentID))
        phase = .uncertain; message = "A prior operation requires fresh readback. Requests will not be repeated."
    }
    public func resumePending(operationDirectory: URL, planDigest: String) async {
        guard start(.applying) else { return }
        do { try await loadPending(directory: operationDirectory, digest: planDigest, account: providers.account()) }
        catch { fail(error) }
        busy = false
        if receipt != nil { await reconcilePending() }
    }
    private func peers(for selections: [ReviewSelection], in devices: [LogicalDevice]) throws -> [PeerInspection] {
        guard !selections.isEmpty, Set(selections.map(\.environmentID)).count == selections.count else { throw ServiceError.invalidInput }
        return try selections.map { selection in
            guard let device = devices.first(where: { $0.environmentID == selection.environmentID }),
                  let peer = matchingInspection(for: device, in: devices) else { throw ServiceError.drift }
            return peer
        }
    }
    private func peerJSON(_ peers: [PeerInspection], _ selections: [ReviewSelection]) -> JSONValue {
        .array(zip(peers, selections).map { peer, selection in .object(["environment_id": .string(selection.environmentID), "installation_id_sha256": .string(peer.installationIDHash), "host": .string(peer.candidate.host), "computer_name": .string(peer.computerName)]) })
    }
    private func request(_ op: String, capture: RegistryCapture? = nil, peers: [PeerInspection]? = nil, environmentID: String? = nil) throws -> JSONValue {
        guard let directory = operationDirectory else { throw ServiceError.invalidInput }
        var request: [String: JSONValue] = ["schema_version": .number(1), "op": .string(op), "operation_directory": .string(directory.path)]
        if let receipt { request["plan_sha256"] = .string(receipt.planSHA256) }
        if let capture { request["capture"] = capture.json }
        if let peers { request["peers"] = peerJSON(peers, selections) }
        if let environmentID { request["environment_id"] = .string(environmentID) }
        return .object(request)
    }
    private func accept(_ value: OperationReceipt, status: String) throws {
        guard value.status == status, value.operationDirectory == operationDirectory?.path,
              receipt == nil || value.planSHA256 == receipt?.planSHA256 else { throw value.status == "uncertain" ? ServiceError.uncertain : ServiceError.refused }
        receipt = value
    }
    public func prepare(_ selected: [ReviewSelection]) async {
        guard start(.preparing) else { return }; defer { busy = false }
        guard attempted.isEmpty else { fail(ServiceError.uncertain); return }
        do {
            let account = try await providers.account(), capture = try await providers.capture(account)
            if try await restorePending(account: account) { throw ServiceError.uncertain }
            let freshDevices = try capture.devices(), bound = try peers(for: selected, in: freshDevices)
            if !isDemo { try SSHService.privateDirectory(artifactParent) }
            operationDirectory = artifactParent.appendingPathComponent("operation-\(UUID().uuidString)")
            receipt = nil; selections = selected; operationPeers = bound; reviewedAccount = account.accountIDHash
            var json = try request("prepare", capture: capture, peers: bound)
            if case .object(var fields) = json { fields["selections"] = .array(selected.map { .object(["environment_id": .string($0.environmentID), "new_name": .string($0.newName)]) }); json = .object(fields) }
            try accept(await providers.engine(json), status: "prepared")
            devices = freshDevices; phase = .review; message = "Review selected names and the anchored backup before applying."
        } catch { fail(error) }
    }
    private func freshPeers() async throws -> [PeerInspection] {
        var fresh = [PeerInspection]()
        for previous in operationPeers {
            let peer = try await providers.inspect(previous.candidate, sshUser)
            guard peer.installationIDHash == previous.installationIDHash, peer.computerName == previous.computerName, peer.hostKeyFingerprint == previous.hostKeyFingerprint else { throw ServiceError.drift }
            fresh.append(peer)
        }
        return fresh
    }
    public func applyReviewed(planDigest: String) async {
        guard phase == .review, let receipt, receipt.planSHA256 == planDigest, start(.applying) else { return }
        defer { busy = false }
        do {
            for selection in selections {
                guard !attempted.contains(selection.environmentID) else { throw ServiceError.uncertain }
                let account = try await providers.account()
                guard account.accountIDHash == reviewedAccount else { throw ServiceError.drift }
                let capture = try await providers.capture(account), fresh = try await freshPeers()
                let current = try capture.devices()
                guard let device = current.first(where: { $0.environmentID == selection.environmentID }), device.installationIDHash == fresh[selections.firstIndex(of: selection)!].installationIDHash else { throw ServiceError.drift }
                try accept(await providers.engine(request("preflight", capture: capture, peers: fresh)), status: "ready")
                try saveReference(complete: false)
                try accept(await providers.engine(request("intent", capture: capture, peers: fresh, environmentID: selection.environmentID)), status: "intent-recorded")
                // From this point every failure is readback-only. Never repeat this POST.
                attempted.insert(selection.environmentID)
                do { try await providers.rename(account, device.installationID, selection.newName) } catch { message = "Request outcome uncertain. Reading back without retry." }
                let afterAccount = try await providers.account()
                guard afterAccount.accountIDHash == reviewedAccount else { throw ServiceError.drift }
                let after = try await providers.capture(afterAccount)
                try accept(await providers.engine(request("reconcile", capture: after, environmentID: selection.environmentID)), status: "verified")
                devices = try after.devices()
            }
            try saveReference(complete: true)
            phase = .verified; message = isDemo ? "Synthetic repair verified. No live state was accessed." : "Fresh account readback verified every selected occurrence."
        } catch { fail(error) }
    }
    public func reconcilePending() async {
        guard let receipt, start(.applying) else { return }; defer { busy = false }
        do {
            let account = try await providers.account()
            guard account.accountIDHash == reviewedAccount else { throw ServiceError.drift }
            let status = try await providers.engine(request("status"))
            guard status.planSHA256 == receipt.planSHA256, status.operationDirectory == operationDirectory?.path else { throw ServiceError.drift }
            for record in status.records where record.state == "intent-recorded" || record.state == "uncertain" || record.state == "verified" {
                attempted.insert(record.environmentID)
                let capture = try await providers.capture(account)
                try accept(await providers.engine(request("reconcile", capture: capture, environmentID: record.environmentID)), status: "verified")
                devices = try capture.devices()
            }
            guard self.receipt?.records.allSatisfy({ $0.state == "verified" }) == true else { throw ServiceError.uncertain }
            try saveReference(complete: true)
            phase = .verified; message = isDemo ? "Synthetic readback verified." : "Fresh account readback verified."
        } catch { fail(error) }
    }
    public static func demo() -> RepairCoordinator {
        let fixture = DemoRegistry()
        return RepairCoordinator(providers: fixture.providers, artifactParent: URL(fileURLWithPath: "/tmp/codex-devices-synthetic-demo"), isDemo: true)
    }
}

private actor DemoRegistry {
    private var name = "Old Studio"
    private let installation = "12345678-1234-1234-1234-123456789abc"
    private let candidate = HostCandidate(host: "studio.example", name: "Studio", source: "Synthetic")
    private var attempted = false
    private var verified = false
    private var directory = ""
    private var proposed = ""
    private let digest = String(repeating: "a", count: 64)
    nonisolated var providers: ServiceProviders {
        ServiceProviders(discover: { _ in DiscoveryResult(candidates: [await self.host()], failures: []) }, inspect: { candidate, _ in await self.peer(candidate) }, account: { try await self.account() }, capture: { _ in await self.capture() }, rename: { _, _, name in await self.rename(name) }, engine: { try await self.engine($0) })
    }
    func host() -> HostCandidate { candidate }
    func peer(_ candidate: HostCandidate) -> PeerInspection { PeerInspection(candidate: candidate, computerName: "Studio", installationIDHash: hash(installation), hostKeyFingerprint: "SHA256:synthetic") }
    func account() throws -> AccountSession {
        let metadata = Data(#"{"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}"#.utf8).base64EncodedString()
        return try AccountSession(token: "synthetic.\(metadata).synthetic")
    }
    func capture() -> RegistryCapture {
        let row: JSONValue = .object(["environment_id": .string("synthetic-studio"), "installation_id": .string(installation), "kind": .string("user"), "computer_type": .string("app_connection"), "display_name": .string(name), "status": .string("connected"), "grant": .string("synthetic-owner")])
        let response: JSONValue = .object(["thread_id": .string("synthetic-thread"), "computers": .array([row]), "next_cursor": .null])
        return RegistryCapture(accountIDHash: hash("synthetic-account"), rootResponse: #"{"items":[{"id":"synthetic~dot","active_root_thread_id":"synthetic-thread","display_name":"Demo"}],"cursor":null}"#, scopes: [.init(dotID: "synthetic~dot", threadID: "synthetic-thread", computersResponse: String(data: try! response.data(), encoding: .utf8)!)])
    }
    func rename(_ newName: String) { name = newName }
    func engine(_ request: JSONValue) throws -> OperationReceipt {
        let op = request["op"].string ?? ""
        var status = "refused"
        switch op {
        case "prepare":
            directory = request["operation_directory"].string ?? ""; proposed = request["selections"].array?.first?["new_name"].string ?? ""; status = "prepared"
        case "preflight": status = "ready"
        case "intent": guard !attempted else { throw ServiceError.uncertain }; attempted = true; status = "intent-recorded"
        case "reconcile": verified = name == proposed; status = verified ? "verified" : "uncertain"
        case "status": status = verified ? "verified" : "uncertain"
        default: throw ServiceError.invalidInput
        }
        let value: JSONValue = .object(["schema_version": .number(1), "op": .string(op), "status": .string(status), "operation_directory": .string(directory), "plan_sha256": .string(digest), "reason_code": .string("synthetic"), "records": .array([.object(["environment_id": .string("synthetic-studio"), "installation_id_sha256": .string(hash(installation)), "old_name": .string("Old Studio"), "new_name": .string(proposed), "state": .string(verified ? "verified" : attempted ? "intent-recorded" : "pending")])])])
        return try JSONDecoder().decode(OperationReceipt.self, from: value.data())
    }
}
