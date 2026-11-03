import AppKit
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

@main
@MainActor
struct CodexDevicesApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var coordinator: RepairCoordinator
    private let isDemo: Bool

    init() {
        let demo = CommandLine.arguments.contains("--demo")
        isDemo = demo
        if demo {
            _coordinator = StateObject(wrappedValue: RepairCoordinator.demo())
        } else {
            let resources = Bundle.main.resourceURL ?? Bundle.main.bundleURL
            let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("Codex Devices", isDirectory: true)
            _coordinator = StateObject(wrappedValue: RepairCoordinator(
                engineURL: resources.appendingPathComponent("codex-devices"),
                helperURL: resources.appendingPathComponent("SSHAskpass"),
                artifactParent: support.appendingPathComponent("Operations", isDirectory: true),
                trust: { challenge in await NativePrompts.trust(challenge) }
            ))
        }
    }

    var body: some Scene {
        WindowGroup("Codex Devices") {
            DevicesView(coordinator: coordinator, isDemo: isDemo)
                .frame(minWidth: 940, minHeight: 580)
        }
        .commands {
            CommandGroup(replacing: .newItem) {}
        }
    }
}
