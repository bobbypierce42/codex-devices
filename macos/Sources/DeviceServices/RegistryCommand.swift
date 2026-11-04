import Foundation
import Darwin

public struct RegistryCommand: Sendable, Equatable {
    public enum Action: String, Sendable { case help, audit, rename, apply, verify }
    public let action: Action
    public let environmentID: String?
    public let newName: String?
    public let operationDirectory: URL?
    public let planSHA256: String?
    public let appServerURL: URL?
    public let engineURL: URL?

    public static let help = """
    codex-devices-mac audit [--app-server ABS] [--engine ABS]
    codex-devices-mac rename --id FULL_ENV_ID --name LABEL --out ABS_NEW_DIR
    codex-devices-mac apply --operation ABS_DIR --approve-plan-sha256 HASH
    codex-devices-mac verify --operation ABS_DIR --expected-plan-sha256 HASH
    All commands accept --app-server ABS and --engine ABS. Output is JSON.
    Rename prepares a private backup and plan. Apply requires its reviewed hash.
    Registry-label evidence does not establish a freshly inspected physical Mac.
    A missing or mismatched POST response remains uncertain and requires maintainer
    investigation. Verify reads back without sending another rename request.
    """

    public static func parse(_ arguments: [String]) throws -> RegistryCommand {
        let names = ["audit", "rename", "apply", "verify"]
        if arguments == ["--help"] || arguments == ["help"] || (arguments.count == 2 && names.contains(arguments[0]) && arguments[1] == "--help") {
            return RegistryCommand(action: .help, environmentID: nil, newName: nil, operationDirectory: nil, planSHA256: nil, appServerURL: nil, engineURL: nil)
        }
        guard let first = arguments.first, let action = Action(rawValue: first), action != .help else { throw ServiceError.invalidInput }
        let required: Set<String>
        switch action {
        case .audit: required = []
        case .rename: required = ["--id", "--name", "--out"]
        case .apply: required = ["--operation", "--approve-plan-sha256"]
        case .verify: required = ["--operation", "--expected-plan-sha256"]
        case .help: throw ServiceError.invalidInput
        }
        let allowed = required.union(["--app-server", "--engine"])
        var flags = [String: String](), index = 1
        while index < arguments.count {
            let key = arguments[index]
            guard allowed.contains(key), flags[key] == nil, index + 1 < arguments.count, !arguments[index + 1].hasPrefix("--"), !arguments[index + 1].isEmpty else { throw ServiceError.invalidInput }
            flags[key] = arguments[index + 1]; index += 2
        }
        guard required.isSubset(of: Set(flags.keys)) else { throw ServiceError.invalidInput }
        if let name = flags["--name"] {
            guard name == name.trimmingCharacters(in: .whitespacesAndNewlines), name.utf16.count <= 100, !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw ServiceError.invalidInput }
        }
        if let id = flags["--id"] { guard id.utf8.count <= 4096, !id.unicodeScalars.contains(where: CharacterSet.whitespacesAndNewlines.union(.controlCharacters).contains) else { throw ServiceError.invalidInput } }
        let digest = flags["--approve-plan-sha256"] ?? flags["--expected-plan-sha256"]
        if let digest { guard isHash(digest) else { throw ServiceError.invalidInput } }
        return try RegistryCommand(action: action, environmentID: flags["--id"], newName: flags["--name"], operationDirectory: (flags["--out"] ?? flags["--operation"]).map(absoluteURL), planSHA256: digest, appServerURL: flags["--app-server"].map(absoluteURL), engineURL: flags["--engine"].map(absoluteURL))
    }
    private static func absoluteURL(_ path: String) throws -> URL {
        guard path.hasPrefix("/"), !path.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains), !path.split(separator: "/").contains("..") else { throw ServiceError.invalidInput }
        // Foundation standardization can shorten existing /private paths through
        // symlink aliases. Keep identity stable and let no-follow storage checks run.
        let parts = path.split(separator: "/").filter { $0 != "." }
        return URL(fileURLWithPath: "/" + parts.joined(separator: "/"))
    }
}

