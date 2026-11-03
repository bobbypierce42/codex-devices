use crate::encoding::{compact_json_bytes, json_digest};
use crate::storage::{backup_in, directory::Directory, json_bytes, verify_directory};
use crate::{Error, MAX_BYTES, Result, Source, audit, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

mod journal;
mod rename;
use journal::{Event, Journal, RequestState};
pub use rename::{RenameChange, prepare_rename};

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
enum Kind {
    #[serde(rename = "synthetic-cleanup")]
    Cleanup,
    #[serde(rename = "synthetic-rename")]
    Rename,
}

impl Kind {
    fn action(self) -> &'static str {
        match self {
            Self::Cleanup => "cleanup",
            Self::Rename => "rename",
        }
    }
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    kind: Kind,
    source: Source,
    backup_manifest_sha256: String,
    stable_inventory_sha256: String,
    selected_environment_ids: Vec<String>,
    selected_records: Vec<Value>,
    protected_environment_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    renames: Vec<RenameChange>,
}

impl Plan {
    fn name_for(&self, id: &str) -> &str {
        &self
            .renames
            .iter()
            .find(|change| change.environment_id == id)
            .expect("validated rename selection")
            .new_name
    }

    fn effect_matches(&self, view: &View, id: &str) -> bool {
        match self.kind {
            Kind::Cleanup => !view.records.contains_key(id),
            Kind::Rename => view
                .records
                .get(id)
                .is_some_and(|record| record["display_name"].as_str() == Some(self.name_for(id))),
        }
    }

    fn readback_event(&self, id: String, matched: bool) -> Event {
        match self.kind {
            Kind::Cleanup => Event::Verified {
                environment_id: id,
                absent: matched,
            },
            Kind::Rename => Event::Renamed {
                environment_id: id,
                matches: matched,
            },
        }
    }
}

#[derive(Serialize)]
pub struct OperationReceipt {
    pub schema_version: u32,
    pub operation: &'static str,
    pub action: &'static str,
    pub mode: &'static str,
    pub status: &'static str,
    pub operation_directory: String,
    pub plan_sha256: String,
    pub plan_anchor: &'static str,
    pub selected_environment_ids: Vec<String>,
    pub selected_records: Vec<Value>,
    pub protected_environment_ids: Vec<String>,
    pub verified_deleted_environment_ids: Vec<String>,
    pub verified_renamed_environment_ids: Vec<String>,
    pub renames: Vec<RenameChange>,
    pub remaining_environment_ids: Option<Vec<String>>,
    pub capability_advertisement_changes: Vec<String>,
    pub reason: String,
    pub service: &'static str,
    pub live_changes_authorized: bool,
    pub live_state_verified: bool,
}

pub type CleanupReceipt = OperationReceipt;
pub type RenameReceipt = OperationReceipt;

impl OperationReceipt {
    pub fn exit_code(&self) -> i32 {
        if ["complete", "review_required"].contains(&self.status) {
            0
        } else {
            4
        }
    }
}

struct View {
    raw: Value,
    context: Value,
    records: BTreeMap<String, Value>,
    eligible: BTreeSet<String>,
    source: Source,
}

fn stable_record(record: &Value) -> Value {
    let mut value = record.clone();
    value
        .as_object_mut()
        .expect("audited record")
        .remove("selected_capability_roots");
    value
}

impl View {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let report = audit(bytes)?;
        if !report.complete {
            return Err(Error::refused("Changes require a complete inventory."));
        }
        let raw: Value = serde_json::from_slice(bytes).expect("audited JSON");
        let mut context = raw.clone();
        let mut records = BTreeMap::new();
        for page in context["pages"].as_array_mut().expect("audited pages") {
            let rows = page["computers"].as_array_mut().expect("audited records");
            for row in rows.iter() {
                records.insert(
                    row["environment_id"].as_str().expect("audited ID").into(),
                    row.clone(),
                );
            }
            rows.clear();
        }
        Ok(Self {
            raw,
            context,
            records,
            eligible: report
                .records
                .into_iter()
                .filter(|r| r.cleanup_candidate)
                .map(|r| r.selector)
                .collect(),
            source: report.source,
        })
    }

    fn digest(&self) -> Result<String> {
        let stable: BTreeMap<_, _> = self
            .records
            .iter()
            .map(|(id, record)| (id, stable_record(record)))
            .collect();
        json_digest(&(&self.context, stable))
    }

    fn persisted_bytes(&self) -> Result<Vec<u8>> {
        compact_json_bytes(&self.raw)
    }
}

