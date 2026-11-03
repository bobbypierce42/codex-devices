import Foundation
import Darwin

public struct ProcessRequest: Sendable {
    public let executable: URL
    public let arguments: [String]
    public let environment: [String: String]
    public let input: Data
    public let timeout: TimeInterval
    public let outputLimit: Int
    public init(executable: URL, arguments: [String], environment: [String: String] = [:], input: Data = Data(), timeout: TimeInterval = 15, outputLimit: Int = 4 * 1024 * 1024) {
        self.executable = executable; self.arguments = arguments; self.environment = environment; self.input = input; self.timeout = timeout; self.outputLimit = outputLimit
    }
}
public struct ProcessResult: Sendable {
    public let status: Int32
    public let output: Data
    public init(status: Int32, output: Data) { self.status = status; self.output = output }
}
public typealias ProcessRunner = @Sendable (ProcessRequest) async throws -> ProcessResult

// The child and its descriptors belong exclusively to this worker, never the UI actor.
public enum NativeProcess {
    public static func run(_ request: ProcessRequest) async throws -> ProcessResult {
        let worker = Task.detached { try execute(request) }
        return try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
    }
    static func execute(_ request: ProcessRequest) throws -> ProcessResult {
        if Task.isCancelled { throw ServiceError.canceled }
        guard request.executable.path.hasPrefix("/"), request.input.count <= 4 * 1024 * 1024 else { throw ServiceError.invalidInput }
        let child = Process(), output = Pipe(), input = Pipe()
        child.executableURL = request.executable; child.arguments = request.arguments
        var env = ProcessInfo.processInfo.environment
        for (key, value) in request.environment { env[key] = value }
        child.environment = env; child.standardInput = input; child.standardOutput = output
        child.standardError = FileHandle.nullDevice
        do { try child.run() } catch { throw ServiceError.unavailable }
        defer {
            try? input.fileHandleForWriting.close(); try? output.fileHandleForReading.close()
            if child.isRunning { child.terminate(); usleep(20_000); if child.isRunning { kill(child.processIdentifier, SIGKILL) } }
            child.waitUntilExit()
        }
        let descriptor = output.fileHandleForReading.fileDescriptor
        let inputDescriptor = input.fileHandleForWriting.fileDescriptor
        guard fcntl(descriptor, F_SETFL, O_NONBLOCK) == 0,
              fcntl(inputDescriptor, F_SETFL, O_NONBLOCK) == 0,
              fcntl(inputDescriptor, F_SETNOSIGPIPE, 1) == 0 else { throw ServiceError.unavailable }
        var written = 0, inputClosed = false
        var received = Data(); let deadline = Date().addingTimeInterval(request.timeout)
        var bytes = [UInt8](repeating: 0, count: 8192)
        while true {
            if Task.isCancelled { throw ServiceError.canceled }
            if Date() >= deadline { throw ServiceError.timeout }
            if !inputClosed {
                if written < request.input.count {
                    let count = request.input.withUnsafeBytes { buffer in
                        Darwin.write(inputDescriptor, buffer.baseAddress!.advanced(by: written), min(8192, request.input.count - written))
                    }
                    if count > 0 { written += count }
                    else if count < 0 && errno != EAGAIN && errno != EINTR { throw ServiceError.unavailable }
                }
                if written == request.input.count { try input.fileHandleForWriting.close(); inputClosed = true }
            }
            let count = Darwin.read(descriptor, &bytes, bytes.count)
            if count > 0 {
                guard received.count + count <= request.outputLimit else { throw ServiceError.oversized }
                received.append(contentsOf: bytes.prefix(count))
            } else if count == 0 { break }
            else if errno != EAGAIN && errno != EINTR { throw ServiceError.unavailable }
            else { usleep(10_000) }
        }
        while child.isRunning {
            if Task.isCancelled { throw ServiceError.canceled }
            guard Date() < deadline else { throw ServiceError.timeout }
            usleep(10_000)
        }
        guard written == request.input.count else { throw ServiceError.unavailable }
        return ProcessResult(status: child.terminationStatus, output: received)
    }
}

public struct EngineClient: Sendable {
    private let executable: URL
    private let runner: ProcessRunner
    public init(executable: URL, runner: @escaping ProcessRunner = { try await NativeProcess.run($0) }) { self.executable = executable; self.runner = runner }
    public func send(_ request: JSONValue) async throws -> OperationReceipt {
        let input = try request.data()
        guard input.count <= 4 * 1024 * 1024 else { throw ServiceError.oversized }
        let result = try await runner(ProcessRequest(executable: executable, arguments: ["desktop"], input: input))
        guard result.output.count <= 4 * 1024 * 1024 else { throw ServiceError.oversized }
        if result.status != 0 {
            let error = try JSONValue.parse(result.output)
            if error["schema_version"] == .number(1), error["status"].string == "refused", ["io", "integrity", "refused"].contains(error["reason_code"].string ?? "") {
                throw ServiceError.refused
            }
        }
        guard [0, 3, 4, 5].contains(result.status),
              let receipt = try? JSONDecoder().decode(OperationReceipt.self, from: result.output), receipt.schemaVersion == 1, receipt.op == request["op"].string,
              ["prepared", "ready", "intent-recorded", "verified", "uncertain", "refused"].contains(receipt.status),
              isHash(receipt.planSHA256) else { throw ServiceError.invalidResponse }
        return receipt
    }
}
