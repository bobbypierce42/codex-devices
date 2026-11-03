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
    AcknowledgedOld,
    ReadbackUnavailable,
    ChangedIdentity,
}

struct FaultService {
    file: FileService,
    directory: PathBuf,
    fault: Fault,
    triggered: bool,
    unavailable: bool,
    calls: Vec<String>,
}

impl Service for FaultService {
    fn snapshot(&mut self) -> Result<Vec<u8>> {
        if self.unavailable {
            self.unavailable = false;
            return Err(Error::new(
                "io",
                "Synthetic rename readback unavailable.",
                1,
            ));
        }
        self.file.snapshot()
    }
    fn delete(&mut self, _id: &str, _expected: &str) -> Result<()> {
        panic!("rename must not delete a record")
    }
    fn rename(&mut self, id: &str, name: &str, expected: &str) -> Result<()> {
        self.calls.push(id.into());
        if self.triggered {
            return self.file.rename(id, name, expected);
        }
        self.triggered = true;
        match self.fault {
            Fault::TimeoutBefore => {
                return Err(Error::new(
                    "io",
                    "Synthetic rename timeout before effect.",
                    1,
                ));
            }
            Fault::CrashBefore => panic!("Synthetic rename interruption before effect"),
            Fault::AcknowledgedOld => return Ok(()),
            _ => self.file.rename(id, name, expected)?,
        }
        match self.fault {
            Fault::TimeoutAfter => Err(Error::new(
                "io",
                "Synthetic rename timeout after effect.",
                1,
            )),
            Fault::CrashAfter => panic!("Synthetic rename interruption after effect"),
            Fault::ReadbackUnavailable => {
                self.unavailable = true;
                Ok(())
            }
            Fault::ChangedIdentity => {
                let path = self.directory.join("service.json");
                let mut data: Value = serde_json::from_slice(&read_input(&path)?).unwrap();
                data["pages"][0]["computers"][0]["installation_id"] =
                    Value::String("demo-wrong-installation".into());
                fs::write(path, json_bytes(&data)?)?;
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
        .join("target/rename-fault-tests")
        .join(format!("{}-{stamp}-{sequence}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let directory = root.join("operation");
    let changes = [
        RenameChange {
            environment_id: "demo-active-desktop".into(),
            new_name: "Studio desktop".into(),
        },
        RenameChange {
            environment_id: "demo-active-laptop".into(),
            new_name: "Field laptop".into(),
        },
    ];
    let receipt = prepare_rename(DEMO, &changes, &directory).unwrap();
    let service = FaultService {
        file: FileService {
            files: Directory::open(&directory).unwrap(),
        },
        directory: directory.clone(),
        fault,
        triggered: false,
        unavailable: false,
        calls: Vec::new(),
    };
    (directory, receipt.plan_sha256, service)
}

fn run(directory: &Path, hash: &str, service: &mut impl Service) -> OperationReceipt {
    Operation::load(directory, Some(hash))
        .unwrap()
        .run(service)
        .unwrap()
}

#[test]
fn rename_timeouts_read_back_before_retry_and_never_repeat_verified_effects() {
    for (fault, count) in [(Fault::TimeoutBefore, 3), (Fault::TimeoutAfter, 2)] {
        let (directory, hash, mut service) = setup(fault);
        let partial = run(&directory, &hash, &mut service);
        assert_eq!(partial.status, "partial");
        assert!(partial.reason.contains("Synthetic rename timeout"));
        assert_eq!(run(&directory, &hash, &mut service).status, "complete");
        assert_eq!(service.calls.len(), count);
        let current = View::parse(&service.file.snapshot().unwrap()).unwrap();
        assert_eq!(
            current.records["demo-active-desktop"]["installation_id"],
            "demo-install-desktop"
        );
        assert_eq!(
            current.records["demo-active-laptop"]["installation_id"],
            "demo-install-laptop"
        );
    }
}

#[test]
fn rename_interruptions_reconcile_exact_names_before_retry() {
    for (fault, count) in [(Fault::CrashBefore, 3), (Fault::CrashAfter, 2)] {
        let (directory, hash, mut service) = setup(fault);
        assert!(catch_unwind(AssertUnwindSafe(|| run(&directory, &hash, &mut service))).is_err());
        assert_eq!(
            verify_operation(&directory, Some(&hash)).unwrap().status,
            "partial"
        );
        assert_eq!(run(&directory, &hash, &mut service).status, "complete");
        assert_eq!(service.calls.len(), count);
    }
}

#[test]
fn rename_acknowledged_old_name_stays_pending_without_retry() {
    let (directory, hash, mut service) = setup(Fault::AcknowledgedOld);
    for _ in 0..2 {
        assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
    }
    assert_eq!(service.calls, ["demo-active-desktop"]);
    assert_eq!(
        verify_operation(&directory, Some(&hash)).unwrap().status,
        "unknown"
    );
}

#[test]
fn rename_readback_failure_reconciles_but_changed_identity_stops() {
    for (fault, expected, count) in [
        (Fault::ReadbackUnavailable, "complete", 2),
        (Fault::ChangedIdentity, "unknown", 1),
    ] {
        let (directory, hash, mut service) = setup(fault);
        assert_eq!(run(&directory, &hash, &mut service).status, "unknown");
        assert_eq!(run(&directory, &hash, &mut service).status, expected);
        assert_eq!(service.calls.len(), count);
    }
}

#[test]
fn near_full_rename_journal_refuses_before_any_new_request() {
    let (directory, hash, mut service) = setup(Fault::TimeoutAfter);
    let events = [
        serde_json::json!({"type":"intent","environment_id":"demo-active-desktop","capability_advertisement_changes":[]}),
        serde_json::json!({"type":"response","environment_id":"demo-active-desktop","success":false}),
        serde_json::json!({"type":"renamed","environment_id":"demo-active-desktop","matches":false}),
    ];
    let mut bytes = Vec::new();
    let mut previous = sha256(b"");
    for (sequence, event) in events.into_iter().enumerate() {
        let mut line = if sequence == 0 {
            vec![b' '; MAX_BYTES as usize - 2000]
        } else {
            Vec::new()
        };
        line.extend(serde_json::to_vec(&serde_json::json!({"sequence":sequence,"plan_sha256":hash,"previous_entry_sha256":previous,"event":event})).unwrap());
        previous = sha256(&line);
        bytes.extend(line);
        bytes.push(b'\n');
    }
    fs::write(directory.join("journal.jsonl"), &bytes).unwrap();
    let before = service.file.snapshot().unwrap();
    let error = Operation::load(&directory, Some(&hash))
        .unwrap()
        .run(&mut service)
        .err()
        .unwrap();
    assert_eq!(error.exit_code, 3);
    assert!(service.calls.is_empty());
    assert_eq!(service.file.snapshot().unwrap(), before);
    assert_eq!(read_input(&directory.join("journal.jsonl")).unwrap(), bytes);
}

#[test]
fn conditional_rename_refuses_stale_inventory_before_replacing_the_copy() {
    let (directory, _, mut service) = setup(Fault::TimeoutAfter);
    let before = service.file.snapshot().unwrap();
    assert!(
        service
            .file
            .rename("demo-active-desktop", "Unexpected", &"0".repeat(64))
            .is_err()
    );
    assert_eq!(service.file.snapshot().unwrap(), before);
    assert!(
        read_input(&directory.join("journal.jsonl"))
            .unwrap()
            .is_empty()
    );
}