fn plan_for(view: &View, ids: &[String], backup_hash: String) -> Result<Plan> {
    if ids.iter().any(|id| !view.eligible.contains(id)) {
        return Err(Error::refused(
            "Every selected full ID must be a known eligible offline personal record. Names and list positions are not selectors.",
        ));
    }
    review_plan(view, ids, backup_hash)
}

fn review_plan(view: &View, ids: &[String], backup_hash: String) -> Result<Plan> {
    if ids.is_empty() || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(Error::refused(
            "Select one or more distinct full environment IDs explicitly.",
        ));
    }
    if ids.iter().any(|id| !view.records.contains_key(id)) {
        return Err(Error::refused(
            "Every selector must be a known full environment ID. Names and positions are not selectors.",
        ));
    }
    Ok(Plan {
        schema_version: 1,
        kind: Kind::Cleanup,
        source: view.source.clone(),
        backup_manifest_sha256: backup_hash,
        stable_inventory_sha256: view.digest()?,
        selected_environment_ids: ids.to_vec(),
        selected_records: ids.iter().map(|id| view.records[id].clone()).collect(),
        protected_environment_ids: view
            .records
            .keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect(),
        renames: Vec::new(),
    })
}

fn lock(directory: &Directory) -> Result<File> {
    let file = directory.open_read_write(".lock")?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::refused(
            "Another process holds this operation's lock. No records were changed by this invocation.",
        )),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub fn prepare_cleanup(bytes: &[u8], ids: &[String], directory: &Path) -> Result<CleanupReceipt> {
    let baseline = View::parse(bytes)?;
    let plan = plan_for(&baseline, ids, sha256(b""))?;
    prepare_operation(bytes, baseline, plan, directory)
}

fn prepare_operation(
    bytes: &[u8],
    baseline: View,
    mut plan: Plan,
    directory: &Path,
) -> Result<OperationReceipt> {
    let placeholder_digest = sha256(b"");
    if json_bytes(&plan)?.len() as u64 > MAX_BYTES {
        return Err(Error::refused("Review plan exceeds the 4 MiB limit."));
    }
    baseline.renamed_bytes(&plan.renames)?;
    Journal::empty().reserve_remaining(&plan, &placeholder_digest, &BTreeSet::new())?;
    let files = Directory::create(directory)?;
    files.write_new(".lock", b"")?;
    let guard = lock(&files)?;
    let receipt = backup_in(bytes, &files, &directory.join("backup"))?;
    plan.backup_manifest_sha256 = receipt.manifest_sha256;
    let encoded = json_bytes(&plan)?;
    files.write_new("service.json", bytes)?;
    files.write_new("journal.jsonl", b"")?;
    // A plan appears only after the backup and service copy have been synced.
    files.write_new("plan.json", &encoded)?;
    files.sync()?;
    let operation = Operation {
        directory: directory.into(),
        files,
        _lock: guard,
        baseline,
        digest: sha256(&encoded),
        plan,
        anchored: false,
        journal: Journal::empty(),
    };
    Ok(operation.receipt(Some(&operation.baseline), operation.plan.kind.action(), "review_required", "Review plan.json and the selected records. Apply requires this exact plan SHA-256 and writes only inside this synthetic operation."))
}

struct Operation {
    directory: PathBuf,
    files: Directory,
    _lock: File,
    plan: Plan,
    digest: String,
    anchored: bool,
    baseline: View,
    journal: Journal,
}

fn check_digest(expected: &str) -> Result<()> {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::new(
            "usage",
            "Expected plan SHA-256 must contain exactly 64 hexadecimal digits.",
            2,
        ));
    }
    Ok(())
}

