use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

fn invoke(input: &Value) -> (i32, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .arg("desktop")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    (
        result.status.code().unwrap(),
        serde_json::from_slice(&result.stdout).unwrap(),
    )
}
fn fixture() -> (Value, Value) {
    let root = json!({"items":[{"id":"dot-a","active_root_thread_id":"thread-a"},{"id":"dot-b","active_root_thread_id":"thread-b"}],"cursor":null});
    let record = json!({"environment_id":"env-a","installation_id":"install-a","kind":"user","computer_type":"app_connection","display_name":"Old","status":"connected","can_connect":true});
    let mut second = record.clone();
    second["can_connect"] = json!(false);
    let scope = |dot: &str, thread: &str, row: Value| json!({"dot_id":dot,"thread_id":thread,"computers_response_utf8":json!({"thread_id":thread,"computers":[row],"next_cursor":null}).to_string()});
    let capture = json!({"schema_version":1,"source":{"kind":"desktop-aggregate","account_id_sha256":"a".repeat(64)},"root_response_utf8":root.to_string(),"scopes":[scope("dot-a","thread-a",record),scope("dot-b","thread-b",second)]});
    let peers = json!([{"environment_id":"env-a","installation_id_sha256":codex_devices::sha256(b"install-a"),"host":"example.invalid","computer_name":"New"}]);
    (capture, peers)
}
fn path() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!(
            "desktop-tracer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .to_str()
        .unwrap()
        .into()
}
fn prepare(path: &str, capture: &Value, peers: &Value) -> (i32, Value) {
    invoke(
        &json!({"schema_version":1,"op":"prepare","operation_directory":path,"capture":capture,"selections":[{"environment_id":"env-a","new_name":"New"}],"peers":peers}),
    )
}
fn update_scope(capture: &mut Value, index: usize, f: impl FnOnce(&mut Value)) {
    let text = capture["scopes"][index]["computers_response_utf8"]
        .as_str()
        .unwrap();
    let mut value: Value = serde_json::from_str(text).unwrap();
    f(&mut value);
    capture["scopes"][index]["computers_response_utf8"] = json!(value.to_string());
}
#[test]
fn repeated_identity_in_two_scopes_is_one_logical_record() {
    let (capture, peers) = fixture();
    let p = path();
    let (code, receipt) = prepare(&p, &capture, &peers);
    assert_eq!(code, 0);
    assert_eq!(receipt["status"], "prepared");
    assert_eq!(receipt["records"].as_array().unwrap().len(), 1);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(format!("{p}/before.json")).unwrap()).unwrap();
    assert_eq!(saved, capture);
}
#[test]
fn collision_and_disagreement_refuse() {
    let (mut capture, peers) = fixture();
    update_scope(&mut capture, 1, |v| {
        v["computers"][0]["environment_id"] = json!("env-b")
    });
    assert_eq!(prepare(&path(), &capture, &peers).0, 3);
    let (mut capture, peers) = fixture();
    update_scope(&mut capture, 1, |v| {
        v["computers"][0]["display_name"] = json!("Different")
    });
    assert_eq!(prepare(&path(), &capture, &peers).0, 3);
}
#[test]
fn intent_is_single_attempt_and_all_occurrences_must_match() {
    let (mut capture, peers) = fixture();
    let p = path();
    let (_, prepared) = prepare(&p, &capture, &peers);
    let digest = &prepared["plan_sha256"];
    let request = |op: &str, capture: &Value| json!({"schema_version":1,"op":op,"operation_directory":p,"plan_sha256":digest,"environment_id":"env-a","capture":capture,"peers":peers});
    assert_eq!(invoke(&request("intent", &capture)).0, 0);
    assert_eq!(invoke(&request("intent", &capture)).0, 3);
    let reconcile = |capture: &Value| json!({"schema_version":1,"op":"reconcile","operation_directory":p,"plan_sha256":digest,"environment_id":"env-a","capture":capture});
    update_scope(&mut capture, 0, |v| {
        v["computers"][0]["display_name"] = json!("New")
    });
    assert_eq!(invoke(&reconcile(&capture)).0, 4);
    update_scope(&mut capture, 1, |v| {
        v["computers"][0]["display_name"] = json!("New")
    });
    assert_eq!(invoke(&reconcile(&capture)).0, 0);
    let rechecked = invoke(&reconcile(&capture));
    assert_eq!(rechecked.0, 0);
    assert_eq!(rechecked.1["status"], "verified");
    let mut drift = capture.clone();
    update_scope(&mut drift, 0, |v| {
        v["computers"][0]["can_connect"] = json!(false)
    });
    assert_ne!(invoke(&reconcile(&drift)).0, 0);
    assert_eq!(invoke(&request("intent", &capture)).0, 3);
    let status = invoke(
        &json!({"schema_version":1,"op":"status","operation_directory":p,"plan_sha256":digest}),
    );
    assert_eq!(status.1["status"], "verified");
}

#[test]
fn truncated_journal_never_reauthorizes_intent() {
    let (capture, peers) = fixture();
    let p = path();
    let (_, prepared) = prepare(&p, &capture, &peers);
    let digest = &prepared["plan_sha256"];
    let request = json!({"schema_version":1,"op":"intent","operation_directory":p,"plan_sha256":digest,"environment_id":"env-a","capture":capture,"peers":peers});
    assert_eq!(invoke(&request).0, 0);
    std::fs::write(format!("{p}/journal.jsonl"), b"").unwrap();
    assert_eq!(invoke(&request).0, 3);
    let status = invoke(
        &json!({"schema_version":1,"op":"status","operation_directory":p,"plan_sha256":digest}),
    );
    assert_eq!(status.0, 4);
    assert_eq!(status.1["status"], "uncertain");
}

