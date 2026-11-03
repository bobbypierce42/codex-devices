import SwiftUI
import DeviceServices

@MainActor
struct ReviewView: View {
    @ObservedObject var coordinator: RepairCoordinator
    @Binding var isPresented: Bool
    @ViewState private var confirmed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Review selected repairs").font(.title2).fontWeight(.semibold)
            Text("Only these display names may change. Installation IDs, environment IDs, access, attachments and unselected records must remain unchanged.")
                .foregroundStyle(.secondary)
            if let receipt = coordinator.receipt {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(receipt.records) { record in
                            VStack(alignment: .leading, spacing: 6) {
                                Text(record.oldName).fontWeight(.semibold)
                                Label(record.newName, systemImage: "arrow.right")
                                Text("Installation fingerprint: \(record.installationIDHash)")
                                    .font(.caption.monospaced()).textSelection(.enabled)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                        }
                        Text(coordinator.isDemo ? "Simulated operation folder" : "Private backup and operation folder").font(.headline)
                        Text(receipt.operationDirectory).font(.callout.monospaced()).textSelection(.enabled)
                        Text(coordinator.isDemo
                             ? "This synthetic demo simulates the engine receipt. No real backup or service request is created."
                             : "Keep this folder private. It contains exact registry captures and the reviewed plan.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                Toggle("I reviewed the exact old and new names and the preservation conditions.", isOn: $confirmed)
                    .toggleStyle(.checkbox)
                    .disabled(coordinator.phase != .review)
                if coordinator.phase == .applying {
                    HStack { ProgressView().controlSize(.small); Text("Repairing once, then verifying fresh readback…") }
                } else if coordinator.phase != .review {
                    Text(coordinator.message).foregroundStyle(.secondary)
                }
                HStack {
                    Button(coordinator.phase == .applying ? "Please wait" : "Close") { isPresented = false }
                        .keyboardShortcut(.cancelAction)
                        .disabled(coordinator.phase == .applying)
                    Spacer()
                    Button("Repair Selected") {
                        let digest = receipt.planSHA256
                        confirmed = false
                        Task {
                            await coordinator.applyReviewed(planDigest: digest)
                            if coordinator.phase != .applying { isPresented = false }
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!confirmed || coordinator.phase != .review || receipt.records.isEmpty)
                }
            } else {
                Label("No anchored review is available. Close and prepare the selected changes again.", systemImage: "exclamationmark.triangle")
                Button("Close") { isPresented = false }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(minWidth: 600, idealWidth: 680, minHeight: 440)
        .interactiveDismissDisabled(coordinator.phase == .applying)
    }
}

struct ReviewView_Previews: PreviewProvider {
    static var previews: some View {
        ReviewView(coordinator: .demo(), isPresented: .constant(true))
    }
}