impl Operation {
    fn load(directory: &Path, expected: Option<&str>) -> Result<Self> {
        if let Some(hash) = expected {
            check_digest(hash)?;
        }
        let files = Directory::open(directory)?;
        let guard = lock(&files)?;
        // Check every existing artifact even if a later digest guard refuses the operation.
        files.check_file("service.json")?;
        files.check_file("journal.jsonl")?;
        files.check_file("plan.json")?;
        files.check_members(
            &[
                "backup",
                "plan.json",
                "service.json",
                "journal.jsonl",
                ".lock",
            ],
            Some(".service.pending-"),
        )?;
        let bytes = files.read("plan.json")?;
        let digest = sha256(&bytes);
        if expected.is_some_and(|hash| !hash.eq_ignore_ascii_case(&digest)) {
            return Err(Error::integrity(
                "The operation does not match the reviewed plan SHA-256.",
            ));
        }
        let plan: Plan = serde_json::from_slice(&bytes)
            .map_err(|_| Error::integrity("Invalid cleanup plan."))?;
        let stored_backup = files.open_child("backup")?;
        verify_directory(&stored_backup, Some(&plan.backup_manifest_sha256))?;
        let baseline = View::parse(&stored_backup.read("catalog.json")?)?;
        let expected_plan = match plan.kind {
            Kind::Cleanup => plan_for(
                &baseline,
                &plan.selected_environment_ids,
                plan.backup_manifest_sha256.clone(),
            )?,
            Kind::Rename => rename::plan_for(
                &baseline,
                &plan.renames,
                plan.backup_manifest_sha256.clone(),
            )?,
        };
        if plan != expected_plan {
            return Err(Error::integrity(
                "Operation plan differs from its verified backup and explicit selections.",
            ));
        }
        let journal = Journal::load(&files, &plan, &digest)?;
        Ok(Self {
            directory: directory.into(),
            files,
            _lock: guard,
            plan,
            digest,
            anchored: expected.is_some(),
            baseline,
            journal,
        })
    }

    fn guard(&self, view: &View) -> Result<()> {
        if view.context != self.baseline.context {
            return Err(Error::refused(
                "Account, Dot, thread, page scope, or catalog metadata changed. Progress stopped.",
            ));
        }
        let state = &self.journal.state;
        for (id, record) in &view.records {
            let Some(original) = self.baseline.records.get(id) else {
                return Err(Error::refused(
                    "An unexpected record appeared. Progress stopped.",
                ));
            };
            let completed = self.plan.selected_environment_ids[..state.next].contains(id);
            let pending = state.request != RequestState::Idle
                && self.plan.selected_environment_ids.get(state.next) == Some(id);
            let actual = stable_record(record);
            let mut expected = stable_record(original);
            let allowed = match self.plan.kind {
                Kind::Cleanup => !completed && actual == expected,
                Kind::Rename if completed || pending => {
                    let still_original = pending && actual == expected;
                    expected["display_name"] = Value::String(self.plan.name_for(id).into());
                    still_original || actual == expected
                }
                Kind::Rename => actual == expected,
            };
            if !allowed {
                return Err(Error::refused(
                    "A record differs from its reviewed identity, name, status, access, metadata, or verified effect. Progress stopped.",
                ));
            }
        }
        for id in self.baseline.records.keys() {
            let completed = self.plan.selected_environment_ids[..state.next].contains(id);
            let pending = state.request != RequestState::Idle
                && self.plan.selected_environment_ids.get(state.next) == Some(id);
            let may_be_absent = self.plan.kind == Kind::Cleanup && (completed || pending);
            if !view.records.contains_key(id) && !may_be_absent {
                return Err(Error::refused(
                    "A protected or unattempted record disappeared. Progress stopped.",
                ));
            }
        }
        Ok(())
    }

    fn receipt(
        &self,
        view: Option<&View>,
        operation: &'static str,
        status: &'static str,
        reason: impl Into<String>,
    ) -> CleanupReceipt {
        CleanupReceipt {
            schema_version: 1,
            operation,
            action: self.plan.kind.action(),
            mode: "offline-synthetic",
            status,
            operation_directory: self.directory.to_string_lossy().into_owned(),
            plan_sha256: self.digest.clone(),
            plan_anchor: if self.anchored {
                "matched"
            } else {
                "not_supplied"
            },
            selected_environment_ids: self.plan.selected_environment_ids.clone(),
            selected_records: self.plan.selected_records.clone(),
            protected_environment_ids: self.plan.protected_environment_ids.clone(),
            verified_deleted_environment_ids: if self.plan.kind == Kind::Cleanup {
                self.plan.selected_environment_ids[..self.journal.state.next].to_vec()
            } else {
                Vec::new()
            },
            verified_renamed_environment_ids: if self.plan.kind == Kind::Rename {
                self.plan.selected_environment_ids[..self.journal.state.next].to_vec()
            } else {
                Vec::new()
            },
            renames: self.plan.renames.clone(),
            remaining_environment_ids: view.map(|v| {
                self.plan
                    .selected_environment_ids
                    .iter()
                    .filter(|id| !self.plan.effect_matches(v, id))
                    .cloned()
                    .collect()
            }),
            capability_advertisement_changes: self
                .advertisement_changes(view)
                .into_iter()
                .collect(),
            reason: reason.into(),
            service: "operation-local synthetic file copy",
            live_changes_authorized: false,
            live_state_verified: false,
        }
    }

