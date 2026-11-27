//! Conditional S3 sync for a workspace and its request history.

mod history;
mod s3;
mod snapshot;

use std::collections::HashMap;
use std::sync::Arc;

use iced::Task;
use rustman_core::store::{StoreError, Workspace, WorkspaceStore};
use rustman_store::SqliteStore;
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::state::tabs::RequestTabState;

use self::s3::{Bucket, Download, digest};
use self::snapshot::Snapshot;
use super::AppState;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub key: String,
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub session_token: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            region: "us-east-1".into(),
            bucket: String::new(),
            key: "rustman/workspace.json".into(),
            access_key: String::new(),
            secret_key: String::new(),
            session_token: String::new(),
        }
    }
}

impl Config {
    fn normalized(mut self) -> Self {
        self.endpoint = self.endpoint.trim().trim_end_matches('/').to_owned();
        for field in [
            &mut self.region,
            &mut self.bucket,
            &mut self.key,
            &mut self.access_key,
            &mut self.secret_key,
            &mut self.session_token,
        ] {
            *field = field.trim().to_owned();
        }
        self
    }

    fn same_target(&self, other: &Self) -> bool {
        (&self.endpoint, &self.bucket, &self.key) == (&other.endpoint, &other.bucket, &other.key)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Field {
    Endpoint,
    Region,
    Bucket,
    Key,
    AccessKey,
    SecretKey,
    SessionToken,
}

#[derive(Clone)]
pub(crate) enum CloudMsg {
    Edit(Field, String),
    Connect,
    Disconnect,
    Sync,
    Poll,
    Saved,
    HistorySaved,
    UseLocal,
    UseCloud,
    Finished(Result<Arc<Outcome>, String>),
    HistoryFinished(Result<history::Outcome, String>),
}

impl std::fmt::Debug for CloudMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Edit(field, _) => f.debug_tuple("Edit").field(field).finish(),
            Self::Connect => f.write_str("Connect"),
            Self::Disconnect => f.write_str("Disconnect"),
            Self::Sync => f.write_str("Sync"),
            Self::Poll => f.write_str("Poll"),
            Self::Saved => f.write_str("Saved"),
            Self::HistorySaved => f.write_str("HistorySaved"),
            Self::UseLocal => f.write_str("UseLocal"),
            Self::UseCloud => f.write_str("UseCloud"),
            Self::Finished(_) => f.write_str("Finished"),
            Self::HistoryFinished(_) => f.write_str("HistoryFinished"),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Profile {
    config: Option<Config>,
    checkpoint: Option<Checkpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Checkpoint {
    tag: String,
    hash: String,
}

pub(crate) struct Conflict {
    tag: Option<String>,
    pub device_collections: usize,
    pub cloud_collections: Option<usize>,
}

pub(crate) enum Phase {
    Idle,
    Working,
    Current,
    Retry,
    Error(String),
    Conflict(Conflict),
}

pub(crate) struct State {
    pub draft: Config,
    profile: Profile,
    pub phase: Phase,
    queued: bool,
    history: history::State,
}

impl State {
    pub(super) fn load(db: Option<&SqliteStore>, writable: bool) -> Self {
        let loaded = db.map(SqliteStore::load_cloud::<Profile>).transpose();
        let (profile, phase) = match loaded {
            Ok(loaded) => (loaded.flatten().unwrap_or_default(), Phase::Idle),
            Err(error) => (
                Profile::default(),
                Phase::Error(format!("Couldn't load cloud settings: {error}")),
            ),
        };
        let draft = profile.config.clone().unwrap_or_default();
        Self {
            draft,
            profile: if writable {
                profile
            } else {
                Profile::default()
            },
            phase,
            queued: false,
            history: history::State::default(),
        }
    }

    pub(crate) fn connected(&self) -> bool {
        self.profile.config.is_some()
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(self.phase, Phase::Working) || self.history.busy
    }

    pub(crate) fn history_error(&self) -> Option<&str> {
        self.history.error.as_deref()
    }

    pub(crate) fn history_target(&self) -> Option<String> {
        self.profile.config.as_ref().map(|config| {
            digest(format!("{}\n{}\n{}", config.endpoint, config.bucket, config.key).as_bytes())
        })
    }

    pub(crate) fn label(&self) -> &'static str {
        if matches!(self.phase, Phase::Current) {
            if self.history.busy {
                return "Cloud syncing history…";
            }
            if self.history.error.is_some() {
                return "History sync failed";
            }
            if self.history.retry {
                return "Cloud history changed · retrying";
            }
        }
        match self.phase {
            Phase::Idle if !self.connected() => "Cloud disconnected",
            Phase::Idle => "Cloud waiting",
            Phase::Working => "Cloud syncing…",
            Phase::Current => "Cloud up to date",
            Phase::Retry => "Cloud changed · retrying",
            Phase::Error(_) => "Cloud sync failed",
            Phase::Conflict(_) => "Cloud conflict",
        }
    }
}

#[derive(Clone, Copy)]
enum Choice {
    Device,
    Cloud,
}

pub(crate) struct Outcome(Decision);

enum Decision {
    Current(Checkpoint),
    Install {
        snapshot: Snapshot,
        checkpoint: Checkpoint,
        expected: String,
    },
    Conflict(Conflict),
    Retry,
}

pub(crate) fn handle(state: &mut AppState, msg: CloudMsg) -> Task<Message> {
    match msg {
        CloudMsg::Edit(field, value) => {
            let draft = &mut state.cloud.draft;
            let target = match field {
                Field::Endpoint => &mut draft.endpoint,
                Field::Region => &mut draft.region,
                Field::Bucket => &mut draft.bucket,
                Field::Key => &mut draft.key,
                Field::AccessKey => &mut draft.access_key,
                Field::SecretKey => &mut draft.secret_key,
                Field::SessionToken => &mut draft.session_token,
            };
            *target = crate::services::format::head_capped(&value, 8192).to_owned();
        }
        CloudMsg::Connect if !state.cloud.busy() => {
            let config = state.cloud.draft.clone().normalized();
            if let Err(error) = s3::location(&config) {
                state.cloud.phase = Phase::Error(error);
                return Task::none();
            }
            let mut profile = state.cloud.profile.clone();
            if !profile
                .config
                .as_ref()
                .is_some_and(|old| old.same_target(&config))
            {
                profile.checkpoint = None;
            }
            profile.config = Some(config.clone());
            if save_profile(state, profile) {
                state.cloud.draft = config;
                state.cloud.history = history::State::default();
                return sync_all(state, true);
            }
        }
        CloudMsg::Disconnect if !state.cloud.busy() => {
            if save_profile(state, Profile::default()) {
                state.cloud.draft = Config::default();
                state.cloud.phase = Phase::Idle;
                state.cloud.queued = false;
                state.cloud.history = history::State::default();
            }
        }
        CloudMsg::Sync => return sync_all(state, true),
        CloudMsg::Poll => {
            return sync_all(state, !matches!(state.cloud.phase, Phase::Conflict(_)));
        }
        CloudMsg::Saved if matches!(state.cloud.phase, Phase::Working) => state.cloud.queued = true,
        CloudMsg::Saved if !matches!(state.cloud.phase, Phase::Conflict(_)) => {
            return start(state, None);
        }
        CloudMsg::UseLocal | CloudMsg::UseCloud => {
            if let Phase::Conflict(conflict) = &state.cloud.phase
                && (matches!(msg, CloudMsg::UseLocal) || conflict.cloud_collections.is_some())
            {
                let choice = if matches!(msg, CloudMsg::UseLocal) {
                    Choice::Device
                } else {
                    Choice::Cloud
                };
                return start(state, Some((choice, conflict.tag.clone())));
            }
        }
        CloudMsg::Finished(result) => return finish(state, result),
        CloudMsg::HistorySaved => return history::saved(state),
        CloudMsg::HistoryFinished(result) => return history::finish(state, result),
        _ => {}
    }
    Task::none()
}

fn sync_all(state: &mut AppState, workspace: bool) -> Task<Message> {
    let history = history::start(state);
    if workspace {
        Task::batch([start(state, None), history])
    } else {
        history
    }
}

fn save_profile(state: &mut AppState, profile: Profile) -> bool {
    let Some(store) = state.store.as_ref().filter(|store| store.writable()) else {
        state.cloud.phase = Phase::Error("Cloud sync needs writable local storage".to_owned());
        return false;
    };
    let to_save = profile.clone();
    if let Err(error) = store.run_now(move |db| db.save_cloud(&to_save)) {
        state.cloud.phase = Phase::Error(format!("Couldn't save cloud settings: {error}"));
        return false;
    }
    store.set_cloud_enabled(profile.config.is_some());
    state.cloud.profile = profile;
    true
}

fn start(state: &mut AppState, choice: Option<(Choice, Option<String>)>) -> Task<Message> {
    if matches!(state.cloud.phase, Phase::Working) {
        return Task::none();
    }
    let (Some(config), Some(store)) = (state.cloud.profile.config.clone(), state.store.as_ref())
    else {
        return Task::none();
    };
    let checkpoint = state.cloud.profile.checkpoint.clone();
    state.cloud.phase = Phase::Working;
    state.cloud.queued = false;
    let read = store.read(|db| db.load_workspace());
    Task::perform(
        async move {
            let local = Snapshot::from_local(read.await?);
            let bucket = Bucket::open(config)?;
            reconcile(bucket, local, checkpoint, choice)
                .await
                .map(|decision| Arc::new(Outcome(decision)))
        },
        |result| Message::Cloud(CloudMsg::Finished(result)),
    )
}

async fn reconcile(
    bucket: Bucket,
    local: Snapshot,
    previous: Option<Checkpoint>,
    choice: Option<(Choice, Option<String>)>,
) -> Result<Decision, String> {
    let bytes = local.encode()?;
    let hash = digest(&bytes);
    let saved_tag = previous.as_ref().map(|checkpoint| checkpoint.tag.as_str());
    let conditional_read = if choice.is_none() { saved_tag } else { None };
    let download = bucket.download(conditional_read).await?;
    let (tag, cloud) = match download {
        Download::Missing => (None, None),
        Download::Unchanged => (saved_tag.map(str::to_owned), None),
        Download::Object { tag, data } => (Some(tag), Some(Snapshot::decode(&data)?)),
    };
    let cloud_hash = cloud.as_ref().map(Snapshot::fingerprint).transpose()?;
    if let Some(tag) = &tag
        && cloud_hash.as_deref() == Some(&hash)
    {
        return Ok(Decision::Current(Checkpoint {
            tag: tag.clone(),
            hash,
        }));
    }
    let forced = choice
        .filter(|(_, shown)| *shown == tag)
        .map(|(choice, _)| choice);
    let local_changed = previous.as_ref().is_some_and(|base| hash != base.hash);
    let cloud_changed = tag.as_deref() != saved_tag;
    let install = matches!(forced, Some(Choice::Cloud))
        || (previous.is_none() && local.is_empty())
        || (previous.is_some() && cloud_changed && !local_changed);
    let upload = matches!(forced, Some(Choice::Device))
        || (previous.is_none()
            && (tag.is_none() || cloud.as_ref().is_some_and(Snapshot::is_empty)))
        || (previous.is_some() && !cloud_changed && local_changed);

    match (tag, cloud) {
        (Some(tag), Some(snapshot)) if install && !matches!(forced, Some(Choice::Device)) => {
            let checkpoint = Checkpoint {
                tag,
                hash: cloud_hash.expect("downloaded snapshot has a digest"),
            };
            Ok(Decision::Install {
                snapshot,
                checkpoint,
                expected: hash,
            })
        }
        (tag, _) if upload => match bucket.upload(bytes, tag.as_deref()).await? {
            Some(tag) => Ok(Decision::Current(Checkpoint { tag, hash })),
            None => Ok(Decision::Retry),
        },
        (Some(tag), None) if !local_changed => Ok(Decision::Current(Checkpoint { tag, hash })),
        (tag, cloud) => Ok(Decision::Conflict(Conflict {
            tag,
            device_collections: local.count(),
            cloud_collections: cloud.as_ref().map(Snapshot::count),
        })),
    }
}

fn finish(state: &mut AppState, result: Result<Arc<Outcome>, String>) -> Task<Message> {
    state.cloud.phase = match result {
        Ok(outcome) => match &outcome.0 {
            Decision::Current(checkpoint) => {
                let mut profile = state.cloud.profile.clone();
                if profile.checkpoint.as_ref() != Some(checkpoint) {
                    profile.checkpoint = Some(checkpoint.clone());
                    if !save_profile(state, profile) {
                        return Task::none();
                    }
                }
                Phase::Current
            }
            Decision::Install {
                snapshot,
                checkpoint,
                expected,
            } => install(
                state,
                snapshot.clone(),
                checkpoint.clone(),
                expected.clone(),
            ),
            Decision::Conflict(conflict) => Phase::Conflict(Conflict {
                tag: conflict.tag.clone(),
                device_collections: conflict.device_collections,
                cloud_collections: conflict.cloud_collections,
            }),
            Decision::Retry => Phase::Retry,
        },
        Err(error) => Phase::Error(error),
    };
    if state.cloud.queued && matches!(state.cloud.phase, Phase::Current) {
        return start(state, None);
    }
    Task::none()
}

fn install(
    state: &mut AppState,
    snapshot: Snapshot,
    checkpoint: Checkpoint,
    expected: String,
) -> Phase {
    let Some(store) = state.store.as_ref().filter(|store| store.writable()) else {
        return Phase::Error("Cloud sync needs writable local storage".to_owned());
    };
    let mut profile = state.cloud.profile.clone();
    profile.checkpoint = Some(checkpoint);
    let to_save = profile.clone();
    // Read after earlier local writes. The UI cannot issue another edit while
    // the guarded replacement and its checkpoint are committed.
    let applied = store.run_now(move |db| {
        let local = db.load_workspace()?;
        if Snapshot::from_local(local.clone())
            .fingerprint()
            .map_err(StoreError::new)?
            != expected
        {
            return Ok(None);
        }
        let workspace = snapshot.into_workspace(local);
        db.replace_cloud_workspace(&workspace, &to_save)?;
        Ok(Some(workspace))
    });
    match applied {
        Ok(Some(workspace)) => {
            state.cloud.profile = profile;
            refresh_tabs(state, workspace);
            Phase::Current
        }
        Ok(None) => Phase::Retry,
        Err(error) => Phase::Error(format!("Couldn't apply the cloud workspace: {error}")),
    }
}

fn refresh_tabs(state: &mut AppState, workspace: Workspace) {
    let by_id: HashMap<_, _> = workspace
        .requests
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    for tab in &mut state.tabs.tabs {
        let Some((collection_id, id)) = &tab.saved_as else {
            continue;
        };
        let old = state
            .requests
            .get(collection_id)
            .and_then(|rs| rs.iter().find(|r| &r.id == id));
        let incoming = by_id.get(id.as_str()).copied();
        if old.is_some() && old == incoming {
            continue;
        }
        if let Some(incoming) = incoming
            && !tab.modified
            && old.is_some_and(|old| matches_saved(tab, old))
        {
            tab.reload_saved(incoming);
        } else {
            // Keep open work as a draft if its saved request changed or was removed.
            tab.saved_as = None;
            tab.modified = true;
        }
    }
    state.collections = workspace.collections;
    state.groups = workspace.groups;
    state.requests = state
        .collections
        .iter()
        .map(|c| (c.id.clone(), Vec::new()))
        .collect();
    for request in workspace.requests {
        state
            .requests
            .entry(request.collection_id.clone())
            .or_default()
            .push(request);
    }
    state.sidebar.drag = None;
    state.sidebar.context_menu = None;
    state.sidebar.renaming = None;
    state.sidebar.confirm_delete = None;
    if state
        .sidebar
        .undo
        .as_ref()
        .is_some_and(|undo| matches!(undo.item, crate::state::sidebar::Deleted::Request { .. }))
    {
        state.sidebar.undo = None;
    }
    state.session_dirty = true;
    state.session_changed_at = std::time::Instant::now();
}

fn matches_saved(tab: &RequestTabState, old: &crate::domain::collection::SavedRequest) -> bool {
    use crate::domain::{model::Request, request::reconcile_url_params};
    let mut stored = Request::from(old.clone());
    // The app reconciles params on opening a request and generates row IDs.
    let (url, params) = reconcile_url_params(&old.url, &old.params);
    let mut draft = tab.to_request();
    draft.id = stored.id.clone();
    draft.collection_id = stored.collection_id.clone();
    draft.group_id = stored.group_id.clone();
    draft.sort_order = stored.sort_order;
    stored.url = url;
    stored.params = params;
    for row in &mut stored.params {
        row.id.clear();
    }
    for row in &mut draft.params {
        row.id.clear();
    }
    draft == stored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::collection::Collection;
    use crate::test_support::TempDir;
    use rustman_core::store::Change;

    #[test]
    fn a_download_waits_if_a_local_save_landed_while_it_was_in_flight() {
        let dir = TempDir::new("cloud-install");
        let mut app = super::super::scratch_state(dir.path().to_owned());
        let local = Collection::new("Saved locally".into());
        let other = Collection::new("From the cloud".into());
        let snapshot = Snapshot::from_local(Workspace {
            collections: vec![other.clone()],
            ..Workspace::default()
        });
        let checkpoint = Checkpoint {
            tag: "\"new\"".into(),
            hash: snapshot.fingerprint().unwrap(),
        };

        let before_save = Snapshot::from_local(Workspace::default())
            .fingerprint()
            .unwrap();
        app.write(vec![Change::PutCollection(local.clone())]);
        assert!(matches!(
            install(&mut app, snapshot.clone(), checkpoint.clone(), before_save),
            Phase::Retry
        ));
        let after_save = app.store.as_ref().unwrap().now(move |db| {
            assert!(db.load_cloud::<Profile>()?.is_none());
            let workspace = db.load_workspace()?;
            assert_eq!(workspace.collections[0].id, local.id);
            Ok(Snapshot::from_local(workspace).fingerprint().unwrap())
        });

        assert!(matches!(
            install(&mut app, snapshot, checkpoint.clone(), after_save),
            Phase::Current
        ));
        assert_eq!(app.collections[0].id, other.id);
        app.store.as_ref().unwrap().now(move |db| {
            assert_eq!(db.load_workspace()?.collections[0].id, other.id);
            assert_eq!(
                db.load_cloud::<Profile>()?.unwrap().checkpoint,
                Some(checkpoint)
            );
            Ok(())
        });
    }
}
