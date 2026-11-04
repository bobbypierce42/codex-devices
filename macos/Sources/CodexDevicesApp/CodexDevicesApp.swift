import AppKit
import Darwin
import SwiftUI
import DeviceServices

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@MainActor
private final class AppSession: ObservableObject {
    let workflow: RegistryWorkflow?
    let network: RepairCoordinator?

    init(isDemo: Bool) {
        umask(0o077)
        var limit = rlimit(rlim_cur: 0, rlim_max: 0)
        guard setrlimit(RLIMIT_CORE, &limit) == 0 else {
            workflow = nil
            network = nil
            return
        }
        let resources = Bundle.main.resourceURL ?? Bundle.main.bundleURL
        let engine = resources.appendingPathComponent("codex-devices")
        if isDemo {
            workflow = RegistryWorkflow.demo(engineURL: engine)
            network = RepairCoordinator.demo()
        } else {
            let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("Codex Devices", isDirectory: true)
            workflow = RegistryWorkflow(engineURL: engine,
                                        artifactParent: support.appendingPathComponent("AccountOperations", isDirectory: true))
            network = RepairCoordinator(
                engineURL: engine,
                helperURL: resources.appendingPathComponent("Codex Devices SSH.app/Contents/MacOS/SSHAskpass"),
                artifactParent: support.appendingPathComponent("Operations", isDirectory: true),
                trust: { challenge in await NativePrompts.trust(challenge) }
            )
        }
        network?.sshUser = NSUserName()
    }
}

@main
@MainActor
struct CodexDevicesApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var session: AppSession

    init() {
        _session = StateObject(wrappedValue: AppSession(isDemo: CommandLine.arguments.contains("--demo")))
    }

    var body: some Scene {
        WindowGroup("Codex Devices") {
            Group {
                if let workflow = session.workflow, let network = session.network {
                    DevicesView(workflow: workflow, network: network)
                } else {
                    VStack(spacing: 12) {
                        Label("Cannot start securely", systemImage: "exclamationmark.shield")
                            .font(.title2)
                        Text("Core-dump protection could not be established. No account or network operation was started. Quit and reopen the app.")
                            .foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }
                    .padding(32)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .frame(minWidth: 920, minHeight: 600)
        }
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
    }
}
