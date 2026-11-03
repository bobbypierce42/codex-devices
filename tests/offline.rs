use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch() -> PathBuf {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-runs")
        .join(format!(
            "{}-{now}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codex-devices"))
}

fn json_output(output: &Output, code: i32) -> Value {
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

fn demo() -> Value {
    serde_json::from_slice(include_bytes!("../fixtures/synthetic-catalog.json")).unwrap()
}

fn input(value: &Value) -> PathBuf {
    let path = scratch().join("fixture.json");
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    path
}

fn audit(value: &Value, code: i32) -> Value {
    json_output(
        &cmd()
            .arg("audit")
            .arg("--input")
            .arg(input(value))
            .arg("--json")
            .output()
            .unwrap(),
        code,
    )
}

#[test]
fn audit_demo_keeps_all_personal_and_cloud_records_distinct() {
    let output = cmd().args(["audit", "--demo", "--json"]).output().unwrap();
    let data = json_output(&output, 0);
    assert_eq!(data["mode"], "offline-synthetic");
    assert_eq!(data["complete"], true);
    assert_eq!(data["counts"]["personal"], 11);
    assert_eq!(data["counts"]["cloud"], 1);
    assert_eq!(data["counts"]["connected"], 2);
    assert_eq!(data["counts"]["offline"], 9);
    assert_eq!(data["counts"]["cleanup_candidates"], 9);
    let rows = data["records"].as_array().unwrap();
    let ids: BTreeSet<_> = rows
        .iter()
        .map(|r| r["selector"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 12);
    assert_eq!(
        rows.iter()
            .filter(|r| r["display_name"] == "Studio Mac")
            .count(),
        2
    );
    assert_eq!(
        data["duplicate_installation_groups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        rows.iter()
            .filter(|r| r["status"] == "connected")
            .all(|r| r["protected"] == true)
    );
}

#[test]
fn missing_access_stays_unknown_and_does_not_make_a_candidate() {
    let mut data = demo();
    data["pages"][0]["computers"][2]
        .as_object_mut()
        .unwrap()
        .remove("can_delete");
    let out = audit(&data, 0);
    assert_eq!(out["counts"]["cleanup_candidates"], 8);
    assert!(out["records"][2]["can_delete"].is_null());
    assert_eq!(out["records"][2]["protected"], true);
    assert_eq!(out["records"][1]["can_use_for_tasks"], false);
}

#[test]
fn unknown_status_and_record_kind_are_reported() {
    let mut data = demo();
    data["pages"][0]["computers"][2]["status"] = json!("future-state");
    data["pages"][0]["computers"][3]["kind"] = json!("future-kind");
    let out = audit(&data, 0);
    assert_eq!(out["counts"]["unknown_status"], 1);
    assert_eq!(out["counts"]["other"], 1);
    assert_eq!(out["counts"]["cleanup_candidates"], 7);
}

#[test]
fn incomplete_pagination_is_partial_and_backup_refuses_it() {
    let mut data = demo();
    data["pages"].as_array_mut().unwrap().pop();
    assert_eq!(audit(&data, 4)["complete"], false);
    let target = scratch().join("snapshot");
    let out = cmd()
        .arg("backup")
        .arg("--input")
        .arg(input(&data))
        .arg("--out")
        .arg(&target)
        .arg("--json")
        .output()
        .unwrap();
    json_output(&out, 3);
    assert!(!target.exists());
}

#[test]
fn broken_page_chain_or_thread_change_is_not_complete() {
    let mut data = demo();
    data["pages"][1]["cursor"] = json!("demo-wrong-page");
    assert_eq!(audit(&data, 4)["complete"], false);
    data = demo();
    data["pages"][1]["thread_id"] = json!("demo-other-thread");
    assert_eq!(audit(&data, 4)["complete"], false);
}

#[test]
fn missing_cursor_and_duplicate_ids_are_refused() {
    let mut data = demo();
    data["pages"][1]
        .as_object_mut()
        .unwrap()
        .remove("next_cursor");
    audit(&data, 3);
    data = demo();
    data["pages"][1]["computers"][0]["environment_id"] =
        data["pages"][0]["computers"][0]["environment_id"].clone();
    audit(&data, 3);
}

#[test]
fn live_or_credential_bearing_data_is_refused() {
    let mut data = demo();
    data["source"]["kind"] = json!("live");
    audit(&data, 3);
    data = demo();
    data["pages"][0]["computers"][0]["access_token"] = json!("invented-test-value");
    let out = audit(&data, 3);
    assert!(!out.to_string().contains("invented-test-value"));
}

#[test]
fn urls_live_flags_and_conflicting_sources_fail_before_file_access() {
    for args in [
        vec![
            "audit",
            "--input",
            "https://example.invalid/account",
            "--json",
        ],
        vec![
            "audit",
            "--demo",
            "--endpoint",
            "https://example.invalid",
            "--json",
        ],
        vec!["audit", "--demo", "--input", "/does-not-exist", "--json"],
    ] {
        let out = cmd().args(args).output().unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_ne!(err["error"]["kind"], "io");
    }
    json_output(&cmd().args(["rename", "--json"]).output().unwrap(), 2);
    json_output(&cmd().args(["cleanup", "--json"]).output().unwrap(), 2);
}

#[test]
fn malformed_or_oversized_input_is_refused() {
    let path = scratch().join("bad.json");
    for bytes in [b"{not json}".to_vec(), vec![b' '; 4 * 1024 * 1024 + 1]] {
        std::fs::write(&path, bytes).unwrap();
        json_output(
            &cmd()
                .arg("audit")
                .arg("--input")
                .arg(&path)
                .arg("--json")
                .output()
                .unwrap(),
            3,
        );
    }
}

#[cfg(unix)]
fn backup_at(path: &Path) -> Value {
    json_output(
        &cmd()
            .arg("backup")
            .arg("--demo")
            .arg("--out")
            .arg(path)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    )
}

#[cfg(unix)]
#[test]
fn backup_round_trip_preserves_bytes_and_unknown_fields_without_mutations() {
    let target = scratch().join("保存 snapshot");
    let receipt = backup_at(&target);
    assert_eq!(
        std::fs::read(target.join("catalog.json")).unwrap(),
        include_bytes!("../fixtures/synthetic-catalog.json")
    );
    let plan: Value =
        serde_json::from_slice(&std::fs::read(target.join("plan.json")).unwrap()).unwrap();
    assert_eq!(plan["actions"], json!([]));
    assert_eq!(
        plan["protected_environment_ids"].as_array().unwrap().len(),
        12
    );
    let out = cmd()
        .arg("verify")
        .arg("--backup")
        .arg(&target)
        .arg("--expected-manifest-sha256")
        .arg(receipt["manifest_sha256"].as_str().unwrap())
        .arg("--json")
        .output()
        .unwrap();
    let verified = json_output(&out, 0);
    assert_eq!(verified["integrity"], "verified");
    assert_eq!(verified["manifest_anchor"], "matched");
    assert_eq!(verified["live_state_verified"], false);
    assert_eq!(verified["restore_supported"], false);
    let unanchored = json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(unanchored["manifest_anchor"], "not_supplied");
}

#[cfg(unix)]
#[test]
fn backup_never_overwrites_and_checks_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let target = scratch().join("snapshot");
    backup_at(&target);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["catalog.json", "plan.json", "manifest.json"] {
        assert_eq!(
            std::fs::metadata(target.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let original = std::fs::read(target.join("catalog.json")).unwrap();
    json_output(
        &cmd()
            .arg("backup")
            .arg("--demo")
            .arg("--out")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
    assert_eq!(
        std::fs::read(target.join("catalog.json")).unwrap(),
        original
    );
    std::fs::set_permissions(
        target.join("catalog.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
}

#[cfg(unix)]
#[test]
fn corrupt_payload_manifest_and_untrusted_paths_fail() {
    let target = scratch().join("payload");
    backup_at(&target);
    std::fs::write(target.join("catalog.json"), b"{}").unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        5,
    );
    let target = scratch().join("manifest");
    let receipt = backup_at(&target);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(target.join("manifest.json")).unwrap()).unwrap();
    manifest["files"][0]["path"] = json!("../elsewhere.json");
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        5,
    );
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--expected-manifest-sha256")
            .arg(receipt["manifest_sha256"].as_str().unwrap())
            .arg("--json")
            .output()
            .unwrap(),
        5,
    );
}

#[cfg(unix)]
#[test]
fn symlinks_in_input_output_and_backup_members_are_refused() {
    use std::os::unix::fs::symlink;
    let root = scratch();
    let original = input(&demo());
    symlink(&original, root.join("input.json")).unwrap();
    json_output(
        &cmd()
            .arg("audit")
            .arg("--input")
            .arg(root.join("input.json"))
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
    let actual_parent = scratch();
    symlink(&actual_parent, root.join("parent")).unwrap();
    json_output(
        &cmd()
            .arg("backup")
            .arg("--demo")
            .arg("--out")
            .arg(root.join("parent/snapshot"))
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
    assert!(!actual_parent.join("snapshot").exists());
    let target = root.join("snapshot");
    backup_at(&target);
    std::fs::rename(target.join("plan.json"), target.join("saved-plan.json")).unwrap();
    symlink(target.join("saved-plan.json"), target.join("plan.json")).unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
}

#[test]
fn repeated_cursors_and_duplicate_json_fields_are_not_accepted_as_complete() {
    let mut data = demo();
    data["pages"][1]["next_cursor"] = json!("demo-page-2");
    data["pages"].as_array_mut().unwrap().push(
        json!({"cursor":"demo-page-2","next_cursor":null,"thread_id":"demo-thread","computers":[]}),
    );
    assert_eq!(audit(&data, 4)["complete"], false);
    let path = scratch().join("duplicate-field.json");
    let text = String::from_utf8(include_bytes!("../fixtures/synthetic-catalog.json").to_vec())
        .unwrap()
        .replacen(
            "\"schema_version\": 1",
            "\"schema_version\": 2, \"schema_version\": 1",
            1,
        );
    std::fs::write(&path, text).unwrap();
    json_output(
        &cmd()
            .arg("audit")
            .arg("--input")
            .arg(path)
            .arg("--json")
            .output()
            .unwrap(),
        3,
    );
}

#[test]
fn missing_option_values_are_usage_errors() {
    for args in [
        vec!["audit", "--input", "--json"],
        vec!["backup", "--demo", "--out", "--json"],
        vec!["verify", "--backup", "--json"],
    ] {
        json_output(&cmd().args(args).output().unwrap(), 2);
    }
}

#[cfg(unix)]
#[test]
fn rewritten_plan_cannot_authorize_mutation_even_with_updated_checksums() {
    use sha2::{Digest, Sha256};
    let target = scratch().join("snapshot");
    backup_at(&target);
    let mut plan: Value =
        serde_json::from_slice(&std::fs::read(target.join("plan.json")).unwrap()).unwrap();
    plan["actions"] = json!(["delete demo-retired-01"]);
    let plan = serde_json::to_vec(&plan).unwrap();
    std::fs::write(target.join("plan.json"), &plan).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(target.join("manifest.json")).unwrap()).unwrap();
    manifest["files"][1]["bytes"] = json!(plan.len());
    manifest["files"][1]["sha256"] = json!(
        Sha256::digest(&plan)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        5,
    );
}

#[test]
fn help_and_human_output_explain_scope_without_internal_option_syntax() {
    for args in [
        vec!["--help"],
        vec!["audit", "--help"],
        vec!["backup", "--help"],
        vec!["verify", "--help"],
    ] {
        let out = cmd().args(args).output().unwrap();
        assert!(out.status.success());
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(help.contains("synthetic workflows and Mac sign-in diagnostics"));
        assert!(help.contains("There is no live rename or cleanup"));
    }
    let out = cmd().args(["audit", "--demo"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("protected") && text.contains("candidate; review required"));
    assert!(!text.contains("Some(") && !text.contains("None"));
}

#[cfg(unix)]
#[test]
fn a_trusted_manifest_digest_detects_metadata_rewriting() {
    let target = scratch().join("snapshot");
    let receipt = backup_at(&target);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(target.join("manifest.json")).unwrap()).unwrap();
    manifest["created_at_unix_seconds"] = json!(1);
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--json")
            .output()
            .unwrap(),
        0,
    );
    json_output(
        &cmd()
            .arg("verify")
            .arg("--backup")
            .arg(&target)
            .arg("--expected-manifest-sha256")
            .arg(receipt["manifest_sha256"].as_str().unwrap())
            .arg("--json")
            .output()
            .unwrap(),
        5,
    );
}

#[test]
fn command_specific_arguments_refuse_conflicting_modes_before_file_access() {
    for args in [
        vec![
            "cleanup",
            "--apply",
            "missing",
            "--resume",
            "other",
            "--approve-plan-sha256",
            "bad",
            "--json",
        ],
        vec![
            "cleanup",
            "--demo",
            "--select",
            "demo-retired-01",
            "--out",
            "unused",
            "--approve-plan-sha256",
            "bad",
            "--json",
        ],
        vec![
            "verify",
            "--backup",
            "missing",
            "--operation",
            "other",
            "--json",
        ],
        vec![
            "verify",
            "--backup",
            "missing",
            "--expected-plan-sha256",
            "bad",
            "--json",
        ],
        vec![
            "verify",
            "--operation",
            "missing",
            "--expected-manifest-sha256",
            "bad",
            "--json",
        ],
        vec!["audit", "--demo", "--select", "demo-retired-01", "--json"],
        vec!["backup", "--demo", "--out", "unused", "--json", "--json"],
    ] {
        json_output(&cmd().args(args).output().unwrap(), 2);
    }
}

#[test]
fn flag_tokens_cannot_be_removed_from_a_missing_value_position() {
    json_output(
        &cmd()
            .args([
                "audit",
                "--input",
                "--json",
                "fixtures/synthetic-catalog.json",
            ])
            .output()
            .unwrap(),
        2,
    );
    let path = scratch().join("must-not-be-created");
    json_output(
        &cmd()
            .args(["backup", "--demo", "--out", "--json"])
            .arg(&path)
            .output()
            .unwrap(),
        2,
    );
    assert!(!path.exists());
}
