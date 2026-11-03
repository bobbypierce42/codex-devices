use crate::cleanup::validated_name;
use crate::storage::directory::Directory;
use crate::{Catalog, Cursor, Error, MAX_BYTES, Record, Result, Source, json_bytes, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

struct Snapshot {
    raw: Value,
    catalog: Catalog,
}

fn identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return Err(Error::refused(
            "Snapshot identifiers must be bounded opaque ASCII identifiers.",
        ));
    }
    Ok(())
}

impl Snapshot {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::refused("Snapshot exceeds the 4 MiB limit."));
        }
        let raw: Value =
            serde_json::from_slice(bytes).map_err(|_| Error::refused("Invalid snapshot JSON."))?;
        crate::reject_credentials(&raw)?;
        let catalog: Catalog = serde_json::from_slice(bytes).map_err(|_| {
            Error::refused(
                "Snapshot must use the paginated catalog envelope with unique known fields.",
            )
        })?;
        if catalog.schema_version != 1 || catalog.source.kind != "operator-snapshot" {
            return Err(Error::refused(
                "Name proposals require schema 1 and source.kind=operator-snapshot; this does not authenticate the capture.",
            ));
        }
        identifier(&catalog.source.account_id)?;
        identifier(&catalog.source.dot_id)?;
        if catalog.pages.is_empty() || catalog.pages.len() > 4096 {
            return Err(Error::refused(
                "Snapshot must contain 1 through 4096 explicit pages.",
            ));
        }
        let thread = &catalog.pages[0].thread_id;
        identifier(thread)?;
        let mut expected = None;
        let mut cursors = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for (index, page) in catalog.pages.iter().enumerate() {
            let current = match &page.cursor {
                Cursor::End(()) => None,
                Cursor::Next(s) => Some(s.as_str()),
            };
            if current != expected || (index > 0 && current.is_none()) || &page.thread_id != thread
            {
                return Err(Error::refused(
                    "Snapshot page continuity or thread binding is incomplete.",
                ));
            }
            for record in &page.computers {
                identifier(&record.environment_id)?;
                if !ids.insert(&record.environment_id) || ids.len() > 4096 {
                    return Err(Error::refused(
                        "Snapshot IDs must be unique, with at most 4096 records.",
                    ));
                }
                if let Some(id) = &record.installation_id {
                    identifier(id)?;
                }
            }
            expected = match &page.next_cursor {
                Cursor::End(()) => None,
                Cursor::Next(cursor) => {
                    if cursor.is_empty() || cursor.len() > 4096 || !cursors.insert(cursor) {
                        return Err(Error::refused(
                            "Snapshot pagination repeats or has an invalid cursor.",
                        ));
                    }
                    Some(cursor.as_str())
                }
            };
        }
        if expected.is_some() {
            return Err(Error::refused("Snapshot has an uncollected next page."));
        }
        Ok(Self { raw, catalog })
    }

    fn selected(&self, id: &str) -> Result<&Record> {
        let records = || self.catalog.pages.iter().flat_map(|p| &p.computers);
        let record = records()
            .find(|r| r.environment_id == id)
            .ok_or_else(|| Error::refused("The full selected environment ID is absent."))?;
        let installation = record.installation_id.as_deref().ok_or_else(|| {
            Error::refused("A name proposal requires an existing installation ID.")
        })?;
        if record.kind != "personal"
            || !matches!(record.status.as_deref(), Some("connected" | "offline"))
            || record.display_name.as_ref().is_none_or(|s| s.is_empty())
            || records()
                .filter(|r| r.installation_id.as_deref() == Some(installation))
                .count()
                != 1
        {
            return Err(Error::refused(
                "A name proposal requires a personal record, known name/status, and unambiguous installation binding.",
            ));
        }
        Ok(record)
    }

    fn renamed(&self, id: &str, name: &str) -> Value {
        let mut after = self.raw.clone();
        for page in after["pages"].as_array_mut().expect("validated pages") {
            for record in page["computers"].as_array_mut().expect("validated records") {
                if record["environment_id"].as_str() == Some(id) {
                    record["display_name"] = Value::String(name.into());
                }
            }
        }
        after
    }
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Body {
    installation_id: String,
    custom_display_name: String,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Request {
    method: String,
    path: String,
    body: Body,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    mode: String,
    source: Source,
    snapshot_sha256: String,
    environment_id: String,
    old_name: String,
    new_name: String,
    request: Request,
    apply_supported: bool,
    source_authenticated: bool,
    live_capability_verified: bool,
}

fn plan_for(bytes: &[u8], id: &str, name: &str) -> Result<Plan> {
    let snapshot = Snapshot::parse(bytes)?;
    let record = snapshot.selected(id)?;
    let name = validated_name(name)?;
    if record.display_name.as_deref() == Some(&name) {
        return Err(Error::refused(
            "The selected record already has the requested name.",
        ));
    }
    Ok(Plan {
        schema_version: 1,
        mode: "name-only-proposal".into(),
        source: snapshot.catalog.source.clone(),
        snapshot_sha256: sha256(bytes),
        environment_id: id.into(),
        old_name: record.display_name.clone().expect("validated name"),
        new_name: name.clone(),
        request: Request {
            method: "POST".into(),
            path: "/backend-api/flora/cca/executor".into(),
            body: Body {
                installation_id: record
                    .installation_id
                    .clone()
                    .expect("validated installation"),
                custom_display_name: name,
            },
        },
        apply_supported: false,
        source_authenticated: false,
        live_capability_verified: false,
    })
}

#[derive(Serialize)]
pub struct NameProposalReceipt {
    pub schema_version: u32,
    pub mode: &'static str,
    pub status: &'static str,
    pub proposal_directory: String,
    pub plan_sha256: String,
    pub environment_id: String,
    pub installation_id_sha256: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub old_name: String,
    pub new_name: String,
    pub apply_supported: bool,
    pub live_state_verified: bool,
    pub reason: &'static str,
}

/// Saves an offline request proposal and exact operator-supplied capture. Sends no request.
pub fn prepare_name_proposal(
    bytes: &[u8],
    id: &str,
    name: &str,
    path: &Path,
) -> Result<NameProposalReceipt> {
    let plan = json_bytes(&plan_for(bytes, id, name)?)?;
    let directory = Directory::create(path)?;
    directory.write_new("before.json", bytes)?;
    // The immutable plan is the completion marker and binds the exact backup bytes.
    directory.write_new("plan.json", &plan)?;
    directory.sync()?;
    verify_name_proposal(path, &sha256(&plan), None)
}

/// Checks anchored artifacts and optionally compares an explicit after-capture, without network access.
pub fn verify_name_proposal(
    path: &Path,
    expected: &str,
    after: Option<&[u8]>,
) -> Result<NameProposalReceipt> {
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::new(
            "usage",
            "Name proposal verification requires its trusted 64-digit plan SHA-256.",
            2,
        ));
    }
    let directory = Directory::open(path)?;
    directory.check_members(&["before.json", "plan.json"], None)?;
    let bytes = directory.read("before.json")?;
    let saved = directory.read("plan.json")?;
    let digest = sha256(&saved);
    if !digest.eq_ignore_ascii_case(expected) {
        return Err(Error::integrity(
            "Name proposal does not match the trusted plan digest.",
        ));
    }
    let plan: Plan = serde_json::from_slice(&saved)
        .map_err(|_| Error::integrity("Invalid name-only proposal."))?;
    let rebuilt = plan_for(&bytes, &plan.environment_id, &plan.new_name)?;
    if plan != rebuilt {
        return Err(Error::integrity(
            "Name proposal is not the exact request derived from its backup.",
        ));
    }
    if let Some(after) = after {
        let before = Snapshot::parse(&bytes)?;
        let after = Snapshot::parse(after)?;
        if after.raw != before.renamed(&plan.environment_id, &plan.new_name) {
            return Err(Error::integrity(
                "Capture differs beyond the exact planned display name; IDs, membership, grants and other metadata must stay unchanged.",
            ));
        }
    }
    Ok(NameProposalReceipt {
        schema_version: 1,
        mode: "name-only-proposal",
        status: if after.is_some() {
            "capture-matches-proposal"
        } else {
            "proposal-only"
        },
        proposal_directory: path.to_string_lossy().into_owned(),
        plan_sha256: digest,
        environment_id: plan.environment_id,
        installation_id_sha256: sha256(plan.request.body.installation_id.as_bytes()),
        old_name: plan.old_name,
        new_name: plan.new_name,
        apply_supported: false,
        live_state_verified: false,
        reason: "Operator captures and the observed internal name interface are unqualified. No request was sent; live account transport, peer binding and capability verification are required before any apply.",
    })
}
