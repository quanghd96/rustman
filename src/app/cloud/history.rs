//! History has its own object so sending a request never creates a collection conflict.

use std::collections::HashSet;

use iced::Task;
use rustman_core::domain::history::{HISTORY_LIMIT, HistoryEntry};
use rustman_core::secrets::strip_secrets;
use rustman_core::store::StoreError;
use rustman_store::{SqliteStore, StoreThread};
use serde::{Deserialize, Serialize};

use crate::message::Message;

use super::s3::{Bucket, Download, LIMIT};
use super::{AppState, CloudMsg};

// Leave room for the object envelope and separators.
const ENTRY_LIMIT: usize = (LIMIT - 1024) / HISTORY_LIMIT;

#[derive(Default)]
pub(super) struct State {
    pub(super) busy: bool,
    pub(super) error: Option<String>,
    pub(super) retry: bool,
    pending: bool,
    tag: Option<String>,
}

pub(super) fn saved(state: &mut AppState) -> Task<Message> {
    state.cloud.history.pending = true;
    start(state)
}

pub(super) fn start(state: &mut AppState) -> Task<Message> {
    if state.cloud.history.busy {
        return Task::none();
    }
    let (Some(mut config), Some(store), Some(target)) = (
        state.cloud.profile.config.clone(),
        state.store.as_ref().filter(|store| store.writable()),
        state.cloud.history_target(),
    ) else {
        return Task::none();
    };
    config.key.push_str(".history.json");
    let history = &mut state.cloud.history;
    let tag = if history.pending {
        None
    } else {
        history.tag.clone()
    };
    history.pending = false;
    history.busy = true;
    let worker = store.worker();
    Task::perform(
        async move {
            let bucket = Bucket::open(config)?;
            sync(bucket, tag, worker, target).await.map(Outcome)
        },
        |result| Message::Cloud(CloudMsg::HistoryFinished(result)),
    )
}

#[derive(Clone)]
pub(crate) struct Outcome(Decision);

#[derive(Clone)]
enum Decision {
    Current(Option<String>),
    Merged(String),
    Retry,
}

pub(super) fn finish(state: &mut AppState, result: Result<Outcome, String>) -> Task<Message> {
    let history = &mut state.cloud.history;
    history.busy = false;
    history.retry = false;
    let applied = result.and_then(|result| {
        match result.0 {
            Decision::Merged(tag) => {
                let Some(store) = &state.store else {
                    return Ok(());
                };
                // Read after queued local edits, including requests sent during sync.
                let summaries = store.run_now(|db| {
                    rustman_store::history_summaries(db.connection(), HISTORY_LIMIT)
                        .map_err(StoreError::new)
                })?;
                if state.history != summaries {
                    state.history = summaries;
                    *state.sidebar.history_cache.get_mut() = Default::default();
                }
                history.tag = Some(tag);
            }
            Decision::Current(tag) => history.tag = tag,
            Decision::Retry => {
                history.tag = None;
                history.retry = true;
            }
        }
        Ok(())
    });
    history.error = applied.err();
    if history.error.is_some() {
        history.tag = None;
    } else if history.pending {
        return start(state);
    }
    Task::none()
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u16,
    cleared_at: Option<i64>,
    entries: Vec<HistoryEntry>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 1,
            cleared_at: None,
            entries: Vec::new(),
        }
    }
}

impl Snapshot {
    fn decode(data: &[u8]) -> Result<Self, String> {
        let snapshot: Self =
            serde_json::from_slice(data).map_err(|_| "The cloud object isn't Rustman history")?;
        if snapshot.version != 1 || snapshot.entries.len() > HISTORY_LIMIT {
            return Err("The cloud history uses an unsupported format".into());
        }
        Ok(snapshot)
    }

