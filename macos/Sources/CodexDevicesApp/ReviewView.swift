import SwiftUI
import DeviceServices

@MainActor
struct ReviewView: View {
    @ObservedObject var workflow: RegistryWorkflow
    @Binding var isPresented: Bool
    @ViewState private var confirmed = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Review name change").font(.title2).fontWeight(.semibold)
            Text("Only the selected Codex label may change. Installation IDs, environment IDs, permissions, attachments and unselected records must remain unchanged.")
                .foregroundStyle(.secondary)
            if let receipt = workflow.receipt {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        ForEach(receipt.records) { record in
                            VStack(alignment: .leading, spacing: 10) {
                                Text("Current label").font(.caption).foregroundStyle(.secondary)
                                Text(record.oldName).font(.headline).textSelection(.enabled)
                                Text("Proposed label").font(.caption).foregroundStyle(.secondary)
                                Label(record.newName, systemImage: "arrow.right").font(.headline).textSelection(.enabled)
                                Text("Registration: \(record.environmentID)")
                                    .font(.caption.monospaced()).textSelection(.enabled)
                                Text("Installation fingerprint: \(record.installationIDHash)")
                                    .font(.caption.monospaced()).textSelection(.enabled)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                        }
                        Text("Private backup and operation folder").font(.headline)
                        Text(workflow.operationDirectory?.path ?? receipt.operationDirectory)
                            .font(.callout.monospaced()).textSelection(.enabled)
                        Text(workflow.isDemo
                             ? "This temporary backup contains invented data. The real verification engine is used; no live account request is sent."
                             : "Keep this folder private. It contains exact registry captures and the reviewed plan. A metadata backup does not promise automatic rollback.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                Toggle("I reviewed the exact old and new labels, identity and preservation conditions.", isOn: $confirmed)
                    .toggleStyle(.checkbox)
                    .disabled(workflow.phase != .review || workflow.isBusy)
                if workflow.isBusy {
                    HStack { ProgressView().controlSize(.small); Text("Applying the reviewed change and checking readback…") }
                } else if workflow.phase != .review {
                    Text(workflow.message).foregroundStyle(.secondary)
                }
                HStack {
                    Button("Close") { isPresented = false }
                        .keyboardShortcut(.cancelAction).disabled(workflow.isBusy)
                    Spacer()
                    Button("Apply Reviewed Change") {
                        let digest = receipt.planSHA256
                        confirmed = false
                        Task {
                            await workflow.applyReviewed(planDigest: digest)
                            if !workflow.isBusy { isPresented = false }
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!confirmed || workflow.phase != .review || workflow.isBusy || receipt.records.count != 1)
                }
            } else {
                Label("No anchored review is available. Close and prepare the name change again.", systemImage: "exclamationmark.triangle")
                Button("Close") { isPresented = false }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(24).frame(minWidth: 600, idealWidth: 680, minHeight: 480)
        .interactiveDismissDisabled(workflow.isBusy)
        .onChange(of: workflow.receipt?.planSHA256) { _ in confirmed = false }
    }
}

struct ReviewView_Previews: PreviewProvider {
    static var previews: some View {
        ReviewView(workflow: .demo(engineURL: URL(fileURLWithPath: "/tmp/codex-devices-preview-engine")),
                   isPresented: .constant(true))
    }
}
