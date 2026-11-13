// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide session registry mapping [`SessionId`] and local `u64` IDs, plus seamless-handoff manifest projection.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use aterm_core::terminal::{RenderDebt, Terminal};
use aterm_session::{LaunchNonce, SessionId};
use aterm_types::control_request::Selector;

use crate::SessionCtx;

/// Lifecycle state of a registered terminal session (`Spawning`, `Alive`, or `Exited`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionState {
    Spawning,
    Alive,
    Exited,
}

impl SessionState {
    /// Stable wire token for the `sessions` control verb.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SessionState::Spawning => "spawning",
            SessionState::Alive => "alive",
            SessionState::Exited => "exited",
        }
    }

    /// Parses a [`SessionState`] from its wire token, defaulting unknown tokens to `Spawning`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "alive" => SessionState::Alive,
            "exited" => SessionState::Exited,
            _ => SessionState::Spawning,
        }
    }
}

/// Serializable per-session metadata carried across a seamless re-exec handoff.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionRecord {
    pub local_id: u64,
    pub sid: String,
    pub parent: Option<String>,
    pub state: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenCarry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub land_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen_path: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_holder: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rekey: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loader: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_dropped: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_withheld: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_lost: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_hold"
    )]
    pub hold: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_hold_rows"
    )]
    pub more_holds: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub supervisor: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "lenient_flag"
    )]
    pub claim_known: bool,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub attention_owners: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport_from_bottom: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub render_debt: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub render_debt_cut: u64,
    #[serde(
        default,
        skip_serializing_if = "crate::fabric::FabricCarry::is_empty",
        deserialize_with = "lenient_fabric"
    )]
    pub fabric: crate::fabric::FabricCarry,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub timeline_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_key: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub task: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub log_ids: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub status_seen: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub autopilot_end: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "is_zero",
        deserialize_with = "lenient_count"
    )]
    pub log_seq: u64,
    /// The session's book of declared wakes (`crate::occupancy_wakes::Carry`, one JSON row),
    /// read back leniently: a row this build cannot read restores nothing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_row"
    )]
    pub wakes: Option<String>,
}

/// Formats a `(pid, local_id)` crash-journal key as `<pid>:<id>`.
#[cfg(any(unix, test))]
pub(crate) fn journal_key_row((pid, id): (u32, u64)) -> String {
    format!("{pid}:{id}")
}

/// Parses a `<pid>:<id>` crash-journal key, rejecting PID `0`.
pub(crate) fn parse_journal_key(row: &str) -> Option<(u32, u64)> {
    let (pid, id) = row.split_once(':')?;
    if !pid.bytes().all(|b| b.is_ascii_digit()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((
        pid.parse::<u32>().ok().filter(|pid| *pid != 0)?,
        id.parse::<u64>().ok()?,
    ))
}

/// Encodes an optional [`RenderDebt`] into its manifest `(row_debt, cut_cols)` fields.
#[cfg(any(unix, test))]
pub(crate) fn render_debt_keys(debt: Option<RenderDebt>) -> (Option<String>, u64) {
    let RenderDebt { shift, pushed, cut } = debt.filter(|debt| debt.owed()).unwrap_or_default();
    (
        (shift != 0 || pushed != 0).then(|| format!("{shift},{pushed}")),
        u64::from(cut),
    )
}

/// Reconstructs a [`RenderDebt`] from its manifest `(row_debt, cut_cols)` fields.
pub(crate) fn parse_render_debt(row: Option<&str>, cut: u64) -> Option<RenderDebt> {
    let parse_signed = |s: &str| -> Option<i32> {
        let s = s.trim();
        let valid = match s.strip_prefix('-') {
            Some(rest) => {
                !rest.is_empty()
                    && !rest.starts_with('0')
                    && rest.bytes().all(|b| b.is_ascii_digit())
            }
            None => !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
        };
        valid.then(|| s.parse::<i32>().ok()).flatten()
    };
    let (shift, pushed) = match row {
        Some(row) => {
            let (shift, pushed) = row.split_once(',')?;
            (parse_signed(shift)?, parse_signed(pushed)?)
        }
        None => (0, 0),
    };
    let debt = RenderDebt {
        shift,
        pushed,
        cut: u16::try_from(cut).ok()?,
    };
    (debt.owed()
        && shift.unsigned_abs() <= 4096
        && pushed.unsigned_abs() <= 4096
        && debt.cut <= 4096)
        .then_some(debt)
}

impl SessionRecord {
    /// Stores `debt` onto this session record's manifest fields.
    #[cfg(unix)]
    pub(crate) fn carry_render_debt(&mut self, debt: Option<RenderDebt>) {
        (self.render_debt, self.render_debt_cut) = render_debt_keys(debt);
    }

    /// Reconstructs the [`RenderDebt`] carried on this session record.
    pub(crate) fn carried_render_debt(&self) -> Option<RenderDebt> {
        parse_render_debt(self.render_debt.as_deref(), self.render_debt_cut)
    }

    /// Returns all hold wire rows carried on this session record.
    pub(crate) fn hold_rows(&self) -> Vec<String> {
        self.hold.iter().chain(&self.more_holds).cloned().collect()
    }

    /// Returns whether the successor's in-GUI host should grant adopted-claim grace to this session.
    #[must_use]
    pub(crate) const fn claim_grace(&self) -> bool {
        !self.claim_known || self.supervisor.is_some()
    }
}

fn lenient_fabric<'de, D>(deserializer: D) -> Result<crate::fabric::FabricCarry, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Carry {
        Read(Box<crate::fabric::FabricCarry>),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Carry as serde::Deserialize>::deserialize(deserializer)? {
            Carry::Read(carry) => *carry,
            Carry::Unreadable(_) => crate::fabric::FabricCarry::default(),
        },
    )
}

fn lenient_count<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Count {
        Read(u64),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Count as serde::Deserialize>::deserialize(deserializer)? {
            Count::Read(n) => n,
            Count::Unreadable(_) => 0,
        },
    )
}

fn lenient_row<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    lenient_hold(deserializer)
}

fn lenient_rows<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Rows {
        List(Vec<Row>),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Rows as serde::Deserialize>::deserialize(deserializer)? {
            Rows::List(rows) => rows
                .into_iter()
                .filter_map(|row| match row {
                    Row::Text(text) => Some(text),
                    Row::Unreadable(_) => None,
                })
                .collect(),
            Rows::Unreadable(_) => Vec::new(),
        },
    )
}

fn lenient_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(matches!(
        <Flag as serde::Deserialize>::deserialize(deserializer)?,
        Flag::Bool(true)
    ))
}

fn lenient_hold<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(Some(
        match <Row as serde::Deserialize>::deserialize(deserializer)? {
            Row::Text(row) => row,
            Row::Unreadable(_) => String::new(),
        },
    ))
}

fn lenient_hold_rows<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Row {
        Text(String),
        Unreadable(serde::de::IgnoredAny),
    }
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Rows {
        List(Vec<Row>),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Rows as serde::Deserialize>::deserialize(deserializer)? {
            Rows::List(rows) => rows
                .into_iter()
                .map(|row| match row {
                    Row::Text(text) => text,
                    Row::Unreadable(_) => String::new(),
                })
                .collect(),
            Rows::Unreadable(_) => vec![String::new()],
        },
    )
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_zero(count: &u64) -> bool {
    *count == 0
}

