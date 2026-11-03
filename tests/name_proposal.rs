#![cfg(unix)]

use codex_devices::{prepare_name_proposal, verify_name_proposal};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/name-proposal-tests")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn snapshot() -> Value {
    let mut value: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    value["source"]["kind"] = json!("operator-snapshot");
    value
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

#[test]
fn user_app_connection_is_preserved_and_other_user_types_are_refused() {
    let root = scratch();
    let mut before = snapshot();
    before["source"]["dot_id"] = json!("demo-dot~demo-scope");
    before["pages"][0]["computers"][1]["kind"] = json!("user");
    before["pages"][0]["computers"][1]["computer_type"] = json!("app_connection");
    before["pages"][0]["inventory_metadata"] =
        json!({"local_computer_access_disabled_by_admin": false});
    let input = bytes(&before);
    let path = root.join("proposal");
    let receipt =
        prepare_name_proposal(&input, "demo-active-laptop", "Field laptop", &path).unwrap();
    assert_eq!(std::fs::read(path.join("before.json")).unwrap(), input);
    assert!(!receipt.apply_supported);
    let mut after = before.clone();
    after["pages"][0]["computers"][1]["display_name"] = json!("Field laptop");
    assert!(verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&after))).is_ok());
    after["pages"][0]["inventory_metadata"]["local_computer_access_disabled_by_admin"] =
        json!(true);
    assert!(verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&after))).is_err());
    for (i, computer_type) in [Value::Null, json!("unknown"), json!("cloud")]
        .into_iter()
        .enumerate()
    {
        let mut invalid = before.clone();
        invalid["pages"][0]["computers"][1]["computer_type"] = computer_type;
        let path = root.join(format!("refused-user-{i}"));
        assert!(
            prepare_name_proposal(
                &bytes(&invalid),
                "demo-active-laptop",
                "Field laptop",
                &path
            )
            .is_err()
        );
        assert!(!path.exists());
    }
}

#[test]
fn proposal_saves_exact_input_and_only_the_existing_id_and_name_in_its_request() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch();
    let path = root.join("proposal");
    let before = snapshot();
    let input = bytes(&before);
    let receipt =
        prepare_name_proposal(&input, "demo-active-laptop", " Field laptop ", &path).unwrap();
    assert!(!receipt.apply_supported);
    assert!(!receipt.live_state_verified);
    assert_eq!(receipt.status, "proposal-only");
    assert_eq!(std::fs::read(path.join("before.json")).unwrap(), input);
    let plan: Value =
        serde_json::from_slice(&std::fs::read(path.join("plan.json")).unwrap()).unwrap();
    let installation = &before["pages"][0]["computers"][1]["installation_id"];
    assert_eq!(plan["request"]["method"], "POST");
    assert_eq!(plan["request"]["path"], "/backend-api/flora/cca/executor");
    assert_eq!(
        plan["request"]["body"],
        json!({
            "installation_id": installation,
            "custom_display_name": "Field laptop"
        })
    );
    assert_eq!(plan["environment_id"], "demo-active-laptop");
    assert_eq!(
        plan["old_name"],
        before["pages"][0]["computers"][1]["display_name"]
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
        0
    );
    for name in ["before.json", "plan.json"] {
        assert_eq!(
            std::fs::metadata(path.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    assert!(verify_name_proposal(&path, &receipt.plan_sha256, None).is_ok());
    assert!(prepare_name_proposal(&input, "demo-active-laptop", "Other", &path).is_err());
}

#[test]
fn verification_requires_exact_name_and_preserves_all_other_fields_and_records() {
    let root = scratch();
    let path = root.join("proposal");
    let before = snapshot();
    let receipt =
        prepare_name_proposal(&bytes(&before), "demo-active-laptop", "Field laptop", &path)
            .unwrap();
    let mut after = before.clone();
    after["pages"][0]["computers"][1]["display_name"] = json!("Field laptop");
    let verified = verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&after))).unwrap();
    assert_eq!(verified.status, "capture-matches-proposal");
    assert!(!verified.live_state_verified);
    assert!(!verified.apply_supported);
    for field in [
        "installation_id",
        "environment_id",
        "can_delete",
        "status",
        "unknown_field",
    ] {
        let mut drift = after.clone();
        drift["pages"][0]["computers"][1][field] = json!("changed");
        assert!(
            verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&drift))).is_err(),
            "{field}"
        );
    }
    let mut drift = after.clone();
    drift["pages"][0]["computers"][0]["display_name"] = json!("Unselected change");
    assert!(verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&drift))).is_err());
    assert!(verify_name_proposal(&path, &receipt.plan_sha256, Some(&bytes(&before))).is_err());
    assert!(verify_name_proposal(&path, &"0".repeat(64), None).is_err());
}

