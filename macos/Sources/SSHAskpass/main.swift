import AppKit
import Darwin
import Foundation
import OSLog

// SSH receives the answer only through this helper's dedicated standard output.
let diagnostics = Logger(subsystem: "org.codex-devices.desktop", category: "SSHAskpass")
diagnostics.notice("entry")
let environment = ProcessInfo.processInfo.environment
let now = Date().timeIntervalSince1970
let requestingParent = getppid()
var parentPath = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
let pathLength = proc_pidpath(requestingParent, &parentPath, UInt32(parentPath.count))
guard requestingParent > 1, pathLength > 0 else {
    diagnostics.error("rejected-parent-unavailable")
    exit(1)
}
guard String(cString: parentPath) == "/usr/bin/ssh" else {
    diagnostics.error("rejected-parent-executable")
    exit(1)
}
guard let session = environment["CODEX_DEVICES_ASKPASS_SESSION"], UUID(uuidString: session) != nil else {
    diagnostics.error("rejected-session")
    exit(1)
}
guard
      let value = environment["CODEX_DEVICES_ASKPASS_DEADLINE"],
      let deadline = TimeInterval(value), deadline > now, deadline <= now + 120 else {
    diagnostics.error("rejected-deadline")
    exit(1)
}
diagnostics.notice("invocation-accepted")

let application = NSApplication.shared
application.setActivationPolicy(.accessory)
// Complete app startup before entering the separate modal event loop.
application.finishLaunching()
diagnostics.notice("application-ready")
let alert = NSAlert()
alert.messageText = "SSH authentication"
alert.informativeText = "Enter the password or passphrase requested by SSH. It is sent only to this connection and is never saved."
alert.alertStyle = .informational
alert.addButton(withTitle: "Continue")
alert.addButton(withTitle: "Cancel")
let field = NSSecureTextField(frame: NSRect(x: 0, y: 0, width: 340, height: 24))
field.placeholderString = "Password or passphrase"
field.setAccessibilityLabel("SSH password or passphrase")
alert.accessoryView = field
alert.window.initialFirstResponder = field
var reportedWindowState = false
let timer = Timer(timeInterval: 0.25, repeats: true) { _ in
    if !reportedWindowState {
        reportedWindowState = true
        if alert.window.isVisible { diagnostics.notice("window-visible") }
        else { diagnostics.notice("window-not-visible") }
        if alert.window.isKeyWindow { diagnostics.notice("window-key") }
        else { diagnostics.notice("window-not-key") }
        if application.isActive { diagnostics.notice("application-active") }
        else { diagnostics.notice("application-inactive") }
    }
    if Date().timeIntervalSince1970 >= deadline || getppid() != requestingParent {
        diagnostics.notice("presentation-deadline-or-parent-ended")
        application.abortModal()
        alert.window.orderOut(nil)
    }
}
RunLoop.main.add(timer, forMode: .modalPanel)
application.activate(ignoringOtherApps: true)
diagnostics.notice("presentation-requested")
let response = alert.runModal()
timer.invalidate()
diagnostics.notice("presentation-ended")
guard response == .alertFirstButtonReturn, getppid() == requestingParent,
      Date().timeIntervalSince1970 < deadline else {
    diagnostics.notice("response-canceled-or-expired")
    exit(1)
}
let answer = field.stringValue
field.stringValue = ""
guard !answer.contains("\n"), !answer.contains("\r"), !answer.utf8.contains(0) else {
    diagnostics.error("response-rejected-format")
    exit(1)
}
FileHandle.standardOutput.write(Data((answer + "\n").utf8))
diagnostics.notice("response-delivered")
