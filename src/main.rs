#![forbid(unsafe_code)]

use codex_devices::{
    AuditReport, NameProposalReceipt, OperationReceipt, Result, apply_cleanup, apply_rename, audit,
    backup, json_bytes, prepare_cleanup, prepare_name_proposal, prepare_rename, read_input, verify,
    verify_name_proposal, verify_operation,
};
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use std::io::{self, Write};

mod cli;
mod local;
use cli::{Command, Invocation};

const HELP: &str = "codex-devices 0.1.0 | synthetic workflows and Mac sign-in diagnostics

  codex-devices audit  (--demo | --input FILE) [--json]
  codex-devices audit  --local [--app-server ABSOLUTE_EXECUTABLE] [--json]
  codex-devices audit  --peer SSH_ALIAS --app-server ABSOLUTE_REMOTE_EXECUTABLE [--json]
  codex-devices backup (--demo | --input FILE) --out NEW_DIRECTORY [--json]
  codex-devices verify --backup DIRECTORY [--expected-manifest-sha256 HEX] [--json]
  codex-devices cleanup (--demo | --input FILE) --select FULL_ID [--select FULL_ID ...]
                        --out NEW_OPERATION_DIRECTORY [--json]
  codex-devices cleanup (--apply | --resume) OPERATION --approve-plan-sha256 HEX [--json]
  codex-devices rename (--demo | --input FILE) --set FULL_ID=NAME [--set FULL_ID=NAME ...]
                       --out NEW_OPERATION_DIRECTORY [--json]
  codex-devices rename (--apply | --resume) OPERATION --approve-plan-sha256 HEX [--json]
  codex-devices verify --operation DIRECTORY [--expected-plan-sha256 HEX] [--json]
  codex-devices rename --snapshot FILE --set FULL_ID=NAME --out NEW_DIRECTORY [--json]
  codex-devices verify --name-proposal DIRECTORY --expected-plan-sha256 HEX [--input AFTER] [--json]

Catalog workflows accept synthetic schema-v1 files with demo- identifiers only.
Cleanup and rename preparation are preview-only. Apply writes the private service copy and
journal inside the operation; source input and backup stay unchanged. Review plan.json
and keep its digest separately. Resume reconciles interrupted intents first.
Rename preserves installation IDs and requires an invented can_rename grant.
Names are trimmed and limited to 100 UTF-16 code units, without control characters.
--snapshot prepares an offline name-only request proposal from an operator-snapshot
catalog. It saves the exact capture privately and has no live apply transport.
Proposal verification compares supplied files only; its partial/unknown exit is 4.
The optional macOS --local/--peer audit reads native sign-in metadata only.
--peer uses existing SSH configuration, keys and trusted host keys. It requires
an explicit remote app-server path. No discovery, key enrollment or installation.
Neither diagnostic requests tokens, authentication refresh, computer inventory,
login/logout, or account/device changes. There is no live rename or cleanup.
Backups and changes require private POSIX modes; Windows ACL support is pending.
SHA-256 checks integrity, not authenticity. Verification never checks live devices.

Exit codes: 0 success/preview, 1 I/O, 2 usage, 3 refused, 4 partial/unknown, 5 integrity.
";

