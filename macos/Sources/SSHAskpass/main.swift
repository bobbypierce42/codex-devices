import AppKit
import Darwin
import Foundation

// SSH receives the answer only through this helper's dedicated standard output.
let environment = ProcessInfo.processInfo.environment
let now = Date().timeIntervalSince1970
let requestingParent = getppid()
var parentPath = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
let pathLength = proc_pidpath(requestingParent, &parentPath, UInt32(parentPath.count))
guard requestingParent > 1, pathLength > 0,
      String(cString: parentPath) == "/usr/bin/ssh",
      let session = environment["CODEX_DEVICES_ASKPASS_SESSION"], UUID(uuidString: session) != nil,
      let value = environment["CODEX_DEVICES_ASKPASS_DEADLINE"],
      let deadline = TimeInterval(value), deadline > now, deadline <= now + 120
else { exit(1) }

let application = NSApplication.shared
application.setActivationPolicy(.accessory)
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
let timer = Timer(timeInterval: 0.25, repeats: true) { _ in
    if Date().timeIntervalSince1970 >= deadline || getppid() != requestingParent {
        application.abortModal()
        alert.window.orderOut(nil)
    }
}
RunLoop.main.add(timer, forMode: .modalPanel)
application.activate(ignoringOtherApps: true)
let response = alert.runModal()
timer.invalidate()
guard response == .alertFirstButtonReturn, getppid() == requestingParent,
      Date().timeIntervalSince1970 < deadline else { exit(1) }
let answer = field.stringValue
field.stringValue = ""
guard !answer.contains("\n"), !answer.contains("\r"), !answer.utf8.contains(0) else { exit(1) }
FileHandle.standardOutput.write(Data((answer + "\n").utf8))