/// Returns whether `outgoing_build` predates automatic `<prefix>/bin` PATH self-healing.
#[must_use]
pub(crate) const fn predates_path_self_heal(outgoing_build: Option<u64>) -> bool {
    outgoing_build.is_none()
}

/// Terminal screen checkpoint metadata carried in the TOML handoff manifest.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ScreenCarry {
    #[serde(default)]
    pub schema: u32,
    pub meta: String,
    pub grid_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_grid_file: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repaint: bool,
}

impl ScreenCarry {
    pub(crate) const SCHEMA: u32 = 1;
}

/// Per-window grid dimensions, outer position, and chrome state carried across a seamless update.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct WindowCarry {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub outer_x: Option<i32>,
    #[serde(default)]
    pub outer_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_px: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presence_row: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<CarriedMessage>,
    #[serde(default)]
    pub next_message_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_verified_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_px_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_reset_px_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_reset_size_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub update_health_said: Vec<CarriedHealthSaid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_captured_unix_ms: Option<u64>,
}

/// Unanswered update-health warning carried from a parent instance across an update.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CarriedHealthSaid {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub line: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owed: bool,
}

/// Live message rows and undrained log lines carried inside [`WindowCarry`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
#[cfg(any(unix, test))]
pub(crate) struct MessagesCarry {
    pub messages: Vec<CarriedMessage>,
    pub next_message_id: u64,
    pub update_health_said: Vec<CarriedHealthSaid>,
    pub captured_unix_ms: Option<u64>,
}

#[cfg(any(unix, test))]
impl From<&aterm_messages::Carry> for MessagesCarry {
    fn from(carry: &aterm_messages::Carry) -> Self {
        Self {
            messages: carry.live.iter().map(CarriedMessage::from).collect(),
            next_message_id: carry.next_id,
            update_health_said: Vec::new(),
            captured_unix_ms: None,
        }
    }
}

impl WindowCarry {
    /// Converts carried messages into [`aterm_messages::Carry`] for seeding the successor's message center.
    pub fn message_carry(&self) -> aterm_messages::Carry {
        aterm_messages::Carry {
            next_id: self.next_message_id,
            live: self
                .messages
                .iter()
                .map(aterm_messages::CarriedMessage::from)
                .collect(),
        }
    }
}

/// Serializable projection of a single live [`aterm_messages::CarriedMessage`].
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct CarriedMessage {
    pub id: u64,
    pub unix_ms: u64,
    pub tag: String,
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_severity: Option<String>,
    pub glyph: char,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    pub hold: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_permille: Option<u16>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stats: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wire: bool,
    #[serde(
        default = "carried_repeats",
        skip_serializing_if = "is_carried_repeats"
    )]
    pub repeats: u32,
}

const fn carried_repeats() -> u32 {
    1
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_carried_repeats(repeats: &u32) -> bool {
    *repeats == 1
}

impl From<&aterm_messages::CarriedMessage> for CarriedMessage {
    fn from(m: &aterm_messages::CarriedMessage) -> Self {
        Self {
            id: m.id,
            unix_ms: m.unix_ms,
            tag: m.tag.clone(),
            severity: m.severity.clone(),
            posted_severity: m.posted_severity.clone(),
            glyph: m.glyph,
            title: m.title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            finished: m.finished.clone(),
            wire: m.wire,
            repeats: m.repeats,
        }
    }
}

impl From<&CarriedMessage> for aterm_messages::CarriedMessage {
    fn from(m: &CarriedMessage) -> Self {
        Self {
            id: m.id,
            unix_ms: m.unix_ms,
            tag: m.tag.clone(),
            severity: m.severity.clone(),
            posted_severity: m.posted_severity.clone(),
            glyph: m.glyph,
            title: m.title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            finished: m.finished.clone(),
            wire: m.wire,
            repeats: m.repeats,
        }
    }
}

/// Tokenless `(src, dst, op)` fabric connection edge carried across a seamless update.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConnectionCarry {
    pub src: String,
    pub dst: String,
    pub op: String,
}

/// Top-level TOML manifest for seamless re-exec handoff (`SessionHandoff`).
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionHandoff {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<aterm_json::Value>,
    pub sessions: Vec<SessionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowCarry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ConnectionCarry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_turn_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_build: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldPaneCarry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub fabric_attached: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub mirror_dropped: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror_served: bool,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub machine_names: Vec<String>,
    /// The workers aterm closed once their tasks concluded
    /// ([`crate::retired_workers::RetiredWorkers::carry_rows`]): the successor answers `result`
    /// and `await task` for them as this process did, and drops their result files as they age.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_rows"
    )]
    pub retired_workers: Vec<String>,
}

/// Final screen payload carried for an exited `--hold` pane.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HeldPaneCarry {
    pub local_id: u64,
    pub screen: ScreenCarry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub history_dropped: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_withheld: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history_failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failure_seen: bool,
}

#[cfg(any(unix, test))]
struct Drawn {
    meta: crate::session_timeline::SessionMeta,
    supervisor: Option<String>,
    claim_known: bool,
    attention_owners: Vec<String>,
    topics: Vec<String>,
    task: Option<String>,
}

#[cfg(any(unix, test))]
impl Drawn {
    fn of(ctx: &SessionCtx, now_us: u64, own_holder: &str) -> Self {
        let (meta, (supervisor, claim_known), attention_owners) = {
            let live = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (
                live.sanitized(),
                live.handoff_supervisor(now_us, own_holder),
                live.handoff_attention_owners(),
            )
        };
        let (task, reexec) = {
            let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (timeline.task_carry_row(), timeline.in_self_reexec(now_us))
        };
        Self {
            meta,
            // In the gap of a proven self-claim's own re-exec the claim is given back and
            // owed again: this process cannot vouch for what holds the session next, so the
            // successor holds its loop off as for a proven claim (`SessionRecord::claim_grace`).
            claim_known: claim_known && !(supervisor.is_none() && reexec),
            supervisor,
            attention_owners,
            topics: crate::fabric::render_topics(&ctx.fabric.topics()),
            task,
        }
    }
}

/// Mutable claim, hold and Autopilot state sampled at handoff park.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedClaims {
    pub local_id: u64,
    meta: crate::session_timeline::SessionMeta,
    topics: Vec<String>,
    attention_owners: Vec<String>,
    supervisor: Option<String>,
    claim_known: bool,
    /// The task record's carry row without its ages ([`crate::task_record::TaskRecord::carry_face`]).
    task: Option<String>,
    /// A proven self-claim's task face, which no loop restates at the successor (`App::self_claim_face`): set beside the record's claims at the park, and beside the live ones at the Commit ([`Self::with_self_face`]).
    self_face: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
}

#[cfg(unix)]
impl CarriedClaims {
    /// These claims with session's self-claim face `face` beside them.
    #[must_use]
    pub(crate) const fn with_self_face(
        mut self,
        face: Option<(aterm_agent::harness::acts::TaskFace, i64)>,
    ) -> Self {
        self.self_face = face;
        self
    }