    fn merge(mut self, cleared_at: Option<i64>, entries: Vec<HistoryEntry>) -> Self {
        self.cleared_at = self.cleared_at.max(cleared_at);
        let mut known = HashSet::new();
        self.entries.extend(entries);
        self.entries.retain(|entry| {
            self.cleared_at.is_none_or(|time| entry.timestamp > time)
                && !entry.id.is_empty()
                && known.insert(entry.id.clone())
        });
        self.entries
            .sort_unstable_by(|a, b| (b.timestamp, &b.id).cmp(&(a.timestamp, &a.id)));
        self.entries.truncate(HISTORY_LIMIT);
        self
    }
}

async fn sync(
    bucket: Bucket,
    tag: Option<String>,
    worker: StoreThread,
    target: String,
) -> Result<Decision, String> {
    let (tag, data) = match bucket.download(tag.as_deref()).await? {
        Download::Unchanged => return Ok(Decision::Current(tag)),
        Download::Missing => (None, Vec::new()),
        Download::Object { tag, data } => (Some(tag), data),
    };
    let mut remote = if tag.is_some() {
        Snapshot::decode(&data)?
    } else {
        Snapshot::default()
    };
    for entry in &mut remote.entries {
        prepare_entry(entry)?;
    }
    let known = remote
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let remote_clear = remote.cleared_at;
    let read_target = target.clone();
    let (local_clear, local) = worker
        .run(move |db| load_local(db, &read_target, &known, remote_clear))
        .await
        .map_err(|error| error.to_string())?;
    let snapshot = remote.merge(local_clear, local);
    if tag.is_none() && snapshot.cleared_at.is_none() && snapshot.entries.is_empty() {
        return Ok(Decision::Current(None));
    }
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| "Couldn't encode the cloud history")?;
    let tag = match tag {
        Some(tag) if bytes == data => tag,
        previous => match bucket.upload(bytes, previous.as_deref()).await? {
            Some(tag) => tag,
            None => return Ok(Decision::Retry),
        },
    };
    worker
        .run(move |db| db.merge_cloud_history(&target, snapshot.cleared_at, &snapshot.entries))
        .await
        .map_err(|error| error.to_string())?;
    Ok(Decision::Merged(tag))
}

fn load_local(
    db: &SqliteStore,
    target: &str,
    known: &HashSet<String>,
    remote_clear: Option<i64>,
) -> Result<(Option<i64>, Vec<HistoryEntry>), StoreError> {
    let local_clear = db.history_cleared_at(target)?;
    let cutoff = local_clear.max(remote_clear);
    let summaries = rustman_store::history_summaries(db.connection(), HISTORY_LIMIT)
        .map_err(StoreError::new)?;
    let mut entries = Vec::new();
    for summary in summaries {
        if !known.contains(&summary.id)
            && cutoff.is_none_or(|timestamp| summary.timestamp > timestamp)
            && let Some(mut entry) = rustman_store::history_entry(db.connection(), &summary.id)
                .map_err(StoreError::new)?
        {
            prepare_entry(&mut entry).map_err(StoreError::new)?;
            entries.push(entry);
        }
    }
    Ok((local_clear, entries))
}