fn output(value: &impl Serialize, human: &str, as_json: bool) -> Result<()> {
    let bytes = if as_json {
        json_bytes(value)?
    } else {
        human.as_bytes().to_vec()
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn name_proposal_output(mut receipt: NameProposalReceipt, as_json: bool) -> Result<i32> {
    let human = format!(
        "NAME-ONLY PROPOSAL | {}\nEnvironment: {:?}\nName: {:?} -> {:?}\nInstallation fingerprint: {}\nPrivate proposal: {:?}\nPlan SHA-256: {}\n{}",
        receipt.status,
        receipt.environment_id,
        receipt.old_name,
        receipt.new_name,
        receipt.installation_id_sha256,
        receipt.proposal_directory,
        receipt.plan_sha256,
        receipt.reason
    );
    match output(&receipt, &human, as_json) {
        Err(error) if as_json && error.kind == "refused" => {
            #[derive(Serialize)]
            struct Summary<'a> {
                #[serde(flatten)]
                receipt: &'a NameProposalReceipt,
                review_details_omitted: [&'static str; 1],
                review_details_file: &'static str,
            }
            receipt.old_name.clear();
            output(
                &Summary {
                    receipt: &receipt,
                    review_details_omitted: ["old_name"],
                    review_details_file: "plan.json",
                },
                "",
                true,
            )?;
        }
        result => result?,
    }
    Ok(4)
}

fn run(Invocation { command, json }: Invocation) -> Result<i32> {
    match command {
        Command::Audit(source) => audit_output(audit(&source.read()?)?, json),
        Command::AuditNative(target) => {
            let report = local::audit(target)?;
            let human = format!(
                "MAC SIGN-IN PREFLIGHT | {}\nTransport: {} | Peer: {:?}\nApp server: {:?}\nAccount type: {}\nAuthentication mode: {}\nMetadata only. No tokens, refresh, computer inventory or changes requested.",
                report.status,
                report.transport,
                report.peer.as_deref().unwrap_or("this Mac"),
                report.app_server,
                report.account_type,
                report.auth_method
            );
            output(&report, &human, json)?;
            Ok(report.exit_code())
        }
        Command::Backup { source, out } => {
            let receipt = backup(&source.read()?, &out)?;
            let human = format!(
                "Private synthetic backup created and read back: {:?}\nManifest SHA-256: {}\nKeep this digest separately for later verification. No changes are authorized.",
                receipt.backup_directory, receipt.manifest_sha256
            );
            output(&receipt, &human, json)?;
            Ok(0)
        }
        Command::PrepareCleanup {
            source,
            out,
            selected,
        } => operation_output(prepare_cleanup(&source.read()?, &selected, &out)?, json),
        Command::ApplyCleanup {
            directory,
            approved,
        } => operation_output(apply_cleanup(&directory, &approved)?, json),
        Command::PrepareRename {
            source,
            out,
            changes,
        } => operation_output(prepare_rename(&source.read()?, &changes, &out)?, json),
        Command::PrepareNameProposal {
            snapshot,
            out,
            change,
        } => name_proposal_output(
            prepare_name_proposal(
                &read_input(&snapshot)?,
                &change.environment_id,
                &change.new_name,
                &out,
            )?,
            json,
        ),
        Command::VerifyNameProposal {
            directory,
            expected,
            after,
        } => {
            let after = after.as_deref().map(read_input).transpose()?;
            name_proposal_output(
                verify_name_proposal(&directory, &expected, after.as_deref())?,
                json,
            )
        }
        Command::ApplyRename {
            directory,
            approved,
        } => operation_output(apply_rename(&directory, &approved)?, json),
        Command::VerifyOperation {
            directory,
            expected,
        } => operation_output(verify_operation(&directory, expected.as_deref())?, json),
        Command::VerifyBackup {
            directory,
            expected,
        } => {
            let receipt = verify(&directory, expected.as_deref())?;
            let human = format!(
                "Offline backup integrity verified: {} records.\nManifest SHA-256: {}\nTrusted manifest anchor: {}. No live state or restoration was verified.",
                receipt.record_count, receipt.manifest_sha256, receipt.manifest_anchor
            );
            output(&receipt, &human, json)?;
            Ok(0)
        }
    }
}

fn audit_output(report: AuditReport, as_json: bool) -> Result<i32> {
    let mut human = format!(
        "OFFLINE SYNTHETIC AUDIT | complete: {}\n{} personal / {} cloud / {} other\n{} connected / {} offline / {} unknown personal statuses\n{} informational cleanup candidates; no changes authorized.\n\nFull environment ID | name | status | synthetic rename grant | delete permission | cleanup handling\n",
        report.complete,
        report.counts.personal,
        report.counts.cloud,
        report.counts.other,
        report.counts.connected,
        report.counts.offline,
        report.counts.unknown_status,
        report.counts.cleanup_candidates
    );
    for row in &report.records {
        human.push_str(&format!(
            "{} | {:?} | {:?} | {} | {} | {}\n",
            row.selector,
            row.record.display_name.as_deref().unwrap_or("unknown"),
            row.record.status.as_deref().unwrap_or("unknown"),
            permission(row.record.can_rename),
            permission(row.record.can_delete),
            if row.protected {
                "protected"
            } else {
                "candidate; review required"
            }
        ));
    }
    for warning in &report.warnings {
        human.push_str(&format!("Warning: {warning}\n"));
    }
    output(&report, human.trim_end(), as_json)?;
    Ok(if report.complete { 0 } else { 4 })
}

fn permission(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "allowed",
        Some(false) => "denied",
        None => "unknown",
    }
}

#[derive(Serialize)]
struct OperationSummary<'a> {
    #[serde(flatten)]
    receipt: &'a OperationReceipt,
    review_details_omitted: [&'static str; 2],
    review_details_file: &'static str,
}

fn operation_output(mut receipt: OperationReceipt, as_json: bool) -> Result<i32> {
    let mut human = format!(
        "OFFLINE SYNTHETIC {} | {}\nOperation: {:?}\nPlan SHA-256: {}\n{} selected / {} protected / {} verified deleted / {} verified renamed\n",
        receipt.action.to_ascii_uppercase(),
        receipt.status,
        receipt.operation_directory,
        receipt.plan_sha256,
        receipt.selected_environment_ids.len(),
        receipt.protected_environment_ids.len(),
        receipt.verified_deleted_environment_ids.len(),
        receipt.verified_renamed_environment_ids.len()
    );
    for record in &receipt.selected_records {
        human.push_str(&format!(
            "{} | {:?} | {:?}\n",
            record["environment_id"].as_str().expect("audited ID"),
            record["display_name"],
            record["status"]
        ));
    }
    for change in &receipt.renames {
        human.push_str(&format!(
            "{} -> {:?}\n",
            change.environment_id, change.new_name
        ));
    }
    human.push_str(&receipt.reason);
    match output(&receipt, &human, as_json) {
        Err(error) if as_json && error.kind == "refused" => {
            // Retain every outcome/identity field when repeated review data exceeds the cap.
            receipt.selected_records.clear();
            receipt.renames.clear();
            output(
                &OperationSummary {
                    receipt: &receipt,
                    review_details_omitted: ["selected_records", "renames"],
                    review_details_file: "plan.json",
                },
                "",
                true,
            )?;
        }
        result => result?,
    }
    Ok(receipt.exit_code())
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!(
            "codex-devices {} (synthetic workflows; Mac metadata diagnostics)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }
    if (args.len() == 1 && ["--help", "-h"].iter().any(|flag| args[0] == *flag))
        || (args.len() == 2
            && args[1] == "--help"
            && args[0]
                .to_str()
                .is_some_and(|c| ["audit", "backup", "rename", "cleanup", "verify"].contains(&c)))
    {
        println!("{HELP}");
        return;
    }
    let as_json = args.iter().any(|a| a == "--json");
    match cli::parse(&args).and_then(run) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if as_json {
                eprintln!(
                    "{}",
                    json!({"schema_version":1,"error":{"kind":error.kind,"message":error.message}})
                );
            } else {
                eprintln!("{}: {}", error.kind, error.message);
            }
            std::process::exit(error.exit_code);
        }
    }
}