    fn advertisement_changes(&self, view: Option<&View>) -> BTreeSet<String> {
        let mut changes = self.journal.state.advertisement_changes.clone();
        if let Some(view) = view {
            changes.extend(
                view.records
                    .iter()
                    .filter(|(id, record)| {
                        self.baseline.records.get(*id).is_some_and(|original| {
                            original.get("selected_capability_roots")
                                != record.get("selected_capability_roots")
                        })
                    })
                    .map(|(id, _)| id.clone()),
            );
        }
        changes
    }

    fn append(&mut self, event: Event) -> Result<()> {
        self.journal
            .append(&self.files, &self.plan, &self.digest, event)
    }

    fn observe(&self, service: &mut impl Service) -> Result<View> {
        let view = View::parse(&service.snapshot()?)?;
        self.guard(&view)?;
        Ok(view)
    }

    fn reconcile_request(&mut self, view: &View) -> Result<bool> {
        let id = self.plan.selected_environment_ids[self.journal.state.next].clone();
        let matched = self.plan.effect_matches(view, &id);
        if !matched
            && self.journal.state.request == (RequestState::ResponseReceived { success: true })
        {
            return Ok(false);
        }
        self.append(self.plan.readback_event(id, matched))?;
        Ok(true)
    }

    fn run(&mut self, service: &mut impl Service) -> Result<CleanupReceipt> {
        let action = self.plan.kind.action();
        loop {
            let view = match self.observe(service) {
                Ok(view) => view,
                Err(error) => return Ok(self.receipt(None, action, "unknown", error.message)),
            };
            let state = &self.journal.state;
            if state.request != RequestState::Idle {
                if !self.reconcile_request(&view)? {
                    return Ok(self.receipt(Some(&view), action, "unknown", "The request was acknowledged but its reviewed effect is not visible. The pending intent is retained; it will not be retried blindly."));
                }
                continue;
            }
            if state.finished {
                return Ok(self.receipt(
                    Some(&view),
                    action,
                    "complete",
                    "All selected synthetic changes and protected records are verified.",
                ));
            }
            if state.next == self.plan.selected_environment_ids.len() {
                self.append(Event::Complete)?;
                return Ok(self.receipt(
                    Some(&view),
                    action,
                    "complete",
                    "All selected synthetic changes and protected records are verified.",
                ));
            }
            let id = self.plan.selected_environment_ids[state.next].clone();
            let remaining_renames = if self.plan.kind == Kind::Rename {
                &self.plan.renames[state.next..]
            } else {
                &[]
            };
            view.renamed_bytes(remaining_renames)?;
            let changes = self
                .advertisement_changes(Some(&view))
                .difference(&self.journal.state.advertisement_changes)
                .cloned()
                .collect();
            self.journal
                .reserve_remaining(&self.plan, &self.digest, &changes)?;
            self.append(Event::Intent {
                environment_id: id.clone(),
                capability_advertisement_changes: changes.into_iter().collect(),
            })?;
            let expected = view.digest()?;
            let failure = match self.plan.kind {
                Kind::Cleanup => service.delete(&id, &expected),
                Kind::Rename => service.rename(&id, self.plan.name_for(&id), &expected),
            }
            .err();
            let success = failure.is_none();
            self.append(Event::Response {
                environment_id: id.clone(),
                success,
            })?;
            let after = match self.observe(service) {
                Ok(view) => view,
                Err(error) => return Ok(self.receipt(None, action, "unknown", error.message)),
            };
            if !self.reconcile_request(&after)? {
                return Ok(self.receipt(Some(&after), action, "unknown", "The request was acknowledged but its reviewed effect is not visible. Resume must verify that effect before continuing."));
            }
            if let Some(error) = failure {
                return Ok(self.receipt(Some(&after), action, "partial", format!("Request did not complete normally: {error}. Readback was recorded. Resume this same approved operation to continue.")));
            }
        }
    }
}

