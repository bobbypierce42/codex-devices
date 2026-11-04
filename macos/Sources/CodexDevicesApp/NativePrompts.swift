import AppKit
import DeviceServices

@MainActor
enum NativePrompts {
    static func confirmInspection(_ candidate: HostCandidate, user: String) async -> Bool {
        let alert = NSAlert()
        alert.messageText = candidate.isLocal ? "Inspect this Mac?" : "Inspect this Mac over SSH?"
        if candidate.isLocal {
            alert.informativeText = "Mac: \(candidate.name)\nCurrent account on this Mac: \(NSUserName())\n\nRead this Mac's system name and existing installation fingerprint using its current account. No SSH or password is required. This does not change its system name or Codex label."
        } else {
            alert.informativeText = "Mac: \(candidate.name)\nHost: \(candidate.host)\nAccount on the Mac: \(user)\n\nRead its system name and existing installation fingerprint. This does not change its system name or Codex label. SSH may request authentication in a separate secure window."
        }
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Inspect")
        if let window = NSApp.keyWindow {
            return await withCheckedContinuation { continuation in
                alert.beginSheetModal(for: window) { response in
                    continuation.resume(returning: response == .alertSecondButtonReturn)
                }
            }
        }
        return alert.runModal() == .alertSecondButtonReturn
    }

    static func trust(_ challenge: HostTrustChallenge) async -> Bool {
        let alert = NSAlert()
        alert.messageText = "Trust this Mac's SSH host key?"
        alert.informativeText = "Host: \(challenge.host)\n\nPublic key fingerprint:\n\(challenge.fingerprint)\n\nCompare this fingerprint with the Mac through a trusted channel before continuing. Changed keys are refused."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Trust This Key")
        if let window = NSApp.keyWindow {
            return await withCheckedContinuation { continuation in
                alert.beginSheetModal(for: window) { response in
                    continuation.resume(returning: response == .alertSecondButtonReturn)
                }
            }
        }
        return alert.runModal() == .alertSecondButtonReturn
    }
}
