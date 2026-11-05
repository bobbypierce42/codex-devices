import Foundation
import Darwin

public struct AccountSession: Sendable {
    public let accountIDHash: String
    let accountID: String
    let token: String
    public init(token: String) throws {
        let parts = token.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3, token.utf8.count <= 65536, !token.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw ServiceError.authentication }
        var base64 = String(parts[1]).replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        base64 += String(repeating: "=", count: (4 - base64.count % 4) % 4)
        guard let bytes = Data(base64Encoded: base64), let metadata = try? JSONValue.parse(bytes),
              let id = metadata["https://api.openai.com/auth"]["chatgpt_account_id"].string, !id.isEmpty, id.utf8.count <= 4096,
              id.utf8.allSatisfy({ (33...126).contains($0) }) else { throw ServiceError.authentication }
        // This is routing metadata from an existing token, not signature validation.
        self.accountIDHash = hash(id); self.accountID = id; self.token = token
    }
}
public typealias AccountProvider = @Sendable () async throws -> AccountSession

public struct AppServerSession: Sendable {
    public let executable: URL
    private let timeout: TimeInterval
    public init(executable: URL, timeout: TimeInterval = 15) { self.executable = executable; self.timeout = min(max(timeout, 0.01), 15) }
    public static func installedExecutable() throws -> URL {
        try selectInstalledExecutable(apps: ["/Applications/Codex.app", "/Applications/ChatGPT.app"], isExecutable: { FileManager.default.isExecutableFile(atPath: $0) })
    }
    static func selectInstalledExecutable(apps: [String], isExecutable: (String) -> Bool) throws -> URL {
        for app in apps {
            for path in ["Contents/Resources/codex-cli/bin/codex", "Contents/Resources/codex"] {
                let candidate = URL(fileURLWithPath: app).appendingPathComponent(path)
                if isExecutable(candidate.path) { return candidate }
            }
        }
        throw ServiceError.unavailable
    }
    public func account() async throws -> AccountSession {
        let worker = Task.detached { try Self.exchange(executable: executable, timeout: timeout) }
        return try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
    }
    static func exchange(executable: URL, timeout: TimeInterval) throws -> AccountSession {
        if Task.isCancelled { throw ServiceError.canceled }
        let process = Process(), input = Pipe(), output = Pipe()
        guard fcntl(input.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1) == 0 else { throw ServiceError.unavailable }
        process.executableURL = executable; process.arguments = ["app-server", "--listen", "stdio://"]
        process.standardInput = input; process.standardOutput = output; process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { throw ServiceError.unavailable }
        defer {
            try? input.fileHandleForWriting.close(); try? output.fileHandleForReading.close()
            if process.isRunning { process.terminate(); usleep(20_000); if process.isRunning { kill(process.processIdentifier, SIGKILL) } }
            process.waitUntilExit()
        }
        let fd = output.fileHandleForReading.fileDescriptor
        guard fcntl(fd, F_SETFL, O_NONBLOCK) == 0 else { throw ServiceError.unavailable }
        let deadline = Date().addingTimeInterval(timeout)
        var pending = Data(), total = 0
        func send(_ value: JSONValue) throws {
            do { try input.fileHandleForWriting.write(contentsOf: value.data() + Data([10])) } catch { throw ServiceError.unavailable }
        }
        func response(_ expected: Int) throws -> JSONValue {
            while Date() < deadline {
                if Task.isCancelled { throw ServiceError.canceled }
                if let end = pending.firstIndex(of: 10) {
                    let line = pending.prefix(upTo: end); pending.removeSubrange(...end)
                    let value = try JSONValue.parse(Data(line))
                    if value["method"].string != nil && value["id"] != .null { throw ServiceError.refused }
                    if value["id"] == .number(Double(expected)) {
                        guard value["result"] != .null, value["error"] == .null else { throw ServiceError.authentication }
                        return value["result"]
                    }
                    continue
                }
                var bytes = [UInt8](repeating: 0, count: 4096)
                let count = Darwin.read(fd, &bytes, bytes.count)
                if count > 0 {
                    total += count
                    guard total <= 1024 * 1024, pending.count + count <= 65536 else { throw ServiceError.oversized }
                    pending.append(contentsOf: bytes.prefix(count))
                } else if count == 0 { throw ServiceError.invalidResponse }
                else if errno != EAGAIN && errno != EINTR { throw ServiceError.unavailable }
                else { usleep(10_000) }
            }
            throw ServiceError.timeout
        }
        try send(.object(["id": .number(1), "method": .string("initialize"), "params": .object(["clientInfo": .object(["name": .string("codex-devices-desktop"), "version": .string("0.1.0")]), "capabilities": .object(["experimentalApi": .bool(true), "explicitGatewayOauth": .bool(true)])])]))
        _ = try response(1)
        try send(.object(["method": .string("initialized")]))
        try send(.object(["id": .number(2), "method": .string("account/read"), "params": .object(["refreshToken": .bool(false)])]))
        let account = try response(2)
        guard account["account"]["type"].string == "chatgpt" else { throw ServiceError.authentication }
        try send(.object(["id": .number(3), "method": .string("getAuthStatus"), "params": .object(["includeToken": .bool(true), "refreshToken": .bool(false)])]))
        let auth = try response(3)
        guard ["chatgpt", "chatgptAuthTokens"].contains(auth["authMethod"].string ?? ""), let token = auth["authToken"].string else { throw ServiceError.authentication }
        return try AccountSession(token: token)
    }
}
