#![forbid(unsafe_code)]

mod cleanup;
mod desktop;
mod encoding;
mod name_proposal;
mod storage;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use cleanup::{
    CleanupReceipt, OperationReceipt, RenameChange, RenameReceipt, apply_cleanup, apply_rename,
    prepare_cleanup, prepare_rename, verify_cleanup, verify_operation,
};
pub use desktop::desktop;
pub use encoding::json_bytes;
pub use name_proposal::{NameProposalReceipt, prepare_name_proposal, verify_name_proposal};
pub use storage::{BackupReceipt, Verification, backup, read_input, verify};

pub const DEMO: &[u8] = include_bytes!("../fixtures/synthetic-catalog.json");
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct Error {
    pub kind: &'static str,
    pub message: String,
    pub exit_code: i32,
}

impl Error {
    pub fn new(kind: &'static str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            kind,
            message: message.into(),
            exit_code,
        }
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new("refused", message, 3)
    }

    pub fn integrity(message: impl Into<String>) -> Self {
        Self::new("integrity", message, 5)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new("io", error.to_string(), 1)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Source {
    pub kind: String,
    pub account_id: String,
    pub dot_id: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum Cursor {
    End(()),
    Next(String),
}

#[derive(Deserialize)]
struct Catalog {
    schema_version: u32,
    source: Source,
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Page {
    cursor: Cursor,
    thread_id: String,
    next_cursor: Cursor,
    computers: Vec<Record>,
}

#[derive(Deserialize, Serialize)]
pub struct Record {
    pub kind: String,
    pub environment_id: String,
    pub installation_id: Option<String>,
    pub display_name: Option<String>,
    pub computer_type: Option<String>,
    pub status: Option<String>,
    pub is_attached: Option<bool>,
    pub can_use_for_tasks: Option<bool>,
    pub can_connect: Option<bool>,
    pub can_disconnect: Option<bool>,
    pub can_delete: Option<bool>,
    pub can_rename: Option<bool>,
    pub last_seen: Option<String>,
}

#[derive(Default, Serialize)]
pub struct Counts {
    pub total: usize,
    pub personal: usize,
    pub cloud: usize,
    pub other: usize,
    pub connected: usize,
    pub offline: usize,
    pub unknown_status: usize,
    pub cleanup_candidates: usize,
}

#[derive(Serialize)]
pub struct AuditRow {
    pub selector: String,
    pub fingerprint: String,
    #[serde(flatten)]
    pub record: Record,
    pub cleanup_candidate: bool,
    pub protected: bool,
}

#[derive(Serialize)]
pub struct AuditReport {
    pub schema_version: u32,
    pub operation: &'static str,
    pub mode: &'static str,
    pub source: Source,
    pub complete: bool,
    pub counts: Counts,
    pub records: Vec<AuditRow>,
    pub duplicate_name_groups: Vec<Vec<String>>,
    pub duplicate_installation_groups: Vec<Vec<String>>,
    pub warnings: Vec<String>,
    pub changes_authorized: bool,
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn demo_id(value: &str) -> Result<()> {
    if value.starts_with("demo-")
        && value.len() <= 160
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        Ok(())
    } else {
        Err(Error::refused(
            "Synthetic identifiers must use the demo- prefix and ASCII letters, numbers, dots, underscores, or hyphens.",
        ))
    }
}

fn reject_credentials(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let normalized: String = key
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                if [
                    "token",
                    "authtoken",
                    "accesstoken",
                    "refreshtoken",
                    "idtoken",
                    "apikey",
                    "password",
                    "secret",
                    "clientsecret",
                    "authorization",
                    "cookie",
                    "credentials",
                    "auth",
                ]
                .contains(&normalized.as_str())
                {
                    return Err(Error::refused(
                        "Credential-bearing fields are not allowed in catalog inputs.",
                    ));
                }
                reject_credentials(value)?;
            }
        }
        Value::Array(values) => {
            for item in values {
                reject_credentials(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn duplicate_groups(groups: BTreeMap<String, Vec<String>>) -> Vec<Vec<String>> {
    groups.into_values().filter(|ids| ids.len() > 1).collect()
}

pub fn audit(bytes: &[u8]) -> Result<AuditReport> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::refused("Catalog exceeds the 4 MiB limit."));
    }
    let raw: Value = serde_json::from_slice(bytes).map_err(|e| {
        Error::refused(format!(
            "Invalid catalog JSON at line {}, column {}.",
            e.line(),
            e.column()
        ))
    })?;
    reject_credentials(&raw)?;
    let catalog: Catalog = serde_json::from_slice(bytes).map_err(|_| Error::refused("Catalog does not match the synthetic schema; known fields must be unique and page cursors explicit strings or null."))?;
    if catalog.schema_version != 1 || catalog.source.kind != "synthetic" {
        return Err(Error::refused(
            "Only schema version 1 synthetic catalogs are supported. Live data is not enabled.",
        ));
    }
    demo_id(&catalog.source.account_id)?;
    demo_id(&catalog.source.dot_id)?;
    if catalog.pages.is_empty()
        || catalog
            .pages
            .iter()
            .map(|p| p.computers.len())
            .sum::<usize>()
            > 4096
    {
        return Err(Error::refused(
            "Catalog must have at least one page and at most 4096 records.",
        ));
    }

    let thread = &catalog.pages[0].thread_id;
    let mut expected = &Cursor::End(());
    let mut complete = true;
    let mut warnings = Vec::new();
    let mut seen_cursors = BTreeSet::new();
    for page in &catalog.pages {
        demo_id(&page.thread_id)?;
        if &page.cursor != expected || &page.thread_id != thread {
            complete = false;
        }
        if let Cursor::Next(cursor) = &page.cursor
            && !seen_cursors.insert(cursor)
        {
            complete = false;
        }
        if let Cursor::Next(cursor) = &page.next_cursor {
            demo_id(cursor)?;
        }
        expected = &page.next_cursor;
    }
    if expected != &Cursor::End(()) {
        complete = false;
    }
    if !complete {
        warnings.push("Pagination is incomplete or the cursor/thread chain does not match; no backup or changes are allowed.".into());
    }

    let mut ids = BTreeSet::new();
    let mut names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut installations: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut counts = Counts::default();
    let mut records = Vec::new();
    for record in catalog.pages.into_iter().flat_map(|page| page.computers) {
        demo_id(&record.environment_id)?;
        if !ids.insert(record.environment_id.clone()) {
            return Err(Error::refused(
                "Duplicate environment IDs make this catalog ambiguous.",
            ));
        }
        if let Some(installation) = &record.installation_id {
            demo_id(installation)?;
            installations
                .entry(installation.clone())
                .or_default()
                .push(record.environment_id.clone());
        }
        if let Some(name) = &record.display_name
            && !name.is_empty()
        {
            names
                .entry(name.clone())
                .or_default()
                .push(record.environment_id.clone());
        }
        let candidate = record.kind == "personal"
            && record.status.as_deref() == Some("offline")
            && record.is_attached == Some(false)
            && record.can_use_for_tasks == Some(false)
            && record.can_delete == Some(true)
            && record.installation_id.is_some();
        counts.total += 1;
        match record.kind.as_str() {
            "personal" => {
                counts.personal += 1;
                match record.status.as_deref() {
                    Some("connected") => counts.connected += 1,
                    Some("offline") => counts.offline += 1,
                    _ => counts.unknown_status += 1,
                }
            }
            "cloud" => counts.cloud += 1,
            _ => counts.other += 1,
        }
        if candidate {
            counts.cleanup_candidates += 1;
        }
        records.push(AuditRow {
            selector: record.environment_id.clone(),
            fingerprint: sha256(record.environment_id.as_bytes())[..12].into(),
            record,
            cleanup_candidate: candidate,
            protected: !candidate,
        });
    }
    let duplicate_name_groups = duplicate_groups(names);
    let duplicate_installation_groups = duplicate_groups(installations);
    if !duplicate_name_groups.is_empty() {
        warnings.push("Duplicate display names are present. Select records using full environment IDs, never names or list positions.".into());
    }
    if !duplicate_installation_groups.is_empty() {
        warnings.push("Some records share an installation ID. This is a diagnostic signal, not proof that a migration caused the overlap.".into());
    }
    if counts.unknown_status > 0 || counts.other > 0 {
        warnings.push("Unknown statuses or record kinds are preserved and protected.".into());
    }

    Ok(AuditReport {
        schema_version: 1,
        operation: "audit",
        mode: "offline-synthetic",
        source: catalog.source,
        complete,
        counts,
        records,
        duplicate_name_groups,
        duplicate_installation_groups,
        warnings,
        changes_authorized: false,
    })
}
