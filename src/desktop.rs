use crate::cleanup::validated_name;
use crate::encoding::compact_json_bytes;
use crate::storage::directory::Directory;
use crate::{Error, MAX_BYTES, Result, reject_credentials, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Source {
    kind: String,
    account_id_sha256: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    dot_id: String,
    thread_id: String,
    computers_response_utf8: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    schema_version: u32,
    source: Source,
    root_response_utf8: String,
    scopes: Vec<Scope>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Selection {
    environment_id: String,
    new_name: String,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Peer {
    environment_id: String,
    installation_id_sha256: String,
    host: String,
    computer_name: String,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
enum EvidenceMode {
    #[serde(rename = "registry-label")]
    RegistryLabel,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    op: String,
    operation_directory: String,
    plan_sha256: Option<String>,
    capture: Option<Capture>,
    selections: Option<Vec<Selection>>,
    peers: Option<Vec<Peer>>,
    environment_id: Option<String>,
    evidence_mode: Option<EvidenceMode>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Row {
    environment_id: String,
    installation_id_sha256: String,
    old_name: String,
    new_name: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    before_sha256: String,
    selections: Vec<Selection>,
    peers: Vec<Peer>,
    records: Vec<Row>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_mode: Option<EvidenceMode>,
}
#[derive(Serialize)]
struct ReceiptRow<'a> {
    #[serde(flatten)]
    row: &'a Row,
    state: &'static str,
}
#[derive(Serialize)]
struct Receipt<'a> {
    schema_version: u32,
    op: &'a str,
    status: &'a str,
    operation_directory: &'a str,
    plan_sha256: &'a str,
    records: Vec<ReceiptRow<'a>>,
    reason_code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_mode: Option<EvidenceMode>,
}
type GlobalRow = (
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);
struct Aggregate {
    account: String,
    mapping: BTreeMap<String, String>,
    scopes: BTreeMap<String, Value>,
    rows: BTreeMap<String, GlobalRow>,
}
fn refused() -> Error {
    Error::refused("Desktop request refused.")
}
fn integrity() -> Error {
    Error::integrity("Desktop integrity check failed.")
}
fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:~".contains(&b))
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
struct StrictValue(Value);
impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate field"));
                    }
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn parse_raw(s: &str) -> Result<Value> {
    let v = serde_json::from_str::<StrictValue>(s)
        .map_err(|_| refused())?
        .0;
    reject_credentials(&v)?;
    Ok(v)
}
fn string(v: &Value, key: &str) -> Result<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| ident(s))
        .map(str::to_owned)
        .ok_or_else(refused)
}
fn nullable_string(v: &Value, key: &str) -> Result<Option<String>> {
    match v.get(key) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        None | Some(Value::Null) => Ok(None),
        _ => Err(refused()),
    }
}
impl Aggregate {
    fn parse(c: &Capture) -> Result<Self> {
        if c.schema_version != 1
            || c.source.kind != "desktop-aggregate"
            || !hash(&c.source.account_id_sha256)
            || c.scopes.len() > 4096
        {
            return Err(refused());
        }
        let root = parse_raw(&c.root_response_utf8)?;
        if root.get("cursor") != Some(&Value::Null) {
            return Err(refused());
        }
        let items = root
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(refused)?;
        if items.len() > 4096 || items.len() != c.scopes.len() {
            return Err(refused());
        }
        let mut mapping = BTreeMap::new();
        for item in items {
            let id = string(item, "id")?;
            let thread = string(item, "active_root_thread_id")?;
            if mapping.insert(id, thread).is_some() {
                return Err(refused());
            }
        }
        let mut scopes = BTreeMap::new();
        let mut rows = BTreeMap::new();
        for scope in &c.scopes {
            if mapping.get(&scope.dot_id) != Some(&scope.thread_id) {
                return Err(refused());
            }
            let v = parse_raw(&scope.computers_response_utf8)?;
            if v.get("thread_id").and_then(Value::as_str) != Some(&scope.thread_id)
                || v.get("next_cursor") != Some(&Value::Null)
            {
                return Err(refused());
            }
            let computers = v
                .get("computers")
                .and_then(Value::as_array)
                .ok_or_else(refused)?;
            let mut seen = BTreeSet::new();
            for row in computers {
                let id = string(row, "environment_id")?;
                if !seen.insert(id.clone()) {
                    return Err(refused());
                }
                let installation = nullable_string(row, "installation_id")?;
                if installation.as_ref().is_some_and(|s| !ident(s)) {
                    return Err(refused());
                }
                let kind = string(row, "kind")?;
                let typ = nullable_string(row, "computer_type")?;
                let name = nullable_string(row, "display_name")?;
                let status = nullable_string(row, "status")?;
                let tuple = (installation, kind, typ, name, status);
                if let Some(previous) = rows.get(&id) {
                    if previous != &tuple {
                        return Err(refused());
                    }
                } else {
                    rows.insert(id, tuple);
                    if rows.len() > 4096 {
                        return Err(refused());
                    }
                }
            }
            if scopes.insert(scope.dot_id.clone(), v).is_some() {
                return Err(refused());
            }
        }
        Ok(Self {
            account: c.source.account_id_sha256.clone(),
            mapping,
            scopes,
            rows,
        })
    }
    fn renamed(&mut self, row: &Row) {
        for scope in self.scopes.values_mut() {
            for record in scope["computers"].as_array_mut().expect("validated") {
                if record["environment_id"].as_str() == Some(&row.environment_id) {
                    record["display_name"] = Value::String(row.new_name.clone());
                }
            }
        }
    }
    fn matches(&self, other: &Self) -> bool {
        self.account == other.account
            && self.mapping == other.mapping
            && self.scopes == other.scopes
    }
}
fn plan(
    c: &Capture,
    selections: Vec<Selection>,
    peers: Vec<Peer>,
    before: &[u8],
    mode: Option<EvidenceMode>,
) -> Result<Plan> {
    let aggregate = Aggregate::parse(c)?;
    if selections.is_empty()
        || selections.len() > 4096
        || (mode.is_none() && peers.len() != selections.len())
        || (mode.is_some() && !peers.is_empty())
    {
        return Err(refused());
    }
    let mut selected = BTreeSet::new();
    let mut records = Vec::new();
    for choice in &selections {
        if !selected.insert(&choice.environment_id)
            || validated_name(&choice.new_name)? != choice.new_name
        {
            return Err(refused());
        }
        if aggregate.scopes.values().any(|scope| {
            scope["computers"]
                .as_array()
                .expect("validated")
                .iter()
                .any(|row| {
                    row["environment_id"].as_str() == Some(&choice.environment_id)
                        && row.get("computer_type").is_none()
                })
        }) {
            return Err(refused());
        }
        let (installation, kind, typ, name, status) = aggregate
            .rows
            .get(&choice.environment_id)
            .ok_or_else(refused)?;
        let installation = installation.as_ref().ok_or_else(refused)?;
        let old_name = name
            .as_ref()
            .filter(|s| !s.is_empty())
            .ok_or_else(refused)?;
        if old_name == &choice.new_name
            || !(kind == "personal" || (kind == "user" && typ.as_deref() == Some("app_connection")))
            || !matches!(status.as_deref(), Some("connected" | "offline"))
        {
            return Err(refused());
        }
        let fingerprint = sha256(installation.as_bytes());
        if aggregate
            .rows
            .values()
            .filter(|r| {
                r.0.as_deref()
                    .is_some_and(|i| sha256(i.as_bytes()) == fingerprint)
            })
            .count()
            != 1
        {
            return Err(refused());
        }
        let matched: Vec<_> = peers
            .iter()
            .filter(|p| {
                p.environment_id == choice.environment_id
                    && p.installation_id_sha256 == fingerprint
                    && !p.host.is_empty()
                    && !p.computer_name.is_empty()
            })
            .collect();
        if mode.is_none() && matched.len() != 1 {
            return Err(refused());
        }
        records.push(Row {
            environment_id: choice.environment_id.clone(),
            installation_id_sha256: fingerprint,
            old_name: old_name.clone(),
            new_name: choice.new_name.clone(),
        });
    }
    Ok(Plan {
        schema_version: 1,
        before_sha256: sha256(before),
        selections,
        peers,
        records,
        evidence_mode: mode,
    })
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Event {
    plan_sha256: String,
    previous: String,
    index: usize,
    kind: String,
}
fn journal(bytes: &[u8], digest: &str, count: usize) -> Result<(usize, bool, String)> {
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(integrity());
    }
    let mut index = 0;
    let mut pending = false;
    let mut previous = sha256(b"");
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let body = &line[..line.len() - 1];
        let e: Event = serde_json::from_slice(body).map_err(|_| integrity())?;
        if e.plan_sha256 != digest || e.previous != previous || e.index != index || index >= count {
            return Err(integrity());
        }
        match (e.kind.as_str(), pending) {
            ("intent", false) => pending = true,
            ("verified", true) => {
                pending = false;
                index += 1;
            }
            _ => return Err(integrity()),
        }
        previous = sha256(body);
    }
    Ok((index, pending, previous))
}
fn append(
    dir: &Directory,
    existing: &[u8],
    digest: &str,
    previous: String,
    index: usize,
    kind: &str,
) -> Result<()> {
    let mut file = dir.open_append("journal.jsonl")?;
    if crate::storage::directory::read_private_file(&mut file)? != existing {
        return Err(integrity());
    }
    let mut line = compact_json_bytes(&Event {
        plan_sha256: digest.into(),
        previous,
        index,
        kind: kind.into(),
    })?;
    line.push(b'\n');
    if existing.len() + line.len() > MAX_BYTES as usize {
        return Err(refused());
    }
    file.write_all(&line)?;
    file.sync_all()?;
    Ok(())
}
fn receipt(
    req: &Request,
    plan: &Plan,
    digest: &str,
    index: usize,
    pending: bool,
    status: &'static str,
) -> Result<Vec<u8>> {
    let records = plan
        .records
        .iter()
        .enumerate()
        .map(|(i, row)| ReceiptRow {
            row,
            state: if i < index {
                "verified"
            } else if i == index && pending {
                "intent-recorded"
            } else {
                "pending"
            },
        })
        .collect();
    compact_json_bytes(&Receipt {
        schema_version: 1,
        op: &req.op,
        status,
        operation_directory: &req.operation_directory,
        plan_sha256: digest,
        records,
        reason_code: "ok",
        evidence_mode: plan.evidence_mode,
    })
}
fn execute(req: &Request) -> Result<(Vec<u8>, i32)> {
    if req.schema_version != 1 || req.operation_directory.is_empty() {
        return Err(refused());
    }
    let path = Path::new(&req.operation_directory);
    if req.op == "prepare" {
        if req.plan_sha256.is_some() || req.environment_id.is_some() {
            return Err(refused());
        }
        let capture = req.capture.as_ref().ok_or_else(refused)?;
        let before = compact_json_bytes(capture)?;
        let plan = plan(
            capture,
            req.selections.clone().ok_or_else(refused)?,
            req.peers.clone().ok_or_else(refused)?,
            &before,
            req.evidence_mode,
        )?;
        let bytes = compact_json_bytes(&plan)?;
        let digest = sha256(&bytes);
        let dir = Directory::create(path)?;
        dir.write_new("before.json", &before)?;
        dir.write_new(".lock", b"")?;
        dir.write_new("journal.jsonl", b"")?;
        dir.write_new("plan.json", &bytes)?;
        dir.sync()?;
        return Ok((receipt(req, &plan, &digest, 0, false, "prepared")?, 0));
    }
    if req.selections.is_some()
        || !matches!(
            req.op.as_str(),
            "preflight" | "intent" | "reconcile" | "status"
        )
    {
        return Err(refused());
    }
    let expected = req
        .plan_sha256
        .as_ref()
        .filter(|s| hash(s))
        .ok_or_else(refused)?;
    let dir = Directory::open(path)?;
    dir.check_members(
        &["before.json", "plan.json", ".lock", "journal.jsonl"],
        Some("intent-"),
    )?;
    let guard = dir.open_read_write(".lock")?;
    guard.try_lock().map_err(|_| refused())?;
    let saved = dir.read("plan.json")?;
    if sha256(&saved) != *expected {
        return Err(integrity());
    }
    let saved_plan: Plan = serde_json::from_slice(&saved).map_err(|_| integrity())?;
    let before = dir.read("before.json")?;
    if sha256(&before) != saved_plan.before_sha256 {
        return Err(integrity());
    }
    let capture: Capture = serde_json::from_slice(&before).map_err(|_| integrity())?;
    let rebuilt = plan(
        &capture,
        saved_plan.selections.clone(),
        saved_plan.peers.clone(),
        &before,
        saved_plan.evidence_mode,
    )
    .map_err(|_| integrity())?;
    if compact_json_bytes(&rebuilt)? != saved {
        return Err(integrity());
    }
    let mut members = vec![
        "before.json".to_string(),
        "plan.json".to_string(),
        ".lock".to_string(),
        "journal.jsonl".to_string(),
    ];
    members.extend((0..saved_plan.records.len()).map(|i| format!("intent-{i}")));
    dir.check_members(
        &members.iter().map(String::as_str).collect::<Vec<_>>(),
        None,
    )?;
    let bytes = dir.read("journal.jsonl")?;
    let (index, journal_pending, mut previous) =
        journal(&bytes, expected, saved_plan.records.len())?;
    let mut pending = journal_pending;
    for i in 0..saved_plan.records.len() {
        let name = format!("intent-{i}");
        let exists = match std::fs::symlink_metadata(path.join(&name)) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if exists && dir.read(&name)? != format!("{expected}:{i}").as_bytes() {
            return Err(integrity());
        }
        if (i < index || (i == index && journal_pending)) && !exists {
            return Err(integrity());
        }
        if i == index && exists {
            pending = true;
        }
        if i > index && exists {
            return Err(integrity());
        }
    }
    if req.op == "status" {
        if req.capture.is_some() || req.peers.is_some() || req.environment_id.is_some() {
            return Err(refused());
        }
        return Ok((
            receipt(
                req,
                &saved_plan,
                expected,
                index,
                pending,
                if pending {
                    "uncertain"
                } else if index == saved_plan.records.len() {
                    "verified"
                } else {
                    "prepared"
                },
            )?,
            if pending { 4 } else { 0 },
        ));
    }
    let supplied = req.capture.as_ref().ok_or_else(refused)?;
    let fresh = match Aggregate::parse(supplied) {
        Ok(fresh) => fresh,
        Err(_)
            if req.op == "reconcile"
                && pending
                && req.peers.is_none()
                && req.environment_id.as_deref()
                    == Some(&saved_plan.records[index].environment_id) =>
        {
            return Ok((
                receipt(req, &saved_plan, expected, index, true, "uncertain")?,
                4,
            ));
        }
        Err(error) => return Err(error),
    };
    let mut baseline = Aggregate::parse(&capture).map_err(|_| integrity())?;
    for row in &saved_plan.records[..index] {
        baseline.renamed(row);
    }
    if req.op == "reconcile"
        && req.peers.is_none()
        && saved_plan.records[..index]
            .iter()
            .any(|row| Some(row.environment_id.as_str()) == req.environment_id.as_deref())
    {
        let matches = baseline.matches(&fresh);
        return Ok((
            receipt(
                req,
                &saved_plan,
                expected,
                index,
                pending,
                if matches { "verified" } else { "uncertain" },
            )?,
            if matches { 0 } else { 4 },
        ));
    }
    if req.op == "preflight" || req.op == "intent" {
        if req.peers.as_ref() != Some(&saved_plan.peers)
            || !baseline.matches(&fresh)
            || pending
            || index == saved_plan.records.len()
        {
            return Err(refused());
        }
        if req.op == "preflight" {
            if req.environment_id.is_some() {
                return Err(refused());
            }
            return Ok((
                receipt(req, &saved_plan, expected, index, false, "ready")?,
                0,
            ));
        }
        if req.environment_id.as_deref() != Some(&saved_plan.records[index].environment_id) {
            return Err(refused());
        }
        dir.write_new(
            format!("intent-{index}"),
            format!("{expected}:{index}").as_bytes(),
        )?;
        dir.sync()?;
        append(&dir, &bytes, expected, previous, index, "intent")?;
        return Ok((
            receipt(req, &saved_plan, expected, index, true, "intent-recorded")?,
            0,
        ));
    }
    if req.peers.is_some()
        || !pending
        || req.environment_id.as_deref() != Some(&saved_plan.records[index].environment_id)
    {
        return Err(refused());
    }
    baseline.renamed(&saved_plan.records[index]);
    if !baseline.matches(&fresh) {
        return Ok((
            receipt(req, &saved_plan, expected, index, true, "uncertain")?,
            4,
        ));
    }
    let mut journal_bytes = bytes;
    if !journal_pending {
        append(&dir, &journal_bytes, expected, previous, index, "intent")?;
        journal_bytes = dir.read("journal.jsonl")?;
        previous = journal(&journal_bytes, expected, saved_plan.records.len())?.2;
    }
    append(&dir, &journal_bytes, expected, previous, index, "verified")?;
    Ok((
        receipt(req, &saved_plan, expected, index + 1, false, "verified")?,
        0,
    ))
}
/// Execute the bounded desktop protocol without performing network access.
pub fn desktop(input: &[u8]) -> (Vec<u8>, i32) {
    let result = (|| {
        if input.len() as u64 > MAX_BYTES {
            return Err(refused());
        }
        let raw: Value = serde_json::from_slice(input).map_err(|_| refused())?;
        reject_credentials(&raw)?;
        let req: Request = serde_json::from_slice(input).map_err(|_| refused())?;
        if raw.get("evidence_mode").is_some()
            && (req.op != "prepare" || req.evidence_mode.is_none())
        {
            return Err(refused());
        }
        execute(&req)
    })();
    match result {
        Ok(v) => v,
        Err(e) => {
            let status = "refused";
            let value = serde_json::json!({"schema_version":1,"op":"unknown","status":status,"reason_code":if e.exit_code == 5 {"integrity"} else if e.exit_code == 1 {"io"} else {"refused"}});
            (
                serde_json::to_vec(&value).expect("static receipt"),
                e.exit_code,
            )
        }
    }
}
