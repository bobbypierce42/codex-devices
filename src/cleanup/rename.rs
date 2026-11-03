use super::{Kind, Plan, RenameReceipt, View, prepare_operation, review_plan};
use crate::encoding::compact_json_bytes;
use crate::{Error, MAX_BYTES, Result, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RenameChange {
    pub environment_id: String,
    pub new_name: String,
}

pub(super) fn validated_name(name: &str) -> Result<String> {
    let has_controls = name.chars().any(char::is_control);
    let name = name.trim();
    if name.is_empty() || name.encode_utf16().count() > 100 || has_controls {
        return Err(Error::refused(
            "A name must have 1 through 100 UTF-16 code units after trimming, without control characters.",
        ));
    }
    Ok(name.into())
}

impl View {
    pub(super) fn renameable(&self, id: &str) -> bool {
        let Some(record) = self.records.get(id) else {
            return false;
        };
        let Some(installation) = record["installation_id"].as_str() else {
            return false;
        };
        record["kind"] == "personal"
            && record["can_rename"] == true
            && matches!(record["status"].as_str(), Some("connected" | "offline"))
            && self
                .records
                .values()
                .filter(|row| row["installation_id"].as_str() == Some(installation))
                .count()
                == 1
    }

    pub(super) fn renamed_bytes(&self, changes: &[RenameChange]) -> Result<Vec<u8>> {
        if changes.is_empty() {
            return self.persisted_bytes();
        }
        let mut size = self.persisted_bytes()?.len() as isize;
        for change in changes {
            let old = self
                .records
                .get(&change.environment_id)
                .ok_or_else(|| Error::integrity("A rename target disappeared."))?;
            let mut updated = old.clone();
            updated["display_name"] = Value::String(change.new_name.clone());
            size += compact_json_bytes(&updated)?.len() as isize
                - compact_json_bytes(old)?.len() as isize;
            if size > MAX_BYTES as isize {
                return Err(Error::refused(
                    "An intermediate renamed catalog would exceed the 4 MiB limit. No new request was issued.",
                ));
            }
        }
        let names: BTreeMap<_, _> = changes
            .iter()
            .map(|change| (change.environment_id.as_str(), change.new_name.as_str()))
            .collect();
        let mut raw = self.raw.clone();
        for page in raw["pages"].as_array_mut().expect("audited pages") {
            for row in page["computers"].as_array_mut().expect("audited records") {
                if let Some(name) = names.get(row["environment_id"].as_str().expect("audited ID")) {
                    row["display_name"] = Value::String((*name).into());
                }
            }
        }
        compact_json_bytes(&raw)
    }
}

pub(super) fn plan_for(view: &View, changes: &[RenameChange], backup_hash: String) -> Result<Plan> {
    let changes = changes
        .iter()
        .map(|change| {
            Ok(RenameChange {
                environment_id: change.environment_id.clone(),
                new_name: validated_name(&change.new_name)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let ids: Vec<_> = changes
        .iter()
        .map(|change| change.environment_id.clone())
        .collect();
    let mut plan = review_plan(view, &ids, backup_hash)?;
    for change in &changes {
        if !view.renameable(&change.environment_id) {
            return Err(Error::refused(
                "Rename requires a personal record, known connected/offline status, a unique installation ID, and explicit synthetic can_rename=true.",
            ));
        }
        if view.records[&change.environment_id]["display_name"].as_str() == Some(&change.new_name) {
            return Err(Error::refused(
                "A selected record already has its requested name.",
            ));
        }
    }
    plan.kind = Kind::Rename;
    plan.renames = changes;
    Ok(plan)
}

pub fn prepare_rename(
    bytes: &[u8],
    changes: &[RenameChange],
    directory: &Path,
) -> Result<RenameReceipt> {
    let baseline = View::parse(bytes)?;
    let plan = plan_for(&baseline, changes, sha256(b""))?;
    prepare_operation(bytes, baseline, plan, directory)
}