    /// Extracts [`CarriedClaims`] from a parked [`SessionRecord`].
    #[must_use]
    pub(crate) fn of_record(record: &SessionRecord) -> Self {
        Self {
            local_id: record.local_id,
            meta: crate::session_timeline::SessionMeta {
                user_title: record.user_title.clone(),
                description: record.description.clone(),
                icon: record.icon.clone(),
                role: record.role.clone(),
                attention: record.attention.clone(),
                questions: record.questions.clone(),
                autopilot: record.autopilot.clone(),
                land_at: record.land_at,
                ..Default::default()
            },
            topics: record.topics.clone(),
            attention_owners: record.attention_owners.clone(),
            supervisor: record.supervisor.as_deref().map(lease_holder),
            claim_known: record.claim_known,
            task: record
                .task
                .as_deref()
                .map(crate::task_record::TaskRecord::carry_face),
            self_face: None,
        }
    }

    /// Extracts live [`CarriedClaims`] from `ctx` and `own_holder`.
    #[must_use]
    pub(crate) fn of_session(
        local_id: u64,
        ctx: &SessionCtx,
        now_us: u64,
        own_holder: &str,
    ) -> Self {
        let drawn = Drawn::of(ctx, now_us, own_holder);
        Self {
            local_id,
            meta: drawn.meta,
            topics: drawn.topics,
            attention_owners: drawn.attention_owners,
            supervisor: drawn.supervisor.as_deref().map(lease_holder),
            claim_known: drawn.claim_known,
            task: drawn
                .task
                .as_deref()
                .map(crate::task_record::TaskRecord::carry_face),
            self_face: None,
        }
    }

    /// Project the final Commit while its caller holds the session's meta and timeline locks.
    /// Topics were sampled under the fabric hold fence before those locks were acquired.
    #[must_use]
    pub(crate) fn of_guarded(
        local_id: u64,
        meta: &crate::session_timeline::SessionMeta,
        timeline: &crate::session_timeline::SessionTimeline,
        topics: Vec<String>,
        now_us: u64,
        own_holder: &str,
    ) -> Self {
        let (supervisor, claim_known) = meta.handoff_supervisor(now_us, own_holder);
        let reexec = timeline.in_self_reexec(now_us);
        let carried = timeline.carried_task_face();
        let self_face = if meta.supervisor_proven(now_us) || reexec {
            timeline.task_face().or(carried)
        } else {
            carried
        };
        Self {
            local_id,
            meta: meta.sanitized(),
            topics,
            attention_owners: meta.handoff_attention_owners(),
            claim_known: claim_known && !(supervisor.is_none() && reexec),
            supervisor: supervisor.as_deref().map(lease_holder),
            task: timeline
                .task_carry_row()
                .as_deref()
                .map(crate::task_record::TaskRecord::carry_face),
            self_face,
        }
    }
}

#[cfg(unix)]
fn lease_holder(row: &str) -> String {
    row.split_once(' ')
        .map_or(row, |(holder, _)| holder)
        .to_string()
}

impl SessionHandoff {
    pub(crate) const SCHEMA: u32 = 1;

    /// Projects all sessions in `store` into a [`SessionHandoff`] manifest in `local_id` order.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn from_store(store: &SessionStore) -> Self {
        let handles = store.snapshot();
        let mut connections: Vec<ConnectionCarry> = Vec::new();
        for h in &handles {
            let rows = {
                let edges = h.ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
                edges.edges()
            };
            connections.extend(rows.into_iter().map(|e| ConnectionCarry {
                src: e.src.as_str().to_string(),
                dst: e.dst.as_str().to_string(),
                op: e.op.as_str().to_string(),
            }));
        }
        connections.sort_by(|a, b| (&a.src, &a.dst, &a.op).cmp(&(&b.src, &b.dst, &b.op)));
        connections.dedup();
        let now_us = crate::metrics::now_us();
        let own_holder = crate::harness_host::holder();
        let mut post_budget = crate::fabric::CARRY_MAIL_BYTES;
        Self {
            schema: Self::SCHEMA,
            resources: crate::resource_jobs::carry(),
            window: None,
            connections,
            next_turn_id: None,
            outgoing_build: Some(crate::running_build_number()),
            held: Vec::new(),
            roster_seq: Some(store.roster_seq().min(i64::MAX as u64)),
            #[cfg(unix)]
            fabric_attached: crate::fabric_launch::handoff_attached(),
            #[cfg(not(unix))]
            fabric_attached: Vec::new(),
            mirror_dropped: store.mirror.draw_tombstones(),
            mirror_served: store.mirror.served(),
            machine_names: crate::tab_label::machine_names_for_handoff(),
            retired_workers: store.retired_workers.carry_rows(std::time::Instant::now()),
            sessions: handles
                .into_iter()
                .map(|h| {
                    let Drawn {
                        meta,
                        supervisor,
                        claim_known,
                        attention_owners,
                        topics,
                        task,
                    } = Drawn::of(&h.ctx, now_us, &own_holder);
                    let (timeline_id, log_ids, log_seq) = {
                        let mut tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        (tl.handoff_id(), tl.handoff_log_ids(), tl.handoff_log_seq())
                    };
                    let mut hold_rows =
                        crate::fabric::render_holds(&h.ctx.fabric.holds()).into_iter();
                    SessionRecord {
                        local_id: h.local_id,
                        sid: h.sid.as_str().to_string(),
                        parent: h.parent.as_ref().map(|p| p.as_str().to_string()),
                        state: h.state.as_str().to_string(),
                        title: h.title.clone(),
                        screen: None,
                        user_title: meta.user_title.clone(),
                        description: meta.description.clone(),
                        icon: meta.icon.clone(),
                        role: meta.role.clone(),
                        attention: meta.attention.clone(),
                        questions: meta.questions.clone(),
                        autopilot: meta.autopilot.clone(),
                        land_at: meta.land_at,
                        control: None,
                        frozen_path: store.has_frozen_path(h.local_id),
                        topics,
                        fg_holder: None,
                        rekey: store.has_rekey_channel(h.local_id),
                        loader: store.has_body_loader(h.local_id),
                        history: None,
                        history_dropped: 0,
                        history_withheld: false,
                        history_lost: store.history_lost(h.local_id),
                        hold: hold_rows.next(),
                        more_holds: hold_rows.collect(),
                        supervisor,
                        claim_known,
                        attention_owners,
                        viewport_from_bottom: None,
                        render_debt: None,
                        render_debt_cut: 0,
                        fabric: h.ctx.fabric.handoff_carry(&mut post_budget),
                        timeline_id,
                        journal_key: None,
                        task,
                        log_ids: Some(log_ids),
                        status_seen: None,
                        autopilot_end: None,
                        log_seq,
                        wakes: crate::occupancy_wakes::carry_row(h.sid.as_str()),
                    }
                })
                .collect(),
        }
    }

    /// Serializes the handoff manifest to a TOML string.
    #[cfg(any(unix, test))]
    pub(crate) fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize handoff: {e}"))
    }

    /// Deserializes a [`SessionHandoff`] from TOML, returning `None` on version mismatch or invalid input.
    #[must_use]
    pub(crate) fn from_toml(s: &str) -> Option<Self> {
        let h: Self = aterm_toml::from_str(s).ok()?;
        (h.schema == Self::SCHEMA).then_some(h)
    }

    /// Returns whether `self` round-trips through `to_toml` and `from_toml` without loss.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn roundtrips(&self) -> bool {
        match self.to_toml() {
            Ok(text) => Self::from_toml(&text).as_ref() == Some(self),
            Err(_) => false,
        }
    }
}