public struct RegistryCommandProviders: Sendable {
    public let account: AccountProvider
    public let capture: @Sendable (AccountSession) async throws -> RegistryCapture
    public let rename: @Sendable (AccountSession, String, String, String) async throws -> RegistryRenameReceipt
    public let engine: @Sendable (JSONValue) async throws -> OperationReceipt
    public init(account: @escaping AccountProvider, capture: @escaping @Sendable (AccountSession) async throws -> RegistryCapture, rename: @escaping @Sendable (AccountSession, String, String, String) async throws -> RegistryRenameReceipt, engine: @escaping @Sendable (JSONValue) async throws -> OperationReceipt) {
        self.account = account; self.capture = capture; self.rename = rename; self.engine = engine
    }
    public static func native(command: RegistryCommand, executableURL: URL) -> RegistryCommandProviders {
        native(engineURL: command.engineURL ?? executableURL.deletingLastPathComponent().appendingPathComponent("codex-devices"), appServerURL: command.appServerURL)
    }
    public static func native(engineURL: URL, appServerURL: URL? = nil) -> RegistryCommandProviders {
        let registry = RegistryClient(), engine = EngineClient(executable: engineURL)
        return RegistryCommandProviders(account: { try await AppServerSession(executable: appServerURL ?? AppServerSession.installedExecutable()).account() }, capture: { try await registry.capture(account: $0) }, rename: { try await registry.rename(account: $0, installationID: $1, newName: $2, expectedEnvironmentID: $3) }, engine: { try await engine.send($0) })
    }
}
public struct RegistryCommandResult: Sendable {
    public let exitCode: Int32
    public let json: JSONValue?
    public let text: String?
    public var output: String { text ?? String(data: (try? json?.data()) ?? Data(), encoding: .utf8) ?? "{}" }
}

