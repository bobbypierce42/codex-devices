use super::*;
use crate::{DEMO, read_input};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Clone, Copy)]
enum Fault {
    TimeoutBefore,
    TimeoutAfter,
    CrashBefore,
    CrashAfter,
    SuccessWithoutEffect,
    ProtectedDrift,
    ReadbackUnavailable,
}

struct FaultService {
    file: FileService,
    directory: PathBuf,
    fault: Fault,
    triggered: bool,
    calls: Vec<String>,
    fail_read: bool,
}

impl Service for FaultService {
    fn snapshot(&mut self) -> Result<Vec<u8>> {
        if self.fail_read {
            self.fail_read = false;
            return Err(Error::new("io", "Synthetic readback unavailable.", 1));
        }
        self.file.snapshot()
    }

    fn delete(&mut self, id: &str, expected: &str) -> Result<()> {
        self.calls.push(id.into());
        if self.triggered {
            return self.file.delete(id, expected);
        }
        self.triggered = true;
        match self.fault {
            Fault::TimeoutBefore => {
                return Err(Error::new("io", "Synthetic timeout before effect.", 1));
            }
            Fault::CrashBefore => panic!("Synthetic interruption before effect"),
            Fault::SuccessWithoutEffect => return Ok(()),
            _ => self.file.delete(id, expected)?,
        }
        match self.fault {
            Fault::TimeoutAfter => Err(Error::new("io", "Synthetic timeout after effect.", 1)),
            Fault::CrashAfter => panic!("Synthetic interruption after effect"),
            Fault::ProtectedDrift => {
                let path = self.directory.join("service.json");
                let mut data: Value = serde_json::from_slice(&read_input(&path)?).unwrap();
                data["pages"][0]["computers"][0]["display_name"] =
                    Value::String("Changed during request".into());
                fs::write(path, json_bytes(&data)?)?;
                Ok(())
            }
            Fault::ReadbackUnavailable => {
                self.fail_read = true;
                Ok(())
            }
            _ => unreachable!(),
        }
    }
}

