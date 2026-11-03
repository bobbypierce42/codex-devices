import SwiftUI
import DeviceServices

// A type alias selects the macOS 13 property wrapper on SDKs that also export a State macro.
typealias ViewState<Value> = SwiftUI.State<Value>

@MainActor
struct DevicesView: View {
    @ObservedObject var coordinator: RepairCoordinator
    var isDemo = false
    @ViewState private var selected = Set<String>()
    @ViewState private var proposedNames: [String: String] = [:]
    @ViewState private var showsReview = false

    private var busy: Bool {
        [.scanning, .inspecting, .preparing, .applying].contains(coordinator.phase)
    }

    private var selectedChanges: [ReviewSelection] {
        coordinator.devices.compactMap { device in
            guard selected.contains(device.id), let inspection = coordinator.matchingInspection(for: device) else { return nil }
            let name = (proposedNames[device.id] ?? inspection.computerName).trimmingCharacters(in: .whitespacesAndNewlines)
            guard validName(name), name != device.displayName else { return nil }
            return ReviewSelection(environmentID: device.id, newName: name)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Codex Devices").font(.title).fontWeight(.semibold)
                    Text("Compare Mac system names with existing Codex registrations.")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button { scan() } label: { Label("Scan", systemImage: "arrow.clockwise") }
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(busy || coordinator.phase == .uncertain)
                    .help("Find Macs and read the complete Codex computer list")
            }
            if isDemo {
                Label("Synthetic demo: invented computers and simulated engine receipts", systemImage: "testtube.2")
                    .font(.callout).padding(8).frame(maxWidth: .infinity, alignment: .leading)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
            }
            HStack {
                TextField("Mac hostname or address", text: $coordinator.manualHost)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Mac hostname or address")
                    .onSubmit { if !busy && coordinator.phase != .uncertain { scan() } }
                TextField("SSH user (optional)", text: $coordinator.sshUser)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("SSH user, optional")
                    .frame(maxWidth: 200)
                Button("Add & Scan") { scan() }
                    .disabled(busy || coordinator.phase == .uncertain || coordinator.manualHost.isEmpty)
            }
            .disabled(busy || coordinator.phase == .uncertain)
            HStack(alignment: .top, spacing: 8) {
                if busy { ProgressView().controlSize(.small).accessibilityLabel(statusTitle) }
                else { Image(systemName: statusIcon).accessibilityHidden(true) }
                VStack(alignment: .leading, spacing: 4) {
                    Text(statusTitle).fontWeight(.semibold)
                    Text(coordinator.message.isEmpty ? "Scan, inspect a Mac, then select its exact name change." : coordinator.message)
                        .foregroundStyle(.secondary).textSelection(.enabled)
                }
                Spacer()
                if coordinator.phase == .uncertain {
                    Button("Check Readback") { Task { await coordinator.reconcilePending() } }
                        .help("Read the current registry without repeating a repair request")
                }
            }
            ForEach(Array(coordinator.sourceFailures.enumerated()), id: \.offset) { _, failure in
                Label("\(failure.source): \(failure.error.localizedDescription)", systemImage: "exclamationmark.triangle")
                    .font(.callout).foregroundStyle(.secondary)
            }
            Divider()
            if coordinator.candidates.isEmpty && coordinator.devices.isEmpty {
                VStack(spacing: 12) {
                    Image(systemName: "desktopcomputer").font(.largeTitle).accessibilityHidden(true)
                    Text("No computers loaded").font(.headline)
                    Text("Choose Scan to discover Macs. Add a hostname if discovery cannot find one.")
                        .foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("Discovered Macs").font(.headline)
                        ForEach(coordinator.candidates) { candidate in
                            HStack {
                                Image(systemName: candidate.isLocal ? "desktopcomputer" : "network")
                                    .accessibilityHidden(true)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(candidate.name).fontWeight(.medium)
                                    Text("\(candidate.host) · \(candidate.source)").font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if let inspection = coordinator.inspections.first(where: { $0.candidate.id == candidate.id }) {
                                    VStack(alignment: .trailing, spacing: 2) {
                                        Label(inspection.computerName, systemImage: "checkmark.circle")
                                        Text(inspection.hostKeyFingerprint ?? "Local inspection")
                                            .font(.caption.monospaced()).foregroundStyle(.secondary)
                                            .textSelection(.enabled)
                                    }
                                    .help("Reachable and inspected; system name and SSH host key fingerprint")
                                } else { Text("Not inspected").foregroundStyle(.secondary) }
                                Button("Inspect") { Task { await coordinator.inspect(candidate) } }
                                    .accessibilityLabel("Inspect \(candidate.name)")
                                    .disabled(busy || coordinator.phase == .uncertain)
                            }
                            .padding(10).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                        }
                        Divider().padding(.vertical, 4)
                        Text("Compare names").font(.headline)
                        Text("Only a uniquely matched, inspected installation can be selected. Repeated registrations across Dots appear once.")
                            .font(.callout).foregroundStyle(.secondary)
                        if coordinator.devices.isEmpty {
                            Text("No complete Codex computer list is available. Check the scan status above.")
                                .foregroundStyle(.secondary)
                        }
                        ForEach(coordinator.devices) { device in
                            deviceRow(device)
                        }
                    }
                    .padding(.trailing, 8)
                }
            }
            Divider()
            HStack {
                Text("Installation IDs and unselected records are preserved.")
                    .font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button("Review \(selectedChanges.count) Change\(selectedChanges.count == 1 ? "" : "s")") {
                    let changes = selectedChanges
                    Task { await coordinator.prepare(changes) }
                }
                .keyboardShortcut(.return, modifiers: .command)
                .disabled(busy || coordinator.phase == .uncertain || selectedChanges.isEmpty || selectedChanges.count != selected.count)
            }
        }
        .padding(20)
        .onChange(of: coordinator.inspections) { _ in
            for device in coordinator.devices {
                if let inspection = coordinator.matchingInspection(for: device), proposedNames[device.id] == nil {
                    proposedNames[device.id] = inspection.computerName
                }
            }
        }
        .onChange(of: coordinator.phase) { phase in
            if phase == .review { showsReview = true }
            if phase == .verified { selected.removeAll() }
        }
        .sheet(isPresented: $showsReview) {
            ReviewView(coordinator: coordinator, isPresented: $showsReview)
        }
    }

    private func scan() {
        selected.removeAll()
        proposedNames.removeAll()
        Task { await coordinator.scan() }
    }

    private func validName(_ name: String) -> Bool {
        !name.isEmpty && name.utf16.count <= 100
            && name.unicodeScalars.allSatisfy { !CharacterSet.controlCharacters.contains($0) }
    }

    @ViewBuilder
    private func deviceRow(_ device: LogicalDevice) -> some View {
        let inspection = coordinator.matchingInspection(for: device)
        let hasInspectedMatch = coordinator.inspections.contains { $0.installationIDHash == device.installationIDHash }
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 16) {
                Toggle(isOn: Binding(
                    get: { selected.contains(device.id) },
                    set: { if $0 { selected.insert(device.id) } else { selected.remove(device.id) } }
                )) { Text("Select").hidden() }
                .labelsHidden().toggleStyle(.checkbox)
                .accessibilityLabel("Select name change for \(device.displayName)")
                .disabled(inspection == nil || busy || coordinator.phase == .uncertain)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Mac system name").font(.caption).foregroundStyle(.secondary)
                    Text(inspection?.computerName ?? "Unknown: inspect a matching Mac")
                    Text(inspection?.candidate.host ?? "No unique inspected match").font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Codex name").font(.caption).foregroundStyle(.secondary)
                    Text(device.displayName).textSelection(.enabled)
                    Text("Fingerprint \(device.installationIDHash.prefix(12))…")
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                        .help(device.installationIDHash)
                }.frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Proposed name").font(.caption).foregroundStyle(.secondary)
                    TextField("Inspect to propose a name", text: Binding(
                        get: { proposedNames[device.id] ?? inspection?.computerName ?? "" },
                        set: { proposedNames[device.id] = $0 }
                    ))
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Proposed Codex name for \(device.displayName)")
                    .disabled(inspection == nil || busy || coordinator.phase == .uncertain)
                    if inspection != nil {
                        let name = (proposedNames[device.id] ?? inspection?.computerName ?? "")
                            .trimmingCharacters(in: .whitespacesAndNewlines)
                        if !validName(name) {
                            Label("Name is empty, too long, or contains unsupported characters.", systemImage: "exclamationmark.triangle")
                                .font(.caption)
                        } else if name == device.displayName {
                            Text("Already matches").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
            if inspection == nil && hasInspectedMatch {
                Label("Blocked: inspection does not identify one unambiguous registration and Mac.", systemImage: "exclamationmark.triangle")
                    .font(.callout)
            }
            DisclosureGroup("\(device.occurrences.count) Dot occurrence\(device.occurrences.count == 1 ? "" : "s")") {
                ForEach(Array(device.occurrences.enumerated()), id: \.offset) { _, occurrence in
                    Text("Dot \(occurrence.dotID) · Thread \(occurrence.threadID)")
                        .font(.caption.monospaced()).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                Text("Each occurrence keeps its own access and attachment fields.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
    }

    private var statusTitle: String {
        switch coordinator.phase {
        case .idle: return "Ready to scan"
        case .scanning: return "Scanning"
        case .inspecting: return "Inspecting Mac"
        case .ready: return "Ready to compare"
        case .preparing: return "Preparing private backup and review"
        case .review: return "Review prepared"
        case .applying: return "Repairing and checking fresh readback"
        case .verified: return isDemo ? "Verified in synthetic demo" : "Verified"
        case .uncertain: return "Outcome unknown: readback required"
        case .failed: return "Action blocked or incomplete"
        }
    }

    private var statusIcon: String {
        switch coordinator.phase {
        case .verified: return "checkmark.shield"
        case .failed, .uncertain: return "exclamationmark.triangle"
        default: return "info.circle"
        }
    }
}

struct DevicesView_Previews: PreviewProvider {
    static var previews: some View {
        DevicesView(coordinator: .demo(), isDemo: true).frame(width: 1040, height: 720)
    }
}
