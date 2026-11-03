#![cfg(unix)]

use serde_json::Value;
use std::collections::BTreeMap;
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
        .join("target/rename-tests")
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

fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn rows(catalog: &Value) -> BTreeMap<String, Value> {
    catalog["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["computers"].as_array().unwrap())
        .map(|r| (r["environment_id"].as_str().unwrap().into(), r.clone()))
        .collect()
}

fn snapshot(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for name in [
        "plan.json",
        "service.json",
        "journal.jsonl",
        ".lock",
        "backup/catalog.json",
        "backup/plan.json",
        "backup/manifest.json",
    ] {
        files.insert(name.into(), std::fs::read(path.join(name)).unwrap());
    }
    files
}

#[test]
fn five_reviewed_names_preserve_all_ids_grants_and_unselected_records() {
    let root = scratch();
    let input = root.join("input.json");
    let path = root.join("operation");
    let mut catalog: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    for page in catalog["pages"].as_array_mut().unwrap() {
        for row in page["computers"].as_array_mut().unwrap() {
            row["can_rename"] = Value::Bool(row["kind"] == "personal");
        }
    }
    let original = serde_json::to_vec(&catalog).unwrap();
    std::fs::write(&input, &original).unwrap();
    let mappings = [
        ("demo-active-desktop", "Studio desktop"),
        ("demo-active-laptop", "Field laptop"),
        ("demo-retired-01", "Build Mac"),
        ("demo-retired-02", "Windows desk"),
        ("demo-retired-03", "Test Windows"),
    ];
    let mut command = cmd();
    command
        .args(["rename", "--input"])
        .arg(&input)
        .arg("--out")
        .arg(&path)
        .arg("--json");
    for (id, name) in mappings {
        command.arg("--set").arg(format!("{id}={name}"));
    }
    let preview = result(&command.output().unwrap(), 0);
    assert_eq!(preview["status"], "review_required");
    assert_eq!(std::fs::read(path.join("service.json")).unwrap(), original);
    let hash = preview["plan_sha256"].as_str().unwrap();
    let before = snapshot(&path);
    let checked = result(
        &cmd()
            .args(["verify", "--operation"])
            .arg(&path)
            .args(["--expected-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        4,
    );
    assert_eq!(checked["status"], "partial");
    assert_eq!(snapshot(&path), before);
    let applied = result(
        &cmd()
            .args(["rename", "--apply"])
            .arg(&path)
            .args(["--approve-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(applied["status"], "complete");
    assert_eq!(
        applied["verified_renamed_environment_ids"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert!(
        applied["verified_deleted_environment_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut expected = rows(&catalog);
    for (id, name) in mappings {
        expected.get_mut(id).unwrap()["display_name"] = Value::String(name.into());
    }
    assert_eq!(rows(&read(&path.join("service.json"))), expected);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(
        std::fs::read(path.join("backup/catalog.json")).unwrap(),
        original
    );
    let complete = snapshot(&path);
    result(
        &cmd()
            .args(["verify", "--operation"])
            .arg(&path)
            .args(["--expected-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        0,
    );
    result(
        &cmd()
            .args(["rename", "--resume"])
            .arg(&path)
            .args(["--approve-plan-sha256", hash, "--json"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(snapshot(&path), complete);
}

fn changes(pairs: &[(&str, &str)]) -> Vec<codex_devices::RenameChange> {
    pairs
        .iter()
        .map(|(id, name)| codex_devices::RenameChange {
            environment_id: (*id).into(),
            new_name: (*name).into(),
        })
        .collect()
}

fn prepare(path: &Path) -> String {
    codex_devices::prepare_rename(
        codex_devices::DEMO,
        &changes(&[("demo-active-laptop", "Field laptop")]),
        path,
    )
    .unwrap()
    .plan_sha256
}

fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn names_use_trimmed_unicode_and_utf16_limits_and_preserve_equals() {
    for name in ["   café 💻 = lab  ".to_owned(), "💻".repeat(50)] {
        let path = scratch().join("operation");
        let preview = codex_devices::prepare_rename(
            codex_devices::DEMO,
            &changes(&[("demo-active-laptop", &name)]),
            &path,
        )
        .unwrap();
        assert_eq!(preview.renames[0].new_name, name.trim());
        codex_devices::apply_rename(&path, &preview.plan_sha256).unwrap();
        assert_eq!(
            rows(&read(&path.join("service.json")))["demo-active-laptop"]["display_name"],
            name.trim()
        );
    }
    for name in [
        " ".to_owned(),
        "x".repeat(101),
        "💻".repeat(51),
        "bad\nname".into(),
        "name\n".into(),
        "old-laptop.example".into(),
    ] {
        let path = scratch().join("refused");
        let error = codex_devices::prepare_rename(
            codex_devices::DEMO,
            &changes(&[("demo-active-laptop", &name)]),
            &path,
        )
        .err()
        .expect("invalid name must be refused");
        assert_eq!(error.exit_code, 3);
        assert!(!path.exists());
    }
    let path = scratch().join("equals");
    let output = cmd()
        .args([
            "rename",
            "--demo",
            "--set",
            "demo-active-laptop=Lab=East",
            "--out",
        ])
        .arg(&path)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(result(&output, 0)["renames"][0]["new_name"], "Lab=East");
}

#[test]
fn selection_requires_unique_ids_and_explicit_rename_permission() {
    for pairs in [
        vec![],
        vec![("Studio Mac", "New")],
        vec![("demo-missing", "New")],
        vec![("demo-active-laptop", "One"), ("demo-active-laptop", "Two")],
        vec![("demo-cloud", "New")],
        vec![("demo-retired-08", "New")],
        vec![("demo-retired-04", "New")],
    ] {
        let path = scratch().join("refused");
        assert!(
            codex_devices::prepare_rename(codex_devices::DEMO, &changes(&pairs), &path).is_err()
        );
        assert!(!path.exists());
    }
    for (key, value) in [
        ("can_rename", Value::Null),
        ("can_rename", Value::Bool(false)),
        ("can_rename", Value::String("true".into())),
        ("installation_id", Value::Null),
        ("status", Value::String("future-status".into())),
        ("kind", Value::String("future-kind".into())),
        (
            "installation_id",
            Value::String("demo-install-desktop".into()),
        ),
    ] {
        let mut data: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
        data["pages"][0]["computers"][1][key] = value;
        let path = scratch().join("refused");
        assert!(
            codex_devices::prepare_rename(
                &serde_json::to_vec(&data).unwrap(),
                &changes(&[("demo-active-laptop", "New")]),
                &path
            )
            .is_err()
        );
        assert!(!path.exists());
    }
}

#[test]
fn rename_cli_rejects_ambiguous_modes_and_malformed_mappings_before_file_access() {
    for args in [
        vec![
            "rename",
            "--input",
            "missing",
            "--out",
            "unused",
            "--set",
            "missing-equals",
        ],
        vec![
            "rename", "--input", "missing", "--out", "unused", "--set", "=New",
        ],
        vec![
            "rename",
            "--input",
            "missing",
            "--out",
            "unused",
            "--set",
            "--json",
            "demo-id=New",
        ],
        vec![
            "rename",
            "--apply",
            "missing",
            "--resume",
            "missing",
            "--approve-plan-sha256",
            "bad",
        ],
        vec![
            "rename",
            "--apply",
            "missing",
            "--approve-plan-sha256",
            "bad",
            "--set",
            "demo-id=New",
        ],
        vec![
            "rename",
            "--input",
            "missing",
            "--out",
            "unused",
            "--set",
            "demo-id=New",
            "--select",
            "demo-id",
        ],
        vec![
            "cleanup",
            "--input",
            "missing",
            "--out",
            "unused",
            "--select",
            "demo-id",
            "--set",
            "demo-id=New",
        ],
        vec![
            "rename",
            "--demo",
            "--live",
            "--set",
            "demo-id=New",
            "--out",
            "unused",
        ],
    ] {
        let output = cmd().args(args).arg("--json").output().unwrap();
        assert!(!output.status.success());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["kind"], "usage");
    }
}

#[test]
fn plan_hash_target_binding_and_operation_kind_cannot_be_bypassed() {
    let path = scratch().join("operation");
    let hash = prepare(&path);
    let before = snapshot(&path);
    assert_eq!(
        codex_devices::apply_rename(&path, &"0".repeat(64))
            .err()
            .unwrap()
            .exit_code,
        5
    );
    assert_eq!(
        codex_devices::apply_cleanup(&path, &hash)
            .err()
            .unwrap()
            .exit_code,
        3
    );
    let plan_path = path.join("plan.json");
    let mut plan = read(&plan_path);
    plan["selected_records"][0]["installation_id"] = Value::String("demo-forged-install".into());
    write(&plan_path, &plan);
    let forged = codex_devices::sha256(&std::fs::read(&plan_path).unwrap());
    assert_eq!(
        codex_devices::apply_rename(&path, &forged)
            .err()
            .unwrap()
            .exit_code,
        5
    );
    assert_eq!(
        std::fs::read(path.join("service.json")).unwrap(),
        before["service.json"]
    );
    assert_eq!(
        std::fs::read(path.join("journal.jsonl")).unwrap(),
        before["journal.jsonl"]
    );
    let path = scratch().join("cleanup");
    let preview =
        codex_devices::prepare_cleanup(codex_devices::DEMO, &["demo-retired-01".into()], &path)
            .unwrap();
    assert_eq!(
        codex_devices::apply_rename(&path, &preview.plan_sha256)
            .err()
            .unwrap()
            .exit_code,
        3
    );
    assert!(
        read(&path.join("plan.json")).get("renames").is_none(),
        "cleanup v1 wire format must stay readable"
    );
    assert_eq!(
        codex_devices::apply_cleanup(&path, &preview.plan_sha256)
            .unwrap()
            .status,
        "complete"
    );
}

#[test]
fn scope_name_identity_access_and_inventory_drift_stop_rename_before_effects() {
    for case in 0..13 {
        let path = scratch().join("operation");
        let hash = prepare(&path);
        let service = path.join("service.json");
        let mut data = read(&service);
        match case {
            0 => data["source"]["account_id"] = Value::String("demo-other-account".into()),
            1 => data["source"]["dot_id"] = Value::String("demo-other-dot".into()),
            2 => {
                for page in data["pages"].as_array_mut().unwrap() {
                    page["thread_id"] = Value::String("demo-other-thread".into());
                }
            }
            3 => {
                data["pages"][0]["computers"][1]["installation_id"] =
                    Value::String("demo-changed-install".into())
            }
            4 => data["pages"][0]["computers"][1]["status"] = Value::String("offline".into()),
            5 => data["pages"][0]["computers"][1]["can_rename"] = Value::Bool(false),
            6 => data["pages"][0]["computers"][1]["can_use_for_tasks"] = Value::Bool(true),
            7 => data["pages"][0]["computers"][1]["is_attached"] = Value::Bool(true),
            8 => {
                data["pages"][0]["computers"][1]["display_name"] =
                    Value::String("Field laptop".into())
            }
            9 => {
                data["pages"][0]["computers"][0]["display_name"] =
                    Value::String("Unexpected protected name".into())
            }
            10 => {
                data["pages"][0]["computers"]
                    .as_array_mut()
                    .unwrap()
                    .remove(1);
            }
            11 => {
                let mut extra = data["pages"][0]["computers"][2].clone();
                extra["environment_id"] = Value::String("demo-extra".into());
                data["pages"][0]["computers"]
                    .as_array_mut()
                    .unwrap()
                    .push(extra);
            }
            _ => data["pages"][0]["computers"][1]["future_metadata"] = Value::Bool(true),
        }
        write(&service, &data);
        let before = snapshot(&path);
        let receipt = codex_devices::apply_rename(&path, &hash).unwrap();
        assert_eq!(receipt.status, "unknown", "case {case}");
        assert_eq!(snapshot(&path), before);
    }
}

#[test]
fn completed_name_regression_is_unknown_and_capability_changes_stay_separate() {
    let path = scratch().join("operation");
    let hash = prepare(&path);
    let service = path.join("service.json");
    let mut data = read(&service);
    for index in [0, 1] {
        data["pages"][0]["computers"][index]["selected_capability_roots"] =
            serde_json::json!(["demo-new-root"]);
    }
    write(&service, &data);
    let receipt = codex_devices::apply_rename(&path, &hash).unwrap();
    assert_eq!(receipt.status, "complete");
    assert_eq!(
        receipt.capability_advertisement_changes,
        ["demo-active-desktop", "demo-active-laptop"]
    );
    data = read(&service);
    data["pages"][0]["computers"][1]["display_name"] = Value::String("old-laptop.example".into());
    write(&service, &data);
    assert_eq!(
        codex_devices::verify_operation(&path, Some(&hash))
            .unwrap()
            .status,
        "unknown"
    );
    assert_eq!(
        codex_devices::apply_rename(&path, &hash).unwrap().status,
        "unknown"
    );
}

#[test]
fn rename_rejects_cleanup_readback_events_and_retains_torn_journals() {
    for torn in [false, true] {
        let path = scratch().join("operation");
        let hash = prepare(&path);
        let events = [
            serde_json::json!({"type":"intent","environment_id":"demo-active-laptop","capability_advertisement_changes":[]}),
            serde_json::json!({"type":"response","environment_id":"demo-active-laptop","success":true}),
            serde_json::json!({"type":"verified","environment_id":"demo-active-laptop","absent":true}),
        ];
        let mut bytes = Vec::new();
        let mut previous = codex_devices::sha256(b"");
        for (sequence, event) in events.into_iter().enumerate() {
            let line = serde_json::to_vec(&serde_json::json!({"sequence":sequence,"plan_sha256":hash,"previous_entry_sha256":previous,"event":event})).unwrap();
            previous = codex_devices::sha256(&line);
            bytes.extend(line);
            if !torn || sequence != 2 {
                bytes.push(b'\n');
            }
        }
        std::fs::write(path.join("journal.jsonl"), bytes).unwrap();
        let before = snapshot(&path);
        assert_eq!(
            codex_devices::apply_rename(&path, &hash)
                .err()
                .unwrap()
                .exit_code,
            5
        );
        assert_eq!(snapshot(&path), before);
    }
}

#[test]
fn every_intermediate_rename_size_is_checked_before_preparation() {
    let mut data: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    data["pages"][0]["computers"][0]["display_name"] = Value::String("a".into());
    data["pages"][0]["computers"][1]["display_name"] = Value::String("b".repeat(100));
    data["padding"] = Value::String(String::new());
    let initial = serde_json::to_vec(&data).unwrap().len();
    data["padding"] = Value::String("x".repeat(codex_devices::MAX_BYTES as usize - initial - 20));
    let bytes = serde_json::to_vec(&data).unwrap();
    assert!(bytes.len() < codex_devices::MAX_BYTES as usize);
    let path = scratch().join("refused");
    let error = codex_devices::prepare_rename(
        &bytes,
        &changes(&[
            ("demo-active-desktop", &"c".repeat(100)),
            ("demo-active-laptop", "d"),
        ]),
        &path,
    )
    .err()
    .unwrap();
    assert_eq!(error.exit_code, 3);
    assert!(error.message.contains("intermediate"));
    assert!(!path.exists());
}

#[test]
fn rename_grants_are_independent_of_delete_and_equal_names_stay_distinct() {
    let mut data: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    for index in [0, 1] {
        data["pages"][0]["computers"][index]["can_delete"] = Value::Bool(false);
        data["pages"][0]["computers"][index]["future_metadata"] =
            serde_json::json!({"nested":[null,"invented",42]});
    }
    let path = scratch().join("operation");
    let preview = codex_devices::prepare_rename(
        &serde_json::to_vec(&data).unwrap(),
        &changes(&[
            ("demo-active-desktop", "Same name"),
            ("demo-active-laptop", "Same name"),
        ]),
        &path,
    )
    .unwrap();
    assert_eq!(
        codex_devices::apply_rename(&path, &preview.plan_sha256)
            .unwrap()
            .status,
        "complete"
    );
    let mut expected = rows(&data);
    for id in ["demo-active-desktop", "demo-active-laptop"] {
        expected.get_mut(id).unwrap()["display_name"] = Value::String("Same name".into());
    }
    assert_eq!(rows(&read(&path.join("service.json"))), expected);
}
