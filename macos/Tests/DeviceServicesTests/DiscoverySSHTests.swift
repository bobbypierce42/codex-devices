import Foundation
import XCTest
@testable import DeviceServices

final class DiscoverySSHTests: XCTestCase {
    func testSourceFailuresKeepLocalAndManualCandidates() async {
        let service = DiscoveryService(bonjour: { throw ServiceError.timeout }, tailscale: { throw ServiceError.unavailable })
        let result = await service.scan(manualHost: "studio.example")
        XCTAssertEqual(result.candidates.map(\.host), ["localhost", "studio.example"])
        XCTAssertEqual(result.failures.map(\.error), [.timeout, .unavailable])
        let refused = await service.scan(manualHost: "-oProxyCommand=bad")
        XCTAssertEqual(refused.failures.last?.error, .invalidInput)
    }
    func testTailscaleFiltersOfflineAndOtherSystems() throws {
        let data = Data(#"{"BackendState":"Running","Peer":{"one":{"Online":true,"OS":"macOS","DNSName":"studio.example.","HostName":"Studio"},"two":{"Online":false,"OS":"macOS","DNSName":"old.example."},"three":{"Online":true,"OS":"linux","DNSName":"server.example."}}}"#.utf8)
        XCTAssertEqual(try DiscoveryService.parseTailscale(data).map(\.host), ["studio.example"])
    }
    func testInspectionParsesNameAndFingerprintOnly() throws {
        let name = Data("Studio Mac".utf8).base64EncodedString()
        let result = ProcessResult(status: 0, output: Data("\(name)\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        let peer = try SSHService.parseInspection(result, candidate: fixtureHost, hostKey: "SHA256:fixture")
        XCTAssertEqual(peer.computerName, "Studio Mac")
        XCTAssertEqual(peer.installationIDHash, DeviceServices.hash(fixtureInstallation))
        XCTAssertEqual(peer.hostKeyFingerprint, "SHA256:fixture")
    }
    func testInspectionCommandReportsHostnameWithSyntheticIdentity() async throws {
        let directory = try temporaryDirectory()
        let identity = directory.appendingPathComponent("synthetic-id")
        try Data(fixtureInstallation.utf8).write(to: identity)
        let original = #"p="$HOME/Library/Application Support/Codex/.codex-tpp-local-executor-installation-id""#
        guard SSHService.inspectionCommand.components(separatedBy: original).count == 2 else {
            XCTFail("The fixture must replace exactly one identity path")
            return
        }
        let script = SSHService.inspectionCommand.replacingOccurrences(of: original, with: "p=\"\(identity.path)\"")
        guard !script.contains("$HOME"), !script.contains(".codex-tpp-local-executor-installation-id") else {
            XCTFail("Inspection test must not access a real identity")
            return
        }
        let result = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", script]))
        let hostname = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/hostname"), arguments: []))
        XCTAssertEqual(hostname.status, 0)
        let peer = try SSHService.parseInspection(result, candidate: fixtureHost, hostKey: nil)
        XCTAssertEqual(peer.computerName, String(decoding: hostname.output, as: UTF8.self).trimmingCharacters(in: .newlines))
        XCTAssertEqual(peer.installationIDHash, DeviceServices.hash(fixtureInstallation))
    }
    func testMaliciousHostNeverStartsProcess() async {
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: URL(fileURLWithPath: "/fixture/trust"), runner: { _ in XCTFail("Process must not run"); throw ServiceError.unavailable }, trust: { _ in true })
        await assertServiceError(.invalidInput) { _ = try await service.inspect(HostCandidate(host: "studio;touch bad")) }
    }
    func testLocalInspectionUsesFixedCommand() async throws {
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: URL(fileURLWithPath: "/fixture/trust"), runner: { request in
            XCTAssertEqual(request.executable.path, "/bin/sh")
            if request.arguments == ["-c", SSHService.publicHostKeyCommand] {
                return ProcessResult(status: 0, output: Data("256 SHA256:fixture public-system-host-key (ED25519)\n".utf8))
            }
            XCTAssertEqual(request.arguments, ["-c", SSHService.inspectionCommand])
            return ProcessResult(status: 0, output: Data("\(Data("Studio".utf8).base64EncodedString())\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        }, trust: { _ in XCTFail("Local inspection has no SSH prompt"); return false })
        let peer = try await service.inspect(HostCandidate(host: "localhost", isLocal: true), user: "ignored remote\nuser")
        XCTAssertEqual(peer.computerName, "Studio")
        XCTAssertEqual(peer.hostKeyFingerprint, "SHA256:fixture")
    }
    func testLocalMissingPublicHostKeyPreservesUnknownProvenance() async throws {
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: URL(fileURLWithPath: "/fixture/trust"), runner: { request in
            if request.arguments == ["-c", SSHService.publicHostKeyCommand] { return ProcessResult(status: 31, output: Data()) }
            return ProcessResult(status: 0, output: Data("\(Data("Studio".utf8).base64EncodedString())\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        }, trust: { _ in false })
        let peer = try await service.inspect(HostCandidate(host: "localhost", isLocal: true))
        XCTAssertNil(peer.hostKeyFingerprint)
    }
    func testTrustRejectionAndChangedKeyStopSSH() async throws {
        let root = try temporaryDirectory()
        let key = "studio.example ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJzfixture\n"
        // Valid base64 is sufficient for the synthetic scanner; no key is installed.
        let line = "studio.example ssh-ed25519 \(Data("invented-ed25519-key".utf8).base64EncodedString())\n"
        let runner: ProcessRunner = { request in
            if request.executable.lastPathComponent == "ssh-keyscan" { return .init(status: 0, output: Data(line.utf8)) }
            if request.executable.lastPathComponent == "ssh-keygen" { return .init(status: 0, output: Data("256 SHA256:invented studio.example (ED25519)\n".utf8)) }
            XCTFail("SSH must not run"); throw ServiceError.unavailable
        }
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: root, runner: runner, trust: { challenge in XCTAssertEqual(challenge.fingerprint, "SHA256:invented"); return false })
        await assertServiceError(.trustRejected) { _ = try await service.inspect(fixtureHost, user: "synthetic-user") }
        try Data(key.utf8).write(to: root.appendingPathComponent("known_hosts"))
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: root.appendingPathComponent("known_hosts").path)
        await assertServiceError(.changedHostKey) { _ = try await service.inspect(fixtureHost, user: "synthetic-user") }
    }
    func testPrivateTrustRefusesSymlinkAndBroadMode() throws {
        let root = try temporaryDirectory(), target = root.appendingPathComponent("target"), link = root.appendingPathComponent("link")
        XCTAssertTrue(FileManager.default.createFile(atPath: target.path, contents: Data(), attributes: [.posixPermissions: 0o644]))
        XCTAssertThrowsError(try SSHService.privateFile(target))
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: target)
        XCTAssertThrowsError(try SSHService.privateDirectory(link))
    }
    func testProcessCancellationTimeoutAndOutputBound() async {
        await assertServiceError(.timeout) { _ = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["2"], timeout: 0.05)) }
        await assertServiceError(.oversized) { _ = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/usr/bin/printf"), arguments: ["invented-output"], outputLimit: 2)) }
        let task = Task { try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["2"])) }
        task.cancel()
        await assertServiceError(.canceled) { _ = try await task.value }
    }
    func testLargeChildInputIsDrainedWithoutBlockingDeadline() async throws {
        let data = Data(repeating: 65, count: 128 * 1024)
        let result = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/cat"), arguments: [], input: data))
        XCTAssertEqual(result.status, 0); XCTAssertEqual(result.output, data)
        await assertServiceError(.timeout) {
            _ = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/sleep"), arguments: ["2"], input: Data(repeating: 65, count: 1024 * 1024), timeout: 0.05))
        }
    }
    func testExistingNestedDirectoryURLReachesExplicitTrustPrompt() async throws {
        let root = try temporaryDirectory()
        let support = root.appendingPathComponent("Library", isDirectory: true).appendingPathComponent("Application Support", isDirectory: true).appendingPathComponent("Codex Devices", isDirectory: true)
        let operations = support.appendingPathComponent("Operations", isDirectory: true)
        let directory = operations.appendingPathComponent("ssh")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let calls = SSHStageLog()
        let key = "studio.example ssh-ed25519 \(Data("synthetic-host-key".utf8).base64EncodedString())\n"
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: directory, runner: { request in
            await calls.record(request.executable.lastPathComponent)
            if request.executable.lastPathComponent == "ssh-keyscan" { return .init(status: 0, output: Data(key.utf8)) }
            if request.executable.lastPathComponent == "ssh-keygen" { return .init(status: 0, output: Data("256 SHA256:fixture studio.example (ED25519)\n".utf8)) }
            throw ServiceError.unavailable
        }, trust: { _ in await calls.record("trust"); return false })
        await assertServiceError(.trustRejected) { _ = try await service.inspect(fixtureHost, user: "synthetic-user") }
        let stages = await calls.values()
        XCTAssertEqual(stages, ["ssh-keyscan", "ssh-keygen", "trust"])
        let knownHosts = directory.appendingPathComponent("known_hosts")
        XCTAssertTrue(FileManager.default.fileExists(atPath: knownHosts.path))
        XCTAssertNoThrow(try SSHService.privateFile(knownHosts))
    }
    func testKnownHostsPathRemainsOneFileInRealOpenSSHGrammar() async throws {
        let root = try temporaryDirectory()
        // Enough spaces make OpenSSH's multiple-file interpretation observable in
        // configuration-only mode, without opening a socket or reading SSH config.
        let directory = root.appendingPathComponent(String(repeating: "x ", count: 70) + "ssh")
        let key = "studio.example ssh-ed25519 \(Data("synthetic-host-key".utf8).base64EncodedString())\n"
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: directory, runner: { request in
            if request.executable.lastPathComponent == "ssh-keyscan" { return .init(status: 0, output: Data(key.utf8)) }
            if request.executable.lastPathComponent == "ssh-keygen" { return .init(status: 0, output: Data("256 SHA256:fixture studio.example (ED25519)\n".utf8)) }
            guard let option = request.arguments.first(where: { $0.hasPrefix("-oUserKnownHostsFile=") }) else { throw ServiceError.invalidInput }
            let parsed = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/usr/bin/ssh"), arguments: ["-G", "-T", "-F", "/dev/null", "-oCanonicalizeHostname=no", option, "--", "studio.example"]))
            XCTAssertEqual(parsed.status, 0, "The known-hosts path must be one OpenSSH configuration value.")
            guard parsed.status == 0 else { throw ServiceError.refused }
            XCTAssertTrue(request.arguments.contains("-oStrictHostKeyChecking=yes"))
            XCTAssertTrue(request.arguments.contains("-oGlobalKnownHostsFile=/dev/null"))
            XCTAssertEqual(request.environment["CODEX_DEVICES_SSH_HOST"], "studio.example")
            XCTAssertEqual(request.environment["CODEX_DEVICES_SSH_USER"], "synthetic-user")
            return .init(status: 0, output: Data("\(Data("Studio".utf8).base64EncodedString())\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        }, trust: { _ in true })
        let peer = try await service.inspect(fixtureHost, user: "synthetic-user")
        XCTAssertEqual(peer.computerName, "Studio")
    }
    func testPinnedHostLookupOverridesAnAmbientHostKeyAlias() async throws {
        let root = try temporaryDirectory(), configuration = root.appendingPathComponent("ssh-config")
        try Data("Host studio.example\n    HostKeyAlias invented-unrelated-alias\n".utf8).write(to: configuration)
        let key = "studio.example ssh-ed25519 \(Data("synthetic-host-key".utf8).base64EncodedString())\n"
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: root.appendingPathComponent("trust"), runner: { request in
            if request.executable.lastPathComponent == "ssh-keyscan" { return .init(status: 0, output: Data(key.utf8)) }
            if request.executable.lastPathComponent == "ssh-keygen" { return .init(status: 0, output: Data("256 SHA256:fixture studio.example (ED25519)\n".utf8)) }
            let parsed = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/usr/bin/ssh"), arguments: ["-G", "-F", configuration.path, "-oCanonicalizeHostname=no"] + request.arguments))
            XCTAssertEqual(parsed.status, 0)
            let lines = String(decoding: parsed.output, as: UTF8.self).split(separator: "\n")
            XCTAssertEqual(lines.first(where: { $0.hasPrefix("hostkeyalias ") }), "hostkeyalias studio.example")
            XCTAssertTrue(lines.contains("stricthostkeychecking true"))
            return .init(status: 0, output: Data("\(Data("Studio".utf8).base64EncodedString())\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        }, trust: { _ in true })
        let peer = try await service.inspect(fixtureHost, user: "synthetic-user")
        XCTAssertEqual(peer.computerName, "Studio")
    }
    func testRemoteInspectionUsesPOSIXShellWithZshLoginShell() async throws {
        let root = try temporaryDirectory()
        let key = "studio.example ssh-ed25519 \(Data("synthetic-host-key".utf8).base64EncodedString())\n"
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: root, runner: { request in
            if request.executable.lastPathComponent == "ssh-keyscan" { return .init(status: 0, output: Data(key.utf8)) }
            if request.executable.lastPathComponent == "ssh-keygen" { return .init(status: 0, output: Data("256 SHA256:fixture studio.example (ED25519)\n".utf8)) }
            XCTAssertEqual(request.arguments.last, "/bin/sh -s")
            guard request.arguments.last == "/bin/sh -s" else { throw ServiceError.refused }
            XCTAssertEqual(request.input, Data(SSHService.inspectionCommand.utf8))
            let fixtureScript = Data("set -eu\nbefore='501 1 37 600'\nset -- $before\ntest \"$1\" = 501\nprintf synthetic-posix-result\n".utf8)
            let parsed = try await NativeProcess.run(.init(executable: URL(fileURLWithPath: "/bin/zsh"), arguments: ["-f", "-c", request.arguments.last!], environment: ["HOME": root.path], input: fixtureScript))
            XCTAssertEqual(parsed.status, 0)
            XCTAssertEqual(String(decoding: parsed.output, as: UTF8.self), "synthetic-posix-result")
            return .init(status: 0, output: Data("\(Data("Studio".utf8).base64EncodedString())\n\(DeviceServices.hash(fixtureInstallation))\n".utf8))
        }, trust: { _ in true })
        let peer = try await service.inspect(fixtureHost, user: "synthetic-user")
        XCTAssertEqual(peer.computerName, "Studio")
    }
    func testKnownHostsConfigurationRefusesQuotesBackslashesAndControls() {
        for path in ["/private/tmp/synthetic\"path/known_hosts", "/private/tmp/synthetic\\path/known_hosts", "/private/tmp/synthetic\npath/known_hosts"] {
            XCTAssertThrowsError(try SSHService.knownHostsOption(URL(fileURLWithPath: path))) { XCTAssertEqual($0 as? ServiceError, .refused) }
        }
    }
    func testPromptContextNamesTargetAndDistinguishesPasswordFromKeyPassphrase() throws {
        let context = try SSHAuthenticationContext(host: "studio.example", user: "synthetic-user")
        XCTAssertEqual(context.kind(for: "synthetic-user@studio.example's password:"), .accountPassword)
        XCTAssertEqual(context.kind(for: "Password:"), .accountPassword)
        XCTAssertEqual(context.kind(for: "(synthetic-user@studio.example) Password:"), .accountPassword)
        XCTAssertEqual(context.kind(for: "Enter passphrase for key '/invented/local-key':"), .keyPassphrase)
        XCTAssertEqual(context.kind(for: "Are you sure you want to continue connecting?"), .unsupported)
        XCTAssertEqual(context.title(for: .accountPassword), "Remote account password")
        XCTAssertEqual(context.title(for: .keyPassphrase), "Local SSH key passphrase")
        XCTAssertTrue(context.explanation(for: .accountPassword).contains("studio.example as synthetic-user"))
        XCTAssertTrue(context.explanation(for: .keyPassphrase).contains("local SSH key"))
        for user in ["", "-lroot", "invented\nuser"] { XCTAssertThrowsError(try SSHAuthenticationContext(host: "studio.example", user: user)) }
        XCTAssertThrowsError(try SSHAuthenticationContext(host: "host;command", user: "synthetic-user"))
    }
    func testRemoteInspectionRequiresExplicitUserBeforeAnyProcessOrTrust() async {
        let service = SSHService(helperURL: URL(fileURLWithPath: "/fixture/helper"), trustDirectory: URL(fileURLWithPath: "/fixture/trust"), runner: { _ in XCTFail("No process without explicit SSH account"); throw ServiceError.unavailable }, trust: { _ in XCTFail(); return false })
        await assertServiceError(.invalidInput) { _ = try await service.inspect(fixtureHost) }
    }
}

private actor SSHStageLog {
    private var stages = [String]()
    func record(_ stage: String) { stages.append(stage) }
    func values() -> [String] { stages }
}
