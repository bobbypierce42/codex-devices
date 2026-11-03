import Foundation

public typealias DiscoveryProvider = @Sendable () async throws -> [HostCandidate]
public struct DiscoveryService: Sendable {
    private let bonjour: DiscoveryProvider
    private let tailscale: DiscoveryProvider
    public init() {
        self.bonjour = { try await BonjourScan.perform() }; self.tailscale = { try await DiscoveryService.tailscaleCandidates() }
    }
    public init(bonjour: @escaping DiscoveryProvider, tailscale: @escaping DiscoveryProvider) {
        self.bonjour = bonjour; self.tailscale = tailscale
    }
    public func scan(manualHost: String = "") async -> DiscoveryResult {
        async let first = collect("Bonjour", bonjour)
        async let second = collect("Tailscale", tailscale)
        let values = await [first, second]
        var candidates = [HostCandidate(host: "localhost", name: "This Mac", source: "local", isLocal: true)]
        var failures = [SourceFailure]()
        for value in values { candidates += value.0; if let failure = value.1 { failures.append(failure) } }
        if !manualHost.isEmpty {
            if safeHost(manualHost) { candidates.append(HostCandidate(host: manualHost)) }
            else { failures.append(SourceFailure(source: "Manual host", error: .invalidInput)) }
        }
        var seen = Set<String>()
        return DiscoveryResult(candidates: candidates.filter { seen.insert($0.host).inserted }, failures: failures)
    }
    private func collect(_ source: String, _ provider: DiscoveryProvider) async -> ([HostCandidate], SourceFailure?) {
        do { return (try await provider(), nil) }
        catch { return ([], SourceFailure(source: source, error: error as? ServiceError ?? .unavailable)) }
    }
    public static func parseTailscale(_ data: Data) throws -> [HostCandidate] {
        let value = try JSONValue.parse(data)
        guard value["BackendState"].string == "Running", case .object(let peers) = value["Peer"], peers.count <= 4096 else { throw ServiceError.unavailable }
        return peers.values.compactMap { peer in
            guard peer["Online"] == .bool(true), peer["OS"].string == "macOS", let dns = peer["DNSName"].string else { return nil }
            let host = dns.hasSuffix(".") ? String(dns.dropLast()) : dns
            guard safeHost(host) else { return nil }
            return HostCandidate(host: host, name: peer["HostName"].string ?? host, source: "Tailscale")
        }.sorted { $0.host < $1.host }
    }
    static func tailscaleCandidates() async throws -> [HostCandidate] {
        let paths = ["/Applications/Tailscale.app/Contents/MacOS/Tailscale", "/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"]
        guard let path = paths.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else { throw ServiceError.unavailable }
        let response = try await NativeProcess.run(ProcessRequest(executable: URL(fileURLWithPath: path), arguments: ["status", "--json"], timeout: 5))
        guard response.status == 0 else { throw ServiceError.unavailable }
        return try parseTailscale(response.output)
    }
}
func safeHost(_ value: String) -> Bool {
    !value.isEmpty && value.utf8.count <= 253 && value.utf8.first.map { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) } == true && value.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45,46,95].contains($0) }
}

@MainActor private final class BonjourScan: NSObject {
    private let browser = NetServiceBrowser()
    private var services = [NetService]()
    private var found = [HostCandidate]()
    private var continuation: CheckedContinuation<[HostCandidate], Error>?
    private var deadline: Task<Void, Never>?
    static func perform() async throws -> [HostCandidate] {
        let scan = BonjourScan()
        return try await withTaskCancellationHandler(operation: {
            try Task.checkCancellation()
            return try await withCheckedThrowingContinuation { continuation in
                scan.continuation = continuation; scan.browser.delegate = scan
                scan.browser.searchForServices(ofType: "_ssh._tcp.", inDomain: "local.")
                scan.deadline = Task { try? await Task.sleep(nanoseconds: 4_000_000_000); scan.finish(nil) }
            }
        }, onCancel: { Task { @MainActor in scan.finish(.canceled) } })
    }
    private func finish(_ error: ServiceError?) {
        guard let continuation else { return }
        self.continuation = nil; deadline?.cancel(); browser.stop(); services.forEach { $0.stop() }
        if let error { continuation.resume(throwing: error) } else { continuation.resume(returning: found) }
    }
    private func found(_ service: NetService) {
        guard services.count < 4096 else { finish(.oversized); return }
        services.append(service); service.delegate = self; service.resolve(withTimeout: 3)
    }
    private func resolved(_ sender: NetService) {
        guard sender.port == 22, let name = sender.hostName else { return }
        let host = name.hasSuffix(".") ? String(name.dropLast()) : name
        if safeHost(host) { found.append(HostCandidate(host: host, name: sender.name, source: "Bonjour")) }
    }
}
// Browser and services are created and scheduled only on the main run loop.
// Enforce that invariant at the delegate boundary rather than sharing their state.
extension BonjourScan: NetServiceBrowserDelegate, NetServiceDelegate {
    nonisolated func netServiceBrowser(_ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool) { MainActor.assumeIsolated { found(service) } }
    nonisolated func netServiceBrowser(_ browser: NetServiceBrowser, didNotSearch errorDict: [String: NSNumber]) { MainActor.assumeIsolated { finish(.unavailable) } }
    nonisolated func netServiceDidResolveAddress(_ sender: NetService) { MainActor.assumeIsolated { resolved(sender) } }
    nonisolated func netService(_ sender: NetService, didNotResolve errorDict: [String: NSNumber]) { MainActor.assumeIsolated { finish(.unavailable) } }
}