/// Ordered PTY master file descriptors carried alongside a [`SessionHandoff`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct HandoffFds {
    pub entries: Vec<(u64, i32, i32)>,
}

impl HandoffFds {
    /// Encodes the descriptor list into the `ATERM_SEAMLESS_FDS` wire format.
    #[must_use]
    #[cfg(any(unix, test))]
    pub fn encode(&self) -> String {
        self.entries
            .iter()
            .map(|(lid, fd, pid)| format!("{lid}={fd}:{pid}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Live session entry held in [`SessionStore`], pairing [`SessionId`], `local_id`, `Terminal`, and [`SessionCtx`].
#[derive(Clone)]
pub(crate) struct SessionHandle {
    pub sid: SessionId,
    pub nonce: LaunchNonce,
    pub local_id: u64,
    pub parent: Option<SessionId>,
    pub state: SessionState,
    pub title: String,
    pub term: Arc<Mutex<Terminal>>,
    pub master: i32,
    /// The PTY child as the session took it, the root of the session's process tree (pid `0`:
    /// none): its birth read while the pid provably named it (this process's unreaped child, or
    /// the leader of the PTY it adopted), so a pid reused since cannot pass for it.
    pub shell_identity: aterm_pty::ShellIdentity,
    pub ctx: Arc<SessionCtx>,
}

impl SessionHandle {
    /// Returns the session's user-assigned title if set, or its sanitized registry title.
    pub(crate) fn display_title(&self) -> String {
        self.ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get("title")
            .map_or_else(
                || crate::chrome_label::without_spinner_frame(&self.title).to_string(),
                str::to_string,
            )
    }
}

pub(crate) const ROSTER_JOURNAL_CAP: usize = 512;

/// Membership change kind (`Spawned` or `Exited`) recorded on the roster lifecycle journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RosterChange {
    Created,
    Exited,
    RetireRequested,
    #[cfg(any(unix, test))]
    Bye(aterm_types::fabric_grammar::Bye),
    /// A clean quit closed its sessions' logs (`out=app-quit`, fix unit Q1): the line that wakes
    /// a mirroring bridge to take those closes before the quit's `bye`. Only the unix quit
    /// order says it (`fabric_launch`), so it is unix-only, tests included.
    #[cfg(unix)]
    QuitCloses,
}

/// Reason a session left the registry (`reason=<token>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitReason {
    ShellExit,
    CtlClose,
    UiClose,
    WindowClose,
    #[cfg(test)]
    AppQuit,
    /// aterm's reaper of workers' tabs closed it once its task concluded (`App::reap_workers`).
    Reaper,
    Unknown,
}

impl ExitReason {
    #[cfg(test)]
    pub const ALL: [ExitReason; 7] = [
        ExitReason::ShellExit,
        ExitReason::CtlClose,
        ExitReason::UiClose,
        ExitReason::WindowClose,
        ExitReason::AppQuit,
        ExitReason::Reaper,
        ExitReason::Unknown,
    ];

    /// Stable wire token for `reason=<token>`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExitReason::ShellExit => "shell-exit",
            ExitReason::CtlClose => "ctl-close",
            ExitReason::UiClose => "ui-close",
            ExitReason::WindowClose => "window-close",
            #[cfg(test)]
            ExitReason::AppQuit => "app-quit",
            ExitReason::Reaper => "reaper",
            ExitReason::Unknown => "unknown",
        }
    }
}

/// Actor that retired a session (`by=<sid|human|ctl|bridge|harness|fleet:<head>:<sid>|->`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExitActor {
    /// A session: an edge-scoped caller's own sid, or the tab an owner-token `aterm ctl` ran
    /// from (`crate::control::control_session::caller_actor_of`).
    Sid(String),
    Human,
    /// An owner-token client aterm could not place in any tab (a script, a launchd job).
    Ctl,
    /// aterm's own harness, which ended a worker of its own.
    Harness,
    #[cfg(any(unix, test))]
    Bridge,
    /// Another Mac's drive over the fleet: the whole `fleet:<head>:<sid>` token
    /// ([`ExitActor::fleet_drive`]), and the whole node its handshake proved, which the token
    /// names only the head of.
    Fleet {
        token: String,
        node: String,
    },
    Unknown,
}

impl ExitActor {
    /// The drive of the proven `node` for its requester's session `sid`
    /// ([`aterm_slog::Actor::fleet_drive`]'s token), or [`ExitActor::Unknown`] for a node with
    /// nothing to name.
    pub(crate) fn fleet_drive(node: &str, sid: Option<&str>) -> Self {
        match aterm_slog::Actor::fleet_drive(node, sid) {
            aterm_slog::Actor::Fleet(token) => ExitActor::Fleet {
                token: token.as_str().to_string(),
                node: node.trim().to_string(),
            },
            _ => ExitActor::Unknown,
        }
    }

    /// Stable wire token for `by=<actor>`.
    #[must_use]
    pub(crate) const fn as_wire(&self) -> &str {
        match self {
            ExitActor::Sid(sid) => sid.as_str(),
            ExitActor::Human => "human",
            ExitActor::Ctl => "ctl",
            ExitActor::Harness => "harness",
            #[cfg(any(unix, test))]
            ExitActor::Bridge => "bridge",
            ExitActor::Fleet { token, .. } => token.as_str(),
            ExitActor::Unknown => "-",
        }
    }

    /// The proven node of a fleet drive; `None` for an actor on this Mac.
    #[must_use]
    pub(crate) fn fleet_node(&self) -> Option<&str> {
        match self {
            ExitActor::Fleet { node, .. } => Some(node),
            _ => None,
        }
    }
}

/// Single membership-change entry in the bounded roster lifecycle journal:
/// the registry's membership feed. An exit's code and closer are the
/// closed-session index's ([`crate::session_log::ClosedExit`]), which
/// `exits` reads; the roster's copies were deleted in phase 6 (6G-9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterRecord {
    pub seq: u64,
    pub sid: String,
    pub local_id: u64,
    pub change: RosterChange,
    pub t_ms: u64,
    pub reason: ExitReason,
}

/// Process-wide session registry indexed by both [`SessionId`] and local `u64` ID.
#[derive(Default)]
pub(crate) struct SessionStore {
    #[cfg(target_os = "macos")]
    allocation_monitor: Arc<crate::allocation_monitor_host::Host>,
    by_id: HashMap<SessionId, SessionHandle>,
    by_local: HashMap<u64, SessionId>,
    roster: VecDeque<RosterRecord>,
    roster_seq: u64,
    instance_watchers: std::sync::Arc<crate::session_watch::Watchers>,
    exit_codes: HashMap<u64, i32>,
    frozen_path: std::collections::HashSet<u64>,
    rekey_channel: std::collections::HashSet<u64>,
    body_loader: std::collections::HashSet<u64>,
    integration_frozen: std::collections::HashSet<u64>,
    history_lost: HashMap<u64, u64>,
    harness_posture: crate::autopilot::HarnessPosture,
    closed_logs: crate::session_log::ClosedLogs,
    retired_workers: crate::retired_workers::RetiredWorkers,
    journal_retirements: Option<crate::session_log_journal::Retirements>,
    journal_durable: crate::crash_journal::Durable,
    input_replies: crate::control::InputReplies,
    mirror: crate::session_log_mirror::StoreMirror,
}

