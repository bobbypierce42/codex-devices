#![cfg(unix)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codex-devices"))
}

fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/cleanup-tests")
        .join(format!("{}-{stamp}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn result(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(if code == 0 || code == 4 {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap()
}

fn prepare(path: &Path, ids: &[&str]) -> Value {
    let mut command = cmd();
    command
        .args(["cleanup", "--demo", "--out"])
        .arg(path)
        .arg("--json");
    for id in ids {
        command.arg("--select").arg(id);
    }
    result(&command.output().unwrap(), 0)
}

#[test]
fn reviewed_cleanup_previews_without_mutating_the_source_or_service() {
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    assert_eq!(preview["status"], "review_required");
    assert_eq!(
        preview["selected_environment_ids"],
        serde_json::json!(["demo-retired-01"])
    );
    assert_eq!(preview["live_changes_authorized"], false);
    assert_eq!(
        std::fs::read(path.join("service.json")).unwrap(),
        include_bytes!("../fixtures/synthetic-catalog.json")
    );
    assert_eq!(preview["plan_sha256"].as_str().unwrap().len(), 64);
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn rows(catalog: &Value) -> std::collections::BTreeMap<String, Value> {
    catalog["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["computers"].as_array().unwrap())
        .map(|r| (r["environment_id"].as_str().unwrap().into(), r.clone()))
        .collect()
}

fn apply(path: &Path, hash: &str, code: i32) -> Value {
    result(
        &cmd()
            .args(["cleanup", "--apply"])
            .arg(path)
            .args(["--approve-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        code,
    )
}

fn write_json(path: &Path, data: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(data).unwrap()).unwrap();
}

#[test]
fn nine_reviewed_deletions_preserve_the_input_backup_and_every_unselected_record() {
    let root = scratch();
    let input = root.join("source.json");
    let original = include_bytes!("../fixtures/synthetic-catalog.json");
    std::fs::write(&input, original).unwrap();
    let path = root.join("operation");
    let mut command = cmd();
    command
        .args(["cleanup", "--input"])
        .arg(&input)
        .arg("--out")
        .arg(&path)
        .arg("--json");
    for n in 1..=9 {
        command.arg("--select").arg(format!("demo-retired-{n:02}"));
    }
    let preview = result(&command.output().unwrap(), 0);
    let hash = preview["plan_sha256"].as_str().unwrap();
    let before = rows(&read(&input));
    let receipt = apply(&path, hash, 0);
    assert_eq!(receipt["status"], "complete");
    assert_eq!(
        receipt["verified_deleted_environment_ids"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    let after = rows(&read(&path.join("service.json")));
    assert_eq!(after.len(), 3);
    for id in ["demo-active-desktop", "demo-active-laptop", "demo-cloud"] {
        assert_eq!(after[id], before[id]);
    }
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert_eq!(
        std::fs::read(path.join("backup/catalog.json")).unwrap(),
        original
    );
    let journal = std::fs::read(path.join("journal.jsonl")).unwrap();
    let entries: Vec<Value> = journal
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 28);
    for (n, chunk) in entries[..27].chunks(3).enumerate() {
        assert_eq!(chunk[0]["event"]["type"], "intent");
        assert_eq!(chunk[1]["event"]["type"], "response");
        assert_eq!(chunk[2]["event"]["type"], "verified");
        assert_eq!(chunk[2]["event"]["absent"], true);
        assert_eq!(
            chunk[0]["event"]["environment_id"],
            format!("demo-retired-{:02}", n + 1)
        );
    }
    assert_eq!(entries[27]["event"]["type"], "complete");
    apply(&path, hash, 0);
    assert_eq!(std::fs::read(path.join("journal.jsonl")).unwrap(), journal);
    let verified = result(
        &cmd()
            .args(["verify", "--operation"])
            .arg(&path)
            .args(["--expected-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(verified["status"], "complete");
    assert_eq!(verified["plan_anchor"], "matched");
    assert_eq!(verified["live_state_verified"], false);
}

#[test]
fn operation_verification_is_read_only_and_reports_an_incomplete_operation() {
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    let members = [
        "service.json",
        "plan.json",
        "journal.jsonl",
        ".lock",
        "backup/catalog.json",
        "backup/plan.json",
        "backup/manifest.json",
    ];
    let before: Vec<_> = members
        .iter()
        .map(|name| std::fs::read(path.join(name)).unwrap())
        .collect();
    let checked = result(
        &cmd()
            .args(["verify", "--operation"])
            .arg(&path)
            .arg("--json")
            .output()
            .unwrap(),
        4,
    );
    assert_eq!(checked["status"], "partial");
    assert_eq!(checked["plan_anchor"], "not_supplied");
    result(
        &cmd()
            .args(["verify", "--operation"])
            .arg(&path)
            .args(["--expected-plan-sha256", &"0".repeat(64), "--json"])
            .output()
            .unwrap(),
        5,
    );
    for (name, bytes) in members.iter().zip(before) {
        assert_eq!(std::fs::read(path.join(name)).unwrap(), bytes);
    }
    apply(&path, preview["plan_sha256"].as_str().unwrap(), 0);
}

#[test]
fn selection_requires_explicit_distinct_eligible_full_ids_before_creating_files() {
    for ids in [
        vec!["Studio Mac"],
        vec!["demo-active-desktop"],
        vec!["demo-active-laptop"],
        vec!["demo-cloud"],
        vec!["demo-no-such-record"],
        vec!["demo-retired-01", "demo-retired-01"],
    ] {
        let path = scratch().join("refused");
        let mut command = cmd();
        command
            .args(["cleanup", "--demo", "--out"])
            .arg(&path)
            .arg("--json");
        for id in ids {
            command.arg("--select").arg(id);
        }
        result(&command.output().unwrap(), 3);
        assert!(!path.exists());
    }
    for (key, value) in [
        ("status", serde_json::json!("future-status")),
        ("can_delete", Value::Null),
        ("is_attached", serde_json::json!(true)),
        ("can_use_for_tasks", serde_json::json!(true)),
        ("installation_id", Value::Null),
        ("kind", serde_json::json!("future-kind")),
    ] {
        let path = scratch().join("refused");
        let mut data: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
        data["pages"][0]["computers"][2][key] = value;
        let error = codex_devices::prepare_cleanup(
            &serde_json::to_vec(&data).unwrap(),
            &["demo-retired-01".into()],
            &path,
        )
        .err()
        .unwrap();
        assert_eq!(error.exit_code, 3);
        assert!(!path.exists());
    }
}

#[test]
fn approval_cannot_change_or_expand_the_plan_and_tampered_plans_or_backups_stop() {
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    let hash = preview["plan_sha256"].as_str().unwrap();
    result(
        &cmd()
            .args(["cleanup", "--apply"])
            .arg(&path)
            .arg("--json")
            .output()
            .unwrap(),
        2,
    );
    result(
        &cmd()
            .args(["cleanup", "--apply"])
            .arg(&path)
            .args([
                "--approve-plan-sha256",
                hash,
                "--select",
                "demo-retired-02",
                "--json",
            ])
            .output()
            .unwrap(),
        2,
    );
    apply(&path, &"f".repeat(64), 5);
    let plan_path = path.join("plan.json");
    let original = std::fs::read(&plan_path).unwrap();
    let mut plan: Value = serde_json::from_slice(&original).unwrap();
    plan["protected_environment_ids"] = serde_json::json!([]);
    write_json(&plan_path, &plan);
    apply(&path, hash, 5);
    let forged_hash = codex_devices::sha256(&std::fs::read(&plan_path).unwrap());
    apply(&path, &forged_hash, 5);
    std::fs::write(&plan_path, original).unwrap();
    std::fs::write(path.join("backup/catalog.json"), b"{}").unwrap();
    apply(&path, hash, 5);
    assert_eq!(
        std::fs::read(path.join("service.json")).unwrap(),
        codex_devices::DEMO
    );
    assert!(
        std::fs::read(path.join("journal.jsonl"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn scope_identity_access_and_inventory_drift_stop_before_the_first_request() {
    for case in 0..10 {
        let path = scratch().join("operation");
        let preview = prepare(&path, &["demo-retired-01"]);
        let service = path.join("service.json");
        let mut data = read(&service);
        match case {
            0 => data["source"]["account_id"] = serde_json::json!("demo-another-account"),
            1 => data["source"]["dot_id"] = serde_json::json!("demo-another-dot"),
            2 => {
                for page in data["pages"].as_array_mut().unwrap() {
                    page["thread_id"] = serde_json::json!("demo-another-thread");
                }
            }
            3 => {
                data["pages"][0]["computers"][0]["display_name"] =
                    serde_json::json!("Changed protected name")
            }
            4 => {
                data["pages"][0]["computers"][2]["installation_id"] =
                    serde_json::json!("demo-new-installation")
            }
            5 => data["pages"][0]["computers"][2]["can_delete"] = serde_json::json!(false),
            6 => data["pages"][0]["computers"][2]["status"] = serde_json::json!("connected"),
            7 => {
                data["pages"][0]["computers"]
                    .as_array_mut()
                    .unwrap()
                    .remove(2);
            }
            8 => {
                data["pages"][0]["computers"]
                    .as_array_mut()
                    .unwrap()
                    .remove(0);
            }
            _ => {
                let mut extra = data["pages"][0]["computers"][2].clone();
                extra["environment_id"] = serde_json::json!("demo-unreviewed");
                data["pages"][0]["computers"]
                    .as_array_mut()
                    .unwrap()
                    .push(extra);
            }
        }
        write_json(&service, &data);
        let bytes = std::fs::read(&service).unwrap();
        let receipt = apply(&path, preview["plan_sha256"].as_str().unwrap(), 4);
        assert_eq!(receipt["status"], "unknown");
        assert_eq!(std::fs::read(service).unwrap(), bytes);
        assert!(
            std::fs::read(path.join("journal.jsonl"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn capability_advertisements_are_reported_without_weakening_other_record_guards() {
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    let service = path.join("service.json");
    let mut data = read(&service);
    data["pages"][0]["computers"][0]["selected_capability_roots"] =
        serde_json::json!(["demo-new-advertisement"]);
    data["pages"][0]["computers"][2]["selected_capability_roots"] =
        serde_json::json!(["demo-selected-advertisement"]);
    write_json(&service, &data);
    let receipt = apply(&path, preview["plan_sha256"].as_str().unwrap(), 0);
    assert_eq!(
        receipt["capability_advertisement_changes"],
        serde_json::json!(["demo-active-desktop", "demo-retired-01"])
    );
    assert_eq!(
        rows(&read(&service))["demo-active-desktop"]["selected_capability_roots"],
        serde_json::json!(["demo-new-advertisement"])
    );
}

#[test]
fn corrupt_torn_and_invalidly_ordered_journals_are_retained_and_never_retried() {
    for case in 0..4 {
        let path = scratch().join("operation");
        let preview = prepare(&path, &["demo-retired-01"]);
        let hash = preview["plan_sha256"].as_str().unwrap();
        let event = serde_json::json!({"sequence": if case==0 {1} else {0}, "plan_sha256": if case==1 {"unbound"} else {hash}, "previous_entry_sha256": codex_devices::sha256(b""), "event":{"type":"verified","environment_id":"demo-retired-01","absent":true}});
        let mut bytes = serde_json::to_vec(&event).unwrap();
        if case != 3 {
            bytes.push(b'\n');
        }
        std::fs::write(path.join("journal.jsonl"), &bytes).unwrap();
        apply(&path, hash, 5);
        assert_eq!(std::fs::read(path.join("journal.jsonl")).unwrap(), bytes);
        assert_eq!(
            std::fs::read(path.join("service.json")).unwrap(),
            codex_devices::DEMO
        );
    }
}

#[test]
fn process_lock_refuses_a_second_writer_and_releases_without_lock_file_deletion() {
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(".lock"))
        .unwrap();
    file.try_lock().unwrap();
    apply(&path, preview["plan_sha256"].as_str().unwrap(), 3);
    assert!(
        std::fs::read(path.join("journal.jsonl"))
            .unwrap()
            .is_empty()
    );
    drop(file);
    apply(&path, preview["plan_sha256"].as_str().unwrap(), 0);
    assert!(path.join(".lock").exists());
}

#[test]
fn reappearing_completed_targets_and_unsafe_service_members_stop_progress() {
    use std::os::unix::fs::symlink;
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    let hash = preview["plan_sha256"].as_str().unwrap();
    apply(&path, hash, 0);
    std::fs::write(path.join("service.json"), codex_devices::DEMO).unwrap();
    assert_eq!(apply(&path, hash, 4)["status"], "unknown");
    let path = scratch().join("operation");
    let preview = prepare(&path, &["demo-retired-01"]);
    std::fs::rename(path.join("service.json"), path.join("saved-service.json")).unwrap();
    symlink(path.join("saved-service.json"), path.join("service.json")).unwrap();
    apply(&path, preview["plan_sha256"].as_str().unwrap(), 3);
}

#[test]
fn compact_unknown_metadata_can_be_preserved_without_pretty_print_expansion() {
    let mut data: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    let mut payload = serde_json::json!(vec![0; 40_000]);
    for _ in 0..55 {
        payload = serde_json::json!({"nested":payload});
    }
    data["pages"][0]["computers"][0]["future_metadata"] = payload;
    let bytes = serde_json::to_vec(&data).unwrap();
    assert!(bytes.len() < codex_devices::MAX_BYTES as usize);
    assert!(serde_json::to_vec_pretty(&data).unwrap().len() > codex_devices::MAX_BYTES as usize);
    let path = scratch().join("operation");
    let preview =
        codex_devices::prepare_cleanup(&bytes, &["demo-retired-01".into()], &path).unwrap();
    let receipt = codex_devices::apply_cleanup(&path, &preview.plan_sha256).unwrap();
    assert_eq!(receipt.status, "complete", "{}", receipt.reason);
    let after = rows(&read(&path.join("service.json")));
    assert_eq!(
        after["demo-active-desktop"],
        rows(&data)["demo-active-desktop"]
    );
    assert_eq!(
        std::fs::read(path.join("backup/catalog.json")).unwrap(),
        bytes
    );
}

#[test]
fn a_plan_that_cannot_fit_the_journal_is_refused_before_creating_artifacts() {
    let records: Vec<Value> = (0..4096)
        .map(|n| {
            serde_json::json!({
                "environment_id":format!("demo-{n:04}-{}", "x".repeat(150)),
                "installation_id":format!("demo-install-{n}"),
                "kind":"personal","status":"offline","is_attached":false,
                "can_use_for_tasks":false,"can_delete":true,
            })
        })
        .collect();
    let ids: Vec<String> = records
        .iter()
        .map(|r| r["environment_id"].as_str().unwrap().into())
        .collect();
    let data = serde_json::json!({"schema_version":1,"source":{"kind":"synthetic","account_id":"demo-user","dot_id":"demo-dot"},"pages":[{"cursor":null,"next_cursor":null,"thread_id":"demo-thread","computers":records}]});
    let bytes = serde_json::to_vec(&data).unwrap();
    assert!(bytes.len() < codex_devices::MAX_BYTES as usize);
    let path = scratch().join("refused");
    let error = codex_devices::prepare_cleanup(&bytes, &ids, &path)
        .err()
        .expect("review must not promise an operation that cannot be journaled");
    assert_eq!(error.exit_code, 3);
    assert!(error.message.to_lowercase().contains("journal"), "{error}");
    assert!(!path.exists());
}