#[test]
fn drift_credentials_and_tampered_backup_are_refused() {
    let (mut capture, peers) = fixture();
    update_scope(&mut capture, 0, |v| {
        v["computers"][0]["access_token"] = json!("invented")
    });
    assert_eq!(prepare(&path(), &capture, &peers).0, 3);
    let (capture, peers) = fixture();
    let p = path();
    let (_, prepared) = prepare(&p, &capture, &peers);
    let digest = &prepared["plan_sha256"];
    let mut changed = capture.clone();
    changed["source"]["account_id_sha256"] = json!("b".repeat(64));
    assert_eq!(invoke(&json!({"schema_version":1,"op":"preflight","operation_directory":p,"plan_sha256":digest,"capture":changed,"peers":peers})).0,3);
    std::fs::write(format!("{p}/before.json"), b"{}").unwrap();
    assert_eq!(
        invoke(
            &json!({"schema_version":1,"op":"status","operation_directory":p,"plan_sha256":digest})
        )
        .0,
        5
    );
}

#[test]
fn heartbeat_is_ignored_but_grants_and_thread_drift_are_not() {
    let (mut capture, peers) = fixture();
    let p = path();
    let (_, prepared) = prepare(&p, &capture, &peers);
    let digest = &prepared["plan_sha256"];
    let mut root: Value =
        serde_json::from_str(capture["root_response_utf8"].as_str().unwrap()).unwrap();
    root["items"][0]["heartbeat"] = json!("later");
    capture["root_response_utf8"] = json!(root.to_string());
    let preflight = |capture: &Value| json!({"schema_version":1,"op":"preflight","operation_directory":p,"plan_sha256":digest,"capture":capture,"peers":peers});
    assert_eq!(invoke(&preflight(&capture)).0, 0);
    let mut changed = capture.clone();
    update_scope(&mut changed, 0, |v| {
        v["computers"][0]["can_connect"] = json!(false)
    });
    assert_eq!(invoke(&preflight(&changed)).0, 3);
    root["items"][0]["active_root_thread_id"] = json!("thread-other");
    capture["root_response_utf8"] = json!(root.to_string());
    assert_eq!(invoke(&preflight(&capture)).0, 3);
}

#[test]
fn selected_devices_advance_serially_and_preserve_unselected_records() {
    let (mut capture, mut peers) = fixture();
    for i in 0..2 {
        update_scope(&mut capture, i, |v| {
            let mut second = v["computers"][0].clone();
            second["environment_id"] = json!("env-b");
            second["installation_id"] = json!("install-b");
            second["display_name"] = json!("Old B");
            v["computers"].as_array_mut().unwrap().push(second);
        });
    }
    peers.as_array_mut().unwrap().push(json!({"environment_id":"env-b","installation_id_sha256":codex_devices::sha256(b"install-b"),"host":"peer.invalid","computer_name":"New B"}));
    let p = path();
    let (_, prepared) = invoke(
        &json!({"schema_version":1,"op":"prepare","operation_directory":p,"capture":capture,"selections":[{"environment_id":"env-a","new_name":"New"},{"environment_id":"env-b","new_name":"New B"}],"peers":peers}),
    );
    let digest = &prepared["plan_sha256"];
    let intent = |id: &str, capture: &Value| json!({"schema_version":1,"op":"intent","operation_directory":p,"plan_sha256":digest,"environment_id":id,"capture":capture,"peers":peers});
    let reconcile = |id: &str, capture: &Value| json!({"schema_version":1,"op":"reconcile","operation_directory":p,"plan_sha256":digest,"environment_id":id,"capture":capture});
    assert_eq!(invoke(&intent("env-b", &capture)).0, 3);
    assert_eq!(invoke(&intent("env-a", &capture)).0, 0);
    for i in 0..2 {
        update_scope(&mut capture, i, |v| {
            v["computers"][0]["display_name"] = json!("New")
        });
    }
    let mut drift = capture.clone();
    update_scope(&mut drift, 0, |v| {
        v["computers"][1]["can_connect"] = json!("changed")
    });
    assert_eq!(invoke(&reconcile("env-a", &drift)).0, 4);
    assert_eq!(invoke(&reconcile("env-a", &capture)).0, 0);
    assert_eq!(invoke(&intent("env-b", &capture)).0, 0);
    for i in 0..2 {
        update_scope(&mut capture, i, |v| {
            v["computers"][1]["display_name"] = json!("New B")
        });
    }
    assert_eq!(invoke(&reconcile("env-b", &capture)).0, 0);
    assert_eq!(invoke(&reconcile("env-a", &capture)).0, 0);
    update_scope(&mut capture, 0, |v| {
        v["computers"][1]["display_name"] = json!("Old B")
    });
    assert_ne!(invoke(&reconcile("env-a", &capture)).0, 0);
}

#[cfg(unix)]
#[test]
fn desktop_rejects_nonprivate_directory_and_symlinked_journal() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (capture, peers) = fixture();
    let p = path();
    let (_, prepared) = prepare(&p, &capture, &peers);
    let status = json!({"schema_version":1,"op":"status","operation_directory":p,"plan_sha256":prepared["plan_sha256"]});
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(invoke(&status).0, 3);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    let journal = format!("{p}/journal.jsonl");
    let saved = format!("{p}-saved-journal");
    std::fs::rename(&journal, &saved).unwrap();
    symlink(&saved, &journal).unwrap();
    assert_eq!(invoke(&status).0, 3);
}

#[cfg(unix)]
#[test]
fn desktop_reports_failed_stdout_as_io_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .arg("desktop")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
}