/// Shared thread-safe handle to [`SessionStore`].
pub(crate) type Store = Arc<RwLock<SessionStore>>;

/// What [`SessionStore::deregister_closing`] did.
#[must_use]
pub(crate) struct Deregistered {
    /// The sid that left.
    pub(crate) sid: SessionId,
    /// The roster seq of its `Exited` record and closed-index slot; `None` when nothing was recorded.
    pub(crate) exit_seq: Option<u64>,
    /// What the slot's link freed, to drop once the store's lock is released.
    pub(crate) freed: Vec<Arc<aterm_slog::Log>>,
    /// Why it left, as the exits ledger records it (an unknown close of an exited shell is its
    /// shell's exit); `None` when nothing was recorded.
    pub(crate) reason: Option<ExitReason>,
    /// Its shell's exit code, where one was read.
    pub(crate) exit_code: Option<i32>,
}

/// Creates a new empty shared [`Store`].
#[must_use]
pub(crate) fn new_store() -> Store {
    Arc::new(RwLock::new(SessionStore::default()))
}

impl SessionStore {
    #[cfg(target_os = "macos")]
    pub(crate) fn allocation_monitor(&self) -> Arc<crate::allocation_monitor_host::Host> {
        Arc::clone(&self.allocation_monitor)
    }

    /// Registers or replaces a [`SessionHandle`], updating both indices and appending to the roster journal.
    pub(crate) fn register(&mut self, handle: SessionHandle) {
        handle.ctx.native_policy.registered();
        let first = !self.by_id.contains_key(&handle.sid);
        if first {
            let mut tl = handle
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            debug_assert!(
                std::sync::Arc::ptr_eq(tl.watchers(), &handle.ctx.watchers),
                "a session context not built from one session_watch::Observed"
            );
            tl.slog_mut().bind_mirror(self.mirror.shared());
            tl.slog_mut()
                .bind_person(std::sync::Arc::clone(&handle.ctx.human_input));
            tl.slog_mut()
                .bind_screen(std::sync::Arc::clone(&handle.ctx.screen_moves));
            let at = tl.slog().now();
            tl.slog_mut().open_session(
                handle.parent.as_ref().map(SessionId::as_str),
                Some(handle.state.as_str()),
                at,
            );
            tl.record("spawned");
            drop(tl);
            #[cfg(not(test))]
            crate::fabric::watch_direct_mail(&handle.ctx);
        }
        let journal = first.then(|| (handle.sid.as_str().to_string(), handle.local_id));
        // A replace journals nothing, but `bridge_roster` emits these two, so a bridge holding
        // the count must learn of it: the count alone moves, and leaves a hole in the journal's
        // seqs that `roster_since` reads past.
        if self
            .by_id
            .get(&handle.sid)
            .is_some_and(|had| (had.local_id, had.nonce) != (handle.local_id, handle.nonce))
        {
            self.roster_seq += 1;
        }
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
        if let Some((sid, local_id)) = journal {
            self.record_roster(
                sid,
                local_id,
                RosterChange::Created,
                ExitReason::Unknown,
                crate::turn_ledger::now_ms(),
            );
        }
    }

    /// Appends one roster record stamped `t_ms`, evicting the oldest past [`ROSTER_JOURNAL_CAP`]; returns its seq.
    fn record_roster(
        &mut self,
        sid: String,
        local_id: u64,
        change: RosterChange,
        reason: ExitReason,
        t_ms: u64,
    ) -> u64 {
        self.roster_seq += 1;
        if self.roster.len() == ROSTER_JOURNAL_CAP {
            self.roster.pop_front();
        }
        self.roster.push_back(RosterRecord {
            seq: self.roster_seq,
            sid,
            local_id,
            change,
            t_ms,
            reason,
        });
        crate::harness_host::ring();
        self.instance_watchers.ring_watchers();
        self.roster_seq
    }

    /// Returns the instance-wide watcher list notified on every roster journal append.
    #[must_use]
    pub(crate) fn instance_watchers(&self) -> std::sync::Arc<crate::session_watch::Watchers> {
        self.instance_watchers.clone()
    }

    /// Queues a request for the fabric bridge to retire an orphaned presence row `sid`.
    pub(crate) fn request_retire(&mut self, sid: &str) -> bool {
        if self.by_sid(&SessionId::new(sid.to_string())).is_some() {
            return false;
        }
        self.record_roster(
            sid.to_string(),
            0,
            RosterChange::RetireRequested,
            ExitReason::Unknown,
            crate::turn_ledger::now_ms(),
        );
        true
    }

    /// Appends an instance-exit `bye` record to the roster journal for the fabric bridge.
    #[cfg(any(unix, test))]
    pub(crate) fn say_bye(&mut self, bye: aterm_types::fabric_grammar::Bye) {
        self.record_roster(
            "*".to_string(),
            0,
            RosterChange::Bye(bye),
            ExitReason::Unknown,
            crate::turn_ledger::now_ms(),
        );
    }

    /// Appends the quit's `quit-closes` record (fix unit Q1): every live session's log closed
    /// `out=app-quit`, the bridge's mirror owes a take ([`RosterChange::QuitCloses`]).
    #[cfg(unix)]
    pub(crate) fn say_quit_closes(&mut self) {
        self.record_roster(
            "*".to_string(),
            0,
            RosterChange::QuitCloses,
            ExitReason::Unknown,
            crate::turn_ledger::now_ms(),
        );
    }

    /// Marks `local_id` as running a shell with a pre-self-heal frozen `PATH`.
    pub(crate) fn mark_frozen_path(&mut self, local_id: u64) {
        self.frozen_path.insert(local_id);
    }

    /// Clears the frozen-`PATH` mark for `local_id` once a child process confirms `<prefix>/bin` is on `PATH`.
    pub(crate) fn clear_frozen_path(&mut self, local_id: u64) {
        self.frozen_path.remove(&local_id);
    }

