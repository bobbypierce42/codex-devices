use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

const FORMATTING: [char; 18] = [
    '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}',
    '\u{2069}', '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{2028}', '\u{2029}',
    '\u{2060}', '\u{feff}',
];

fn scratch() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/security-tests")
        .join(format!("{}-{stamp}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn label() -> String {
    format!("alpha{}omega", FORMATTING.iter().collect::<String>())
}

fn escaped_review(output: &[u8]) {
    let text = std::str::from_utf8(output).unwrap();
    for character in FORMATTING {
        assert!(!text.contains(character));
        assert!(text.contains(&format!("\\u{{{:x}}}", character as u32)));
    }
}

#[test]
fn audit_visibly_escapes_directional_and_invisible_formatting() {
    let root = scratch();
    let input = root.join("input.json");
    let mut catalog: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    let row = &mut catalog["pages"][0]["computers"][0];
    row["display_name"] = json!(label());
    row["status"] = json!(label());
    std::fs::write(&input, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .args(["audit", "--input"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(output.status.success());
    escaped_review(&output.stdout);
}

#[cfg(unix)]
#[test]
fn rename_review_escapes_old_and_new_labels_without_changing_stored_names() {
    let root = scratch();
    let input = root.join("input.json");
    let operation = root.join("operation");
    let mut catalog: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    let row = &mut catalog["pages"][0]["computers"][0];
    row["can_rename"] = json!(true);
    row["display_name"] = json!(label());
    let new_name = label() + "-new";
    let mapping = format!("{}={new_name}", row["environment_id"].as_str().unwrap());
    std::fs::write(&input, serde_json::to_vec(&catalog).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .args(["rename", "--input"])
        .arg(&input)
        .arg("--out")
        .arg(&operation)
        .arg("--set")
        .arg(mapping)
        .output()
        .unwrap();
    assert!(output.status.success());
    escaped_review(&output.stdout);
    let plan: Value =
        serde_json::from_slice(&std::fs::read(operation.join("plan.json")).unwrap()).unwrap();
    assert_eq!(plan["renames"][0]["new_name"], new_name);
    assert_eq!(plan["selected_records"][0]["display_name"], label());
}

#[cfg(unix)]
#[test]
fn expanded_selected_metadata_is_refused_before_artifact_creation() {
    let root = scratch();
    let operation = root.join("operation");
    let mut catalog: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
    let mut nested = json!(vec![0_u8; 50_000]);
    for _ in 0..50 {
        nested = json!([nested]);
    }
    let row = catalog["pages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .flat_map(|page| page["computers"].as_array_mut().unwrap())
        .find(|row| row["environment_id"] == "demo-retired-01")
        .unwrap();
    row["future_metadata"] = nested;
    let bytes = serde_json::to_vec(&catalog).unwrap();
    assert!(bytes.len() < codex_devices::MAX_BYTES as usize);
    let Err(error) =
        codex_devices::prepare_cleanup(&bytes, &["demo-retired-01".into()], &operation)
    else {
        panic!("Expanded plan must be refused");
    };
    assert_eq!(error.kind, "refused");
    assert!(!operation.exists());
}

#[cfg(unix)]
fn large_operation_receipts_keep_the_outcome(action: &str) {
    for json_preview in [false, true] {
        let root = scratch();
        let input = root.join("input.json");
        let operation = root.join("operation");
        let mut catalog: Value = serde_json::from_slice(codex_devices::DEMO).unwrap();
        let mut row = catalog["pages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|page| page["computers"].as_array().unwrap())
            .find(|row| row["environment_id"] == "demo-retired-01")
            .unwrap()
            .clone();
        row["future_metadata"] = json!("x".repeat(4_193_100));
        row["can_rename"] = json!(true);
        catalog["pages"].as_array_mut().unwrap().truncate(1);
        catalog["pages"][0]["computers"] = json!([row]);
        catalog["pages"][0]["next_cursor"] = Value::Null;
        let original = serde_json::to_vec(&catalog).unwrap();
        assert!(original.len() < codex_devices::MAX_BYTES as usize);
        std::fs::write(&input, &original).unwrap();

        let mut preview = Command::new(env!("CARGO_BIN_EXE_codex-devices"));
        preview.args([action, "--input"]).arg(&input);
        preview.arg("--out").arg(&operation);
        if action == "cleanup" {
            preview.args(["--select", "demo-retired-01"]);
        } else {
            preview.args(["--set", "demo-retired-01=Renamed synthetic Mac"]);
        }
        if json_preview {
            preview.arg("--json");
        }
        let output = preview.output().unwrap();
        assert!(output.status.success(), "{action} preview: {:?}", output);
        let plan = std::fs::read(operation.join("plan.json")).unwrap();
        assert!(plan.len() < codex_devices::MAX_BYTES as usize);
        let digest = codex_devices::sha256(&plan);
        if json_preview {
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(receipt["status"], "review_required");
            assert_eq!(receipt["plan_sha256"], digest);
        }

        for mode in ["--apply", "--resume"] {
            let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
                .args([action, mode])
                .arg(&operation)
                .args(["--approve-plan-sha256", &digest, "--json"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{action} {mode}: {:?}", output);
            assert!(output.stderr.is_empty());
            assert!(output.stdout.len() <= codex_devices::MAX_BYTES as usize + 1);
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(receipt["status"], "complete");
            assert_eq!(receipt["plan_sha256"], digest);
            assert_eq!(
                receipt["selected_environment_ids"],
                json!(["demo-retired-01"])
            );
            assert_eq!(receipt["selected_records"], json!([]));
            assert_eq!(
                receipt["review_details_omitted"],
                json!(["selected_records", "renames"])
            );
            assert_eq!(receipt["review_details_file"], "plan.json");
            let effect = if action == "cleanup" {
                "verified_deleted_environment_ids"
            } else {
                "verified_renamed_environment_ids"
            };
            assert_eq!(receipt[effect], json!(["demo-retired-01"]));
        }

        let journal_before = std::fs::read(operation.join("journal.jsonl")).unwrap();
        let service_before = std::fs::read(operation.join("service.json")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
            .args(["verify", "--operation"])
            .arg(&operation)
            .args(["--expected-plan-sha256", &digest, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{action} verify: {:?}", output);
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(receipt["status"], "complete");
        assert_eq!(
            std::fs::read(operation.join("journal.jsonl")).unwrap(),
            journal_before
        );
        assert_eq!(
            std::fs::read(operation.join("service.json")).unwrap(),
            service_before
        );
        assert_eq!(std::fs::read(&input).unwrap(), original);
        assert_eq!(
            std::fs::read(operation.join("backup/catalog.json")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(operation.join("plan.json")).unwrap(), plan);
    }
}

#[cfg(unix)]
#[test]
fn large_cleanup_json_receipts_keep_completed_effects_and_resume_status() {
    large_operation_receipts_keep_the_outcome("cleanup");
}

#[cfg(unix)]
#[test]
fn large_rename_json_receipts_keep_completed_effects_and_resume_status() {
    large_operation_receipts_keep_the_outcome("rename");
}
