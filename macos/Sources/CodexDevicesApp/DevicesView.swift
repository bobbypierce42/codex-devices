import SwiftUI
import DeviceServices

// Select the macOS 13 property wrapper on SDKs that also export a State macro.
typealias ViewState<Value> = SwiftUI.State<Value>

@MainActor
struct DevicesView: View {
    @ObservedObject var workflow: RegistryWorkflow
    @ObservedObject var network: RepairCoordinator
    @ViewState private var selectedID: String?
    @ViewState private var proposedName = ""
    @ViewState private var activeTab = 0
    @ViewState private var showsReview = false

    private var networkBusy: Bool { [.scanning, .inspecting].contains(network.phase) }
    private var locked: Bool { workflow.isBusy || networkBusy || workflow.phase == .uncertain }
    private var selectedRecord: RegistryComputer? { workflow.records.first { $0.id == selectedID } }
    private var canPrepare: Bool {
        guard let record = selectedRecord, record.isRenameable, !locked else { return false }
        return proposedName != record.displayName && validName(proposedName)
    }
    private var selection: Binding<String?> {
        Binding(get: { selectedID }, set: { value in
            selectedID = value
            proposedName = workflow.records.first { $0.id == value }?.displayName ?? ""
        })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Codex Devices").font(.title).fontWeight(.semibold)
                    Text("Review and update the labels of existing Codex computers.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button { refresh() } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(locked)
                    .help("Read the complete account computer list without SSH")
            }
            if workflow.isDemo {
                Label("Synthetic demo: invented account data, temporary backups and the real verification engine", systemImage: "testtube.2")
                    .font(.callout).padding(8).frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
            }
            HStack(alignment: .top, spacing: 8) {
                if workflow.isBusy { ProgressView().controlSize(.small).accessibilityLabel(statusTitle) }
                else { Image(systemName: statusIcon).accessibilityHidden(true) }
                VStack(alignment: .leading, spacing: 4) {
                    Text(statusTitle).fontWeight(.semibold)
                    Text(workflow.message).foregroundStyle(.secondary).textSelection(.enabled)
                }
                Spacer()
                if workflow.phase == .uncertain {
                    Button("Check Readback") { Task { await workflow.verifyPending() } }
                        .disabled(workflow.isBusy)
                        .help("Read back this operation without repeating a rename request")
                }
            }
            TabView(selection: $activeTab) {
                accountContent
                    .tabItem { Label("Codex computers", systemImage: "desktopcomputer") }.tag(0)
                NetworkInspectionView(coordinator: network, workflow: workflow) { record, name in
                    selectedID = record.id
                    proposedName = name
                    activeTab = 0
                }
                .disabled(workflow.isBusy || workflow.phase == .uncertain || workflow.phase == .review)
                .tabItem { Label("Find Macs", systemImage: "network") }.tag(1)
            }
        }
        .padding(20)
        .task { if workflow.phase == .idle { await workflow.refresh() } }
        .onChange(of: workflow.phase) { phase in
            if phase == .review { showsReview = true }
            if phase == .verified { selectedID = nil; proposedName = "" }
        }
        .onChange(of: workflow.records.map(\.id)) { ids in
            if let selectedID, !ids.contains(selectedID) { self.selectedID = nil; proposedName = "" }
        }
        .sheet(isPresented: $showsReview) {
            ReviewView(workflow: workflow, isPresented: $showsReview)
        }
    }

    @ViewBuilder
    private var accountContent: some View {
        if workflow.records.isEmpty {
            VStack(spacing: 12) {
                Image(systemName: "desktopcomputer").font(.largeTitle).accessibilityHidden(true)
                Text(workflow.isBusy ? "Loading Codex computers" : "No account computers loaded").font(.headline)
                Text("This list includes online and offline account registrations. Network inspection is optional.")
                    .foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            HSplitView {
                List(selection: selection) {
                    ForEach(workflow.records) { record in
                        HStack(spacing: 10) {
                            Image(systemName: record.kind == "openai_cloud" ? "cloud" : "desktopcomputer")
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(record.displayName).lineLimit(1)
                                Text("\(statusText(record.status))\(record.isRenameable ? "" : " · Read only")")
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                        .tag(record.id)
                    }
                }
                .listStyle(.sidebar)
                .accessibilityLabel("Codex account computers")
                .frame(minWidth: 240, idealWidth: 280, maxWidth: 340)
                .disabled(locked)
                ScrollView {
                    if let record = selectedRecord {
                        recordDetail(record)
                    } else {
                        VStack(spacing: 12) {
                            Text("Select a registration").font(.headline)
                            Text("Review its identity and current label before proposing one name change. Nothing is selected automatically.")
                                .foregroundStyle(.secondary).multilineTextAlignment(.center)
                        }
                        .padding(32).frame(maxWidth: .infinity)
                    }
                }
                .frame(minWidth: 420, maxWidth: .infinity, maxHeight: .infinity)
            }
            .padding(.top, 8)
        }
    }

    private func recordDetail(_ record: RegistryComputer) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(record.displayName).font(.title2).fontWeight(.semibold).textSelection(.enabled)
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 10) {
                GridRow { Text("Status").foregroundStyle(.secondary); Text(statusText(record.status)) }
                GridRow { Text("Registration").foregroundStyle(.secondary); Text(record.environmentID).textSelection(.enabled) }
                GridRow { Text("Installation fingerprint").foregroundStyle(.secondary); Text(record.installationIDHash).font(.caption.monospaced()).textSelection(.enabled) }
                GridRow { Text("Dot occurrences").foregroundStyle(.secondary); Text("\(record.scopeCount)") }
            }
            Divider()
            if record.isRenameable {
                Text("Proposed Codex label").font(.headline)
                TextField("Proposed Codex label", text: $proposedName)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Proposed Codex label for \(record.displayName)")
                    .disabled(locked)
                if !validName(proposedName) {
                    Label("Use a nonempty label without leading or trailing spaces, within the supported length.", systemImage: "exclamationmark.triangle")
                        .font(.callout)
                } else if proposedName == record.displayName {
                    Text("No label change proposed.").font(.callout).foregroundStyle(.secondary)
                }
                Text("This changes the selected account label. SSH and the other Mac's password are not required. The system name is not changed.")
                    .font(.callout).foregroundStyle(.secondary)
                Button("Prepare Backup & Review") {
                    let id = record.environmentID, name = proposedName
                    Task { await workflow.prepare(environmentID: id, newName: name) }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(!canPrepare)
                Text("Installation IDs, environment IDs, permissions, attachments and unselected records must remain unchanged.")
                    .font(.callout).foregroundStyle(.secondary)
            } else {
                Label("This registration is read only.", systemImage: "lock")
                Text("Cloud, unsupported and ambiguous registrations cannot be renamed.")
                    .foregroundStyle(.secondary)
            }
        }
        .padding(24).frame(maxWidth: .infinity, alignment: .leading)
    }

    private func refresh() {
        selectedID = nil
        proposedName = ""
        Task { await workflow.refresh() }
    }
    private func validName(_ name: String) -> Bool {
        !name.isEmpty && name == name.trimmingCharacters(in: .whitespacesAndNewlines) && name.utf16.count <= 100
            && name.unicodeScalars.allSatisfy { !CharacterSet.controlCharacters.contains($0) }
    }
    private func statusText(_ status: String?) -> String {
        switch status {
        case "connected", "online": return "Online"
        case "disconnected", "offline": return "Offline"
        default: return "Status unknown"
        }
    }
    private var statusTitle: String {
        switch workflow.phase {
        case .idle: return "Ready to load account computers"
        case .scanning: return "Loading account computers"
        case .inspecting: return "Reading account state"
        case .ready: return "Account computers loaded"
        case .preparing: return "Preparing private backup and review"
        case .review: return "Name change ready for review"
        case .applying: return "Checking operation and fresh readback"
        case .verified: return workflow.isDemo ? "Verified with invented data" : "Verified"
        case .uncertain: return "Outcome uncertain: readback only"
        case .failed: return "Action blocked or incomplete"
        }
    }
    private var statusIcon: String {
        switch workflow.phase {
        case .verified: return "checkmark.shield"
        case .failed, .uncertain: return "exclamationmark.triangle"
        default: return "info.circle"
        }
    }
}

struct DevicesView_Previews: PreviewProvider {
    static var previews: some View {
        DevicesView(workflow: .demo(engineURL: URL(fileURLWithPath: "/tmp/codex-devices-preview-engine")), network: .demo())
            .frame(width: 1040, height: 720)
    }
}