    /// Returns the sorted `local_id`s of all registered sessions whose shell has not exited.
    #[must_use]
    pub(crate) fn running_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .by_local
            .keys()
            .copied()
            .filter(|id| !self.exit_codes.contains_key(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Adds `lines` to the cumulative count of scrollback lines lost across handoffs for `local_id`.
    pub(crate) fn add_history_lost(&mut self, local_id: u64, lines: u64) {
        if lines > 0 {
            let lost = self.history_lost.entry(local_id).or_insert(0);
            *lost = lost.saturating_add(lines);
        }
    }

    /// Returns the cumulative scrollback lines lost across handoffs for `local_id`.
    #[must_use]
    pub(crate) fn history_lost(&self, local_id: u64) -> u64 {
        self.history_lost.get(&local_id).copied().unwrap_or(0)
    }

    /// Returns whether `local_id` is marked as having a frozen `PATH`.
    #[must_use]
    pub(crate) fn has_frozen_path(&self, local_id: u64) -> bool {
        self.frozen_path.contains(&local_id)
    }

    /// Returns the window's current [`crate::autopilot::HarnessPosture`].
    #[must_use]
    pub(crate) const fn harness_posture(&self) -> crate::autopilot::HarnessPosture {
        self.harness_posture
    }

    /// Updates the window's published [`crate::autopilot::HarnessPosture`].
    pub(crate) const fn set_harness_posture(&mut self, posture: crate::autopilot::HarnessPosture) {
        self.harness_posture = posture;
    }

    /// Returns the shared [`crate::control::InputReplies`] tracker for pending input acknowledgements.
    pub(crate) fn input_replies(&self) -> crate::control::InputReplies {
        self.input_replies.clone()
    }

    /// Returns the crash-journal [`crate::crash_journal::Durable`] watermark tracker.
    pub(crate) fn journal_durable(&self) -> crate::crash_journal::Durable {
        self.journal_durable.clone()
    }

    /// Installs the crash-journal [`crate::crash_journal::Durable`] watermark tracker.
    pub(crate) fn set_journal_durable(&mut self, durable: crate::crash_journal::Durable) {
        self.journal_durable = durable;
    }

    /// Marks `local_id` as carrying the shell-integration rekey channel.
    pub(crate) fn mark_rekey_channel(&mut self, local_id: u64) {
        self.rekey_channel.insert(local_id);
    }

    /// Returns whether `local_id` carries the shell-integration rekey channel.
    #[must_use]
    pub(crate) fn has_rekey_channel(&self, local_id: u64) -> bool {
        self.rekey_channel.contains(&local_id)
    }

    /// Marks `local_id` as running a shell-integration body loader.
    pub(crate) fn mark_body_loader(&mut self, local_id: u64) {
        self.body_loader.insert(local_id);
    }

    /// Returns whether `local_id` runs a shell-integration body loader.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn has_body_loader(&self, local_id: u64) -> bool {
        self.body_loader.contains(&local_id)
    }

    /// Marks `local_id` as adopted with a legacy pre-loader shell integration.
    pub(crate) fn mark_integration_frozen(&mut self, local_id: u64) {
        self.integration_frozen.insert(local_id);
    }

    /// Returns whether `local_id` was adopted with a legacy pre-loader shell integration.
    #[must_use]
    pub(crate) fn is_integration_frozen(&self, local_id: u64) -> bool {
        self.integration_frozen.contains(&local_id)
    }

    /// Returns the number of live non-exited tabs marked with a frozen `PATH`.
    #[must_use]
    pub(crate) fn frozen_path_tabs(&self) -> usize {
        self.frozen_path_ids().len()
    }

    /// Returns the sorted `local_id`s of live non-exited tabs marked with a frozen `PATH`.
    #[must_use]
    pub(crate) fn frozen_path_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .frozen_path
            .iter()
            .copied()
            .filter(|id| self.by_local.contains_key(id) && !self.exit_codes.contains_key(id))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Records the child exit code for `local_id` prior to deregistration.
    pub(crate) fn note_exit_code(&mut self, local_id: u64, code: Option<i32>) {
        if let Some(code) = code {
            self.exit_codes.insert(local_id, code);
        }
    }

    /// Returns the exit code recorded by [`Self::note_exit_code`] for `local_id`.
    #[cfg(all(test, unix))]
    pub(crate) fn noted_exit_code(&self, local_id: u64) -> Option<i32> {
        self.exit_codes.get(&local_id).copied()
    }

    /// Deregisters `local_id` with default attribution.
    #[cfg(test)]
    pub(crate) fn deregister_local(&mut self, local_id: u64) -> Option<SessionId> {
        self.deregister_local_as(local_id, ExitReason::Unknown, ExitActor::Unknown)
    }

    /// [`Self::deregister_closing`]'s sid alone, what it freed dropped here: for a test that holds the store alone.
    #[cfg(test)]
    pub(crate) fn deregister_local_as(
        &mut self,
        local_id: u64,
        reason: ExitReason,
        actor: ExitActor,
    ) -> Option<SessionId> {
        self.deregister_closing(local_id, reason, actor)
            .map(|d| d.sid)
    }

    /// Deregisters `local_id` with explicit [`ExitReason`] and [`ExitActor`] attribution, linking its closed-index slot in the write that records its roster exit, so the slots stay in roster-seq order; its log is attached after by that seq ([`Self::keep_closed_log`]).
    pub(crate) fn deregister_closing(
        &mut self,
        local_id: u64,
        reason: ExitReason,
        actor: ExitActor,
    ) -> Option<Deregistered> {
        let exit_code = self.exit_codes.remove(&local_id);
        self.frozen_path.remove(&local_id);
        self.rekey_channel.remove(&local_id);
        self.body_loader.remove(&local_id);
        self.integration_frozen.remove(&local_id);
        self.history_lost.remove(&local_id);
        let sid = self.by_local.remove(&local_id)?;
        let retirements = &mut self.journal_retirements;
        let mirror = &self.mirror;
        let resolved = self.by_id.get(&sid).map(|h| {
            let reason = if reason == ExitReason::Unknown && h.state == SessionState::Exited {
                ExitReason::ShellExit
            } else {
                reason
            };
            let mut tl = h.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let at = tl.slog().now();
            let task = crate::tab_marks::task_end(&tl);
            tl.slog_mut().close_tab(
                crate::session_log::TabClose {
                    out: crate::session_log::exit_out(reason),
                    by: crate::session_log::exit_actor(&actor),
                    why: crate::tab_marks::close_why(reason),
                    task: task.as_deref(),
                },
                at,
            );
            tl.slog_mut().close_session(
                crate::session_log::exit_out(reason),
                crate::session_log::exit_actor(&actor),
                actor.as_wire(),
                exit_code,
                at,
            );
            tl.record("state-change");
            if let Some(retirements) = retirements.as_mut() {
                retirements.retire(&sid, &h.ctx.timeline, tl.slog());
            }
            mirror.retire(sid.as_str(), tl.slog_mut());
            (reason, at.mono_ms)
        });
        let mut closed = Deregistered {
            sid,
            exit_seq: None,
            freed: Vec::new(),
            reason: resolved.map(|(reason, _)| reason),
            exit_code,
        };
        if let Some((reason, t_ms)) = resolved {
            self.by_id.remove(&closed.sid);
            #[cfg(not(test))]
            crate::fabric::forget_direct_mail(&closed.sid);
            let seq = self.record_roster(
                closed.sid.as_str().to_string(),
                local_id,
                RosterChange::Exited,
                reason,
                t_ms,
            );
            closed.freed = self.closed_logs.link(crate::session_log::ClosedExit {
                seq,
                t_ms,
                sid: closed.sid.clone(),
                local_id,
                reason,
                exit_code,
                actor,
            });
            closed.exit_seq = Some(seq);
        }
        Some(closed)
    }

    /// Updates `local_id`'s [`SessionState`] and wakes watchers on transitions.
    pub(crate) fn set_state(&mut self, local_id: u64, state: SessionState) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state != state
        {
            h.state = state;
            record_state_change(&h.ctx.timeline, state);
        }
    }