public struct RegistryCommandRunner: Sendable {
    private let providers: RegistryCommandProviders
    public init(providers: RegistryCommandProviders) { self.providers = providers }
    public func review(operationDirectory: URL, planSHA256: String) async throws -> OperationReceipt {
        let command = try RegistryCommand.parse(["verify", "--operation", operationDirectory.path, "--expected-plan-sha256", planSHA256])
        return try validate(await providers.engine(request(op: "status", directory: operationDirectory.appendingPathComponent("engine", isDirectory: true), digest: planSHA256)), command: command)
    }
    public func run(arguments: [String]) async -> RegistryCommandResult {
        do { return await run(try RegistryCommand.parse(arguments)) }
        catch { return Self.failure(.invalidInput, uncertain: false) }
    }
    public func run(_ command: RegistryCommand) async -> RegistryCommandResult {
        var intentMayExist = false
        do {
            if command.action == .help { return .init(exitCode: 0, json: nil, text: RegistryCommand.help) }
            if command.action == .audit {
                let account = try await providers.account(), capture = try await providers.capture(account)
                let rows = try capture.devices().map { device in JSONValue.object([
                    "environment_id": .string(device.environmentID), "display_name": .string(device.displayName), "installation_id_sha256": .string(device.installationIDHash), "kind": .string(device.kind), "computer_type": .string(device.computerType),
                    "status": device.occurrences.first?.record["status"] ?? .null,
                    "scopes": .array(device.occurrences.map { .object(["dot_id": .string($0.dotID), "thread_id": .string($0.threadID)]) })
                ]) }
                return result(status: "audited", rows: rows)
            }
            guard let directory = command.operationDirectory else { throw ServiceError.invalidInput }
            let engineDirectory = directory.appendingPathComponent("engine", isDirectory: true)
            if command.action == .rename {
                guard let id = command.environmentID, let name = command.newName else { throw ServiceError.invalidInput }
                try RegistryResponseEvidence.createWrapper(directory)
                let account = try await providers.account(), capture = try await providers.capture(account)
                let devices = try capture.devices()
                guard let device = devices.first(where: { $0.environmentID == id }), device.kind == "user", device.computerType == "app_connection", !device.installationID.isEmpty,
                      devices.filter({ $0.installationIDHash == device.installationIDHash }).count == 1 else { throw ServiceError.refused }
                let request: JSONValue = .object(["schema_version": .number(1), "op": .string("prepare"), "operation_directory": .string(engineDirectory.path), "evidence_mode": .string("registry-label"), "capture": capture.json,
                    "selections": .array([.object(["environment_id": .string(id), "new_name": .string(name)])]), "peers": .array([])])
                let receipt = try validate(await providers.engine(request), command: command, status: "prepared")
                return try result(receipt: receipt, directory: directory)
            }
            guard let digest = command.planSHA256 else { throw ServiceError.invalidInput }
            let statusRequest = request(op: "status", directory: engineDirectory, digest: digest)
            let status = try validate(await providers.engine(statusRequest), command: command)
            let record = status.records[0]
            if command.action == .apply {
                guard status.status == "prepared", record.state == "pending" else { throw ServiceError.refused }
                let account = try await providers.account(), capture = try await providers.capture(account)
                let current = try capture.devices()
                guard let device = current.first(where: { $0.environmentID == record.environmentID }), device.installationIDHash == record.installationIDHash else { throw ServiceError.drift }
                _ = try validate(await providers.engine(request(op: "preflight", directory: engineDirectory, digest: digest, capture: capture, includePeers: true)), command: command, status: "ready")
                try RegistryResponseEvidence.create(directory: directory, digest: digest, environmentID: record.environmentID)
                intentMayExist = true
                _ = try validate(await providers.engine(request(op: "intent", directory: engineDirectory, digest: digest, capture: capture, includePeers: true, environmentID: record.environmentID)), command: command, status: "intent-recorded")
                var outcome = RegistryResponseEvidence.Outcome.unknown
                do {
                    let response = try await providers.rename(account, device.installationID, record.newName, record.environmentID)
                    outcome = response.validates(environmentID: record.environmentID, newName: record.newName) ? .accepted : .rejected
                } catch ServiceError.drift { outcome = .rejected }
                catch ServiceError.invalidResponse { outcome = .rejected }
                catch { outcome = .unknown }
                do { try RegistryResponseEvidence.update(directory: directory, digest: digest, environmentID: record.environmentID, outcome: outcome) }
                catch { outcome = .unknown }
                return try await readback(command: command, record: record, accountHash: account.accountIDHash, outcome: outcome)
            }
            guard command.action == .verify else { throw ServiceError.invalidInput }
            guard record.state != "pending" else { return result(status: "not-applied", reason: "no-intent", exit: 4) }
            intentMayExist = true
            let outcome = (try? RegistryResponseEvidence.read(directory: directory, digest: digest, environmentID: record.environmentID)) ?? .unknown
            return try await readback(command: command, record: record, accountHash: nil, outcome: outcome)
        } catch { return Self.failure(error as? ServiceError ?? .unavailable, uncertain: intentMayExist) }
    }
    private func readback(command: RegistryCommand, record: OperationReceipt.Record, accountHash: String?, outcome: RegistryResponseEvidence.Outcome) async throws -> RegistryCommandResult {
        let account = try await providers.account()
        if let accountHash, account.accountIDHash != accountHash { throw ServiceError.drift }
        let capture = try await providers.capture(account)
        let receipt = try validate(await providers.engine(request(op: "reconcile", directory: command.operationDirectory!.appendingPathComponent("engine", isDirectory: true), digest: command.planSHA256!, capture: capture, environmentID: record.environmentID)), command: command)
        guard outcome == .accepted else { return result(status: "uncertain", reason: outcome == .rejected ? "response-mismatch" : "response-unconfirmed", exit: 4) }
        guard receipt.status == "verified", receipt.records[0].state == "verified" else { return try result(receipt: receipt, directory: command.operationDirectory!, exit: 4) }
        return try result(receipt: receipt, directory: command.operationDirectory!)
    }
    private func request(op: String, directory: URL, digest: String, capture: RegistryCapture? = nil, includePeers: Bool = false, environmentID: String? = nil) -> JSONValue {
        var fields: [String: JSONValue] = ["schema_version": .number(1), "op": .string(op), "operation_directory": .string(directory.path), "plan_sha256": .string(digest)]
        if let capture { fields["capture"] = capture.json }
        if includePeers { fields["peers"] = .array([]) }
        if let environmentID { fields["environment_id"] = .string(environmentID) }
        return .object(fields)
    }
    private func validate(_ receipt: OperationReceipt, command: RegistryCommand, status: String? = nil) throws -> OperationReceipt {
        guard receipt.schemaVersion == 1, receipt.evidenceMode == "registry-label", receipt.operationDirectory == command.operationDirectory?.appendingPathComponent("engine", isDirectory: true).path, isHash(receipt.planSHA256), receipt.records.count == 1,
              command.planSHA256 == nil || receipt.planSHA256 == command.planSHA256, status == nil || receipt.status == status,
              ["prepared", "ready", "intent-recorded", "verified", "uncertain"].contains(receipt.status),
              ["pending", "intent-recorded", "verified", "uncertain"].contains(receipt.records[0].state) else { throw ServiceError.refused }
        if command.action == .rename {
            guard receipt.records[0].environmentID == command.environmentID, receipt.records[0].newName == command.newName else { throw ServiceError.refused }
        }
        return receipt
    }
    private func result(receipt: OperationReceipt, directory: URL, exit: Int32 = 0) throws -> RegistryCommandResult {
        let encoded = try JSONEncoder().encode(receipt)
        return .init(exitCode: exit, json: .object(["operation_directory": .string(directory.path), "evidence_mode": .string("registry-label"), "fresh_peer_inspection": .bool(false), "receipt": try JSONValue.parse(encoded)]), text: nil)
    }
    private func result(status: String, reason: String = "ok", exit: Int32 = 0, rows: [JSONValue] = []) -> RegistryCommandResult {
        var fields: [String: JSONValue] = ["status": .string(status), "reason_code": .string(reason), "evidence_mode": .string("registry-label"), "fresh_peer_inspection": .bool(false), "records": .array(rows)]
        if reason == "response-mismatch" || reason == "response-unconfirmed" {
            fields["request_retry_allowed"] = .bool(false)
            fields["message"] = .string("POST response proof is incomplete. Fresh readback cannot resolve this automatically; maintainer investigation is required.")
        }
        return .init(exitCode: exit, json: .object(fields), text: nil)
    }
    public static func failure(_ error: ServiceError, uncertain: Bool) -> RegistryCommandResult {
        let code: Int32 = uncertain ? 4 : error == .invalidInput ? 2 : 3
        return .init(exitCode: code, json: .object(["status": .string(uncertain ? "uncertain" : "refused"), "reason_code": .string(error.rawValue), "evidence_mode": .string("registry-label"), "fresh_peer_inspection": .bool(false)]), text: nil)
    }
}