fn prepare_entry(entry: &mut HistoryEntry) -> Result<(), String> {
    strip_secrets(&mut entry.request);
    entry.omit_files_over(0);
    entry.omit_large_parts(ENTRY_LIMIT);
    let size = |entry: &HistoryEntry| {
        serde_json::to_vec(entry)
            .map(|bytes| bytes.len())
            .map_err(|_| "Couldn't encode a history request")
    };
    if size(entry)? > ENTRY_LIMIT {
        entry.omit_large_parts(0);
        if size(entry)? > ENTRY_LIMIT {
            return Err("A history request exceeds the cloud size limit".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rustman_core::domain::request::BinaryFile;
    use rustman_core::store::HistoryStore;

    use super::*;

    fn entry(id: &str, timestamp: i64) -> HistoryEntry {
        let mut entry = rustman_testkit::history_entry();
        entry.id = id.into();
        entry.timestamp = timestamp;
        entry
    }

    fn summaries(db: &SqliteStore) -> Vec<rustman_core::domain::history::HistorySummary> {
        rustman_store::history_summaries(db.connection(), HISTORY_LIMIT).unwrap()
    }

    #[test]
    fn two_devices_keep_the_newest_unique_entries() {
        let mut first = SqliteStore::open_in_memory().unwrap();
        let mut second = SqliteStore::open_in_memory().unwrap();
        for n in 1..=100 {
            first
                .append_history(entry(&format!("first-{n}"), n))
                .unwrap();
            second
                .append_history(entry(&format!("second-{n}"), n))
                .unwrap();
        }
        let (clear, first_entries) = load_local(&first, "target", &HashSet::new(), None).unwrap();
        let initial = Snapshot::default().merge(clear, first_entries);
        let known = initial
            .entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect();
        let (clear, second_entries) = load_local(&second, "target", &known, None).unwrap();
        let current = initial.merge(clear, second_entries);
        first
            .merge_cloud_history("target", clear, &current.entries)
            .unwrap();
        second
            .merge_cloud_history("target", clear, &current.entries)
            .unwrap();
        let first_list = summaries(&first);
        assert_eq!(first_list, summaries(&second));
        assert_eq!(first_list.len(), HISTORY_LIMIT);
        assert_eq!(first_list.first().unwrap().timestamp, 100);
        assert_eq!(first_list.last().unwrap().timestamp, 51);
    }

    #[test]
    fn a_clear_defeats_an_in_flight_download_and_stays_with_its_bucket() {
        let mut first = SqliteStore::open_in_memory().unwrap();
        let mut second = SqliteStore::open_in_memory().unwrap();
        let old = entry("old", 1);
        second.append_history(old.clone()).unwrap();
        first.clear_cloud_history("target").unwrap();
        first
            .merge_cloud_history("target", None, std::slice::from_ref(&old))
            .unwrap();
        assert!(summaries(&first).is_empty());
        assert!(first.history_cleared_at("other-target").unwrap().is_none());

        let cutoff = first.history_cleared_at("target").unwrap();
        first
            .append_cloud_history("target", entry("new", cutoff.unwrap()))
            .unwrap();
        let (_, entries) = load_local(&first, "target", &HashSet::new(), cutoff).unwrap();
        second
            .merge_cloud_history("target", cutoff, &entries)
            .unwrap();
        let second_list = summaries(&second);
        assert_eq!(second_list.len(), 1);
        assert_eq!(second_list[0].id, "new");
    }

    #[test]
    fn cloud_entries_can_be_reopened_without_overwriting_local_credentials_or_files() {
        let mut original = entry("sent", 1);
        original.request.bearer_token = "local-token".into();
        original.request.body = "payload".into();
        original.request.binary_file = Some(BinaryFile {
            name: "file.bin".into(),
            data: Some("YQ==".into()),
        });
        let mut source = SqliteStore::open_in_memory().unwrap();
        let mut destination = SqliteStore::open_in_memory().unwrap();
        source.append_history(original.clone()).unwrap();

        let (_, entries) = load_local(&source, "target", &HashSet::new(), None).unwrap();
        let snapshot = Snapshot::default().merge(None, entries);
        let restored = Snapshot::decode(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        source
            .merge_cloud_history("target", None, &restored.entries)
            .unwrap();
        destination
            .merge_cloud_history("target", None, &restored.entries)
            .unwrap();

        let local = rustman_store::history_entry(source.connection(), "sent")
            .unwrap()
            .unwrap();
        let remote = rustman_store::history_entry(destination.connection(), "sent")
            .unwrap()
            .unwrap();
        assert_eq!(local.request, original.request);
        assert_eq!(remote.request.body, "payload");
        assert!(remote.request.bearer_token.is_empty());
        let file = remote.request.binary_file.unwrap();
        assert_eq!(file.name, "file.bin");
        assert!(file.data.is_none());
        assert_eq!(remote.omitted.len(), 1);
    }
}