    /// Transitions `local_id` from `Spawning` to `Alive`, returning `true` if transitioned.
    pub(crate) fn mark_alive(&mut self, local_id: u64) -> bool {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.state == SessionState::Spawning
        {
            h.state = SessionState::Alive;
            record_state_change(&h.ctx.timeline, SessionState::Alive);
            return true;
        }
        false
    }

    /// Updates the live title cached on `local_id`'s handle.
    pub(crate) fn set_title(&mut self, local_id: u64, title: &str) {
        if let Some(sid) = self.by_local.get(&local_id)
            && let Some(h) = self.by_id.get_mut(sid)
            && h.title != title
        {
            h.title.clear();
            h.title.push_str(title);
        }
    }

    /// Attaches a closed session's log to the slot [`Self::deregister_closing`] linked under roster seq `seq`; returns what it freed, to drop once this lock is released.
    pub(crate) fn keep_closed_log(
        &mut self,
        seq: u64,
        log: crate::session_log::ClosedLog,
    ) -> Vec<Arc<aterm_slog::Log>> {
        self.closed_logs.keep_closed(seq, log)
    }

    /// Returns the bounded [`crate::session_log::ClosedLogs`] index.
    pub(crate) const fn closed_logs(&self) -> &crate::session_log::ClosedLogs {
        &self.closed_logs
    }

    /// The workers aterm closed once their tasks concluded ([`crate::retired_workers`]).
    pub(crate) const fn retired_workers(&self) -> &crate::retired_workers::RetiredWorkers {
        &self.retired_workers
    }

    /// [`Self::retired_workers`], to keep or drop a row.
    pub(crate) const fn retired_workers_mut(
        &mut self,
    ) -> &mut crate::retired_workers::RetiredWorkers {
        &mut self.retired_workers
    }

    /// Returns the bus session-log mirror state.
    pub(crate) const fn mirror(&self) -> &crate::session_log_mirror::StoreMirror {
        &self.mirror
    }

    /// Looks up a [`SessionHandle`] by its stable [`SessionId`].
    #[must_use]
    pub(crate) fn by_sid(&self, sid: &SessionId) -> Option<&SessionHandle> {
        self.by_id.get(sid)
    }

    /// Looks up a [`SessionHandle`] by its process-local `u64` ID.
    #[must_use]
    pub(crate) fn by_local(&self, local_id: u64) -> Option<&SessionHandle> {
        self.by_local
            .get(&local_id)
            .and_then(|sid| self.by_id.get(sid))
    }

    /// Resolves a wire `@<selector>` (`Id` or `Sid`) to a [`SessionHandle`].
    #[must_use]
    pub(crate) fn by_selector(&self, selector: &Selector<'_>) -> Option<&SessionHandle> {
        match selector {
            Selector::Local(local_id) => self.by_local(*local_id),
            Selector::Sid(sid) => self.by_id.get(sid.as_str()),
            Selector::SelfTok | Selector::Invalid(_) => None,
        }
    }

    /// Returns the number of currently registered sessions.
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Returns whether no sessions are currently registered.
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Returns a cloned snapshot of all registered handles sorted by `local_id`.
    #[must_use]
    pub(crate) fn snapshot(&self) -> Vec<SessionHandle> {
        let mut v: Vec<SessionHandle> = self.by_id.values().cloned().collect();
        v.sort_by_key(|h| h.local_id);
        v
    }

    /// Returns `(local_id, sid, launch_nonce)` tuples sorted by `local_id` for the fabric bridge.
    #[must_use]
    pub(crate) fn bridge_roster(&self) -> Vec<(u64, SessionId, aterm_session::LaunchNonce)> {
        let mut rows: Vec<_> = self
            .by_id
            .values()
            .map(|h| (h.local_id, h.sid.clone(), h.nonce))
            .collect();
        rows.sort_by_key(|row| row.0);
        rows
    }

    /// Returns the set of all currently registered session ID strings.
    #[must_use]
    pub(crate) fn live_sids(&self) -> std::collections::HashSet<String> {
        self.by_id
            .values()
            .map(|h| h.sid.as_str().to_string())
            .collect()
    }

    /// Returns the roster's count: the highest sequence number in the lifecycle journal, or above
    /// it after a replace that changed a row, which moves the count with no record
    /// ([`SessionStore::register`]). Equal counts in one process mean one [`SessionStore::bridge_roster`].
    #[must_use]
    pub(crate) const fn roster_seq(&self) -> u64 {
        self.roster_seq
    }

    /// Seeds the roster journal sequence counter from a predecessor handoff before registering sessions.
    pub(crate) fn continue_roster_seq(&mut self, carried: u64) {
        self.roster_seq = self.roster_seq.max(carried.min(i64::MAX as u64));
    }

    /// Returns the lowest sequence number retained in the bounded roster journal.
    #[must_use]
    pub(crate) fn roster_low_seq(&self) -> Option<u64> {
        self.roster.front().map(|r| r.seq)
    }

    /// Iterates over retained roster journal records with `seq > after` in chronological order.
    pub(crate) fn roster_since(
        &self,
        after: u64,
    ) -> impl DoubleEndedIterator<Item = &RosterRecord> {
        let start = self.roster.partition_point(|r| r.seq <= after);
        self.roster.range(start..)
    }

    /// Initializes the on-disk session journal writer if not already armed.
    pub(crate) fn arm_session_journal(
        &mut self,
        wake: Option<aterm_slog::Notify>,
        timeline_max: usize,
        dropped_max: usize,
    ) -> Arc<()> {
        self.journal_retirements
            .get_or_insert_with(|| {
                crate::session_log_journal::Retirements::new(wake, timeline_max, dropped_max)
            })
            .owner()
    }

    /// Drains pending session-journal retirement records for background disk removal.
    pub(crate) fn take_session_journal_retirements(
        &mut self,
    ) -> crate::session_log_journal::RetirementBatch {
        self.journal_retirements.as_mut().map_or_else(
            crate::session_log_journal::RetirementBatch::default,
            crate::session_log_journal::Retirements::take,
        )
    }

    /// Returns whether any session-journal retirement records are queued.
    pub(crate) fn session_journal_pending(&self) -> bool {
        self.journal_retirements
            .as_ref()
            .is_some_and(crate::session_log_journal::Retirements::pending)
    }

    /// Takes the session journal and queued retirements out of the store for shutdown outside the lock.
    pub(crate) fn disarm_session_journal(
        &mut self,
        owner: &Arc<()>,
    ) -> Option<crate::session_log_journal::Retirements> {
        if self
            .journal_retirements
            .as_ref()
            .is_some_and(|r| r.owned_by(owner))
        {
            self.journal_retirements.take()
        } else {
            None
        }
    }

    /// Iterates over all registered [`SessionHandle`]s by reference without cloning or sorting.
    pub(crate) fn live_handles(&self) -> impl Iterator<Item = &SessionHandle> {
        self.by_id.values()
    }
}

fn record_state_change(
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    state: SessionState,
) {
    let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
    let at = tl.slog().now();
    tl.slog_mut().observe_state(state.as_str(), at);
    tl.record("state-change");
    let _ = tl.slog_take_wake();
}

thread_local! {
    static CLOSE_ATTRIBUTION: std::cell::RefCell<Option<(ExitReason, ExitActor)>> =
        const { std::cell::RefCell::new(None) };
}