fn setup(fault: Fault) -> (PathBuf, String, FaultService) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/cleanup-fault-tests")
        .join(format!("{}-{stamp}-{sequence}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let directory = root.join("operation");
    let receipt = prepare_cleanup(
        DEMO,
        &["demo-retired-01".into(), "demo-retired-02".into()],
        &directory,
    )
    .unwrap();
    let service = FaultService {
        file: FileService {
            files: Directory::open(&directory).unwrap(),
        },
        directory: directory.clone(),
        fault,
        triggered: false,
        calls: Vec::new(),
        fail_read: false,
    };
    (directory, receipt.plan_sha256, service)
}

#[test]
fn journal_append_does_not_follow_a_replaced_operation_directory() {
    let (directory, hash, _) = setup(Fault::TimeoutBefore);
    let mut operation = Operation::load(&directory, Some(&hash)).unwrap();
    let held = directory.with_file_name("held-operation");
    fs::rename(&directory, &held).unwrap();
    let replacement = Directory::create(&directory).unwrap();
    let substituted = directory.join("journal.jsonl");
    replacement.write_new("journal.jsonl", b"").unwrap();
    let result = operation.append(Event::Intent {
        environment_id: "demo-retired-01".into(),
        capability_advertisement_changes: Vec::new(),
    });
    assert_eq!(fs::read(&substituted).unwrap(), b"");
    if result.is_ok() {
        assert!(!fs::read(held.join("journal.jsonl")).unwrap().is_empty());
    }
}

#[test]
fn effects_and_temporary_replacement_stay_in_the_loaded_operation() {
    let (directory, hash, _) = setup(Fault::TimeoutBefore);
    let mut operation = Operation::load(&directory, Some(&hash)).unwrap();
    let mut service = FileService {
        files: operation.files.clone(),
    };
    let held = directory.with_file_name("held-operation");
    fs::rename(&directory, &held).unwrap();
    prepare_cleanup(DEMO, &["demo-retired-01".into()], &directory).unwrap();
    let original_service = fs::read(directory.join("service.json")).unwrap();
    let original_journal = fs::read(directory.join("journal.jsonl")).unwrap();
    let receipt = operation.run(&mut service).unwrap();
    assert_eq!(receipt.status, "complete");
    assert_eq!(
        fs::read(directory.join("service.json")).unwrap(),
        original_service
    );
    assert_eq!(
        fs::read(directory.join("journal.jsonl")).unwrap(),
        original_journal
    );
    drop(operation);
    assert_eq!(
        verify_cleanup(&held, Some(&hash)).unwrap().status,
        "complete"
    );
}

#[test]
fn a_substituted_journal_symlink_is_refused_without_touching_its_target() {
    let (directory, hash, _) = setup(Fault::TimeoutBefore);
    let mut operation = Operation::load(&directory, Some(&hash)).unwrap();
    let journal = directory.join("journal.jsonl");
    let victim = directory.with_file_name("synthetic-victim");
    fs::write(&victim, b"").unwrap();
    fs::rename(&journal, directory.join("saved-journal")).unwrap();
    std::os::unix::fs::symlink(&victim, &journal).unwrap();
    let error = operation
        .append(Event::Intent {
            environment_id: "demo-retired-01".into(),
            capability_advertisement_changes: Vec::new(),
        })
        .unwrap_err();
    assert_eq!(error.kind, "refused");
    assert_eq!(fs::read(victim).unwrap(), b"");
    assert_eq!(operation.journal.state.request, RequestState::Idle);
}

fn run(directory: &Path, hash: &str, service: &mut impl Service) -> CleanupReceipt {
    Operation::load(directory, Some(hash))
        .unwrap()
        .run(service)
        .unwrap()
}

#[test]
fn timeout_before_effect_is_read_back_before_an_explicit_resume_retries() {
    let (directory, hash, mut service) = setup(Fault::TimeoutBefore);
    let partial = run(&directory, &hash, &mut service);
    assert_eq!(partial.status, "partial");
    assert!(partial.reason.contains("Synthetic timeout before effect"));
    assert!(partial.verified_deleted_environment_ids.is_empty());
    assert_eq!(service.calls, ["demo-retired-01"]);
    let journal = read_input(&directory.join("journal.jsonl")).unwrap();
    let entries: Vec<Value> = journal
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(entries[2]["event"]["type"], "verified");
    assert_eq!(entries[2]["event"]["absent"], false);
    assert_eq!(run(&directory, &hash, &mut service).status, "complete");
    assert_eq!(
        service.calls,
        ["demo-retired-01", "demo-retired-01", "demo-retired-02"]
    );
}

#[test]
fn timeout_after_effect_never_repeats_the_deleted_target() {
    let (directory, hash, mut service) = setup(Fault::TimeoutAfter);
    let partial = run(&directory, &hash, &mut service);
    assert_eq!(partial.status, "partial");
    assert_eq!(
        partial.verified_deleted_environment_ids,
        ["demo-retired-01"]
    );
    assert_eq!(run(&directory, &hash, &mut service).status, "complete");
    assert_eq!(service.calls, ["demo-retired-01", "demo-retired-02"]);
}

#[test]
fn interruptions_before_and_after_effect_reconcile_the_durable_intent() {
    for (fault, expected_calls) in [(Fault::CrashBefore, 3), (Fault::CrashAfter, 2)] {
        let (directory, hash, mut service) = setup(fault);
        assert!(catch_unwind(AssertUnwindSafe(|| run(&directory, &hash, &mut service))).is_err());
        let pending = verify_cleanup(&directory, Some(&hash)).unwrap();
        assert_eq!(pending.status, "partial");
        assert_eq!(run(&directory, &hash, &mut service).status, "complete");
        assert_eq!(service.calls.len(), expected_calls);
        assert_eq!(service.calls.last().unwrap(), "demo-retired-02");
        assert_eq!(
            verify_cleanup(&directory, Some(&hash)).unwrap().status,
            "complete"
        );
    }
}

#[test]
fn acknowledged_but_present_target_remains_unknown_and_is_never_blindly_retried() {
    let (directory, hash, mut service) = setup(Fault::SuccessWithoutEffect);
    assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
    assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
    assert_eq!(service.calls, ["demo-retired-01"]);
    assert_eq!(
        verify_cleanup(&directory, Some(&hash)).unwrap().status,
        "unknown"
    );
    let pending = Operation::load(&directory, Some(&hash)).unwrap();
    assert_eq!(
        pending.journal.state.request,
        RequestState::ResponseReceived { success: true }
    );
}

#[test]
fn protected_record_drift_after_a_request_stops_before_the_next_request() {
    let (directory, hash, mut service) = setup(Fault::ProtectedDrift);
    assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
    assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
    assert_eq!(service.calls, ["demo-retired-01"]);
    let view = View::parse(&service.file.snapshot().unwrap()).unwrap();
    assert!(!view.records.contains_key("demo-retired-01"));
    assert!(view.records.contains_key("demo-retired-02"));
}

#[test]
fn unavailable_readback_preserves_the_intent_until_resume_can_establish_absence() {
    let (directory, hash, mut service) = setup(Fault::ReadbackUnavailable);
    let unknown = run(&directory, &hash, &mut service);
    assert_eq!(unknown.status, "unknown");
    assert!(unknown.remaining_environment_ids.is_none());
    assert_eq!(service.calls, ["demo-retired-01"]);
    assert_eq!(run(&directory, &hash, &mut service).status, "complete");
    assert_eq!(service.calls, ["demo-retired-01", "demo-retired-02"]);
}

#[test]
fn conditional_service_write_refuses_a_stale_inventory_without_effect() {
    let (directory, _, mut service) = setup(Fault::TimeoutBefore);
    let original = service.file.snapshot().unwrap();
    assert!(
        service
            .file
            .delete("demo-retired-01", &"0".repeat(64))
            .is_err()
    );
    assert_eq!(service.file.snapshot().unwrap(), original);
    assert!(
        read_input(&directory.join("journal.jsonl"))
            .unwrap()
            .is_empty()
    );
}

fn history_with_padding(hash: &str, padding: usize) -> Vec<u8> {
    let events = [
        serde_json::json!({"type":"intent","environment_id":"demo-retired-01","capability_advertisement_changes":[]}),
        serde_json::json!({"type":"response","environment_id":"demo-retired-01","success":false}),
        serde_json::json!({"type":"verified","environment_id":"demo-retired-01","absent":false}),
    ];
    let mut bytes = Vec::new();
    let mut previous = sha256(b"");
    for (sequence, event) in events.into_iter().enumerate() {
        let mut line = if sequence == 0 {
            vec![b' '; padding]
        } else {
            Vec::new()
        };
        line.extend(serde_json::to_vec(&serde_json::json!({"sequence":sequence,"plan_sha256":hash,"previous_entry_sha256":previous,"event":event})).unwrap());
        previous = sha256(&line);
        bytes.extend(line);
        bytes.push(b'\n');
    }
    bytes
}

#[test]
fn journal_capacity_is_reserved_before_any_effect() {
    let (directory, hash, mut service) = setup(Fault::TimeoutAfter);
    let intent = serde_json::json!({"sequence":3,"plan_sha256":hash,"previous_entry_sha256":"0".repeat(64),"event":{"type":"intent","environment_id":"demo-retired-01","capability_advertisement_changes":[]}});
    let intent_bytes = serde_json::to_vec(&intent).unwrap().len() + 1;
    let history_size = MAX_BYTES as usize - intent_bytes - 1;
    let unpadded = history_with_padding(&hash, 0).len();
    let history = history_with_padding(&hash, history_size - unpadded);
    assert_eq!(history.len(), history_size);
    fs::write(directory.join("journal.jsonl"), &history).unwrap();
    let before = service.file.snapshot().unwrap();
    let error = Operation::load(&directory, Some(&hash))
        .unwrap()
        .run(&mut service)
        .err()
        .unwrap();
    assert_eq!(error.exit_code, 3);
    assert!(
        service.calls.is_empty(),
        "journal headroom must be checked before issuing a request"
    );
    assert_eq!(service.file.snapshot().unwrap(), before);
    assert_eq!(
        read_input(&directory.join("journal.jsonl")).unwrap(),
        history
    );
}

#[test]
fn reserved_headroom_reconciles_an_interruption_near_the_journal_limit() {
    let (directory, hash, mut service) = setup(Fault::CrashAfter);
    let history_size = MAX_BYTES as usize - 4096;
    let history = history_with_padding(&hash, history_size - history_with_padding(&hash, 0).len());
    fs::write(directory.join("journal.jsonl"), history).unwrap();
    assert!(catch_unwind(AssertUnwindSafe(|| run(&directory, &hash, &mut service))).is_err());
    assert_eq!(run(&directory, &hash, &mut service).status, "complete");
    assert_eq!(service.calls, ["demo-retired-01", "demo-retired-02"]);
    assert_eq!(
        verify_cleanup(&directory, Some(&hash)).unwrap().status,
        "complete"
    );
    assert!(fs::metadata(directory.join("journal.jsonl")).unwrap().len() <= MAX_BYTES);
}
