use super::{Kind, Plan};
use crate::encoding::compact_json_bytes;
use crate::storage::directory::{Directory, read_private_file};
use crate::{Error, MAX_BYTES, Result, sha256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write;

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Event {
    Intent {
        environment_id: String,
        capability_advertisement_changes: Vec<String>,
    },
    Response {
        environment_id: String,
        success: bool,
    },
    Verified {
        environment_id: String,
        absent: bool,
    },
    Renamed {
        environment_id: String,
        matches: bool,
    },
    Complete,
}

impl Event {
    fn readback(&self, kind: Kind) -> Option<bool> {
        match (self, kind) {
            (Self::Verified { absent, .. }, Kind::Cleanup) => Some(*absent),
            (Self::Renamed { matches, .. }, Kind::Rename) => Some(*matches),
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    sequence: usize,
    plan_sha256: String,
    previous_entry_sha256: String,
    event: Event,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum RequestState {
    #[default]
    Idle,
    IntentRecorded,
    ResponseReceived {
        success: bool,
    },
}

#[derive(Clone, Default)]
pub(super) struct State {
    pub(super) next: usize,
    pub(super) request: RequestState,
    pub(super) finished: bool,
    pub(super) advertisement_changes: BTreeSet<String>,
}

impl State {
    fn advance(&mut self, event: &Event, plan: &Plan) -> Result<()> {
        let ids = &plan.selected_environment_ids;
        let invalid = || Error::integrity("Journal event order or target binding is invalid.");
        if self.finished {
            return Err(invalid());
        }
        if let Event::Complete = event {
            if self.request != RequestState::Idle || self.next != ids.len() {
                return Err(invalid());
            }
            self.finished = true;
            return Ok(());
        }
        let id = match event {
            Event::Intent { environment_id, .. }
            | Event::Response { environment_id, .. }
            | Event::Verified { environment_id, .. }
            | Event::Renamed { environment_id, .. } => environment_id,
            Event::Complete => unreachable!(),
        };
        if ids.get(self.next) != Some(id) {
            return Err(invalid());
        }
        if let Some(matched) = event.readback(plan.kind) {
            if self.request == RequestState::Idle
                || (!matched && self.request == (RequestState::ResponseReceived { success: true }))
            {
                return Err(invalid());
            }
            self.next += usize::from(matched);
            self.request = RequestState::Idle;
            return Ok(());
        }
        match event {
            Event::Intent {
                capability_advertisement_changes,
                ..
            } if self.request == RequestState::Idle => {
                if capability_advertisement_changes
                    .iter()
                    .any(|id| !ids.contains(id) && !plan.protected_environment_ids.contains(id))
                {
                    return Err(invalid());
                }
                self.advertisement_changes
                    .extend(capability_advertisement_changes.iter().cloned());
                self.request = RequestState::IntentRecorded;
            }
            Event::Response { success, .. } if self.request == RequestState::IntentRecorded => {
                self.request = RequestState::ResponseReceived { success: *success }
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
}

pub(super) struct Journal {
    pub(super) state: State,
    sequence: usize,
    previous: String,
    bytes: Vec<u8>,
}

fn encode(sequence: usize, digest: &str, previous: &str, event: Event) -> Result<Vec<u8>> {
    let entry = Entry {
        sequence,
        plan_sha256: digest.into(),
        previous_entry_sha256: previous.into(),
        event,
    };
    let mut line = compact_json_bytes(&entry)?;
    line.push(b'\n');
    Ok(line)
}

impl Journal {
    pub(super) fn empty() -> Self {
        Self {
            state: State::default(),
            sequence: 0,
            previous: sha256(b""),
            bytes: Vec::new(),
        }
    }

    pub(super) fn load(directory: &Directory, plan: &Plan, digest: &str) -> Result<Self> {
        let bytes = directory.read("journal.jsonl")?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(Error::integrity(
                "Journal has a torn final entry. It is retained unchanged; automatic truncation is refused.",
            ));
        }
        let mut journal = Self::empty();
        for terminated in bytes.split_inclusive(|b| *b == b'\n') {
            let line = &terminated[..terminated.len() - 1];
            let entry: Entry = serde_json::from_slice(line)
                .map_err(|_| Error::integrity("Journal contains an invalid entry."))?;
            if entry.sequence != journal.sequence
                || entry.plan_sha256 != digest
                || entry.previous_entry_sha256 != journal.previous
            {
                return Err(Error::integrity(
                    "Journal sequence, hash chain, or plan binding changed.",
                ));
            }
            journal.state.advance(&entry.event, plan)?;
            journal.sequence += 1;
            journal.previous = sha256(line);
        }
        journal.bytes = bytes;
        Ok(journal)
    }

    pub(super) fn reserve_remaining(
        &self,
        plan: &Plan,
        digest: &str,
        changes: &BTreeSet<String>,
    ) -> Result<()> {
        let mut required = self.bytes.len();
        let mut sequence = self.sequence;
        for (index, id) in plan.selected_environment_ids[self.state.next..]
            .iter()
            .enumerate()
        {
            let events = [
                Event::Intent {
                    environment_id: id.clone(),
                    capability_advertisement_changes: if index == 0 {
                        changes.iter().cloned().collect()
                    } else {
                        Vec::new()
                    },
                },
                Event::Response {
                    environment_id: id.clone(),
                    success: false,
                },
                plan.readback_event(id.clone(), false),
            ];
            for event in events {
                required += encode(sequence, digest, &self.previous, event)?.len();
                sequence += 1;
                self.check_size(required)?;
            }
        }
        self.check_size(required + encode(sequence, digest, &self.previous, Event::Complete)?.len())
    }

    fn check_size(&self, bytes: usize) -> Result<()> {
        if bytes > MAX_BYTES as usize {
            return Err(Error::refused(
                "Journal cannot fit the remaining requests, readbacks, and completion within 4 MiB. No further request was issued.",
            ));
        }
        Ok(())
    }

    pub(super) fn append(
        &mut self,
        directory: &Directory,
        plan: &Plan,
        digest: &str,
        event: Event,
    ) -> Result<()> {
        let mut next = self.state.clone();
        next.advance(&event, plan)?;
        let mut file = directory.open_append("journal.jsonl")?;
        if read_private_file(&mut file)? != self.bytes {
            return Err(Error::integrity(
                "Journal changed while the operation lock was held.",
            ));
        }
        let line = encode(self.sequence, digest, &self.previous, event)?;
        self.check_size(self.bytes.len() + line.len())?;
        file.write_all(&line)?;
        file.sync_all()?;
        self.previous = sha256(&line[..line.len() - 1]);
        self.bytes.extend_from_slice(&line);
        self.sequence += 1;
        self.state = next;
        Ok(())
    }
}