/// RAII guard attributing all session deregistrations on the current thread to `(reason, actor)`.
pub(crate) struct CloseAttribution {
    opened: bool,
}

impl CloseAttribution {
    /// Enters a [`CloseAttribution`] scope on the current thread if none is already active.
    #[must_use = "the attribution lasts exactly as long as this guard lives"]
    pub(crate) fn enter(reason: ExitReason, actor: ExitActor) -> Self {
        let opened = CLOSE_ATTRIBUTION.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some((reason, actor));
                true
            } else {
                false
            }
        });
        Self { opened }
    }
}

impl Drop for CloseAttribution {
    fn drop(&mut self) {
        if self.opened {
            CLOSE_ATTRIBUTION.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// The `(ExitReason, ExitActor)` scope open on the current thread, if any, for a close
/// that runs later (a teardown a pending update handoff defers) to re-enter.
#[must_use]
pub(crate) fn open_close_attribution() -> Option<(ExitReason, ExitActor)> {
    CLOSE_ATTRIBUTION.with(|slot| slot.borrow().clone())
}

/// Returns the active `(ExitReason, ExitActor)` attribution on the current thread.
#[must_use]
pub(crate) fn current_close_attribution() -> (ExitReason, ExitActor) {
    CLOSE_ATTRIBUTION
        .with(|slot| slot.borrow().clone())
        .unwrap_or((ExitReason::Unknown, ExitActor::Unknown))
}

impl crate::App {
    /// Samples the exited child's status into the session store before `exit_session_logical` deregisters it.
    pub(crate) fn note_shell_exit_for_ledger(&mut self, session: u64) {
        let code = self.shell_exit_code(session);
        self.store
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .note_exit_code(session, code);
    }

    fn shell_exit_code(&self, session: u64) -> Option<i32> {
        use crate::app_tabs::ExitLook;
        use crate::session_status::{Outcome, Phase};
        let mut look = ExitLook::First;
        if let Some(status) = self.session_status.status(session)
            && matches!(status.phase, Phase::Exited)
        {
            match status.last_outcome {
                Outcome::Success => return Some(0),
                Outcome::Failure { exit_code } => return Some(exit_code),
                Outcome::Signal { .. } => return None,
                Outcome::None => look = ExitLook::Retry,
            }
        }
        match self.exit_status(session, look)? {
            aterm_pty::ChildExit::Code(code) => Some(code),
            aterm_pty::ChildExit::Signal(_) => None,
        }
    }
}

/// Creates an `Alive` headless [`SessionHandle`] for unit tests.
#[cfg(test)]
pub(crate) fn test_handle(local_id: u64) -> SessionHandle {
    handle_alive(local_id, None, -1)
}

/// Creates an `Alive` headless handle with explicit fixture identities.
#[cfg(all(test, unix))]
pub(crate) fn test_handle_identified(
    local_id: u64,
    sid: SessionId,
    nonce: LaunchNonce,
    epochs: Arc<crate::session_log::Epochs>,
    at: aterm_slog::Stamp,
) -> SessionHandle {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let handle = handle_ran_identified(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new(-1),
        Arc::default(),
        sid,
        nonce,
    );
    let mut slog = crate::session_log::SessionLog::starting(epochs, at);
    slog.set_clock(crate::session_log::Clock::reading(move || at));
    *handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .slog_mut() = slog;
    handle
}

/// Creates a test [`SessionHandle`] wired to a caller-supplied raw PTY master fd.
#[cfg(test)]
pub(crate) fn test_handle_on(local_id: u64, master: i32) -> SessionHandle {
    handle_alive(local_id, None, master)
}

/// Creates a test [`SessionHandle`] wrapping a caller-supplied `Terminal` instance.
#[cfg(test)]
pub(crate) fn test_handle_with(local_id: u64, term: Arc<Mutex<Terminal>>) -> SessionHandle {
    handle_over(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new(-1),
    )
}

/// Creates a test [`SessionHandle`] over `term` whose ran log is `ran` (a test's file table).
#[cfg(all(test, unix))]
pub(crate) fn test_handle_ran(
    local_id: u64,
    term: Arc<Mutex<Terminal>>,
    ran: Arc<crate::command_ran::Ran>,
) -> SessionHandle {
    handle_ran(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new(-1),
        ran,
    )
}

/// Creates a test [`SessionHandle`] whose sink owns `master`.
#[cfg(all(test, unix))]
pub(crate) fn test_handle_owning(local_id: u64, master: std::os::fd::OwnedFd) -> SessionHandle {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    handle_over(
        local_id,
        None,
        term,
        aterm_session::sink::SinkWriter::new_owned(master),
    )
}

#[cfg(test)]
fn handle_alive(local_id: u64, parent: Option<SessionId>, master: i32) -> SessionHandle {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    handle_over(
        local_id,
        parent,
        term,
        aterm_session::sink::SinkWriter::new(master),
    )
}

#[cfg(test)]
fn handle_over(
    local_id: u64,
    parent: Option<SessionId>,
    term: Arc<Mutex<Terminal>>,
    sink: aterm_session::sink::SinkWriter,
) -> SessionHandle {
    handle_ran(local_id, parent, term, sink, Arc::default())
}

#[cfg(test)]
fn handle_ran(
    local_id: u64,
    parent: Option<SessionId>,
    term: Arc<Mutex<Terminal>>,
    sink: aterm_session::sink::SinkWriter,
    ran: Arc<crate::command_ran::Ran>,
) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    handle_ran_identified(local_id, parent, term, sink, ran, sid, nonce)
}

#[cfg(test)]
fn handle_ran_identified(
    local_id: u64,
    parent: Option<SessionId>,
    term: Arc<Mutex<Terminal>>,
    sink: aterm_session::sink::SinkWriter,
    ran: Arc<crate::command_ran::Ran>,
    sid: SessionId,
    nonce: LaunchNonce,
) -> SessionHandle {
    use aterm_session::EdgeTable;
    crate::prime_test_timing();
    let master = sink.master();
    let observed = crate::session_watch::Observed::default();
    observed.watchers.bind_frame(&term);
    *observed
        .timeline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .slog_mut() = crate::session_log::SessionLog::for_test();
    let ctx = Arc::new(SessionCtx {
        sink: Arc::new(sink),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(Mutex::new(EdgeTable::new())),
        self_id: sid.clone(),
        nonce,
        turn_lease: Mutex::new(None),
        cast: Arc::new(Mutex::new(crate::cast::CastRecorder::new(80, 24))),
        temporal: Arc::new(Mutex::new(crate::temporal::TemporalRecorder::new())),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: observed.turns,
        meta: Arc::default(),
        timeline: observed.timeline,
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        licence: Default::default(),
        launch: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        typed: Default::default(),
        bells: Default::default(),
        screen_moves: Default::default(),
        job_feed: Default::default(),
        ran,
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
        native_policy: Default::default(),
        spawn_mark: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent,
        state: SessionState::Alive,
        title: format!("tab-{local_id}"),
        term,
        master,
        shell_identity: aterm_pty::ShellIdentity::unproven(0),
        ctx,
    }
}

#[cfg(test)]
#[path = "session_store_tests.rs"]
mod tests;