trait Service {
    fn snapshot(&mut self) -> Result<Vec<u8>>;
    fn delete(&mut self, id: &str, expected_stable: &str) -> Result<()>;
    fn rename(&mut self, _id: &str, _name: &str, _expected_stable: &str) -> Result<()> {
        Err(Error::refused(
            "This synthetic service does not support rename.",
        ))
    }
}

struct FileService {
    files: Directory,
}

impl Service for FileService {
    fn snapshot(&mut self) -> Result<Vec<u8>> {
        self.files.read("service.json")
    }

    fn delete(&mut self, id: &str, expected_stable: &str) -> Result<()> {
        let mut view = View::parse(&self.snapshot()?)?;
        if view.digest()? != expected_stable || !view.eligible.contains(id) {
            return Err(Error::refused(
                "Conditional synthetic deletion failed because inventory or eligibility changed.",
            ));
        }
        for page in view.raw["pages"].as_array_mut().expect("audited pages") {
            page["computers"]
                .as_array_mut()
                .expect("audited records")
                .retain(|row| row["environment_id"] != id);
        }
        self.replace(&view.persisted_bytes()?)
    }

    fn rename(&mut self, id: &str, name: &str, expected_stable: &str) -> Result<()> {
        let view = View::parse(&self.snapshot()?)?;
        if view.digest()? != expected_stable || !view.renameable(id) {
            return Err(Error::refused(
                "Conditional synthetic rename failed because inventory or eligibility changed.",
            ));
        }
        let change = RenameChange {
            environment_id: id.into(),
            new_name: rename::validated_name(name)?,
        };
        self.replace(&view.renamed_bytes(&[change])?)
    }
}

impl FileService {
    fn replace(&self, encoded: &[u8]) -> Result<()> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new("io", "Clock is before the Unix epoch.", 1))?
            .as_nanos();
        let pending = format!(".service.pending-{}-{stamp}", std::process::id());
        self.files.write_new(&pending, encoded)?;
        self.files.check_file("service.json")?;
        self.files.replace(&pending, "service.json")
    }
}

pub fn apply_cleanup(directory: &Path, approved_plan_sha256: &str) -> Result<CleanupReceipt> {
    apply(directory, approved_plan_sha256, Kind::Cleanup)
}

pub fn apply_rename(directory: &Path, approved_plan_sha256: &str) -> Result<RenameReceipt> {
    apply(directory, approved_plan_sha256, Kind::Rename)
}

fn apply(directory: &Path, approved_plan_sha256: &str, kind: Kind) -> Result<OperationReceipt> {
    let mut operation = Operation::load(directory, Some(approved_plan_sha256))?;
    if operation.plan.kind != kind {
        return Err(Error::refused(
            "The command does not match the reviewed operation kind.",
        ));
    }
    let mut service = FileService {
        files: operation.files.clone(),
    };
    operation.run(&mut service)
}

pub fn verify_cleanup(
    directory: &Path,
    expected_plan_sha256: Option<&str>,
) -> Result<CleanupReceipt> {
    let operation = Operation::load(directory, expected_plan_sha256)?;
    if operation.plan.kind != Kind::Cleanup {
        return Err(Error::refused("Expected a cleanup operation."));
    }
    verify_loaded(operation)
}

pub fn verify_operation(
    directory: &Path,
    expected_plan_sha256: Option<&str>,
) -> Result<OperationReceipt> {
    verify_loaded(Operation::load(directory, expected_plan_sha256)?)
}

fn verify_loaded(operation: Operation) -> Result<OperationReceipt> {
    let view = match operation.observe(&mut FileService {
        files: operation.files.clone(),
    }) {
        Ok(view) => view,
        Err(error) => return Ok(operation.receipt(None, "verify", "unknown", error.message)),
    };
    let complete = operation.journal.state.finished;
    let state = &operation.journal.state;
    if state.request == (RequestState::ResponseReceived { success: true })
        && !operation
            .plan
            .effect_matches(&view, &operation.plan.selected_environment_ids[state.next])
    {
        return Ok(operation.receipt(Some(&view), "verify", "unknown", "A request was acknowledged but its reviewed effect is not visible. Verification leaves the pending intent unchanged."));
    }
    Ok(operation.receipt(Some(&view), "verify", if complete { "complete" } else { "partial" }, if complete { "Operation journal, current synthetic catalog, and protected records agree. No live state was checked." } else { "Operation has not completed. Verification is read-only; apply or resume requires the reviewed plan digest." }))
}

#[cfg(all(test, unix))]
mod rename_tests;
#[cfg(all(test, unix))]
mod tests;
