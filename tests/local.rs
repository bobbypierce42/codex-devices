#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn server(auth_reply: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/local-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).unwrap();
    let executable = root.join("fake-codex");
    let script = format!(
        r#"#!/bin/sh
log="$(dirname "$0")/requests.jsonl"
while IFS= read -r request; do
  printf '%s\n' "$request" >> "$log"
  case "$request" in
    *'"method":"initialize"'*) printf '%s\n' '{{"id":1,"result":{{"codexHome":"/invented/private-home"}}}}' ;;
    *'"method":"account/read"'*) printf '%s\n' '{{"id":2,"result":{{"account":{{"type":"chatgpt","email":"private@example.invalid"}}}}}}' ;;
    *'"method":"getAuthStatus"'*) printf '%s\n' '{auth_reply}' ;;
  esac
done
"#
    );
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

fn run(executable: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codex-devices"))
        .args(["audit", "--local", "--app-server"])
        .arg(executable)
        .arg("--json")
        .output()
        .unwrap()
}

fn receipt(output: &Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    for secret in [
        "private@example.invalid",
        "/invented/private-home",
        "demo-sensitive-value",
    ] {
        assert!(!text.contains(secret));
    }
    serde_json::from_slice(if code == 0 || code == 4 && !output.stdout.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap()
}

#[test]
fn local_audit_uses_only_approved_metadata_requests_and_filters_output() {
    let executable = server(r#"{"id":3,"result":{"authMethod":"chatgpt","authToken":null}}"#);
    let result = receipt(&run(&executable), 0);
    assert_eq!(result["mode"], "local-sign-in-preflight");
    assert_eq!(result["account_type"], "chatgpt");
    assert_eq!(result["auth_method"], "chatgpt");
    assert_eq!(result["token_requested"], false);
    assert_eq!(result["refresh_requested"], false);
    assert_eq!(result["device_requests"], 0);
    assert_eq!(result["mutating_requests"], 0);
    assert_eq!(result["device_inventory_verified"], false);
    let requests: Vec<Value> =
        std::fs::read_to_string(executable.parent().unwrap().join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["initialize", "initialized", "account/read", "getAuthStatus"]
    );
    assert_eq!(requests[2]["params"], json!({"refreshToken": false}));
    assert_eq!(
        requests[3]["params"],
        json!({"refreshToken": false, "includeToken": false})
    );
}

#[test]
fn unsupported_native_auth_status_stays_partial_without_server_message_disclosure() {
    let executable = server(r#"{"id":3,"error":{"code":-32601,"message":"demo-sensitive-value"}}"#);
    let result = receipt(&run(&executable), 4);
    assert_eq!(result["status"], "partial");
    assert_eq!(result["auth_status_supported"], false);
    assert_eq!(result["auth_status_error_code"], -32601);
}

#[test]
fn unexpected_token_is_refused_without_outputting_its_value() {
    let executable =
        server(r#"{"id":3,"result":{"authMethod":"chatgpt","authToken":"demo-sensitive-value"}}"#);
    let result = receipt(&run(&executable), 4);
    assert_eq!(result["error"]["kind"], "local-preflight");
}

#[test]
fn conflicting_local_modes_fail_before_starting_the_selected_executable() {
    let executable = server(r#"{"id":3,"result":{}}"#);
    for flags in [
        vec!["audit", "--local", "--demo"],
        vec!["audit", "--local", "--peer", "demo-mac"],
        vec!["audit", "--peer", "demo-mac", "--demo"],
        vec!["audit", "--local", "--input", "missing"],
        vec!["backup", "--local", "--demo", "--out", "unused"],
        vec!["audit", "--demo"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
            .args(flags)
            .arg("--app-server")
            .arg(&executable)
            .arg("--json")
            .output()
            .unwrap();
        receipt(&output, 2);
    }
    assert!(!executable.parent().unwrap().join("requests.jsonl").exists());
}

#[test]
fn invalid_peer_arguments_refuse_before_any_transport_is_started() {
    for (alias, path) in [
        ("-oProxyCommand=evil", "/invented/codex"),
        ("demo;evil", "/invented/codex"),
        ("demo-mac", "codex"),
        ("demo-mac", "/invented/../codex"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_codex-devices"))
            .args(["audit", "--peer", alias, "--app-server", path, "--json"])
            .output()
            .unwrap();
        let result = receipt(&output, 2);
        assert_eq!(result["error"]["kind"], "usage");
    }
}

#[test]
fn unsolicited_server_request_is_refused_without_answering_it() {
    let executable = server(
        r#"{"id":900,"method":"account/login/start","params":{"hint":"demo-sensitive-value"}}"#,
    );
    receipt(&run(&executable), 4);
    let requests =
        std::fs::read_to_string(executable.parent().unwrap().join("requests.jsonl")).unwrap();
    assert_eq!(requests.lines().count(), 4);
    assert!(!requests.contains("900"));
}

#[test]
fn oversized_native_output_is_refused_without_echoing_it() {
    let reply = serde_json::to_string(&json!({"id":3,"result":{
        "authMethod":"chatgpt","future":"x".repeat(66_000)}}))
    .unwrap();
    let executable = server(&reply);
    let result = receipt(&run(&executable), 4);
    assert_eq!(result["error"]["kind"], "local-preflight");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("size budget")
    );
}
