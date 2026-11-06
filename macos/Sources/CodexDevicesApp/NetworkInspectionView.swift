import SwiftUI
import DeviceServices

@MainActor
struct NetworkInspectionView: View {
    @ObservedObject var coordinator: RepairCoordinator
    @ObservedObject var workflow: RegistryWorkflow
    let onUseName: (RegistryComputer, String) -> Void
    @ViewState private var confirmingInspection = false

    private var busy: Bool { [.scanning, .inspecting].contains(coordinator.phase) || confirmingInspection }
    private var user: String { coordinator.sshUser.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Find Macs").font(.title2).fontWeight(.semibold)
                    Text("Optional: discover Macs and inspect their hostnames. Known account labels can be edited without SSH.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button { Task { await coordinator.discoverOnly() } } label: { Label("Scan Network", systemImage: "network") }
                    .disabled(busy)
            }
            HStack(alignment: .top, spacing: 16) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Mac hostname or address (optional)").font(.callout)
                    TextField("Mac hostname or address", text: $coordinator.manualHost)
                        .textFieldStyle(.roundedBorder).accessibilityLabel("Mac hostname or address")
                }
                VStack(alignment: .leading, spacing: 6) {
                    Text("Account on the remote Mac").font(.callout)
                    TextField("Account on the remote Mac", text: $coordinator.sshUser)
                        .textFieldStyle(.roundedBorder).accessibilityLabel("Account on the remote Mac")
                }
            }
            .disabled(busy)
            if user.isEmpty {
                Label("Enter the account on a remote Mac before inspecting it. This Mac uses its current account.", systemImage: "person.crop.circle.badge.exclamationmark")
                    .font(.callout)
            }
            HStack(alignment: .top) {
                if busy { ProgressView().controlSize(.small).accessibilityLabel("Network operation in progress") }
                Text(coordinator.phase == .idle ? "Network discovery starts only when you choose Scan Network." : coordinator.message)
                    .foregroundStyle(.secondary).textSelection(.enabled)
            }
            ForEach(Array(coordinator.sourceFailures.enumerated()), id: \.offset) { _, failure in
                Label("\(failure.source): \(failure.error.localizedDescription)", systemImage: "exclamationmark.triangle")
                    .font(.callout).foregroundStyle(.secondary)
            }
            Divider()
            if coordinator.candidates.isEmpty {
                Text("No network Macs loaded. Scan, or enter a hostname and scan again.")
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(coordinator.candidates) { candidate in
                            candidateRow(candidate)
                        }
                    }
                }
            }
        }
        .padding(20)
        .onAppear { if coordinator.sshUser.isEmpty { coordinator.sshUser = NSUserName() } }
    }

    private func candidateRow(_ candidate: HostCandidate) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(candidate.name).font(.headline)
                    Text("\(candidate.host) · \(candidate.source)").font(.caption).foregroundStyle(.secondary)
                    if candidate.isLocal {
                        Text("Current account on this Mac: \(NSUserName()). No SSH or password is required.")
                            .font(.callout).foregroundStyle(.secondary)
                    } else {
                        Text("Account: \(user.isEmpty ? "Required before inspection" : user)")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button("Inspect") { inspect(candidate) }
                    .accessibilityLabel(candidate.isLocal ? "Inspect this Mac using its current account, without SSH" : "Inspect \(candidate.name) using the specified Mac account")
                    .disabled(busy || (!candidate.isLocal && user.isEmpty))
            }
            if let inspection = coordinator.inspections.first(where: { $0.candidate.id == candidate.id }) {
                Label("Hostname: \(inspection.computerName)", systemImage: "checkmark.circle")
                    .textSelection(.enabled)
                Text("Installation fingerprint: \(inspection.installationIDHash)")
                    .font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                Text(inspection.hostKeyFingerprint.map { "SSH host key: \($0)" } ?? "SSH host key provenance unknown")
                    .font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
                if let record = matchingRecord(inspection),
                   let representative = coordinator.matchingInspection(installationIDHash: inspection.installationIDHash) {
                    HStack {
                        Text("Codex label: \(record.displayName)").font(.callout)
                        Spacer()
                        Button("Use Hostname in Editor") { onUseName(record, representative.computerName) }
                            .disabled(busy || workflow.isBusy)
                            .help("Copy the inspected hostname into the account editor; review is still required")
                    }
                } else {
                    Text("No unique editable account registration matches this inspection.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            } else {
                Text("Not inspected").font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
    }

    private func matchingRecord(_ inspection: PeerInspection) -> RegistryComputer? {
        let matches = workflow.records.filter { $0.installationIDHash == inspection.installationIDHash }
        guard matches.count == 1, let record = matches.first, record.isRenameable else { return nil }
        return record
    }

    private func inspect(_ candidate: HostCandidate) {
        let account = candidate.isLocal ? NSUserName() : user
        guard !account.isEmpty, !busy else { return }
        confirmingInspection = true
        Task {
            let approved = await NativePrompts.confirmInspection(candidate, user: account)
            if approved {
                if !candidate.isLocal { coordinator.sshUser = account }
                await coordinator.inspect(candidate)
            }
            confirmingInspection = false
        }
    }
}

struct NetworkInspectionView_Previews: PreviewProvider {
    static var previews: some View {
        NetworkInspectionView(coordinator: .demo(),
                              workflow: .demo(engineURL: URL(fileURLWithPath: "/tmp/codex-devices-preview-engine")),
                              onUseName: { _, _ in })
            .frame(width: 980, height: 680)
    }
}
