import Foundation
import Darwin

public typealias TrustDecision = @Sendable (HostTrustChallenge) async -> Bool
public struct SSHService: Sendable {
    private let helperURL: URL
    private let trustDirectory: URL
    private let runner: ProcessRunner
    private let trust: TrustDecision
    public init(helperURL: URL, trustDirectory: URL, runner: @escaping ProcessRunner = { try await NativeProcess.run($0) }, trust: @escaping TrustDecision) {
        self.helperURL = helperURL; self.trustDirectory = trustDirectory; self.runner = runner; self.trust = trust
    }
    public func inspect(_ candidate: HostCandidate, user: String = "") async throws -> PeerInspection {
        guard safeHost(candidate.host), user.isEmpty || (safeHost(user) && !user.contains(".")), helperURL.path.hasPrefix("/") else { throw ServiceError.invalidInput }
        if candidate.isLocal {
            guard candidate.host == "localhost" else { throw ServiceError.invalidInput }
            let result = try await runner(ProcessRequest(executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", Self.inspectionCommand]))
            return try Self.parseInspection(result, candidate: candidate, hostKey: await localHostKey())
        }
        let knownHosts = trustDirectory.appendingPathComponent("known_hosts")
        let knownHostsOption = try Self.knownHostsOption(knownHosts)
        try Self.privateDirectory(trustDirectory)
        if !FileManager.default.fileExists(atPath: knownHosts.path) {
            guard FileManager.default.createFile(atPath: knownHosts.path, contents: Data(), attributes: [.posixPermissions: 0o600]) else { throw ServiceError.refused }
        }
        try Self.privateFile(knownHosts)
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh-keyscan"), .keyscanBegin)
        let scan = try await runner(ProcessRequest(executable: URL(fileURLWithPath: "/usr/bin/ssh-keyscan"), arguments: ["-T", "5", "-t", "ed25519", "--", candidate.host], timeout: 7, outputLimit: 65536))
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh-keyscan"), .keyscanReturned, status: scan.status)
        guard scan.status == 0, let text = String(data: scan.output, encoding: .utf8) else { throw ServiceError.unavailable }
        let keys = text.split(separator: "\n").filter { !$0.hasPrefix("#") }
        guard keys.count == 1 else { throw ServiceError.refused }
        let fields = keys[0].split(whereSeparator: { $0 == " " || $0 == "\t" })
        guard fields.count == 3, fields[0] == Substring(candidate.host), fields[1] == "ssh-ed25519", Data(base64Encoded: String(fields[2])) != nil else { throw ServiceError.invalidResponse }
        let key = String(keys[0]) + "\n"
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh-keygen"), .fingerprintBegin)
        let fingerprint = try await runner(ProcessRequest(executable: URL(fileURLWithPath: "/usr/bin/ssh-keygen"), arguments: ["-lf", "-"], input: Data(key.utf8), outputLimit: 4096))
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh-keygen"), .fingerprintReturned, status: fingerprint.status)
        guard fingerprint.status == 0, let description = String(data: fingerprint.output, encoding: .utf8), let digest = description.split(separator: " ").first(where: { $0.hasPrefix("SHA256:") }) else { throw ServiceError.invalidResponse }
        let pinned = try String(contentsOf: knownHosts, encoding: .utf8).split(separator: "\n").filter { $0.split(separator: " ").first == Substring(candidate.host) }
        if !pinned.isEmpty {
            guard pinned.contains(Substring(key.trimmingCharacters(in: .newlines))) else { throw ServiceError.changedHostKey }
        } else {
            guard await trust(HostTrustChallenge(host: candidate.host, fingerprint: String(digest))) else { throw ServiceError.trustRejected }
            try Self.privateFile(knownHosts)
            let handle = try FileHandle(forWritingTo: knownHosts)
            defer { try? handle.close() }
            try handle.seekToEnd(); try handle.write(contentsOf: Data(key.utf8)); try handle.synchronize()
        }
        var arguments = ["-T", "-oStrictHostKeyChecking=yes", knownHostsOption, "-oGlobalKnownHostsFile=/dev/null", "-oUpdateHostKeys=no", "-oConnectTimeout=5", "-oPermitLocalCommand=no", "-oClearAllForwardings=yes", "-oForwardAgent=no", "-oForwardX11=no", "-oControlMaster=no", "-oControlPath=none", "-oNumberOfPasswordPrompts=1"]
        if !user.isEmpty { arguments += ["-l", user] }
        arguments += ["--", candidate.host, Self.inspectionCommand]
        let environment = ["SSH_ASKPASS": helperURL.path, "SSH_ASKPASS_REQUIRE": "force", "DISPLAY": "codex-devices:0", "CODEX_DEVICES_ASKPASS_SESSION": UUID().uuidString, "CODEX_DEVICES_ASKPASS_DEADLINE": String(Int(Date().timeIntervalSince1970 + 60))]
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh"), .sshBegin)
        let result = try await runner(ProcessRequest(executable: URL(fileURLWithPath: "/usr/bin/ssh"), arguments: arguments, environment: environment, timeout: 75, outputLimit: 8192))
        SSHProcessDiagnostics.emit(URL(fileURLWithPath: "/usr/bin/ssh"), .sshReturned, status: result.status)
        return try Self.parseInspection(result, candidate: candidate, hostKey: String(digest))
    }
    private func localHostKey() async throws -> String? {
        do {
            let result = try await runner(ProcessRequest(executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", Self.publicHostKeyCommand], timeout: 5, outputLimit: 4096))
            guard result.status == 0, let text = String(data: result.output, encoding: .utf8), let fingerprint = text.split(separator: " ").first(where: { $0.hasPrefix("SHA256:") }) else { return nil }
            return String(fingerprint)
        } catch ServiceError.canceled { throw ServiceError.canceled }
        catch is CancellationError { throw ServiceError.canceled }
        catch { return nil }
    }
    static func parseInspection(_ result: ProcessResult, candidate: HostCandidate, hostKey: String?) throws -> PeerInspection {
        guard result.status == 0, let text = String(data: result.output, encoding: .utf8) else { throw ServiceError.refused }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        guard lines.count == 3, lines[2].isEmpty, let data = Data(base64Encoded: String(lines[0])), let name = String(data: data, encoding: .utf8), !name.isEmpty, name.utf8.count <= 1024,
              !name.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains), isHash(String(lines[1])) else { throw ServiceError.invalidResponse }
        return PeerInspection(candidate: candidate, computerName: name, installationIDHash: String(lines[1]), hostKeyFingerprint: hostKey)
    }
    static func knownHostsOption(_ url: URL) throws -> String {
        let path = url.path
        guard path.hasPrefix("/"), !path.contains("\""), !path.contains("\\"), !path.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw ServiceError.refused }
        // OpenSSH parses -o values as configuration text even within one argv item.
        return "-oUserKnownHostsFile=\"\(path)\""
    }
    static func privateDirectory(_ url: URL) throws {
        guard url.path.hasPrefix("/"), !url.pathComponents.contains("..") else { throw ServiceError.refused }
        var current = URL(fileURLWithPath: "/")
        for part in url.pathComponents.dropFirst() {
            current.appendPathComponent(part)
            var info = stat()
            if lstat(current.path, &info) != 0 {
                guard mkdir(current.path, 0o700) == 0 else { throw ServiceError.refused }
                guard lstat(current.path, &info) == 0 else { throw ServiceError.refused }
            }
            guard info.st_mode & S_IFMT == S_IFDIR else { throw ServiceError.refused }
        }
        var info = stat(); guard lstat(url.path, &info) == 0, info.st_uid == getuid(), info.st_mode & 0o077 == 0 else { throw ServiceError.refused }
    }
    static func privateFile(_ url: URL) throws {
        var info = stat()
        guard lstat(url.path, &info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_uid == getuid(), info.st_nlink == 1, info.st_mode & 0o077 == 0, info.st_size <= 65536 else { throw ServiceError.refused }
    }
    static let publicHostKeyCommand = #"""
    set -eu
    p=/private/etc/ssh/ssh_host_ed25519_key.pub
    for d in /private /private/etc /private/etc/ssh "$p"; do test ! -L "$d" || exit 31; done
    test -f "$p" || exit 31
    before=$(/usr/bin/stat -f '%u %l %z %Lp %i %m %c' "$p")
    set -- $before
    test "$1" = 0 && test "$2" = 1 && test "$3" -gt 0 && test "$3" -le 8192 || exit 31
    test $((0$4 & 022)) -eq 0 || exit 31
    fingerprint=$(/usr/bin/ssh-keygen -lf "$p")
    test "$(/usr/bin/stat -f '%u %l %z %Lp %i %m %c' "$p")" = "$before" || exit 31
    printf '%s\n' "$fingerprint"
    """#
    public static let inspectionCommand = #"""
    set -eu
    p="$HOME/Library/Application Support/Codex/.codex-tpp-local-executor-installation-id"
    d="$p"
    while test "$d" != /; do
      test ! -L "$d" || exit 31
      d=$(/usr/bin/dirname "$d")
    done
    test -f "$p" || exit 31
    before=$(/usr/bin/stat -f '%u %l %z %Lp %i %m %c' "$p")
    set -- $before
    test "$1" = "$(/usr/bin/id -u)" && test "$2" = 1 && test "$3" -gt 0 && test "$3" -le 128 || exit 31
    mode=$4; size=$3
    test $((0$mode & 022)) -eq 0 || exit 31
    value=$(/bin/cat "$p")
    test "$(/usr/bin/stat -f '%u %l %z %Lp %i %m %c' "$p")" = "$before" || exit 31
    printf '%s' "$value" | /usr/bin/grep -Eq '^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$' || exit 31
    test ${#value} -eq 36 || exit 31
    test "$size" -eq 36 || { test "$size" -eq 37 && test "$(/usr/bin/tail -c 1 "$p" | /usr/bin/od -An -tu1 | /usr/bin/tr -d ' ')" = 10; } || exit 31
    name=$(/usr/sbin/scutil --get ComputerName)
    printf '%s' "$name" | /usr/bin/base64 | /usr/bin/tr -d '\n'
    printf '\n'
    printf '%s' "$value" | /usr/bin/shasum -a 256 | /usr/bin/cut -d ' ' -f 1
    """#
}