#[test]
fn ambiguous_identity_incomplete_source_and_credentials_refuse_before_writing() {
    let root = scratch();
    let mut cases = Vec::new();
    let mut shared = snapshot();
    shared["pages"][0]["computers"][0]["installation_id"] =
        shared["pages"][0]["computers"][1]["installation_id"].clone();
    cases.push(shared);
    let mut incomplete = snapshot();
    incomplete["pages"][0]["next_cursor"] = json!("more");
    cases.push(incomplete);
    let mut credentials = snapshot();
    credentials["access_token"] = json!("invented-do-not-store");
    cases.push(credentials);
    let mut duplicate = snapshot();
    let row = duplicate["pages"][0]["computers"][1].clone();
    duplicate["pages"][0]["computers"]
        .as_array_mut()
        .unwrap()
        .push(row);
    cases.push(duplicate);
    let mut wrong_source = snapshot();
    wrong_source["source"]["kind"] = json!("synthetic");
    cases.push(wrong_source);
    for (i, value) in cases.into_iter().enumerate() {
        let path = root.join(format!("refused-{i}"));
        assert!(
            prepare_name_proposal(&bytes(&value), "demo-active-laptop", "Field laptop", &path)
                .is_err()
        );
        assert!(!path.exists());
    }
}

#[test]
fn cli_emits_a_blocked_proposal_without_exposing_the_installation_id() {
    let root = scratch();
    let input = root.join("input.json");
    std::fs::write(&input, bytes(&snapshot())).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .args(["rename", "--snapshot"])
        .arg(&input)
        .args(["--set", "demo-active-laptop=Field laptop", "--out"])
        .arg(root.join("proposal"))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "proposal-only");
    assert_eq!(result["apply_supported"], false);
    assert_eq!(result["live_state_verified"], false);
    assert!(result.get("installation_id").is_none());
    assert!(result.get("request").is_none());
}

#[test]
fn auth_token_fields_refuse_before_any_proposal_is_created() {
    let root = scratch();
    for (i, field) in ["authToken", "AUTH_TOKEN", "auth-token"]
        .into_iter()
        .enumerate()
    {
        for nested in [false, true] {
            let mut capture = snapshot();
            if nested {
                capture["extra"] = json!([{field: "invented-value"}]);
            } else {
                capture[field] = json!("invented-value");
            }
            let path = root.join(format!("token-{i}-{nested}"));
            assert!(
                prepare_name_proposal(
                    &bytes(&capture),
                    "demo-active-laptop",
                    "Field laptop",
                    &path
                )
                .is_err()
            );
            assert!(!path.exists());
        }
    }
}

#[test]
fn maximum_plan_keeps_a_bounded_anchor_receipt_for_prepare_and_verify() {
    let root = scratch();
    let capture = json!({
        "schema_version": 1,
        "source": {"kind":"operator-snapshot","account_id":"a","dot_id":"d"},
        "pages": [{"cursor":null,"thread_id":"t","next_cursor":null,"computers":[{
            "kind":"personal","environment_id":"e","installation_id":"i",
            "display_name":"x".repeat(4_193_733),"status":"connected"
        }]}]
    });
    let input = root.join("input.json");
    let proposal = root.join("proposal");
    std::fs::write(&input, bytes(&capture)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .args(["rename", "--snapshot"])
        .arg(&input)
        .args(["--set", "e=n", "--out"])
        .arg(&proposal)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() < 8192);
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["review_details_omitted"], json!(["old_name"]));
    assert_eq!(receipt["review_details_file"], "plan.json");
    assert!(receipt.get("old_name").is_none());
    assert_eq!(
        std::fs::metadata(proposal.join("plan.json")).unwrap().len(),
        codex_devices::MAX_BYTES
    );
    let verified = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .arg("verify")
        .arg("--name-proposal")
        .arg(&proposal)
        .arg("--expected-plan-sha256")
        .arg(receipt["plan_sha256"].as_str().unwrap())
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        verified.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["plan_sha256"], receipt["plan_sha256"]);
    assert_eq!(verified["review_details_omitted"], json!(["old_name"]));
}
