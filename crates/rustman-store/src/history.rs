use std::borrow::Cow;
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Row, params};
use rustman_core::domain::collection::SavedRequest;
use rustman_core::domain::history::{
    HISTORY_BODY_LIMIT, HISTORY_LIMIT, HistoryEntry, HistorySummary, OmittedPart,
};
use rustman_core::time::now_millis;
use serde::{Deserialize, Serialize};

use crate::error::{DbError, atomically};
use crate::rows::{decode, readable};
use crate::session;

/// History keeps this many entries, newest first.
pub(crate) const KEEP: i64 = HISTORY_LIMIT as i64;

const ENTRY_COLUMNS: &str = "id, timestamp, method, url, status, duration, request_json";

/// The newest entries, at most `limit`. Unreadable rows are skipped.
pub(crate) fn recent(conn: &Connection, limit: i64) -> Result<Vec<HistoryEntry>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM history ORDER BY timestamp DESC, id DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map([limit], entry_row)?;
    Ok(rows
        .filter_map(|row| readable(row, "history entry"))
        .collect())
}

/// The newest entries without their requests, which are never read.
pub(crate) fn summaries(conn: &Connection, limit: i64) -> Result<Vec<HistorySummary>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, method, url, status, duration \
         FROM history ORDER BY timestamp DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| {
        Ok(Some(HistorySummary {
            id: r.get(0)?,
            timestamp: r.get(1)?,
            method: r.get(2)?,
            url: r.get(3)?,
            status: r.get(4)?,
            duration_ms: r.get(5)?,
        }))
    })?;
    Ok(rows
        .filter_map(|row| readable(row, "history entry"))
        .collect())
}

/// The entry with `id`, or `None` when there is none or it doesn't decode.
pub(crate) fn entry(conn: &Connection, id: &str) -> Result<Option<HistoryEntry>, DbError> {
    let entry = conn
        .query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM history WHERE id = ?1"),
            [id],
            entry_row,
        )
        .optional()?;
    Ok(entry.flatten())
}

fn entry_row(r: &Row<'_>) -> rusqlite::Result<Option<HistoryEntry>> {
    let id: String = r.get(0)?;
    let json = r.get_ref(6)?.as_str()?;
    let Some(request) = decode(json, "history entry", &id) else {
        return Ok(None);
    };
    Ok(Some(HistoryEntry {
        id,
        timestamp: r.get(1)?,
        method: r.get(2)?,
        url: r.get(3)?,
        status: r.get(4)?,
        duration_ms: r.get(5)?,
        request,
        omitted: omissions(json),
    }))
}

/// A history request as stored: the request, plus what was left out of it.
/// An entry that lost nothing is the request's JSON alone.
#[derive(Serialize)]
struct StoredRequest<'a> {
    #[serde(flatten)]
    request: &'a SavedRequest,
    #[serde(skip_serializing_if = "<[OmittedPart]>::is_empty")]
    history_omitted: &'a [OmittedPart],
}

/// What [`add`] left out of a stored request. Only entries that lost
/// something carry the key, so the rest skip the second parse.
fn omissions(json: &str) -> Vec<OmittedPart> {
    #[derive(Deserialize)]
    struct Omissions {
        #[serde(default)]
        history_omitted: Vec<OmittedPart>,
    }
    if !json.contains("\"history_omitted\"") {
        return Vec::new();
    }
    serde_json::from_str::<Omissions>(json).map_or_else(|_| Vec::new(), |o| o.history_omitted)
}

/// Records `entry` without request content over `limit` bytes, and drops
/// whatever falls out of the newest [`KEEP`].
pub(crate) fn add(
    conn: &Connection,
    mut entry: Cow<'_, HistoryEntry>,
    limit: usize,
    cloud_target: Option<&str>,
) -> Result<(), DbError> {
    atomically(conn, || {
        if let Some(target) = cloud_target
            && let Some(cutoff) = cleared_at(conn, target)?
            && entry.timestamp <= cutoff
        {
            entry.to_mut().timestamp = cutoff.saturating_add(1);
        }
        insert(conn, entry, limit)?;
        prune(conn)
    })
}

fn insert(
    conn: &Connection,
    mut entry: Cow<'_, HistoryEntry>,
    limit: usize,
) -> Result<(), DbError> {
    if entry.has_large_parts(limit) {
        entry.to_mut().omit_large_parts(limit);
    }
    let request = serde_json::to_string(&StoredRequest {
        request: &entry.request,
        history_omitted: &entry.omitted,
    })?;
    conn.execute(
        "INSERT OR REPLACE INTO history (id,timestamp,method,url,status,duration,request_json) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            entry.id,
            entry.timestamp,
            entry.method,
            entry.url,
            entry.status,
            entry.duration_ms,
            request
        ],
    )?;
    Ok(())
}

fn prune(conn: &Connection) -> Result<(), DbError> {
    conn.execute(
        "DELETE FROM history WHERE id NOT IN \
         (SELECT id FROM history ORDER BY timestamp DESC, id DESC LIMIT ?1)",
        [KEEP],
    )?;
    Ok(())
}

fn clear_key(target: &str) -> String {
    format!("history_cleared_at:{target}")
}

pub(crate) fn cleared_at(conn: &Connection, target: &str) -> Result<Option<i64>, DbError> {
    session::load_named(conn, &clear_key(target))
}

pub(crate) fn merge(
    conn: &Connection,
    target: &str,
    remote_clear: Option<i64>,
    incoming: &[HistoryEntry],
) -> Result<(), DbError> {
    atomically(conn, || {
        let local_clear = cleared_at(conn, target)?;
        if let Some(cutoff) = remote_clear.filter(|&time| Some(time) > local_clear) {
            conn.execute("DELETE FROM history WHERE timestamp <= ?1", [cutoff])?;
            session::save_named(conn, &clear_key(target), &cutoff)?;
        }
        let cutoff = local_clear.max(remote_clear);
        let mut known: HashSet<_> = summaries(conn, KEEP)?
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        for entry in incoming
            .iter()
            .filter(|entry| cutoff.is_none_or(|time| entry.timestamp > time))
        {
            if known.insert(entry.id.clone()) {
                insert(conn, entry.into(), HISTORY_BODY_LIMIT)?;
            }
        }
        prune(conn)
    })
}

pub(crate) fn clear(conn: &Connection) -> Result<(), DbError> {
    conn.execute("DELETE FROM history", [])?;
    Ok(())
}

pub(crate) fn clear_cloud(conn: &Connection, target: &str) -> Result<(), DbError> {
    atomically(conn, || {
        let newest: Option<i64> =
            conn.query_row("SELECT MAX(timestamp) FROM history", [], |r| r.get(0))?;
        let cutoff = newest
            .max(cleared_at(conn, target)?)
            .unwrap_or(i64::MIN)
            .max(now_millis());
        clear(conn)?;
        session::save_named(conn, &clear_key(target), &cutoff)
    })
}