enum RegistryResponseEvidence {
    enum Outcome: String { case unknown, accepted, rejected }
    static let filename = "registry-response.json"
    static func createWrapper(_ directory: URL) throws {
        let parent = try directoryDescriptor(directory.deletingLastPathComponent(), privateMode: false); defer { close(parent) }
        guard !directory.lastPathComponent.isEmpty, mkdirat(parent, directory.lastPathComponent, 0o700) == 0, fsync(parent) == 0 else { throw ServiceError.refused }
    }
    static func directoryDescriptor(_ directory: URL, privateMode: Bool = true) throws -> Int32 {
        var current = URL(fileURLWithPath: "/")
        for part in directory.pathComponents.dropFirst() {
            current.appendPathComponent(part); var info = stat()
            guard lstat(current.path, &info) == 0, info.st_mode & S_IFMT == S_IFDIR else { throw ServiceError.refused }
        }
        let fd = open(directory.path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw ServiceError.refused }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_uid == getuid(), info.st_mode & (privateMode ? 0o077 : 0o022) == 0 else { close(fd); throw ServiceError.refused }
        return fd
    }
    static func create(directory: URL, digest: String, environmentID: String) throws { try write(directory: directory, digest: digest, environmentID: environmentID, outcome: .unknown, create: true) }
    static func update(directory: URL, digest: String, environmentID: String, outcome: Outcome) throws { try write(directory: directory, digest: digest, environmentID: environmentID, outcome: outcome, create: false) }
    private static func write(directory: URL, digest: String, environmentID: String, outcome: Outcome, create: Bool) throws {
        let dir = try directoryDescriptor(directory); defer { close(dir) }
        if !create { _ = try read(directory: directory, digest: digest, environmentID: environmentID) }
        let temporary = create ? filename : "registry-response-next-\(UUID().uuidString).json"
        let fd = openat(dir, temporary, O_WRONLY | O_NOFOLLOW | O_CLOEXEC | O_CREAT | O_EXCL, 0o600)
        guard fd >= 0 else { throw ServiceError.refused }; defer { close(fd) }
        try validate(fd)
        let bytes = try JSONValue.object(["plan_sha256": .string(digest), "environment_id": .string(environmentID), "outcome": .string(outcome.rawValue)]).data()
        guard bytes.count <= 4096 else { throw ServiceError.refused }
        var offset = 0
        while offset < bytes.count {
            let count = bytes.withUnsafeBytes { Darwin.write(fd, $0.baseAddress!.advanced(by: offset), bytes.count - offset) }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw ServiceError.unavailable }; offset += count
        }
        guard fsync(fd) == 0 else { throw ServiceError.unavailable }
        if !create { guard renameat(dir, temporary, dir, filename) == 0 else { throw ServiceError.unavailable } }
        guard fsync(dir) == 0 else { throw ServiceError.unavailable }
    }
    static func read(directory: URL, digest: String, environmentID: String) throws -> Outcome {
        let dir = try directoryDescriptor(directory); defer { close(dir) }
        let fd = openat(dir, filename, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw ServiceError.uncertain }; defer { close(fd) }; try validate(fd)
        var data = Data(), bytes = [UInt8](repeating: 0, count: 4096)
        while true {
            let count = Darwin.read(fd, &bytes, bytes.count)
            if count < 0 && errno == EINTR { continue }
            guard count >= 0 else { throw ServiceError.unavailable }; if count == 0 { break }
            guard data.count + count <= 4096 else { throw ServiceError.refused }; data.append(contentsOf: bytes.prefix(count))
        }
        let value = try JSONValue.parse(data)
        guard value["plan_sha256"].string == digest, value["environment_id"].string == environmentID, let raw = value["outcome"].string, let outcome = Outcome(rawValue: raw) else { throw ServiceError.refused }
        return outcome
    }
    static func validate(_ fd: Int32) throws {
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_uid == getuid(), info.st_nlink == 1, info.st_mode & 0o077 == 0, info.st_size <= 4096 else { throw ServiceError.refused }
    }
}
