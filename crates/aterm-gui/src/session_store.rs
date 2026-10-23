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
}

/// Formats a `(pid, local_id)` crash-journal key as `<pid>:<id>`.
#[cfg(any(unix, test))]
pub(crate) fn journal_key_row((pid, id): (u32, u64)) -> String {
    format!("{pid}:{id}")
}

/// Parses a `<pid>:<id>` crash-journal key, rejecting PID `0`.
pub(crate) fn parse_journal_key(row: &str) -> Option<(u32, u64)> {
    let (pid, id) = row.split_once(':')?;
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
    let (shift, pushed) = match row {
        Some(row) => {
            let (shift, pushed) = row.split_once(',')?;
            (
                shift.trim().parse::<i32>().ok()?,
                pushed.trim().parse::<i32>().ok()?,
            )
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
        Read(crate::fabric::FabricCarry),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Carry as serde::Deserialize>::deserialize(deserializer)? {
            Carry::Read(carry) => carry,
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
    #[serde(default)]
    pub status_bar_rows: u16,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_title: Option<String>,
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
    #[serde(default)]
    pub on_glass: bool,
    #[serde(
        default = "carried_excerpt",
        skip_serializing_if = "is_carried_excerpt"
    )]
    pub excerpt: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retrospective: bool,
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

const fn carried_excerpt() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_carried_excerpt(excerpt: &bool) -> bool {
    *excerpt
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
            short_title: m.short_title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            on_glass: m.on_glass,
            excerpt: m.excerpt,
            finished: m.finished.clone(),
            retrospective: m.retrospective,
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
            short_title: m.short_title.clone(),
            detail: m.detail.clone(),
            actions: m.actions.clone(),
            hold: m.hold.clone(),
            key: m.key.clone(),
            fill_permille: m.fill_permille,
            stats: m.stats.clone(),
            busy: m.busy,
            on_glass: m.on_glass,
            excerpt: m.excerpt,
            finished: m.finished.clone(),
            retrospective: m.retrospective,
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
    pub fleet_told: Vec<String>,
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
        Self {
            meta,
            supervisor,
            claim_known,
            attention_owners,
            topics: crate::fabric::render_topics(&ctx.fabric.topics()),
            task: ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .task_carry_row(),
        }
    }
}

/// Mutable claim, hold, and Autopilot fields of a session sampled at handoff park.
#[cfg(unix)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarriedClaims {
    pub local_id: u64,
    topics: Vec<String>,
    attention_owners: Vec<String>,
    supervisor: Option<String>,
    claim_known: bool,
    task: Option<String>,
}

#[cfg(unix)]
impl CarriedClaims {
    /// Extracts [`CarriedClaims`] from a parked [`SessionRecord`].
    #[must_use]
    pub(crate) fn of_record(record: &SessionRecord) -> Self {
        Self {
            local_id: record.local_id,
            topics: record.topics.clone(),
            attention_owners: record.attention_owners.clone(),
            supervisor: record.supervisor.as_deref().map(lease_holder),
            claim_known: record.claim_known,
            task: record.task.clone(),
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
            topics: drawn.topics,
            attention_owners: drawn.attention_owners,
            supervisor: drawn.supervisor.as_deref().map(lease_holder),
            claim_known: drawn.claim_known,
            task: drawn.task,
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
            fleet_told: Vec::new(),
            mirror_dropped: store.mirror.draw_tombstones(),
            mirror_served: store.mirror.served(),
            machine_names: crate::tab_label::machine_names_for_handoff(),
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
    Unknown,
}

impl ExitReason {
    #[cfg(test)]
    pub const ALL: [ExitReason; 6] = [
        ExitReason::ShellExit,
        ExitReason::CtlClose,
        ExitReason::UiClose,
        ExitReason::WindowClose,
        ExitReason::AppQuit,
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
            ExitReason::Unknown => "unknown",
        }
    }
}

/// Actor that retired a session (`by=<sid|human|ctl|bridge|->`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExitActor {
    Sid(String),
    Human,
    Ctl,
    #[cfg(any(unix, test))]
    Bridge,
    Unknown,
}

impl ExitActor {
    /// Stable wire token for `by=<actor>`.
    #[must_use]
    pub(crate) const fn as_wire(&self) -> &str {
        match self {
            ExitActor::Sid(sid) => sid.as_str(),
            ExitActor::Human => "human",
            ExitActor::Ctl => "ctl",
            #[cfg(any(unix, test))]
            ExitActor::Bridge => "bridge",
            ExitActor::Unknown => "-",
        }
    }
}

/// Single membership-change entry in the bounded roster lifecycle journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterRecord {
    pub seq: u64,
    pub sid: String,
    pub local_id: u64,
    pub change: RosterChange,
    pub t_ms: u64,
    pub reason: ExitReason,
    pub exit_code: Option<i32>,
    pub actor: ExitActor,
}

/// Process-wide session registry indexed by both [`SessionId`] and local `u64` ID.
#[derive(Default)]
pub(crate) struct SessionStore {
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
}

/// Creates a new empty shared [`Store`].
#[must_use]
pub(crate) fn new_store() -> Store {
    Arc::new(RwLock::new(SessionStore::default()))
}

impl SessionStore {
    /// Registers or replaces a [`SessionHandle`], updating both indices and appending to the roster journal.
    pub(crate) fn register(&mut self, handle: SessionHandle) {
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
            tl.record("spawned", format!("state={}", handle.state.as_str()));
            let at = tl.slog().now();
            tl.slog_mut().open_session(
                handle.parent.as_ref().map(SessionId::as_str),
                Some(handle.state.as_str()),
                at,
            );
        }
        let journal = first.then(|| (handle.sid.as_str().to_string(), handle.local_id));
        self.by_local.insert(handle.local_id, handle.sid.clone());
        self.by_id.insert(handle.sid.clone(), handle);
        if let Some((sid, local_id)) = journal {
            self.record_roster(
                sid,
                local_id,
                RosterChange::Created,
                ExitReason::Unknown,
                None,
                ExitActor::Unknown,
                crate::turn_ledger::now_ms(),
            );
        }
    }

    /// Appends one roster record stamped `t_ms`, evicting the oldest past [`ROSTER_JOURNAL_CAP`]; returns its seq.
    #[expect(
        clippy::too_many_arguments,
        reason = "one record's fields, every caller naming each"
    )]
    fn record_roster(
        &mut self,
        sid: String,
        local_id: u64,
        change: RosterChange,
        reason: ExitReason,
        exit_code: Option<i32>,
        actor: ExitActor,
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
            exit_code,
            actor,
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
            None,
            ExitActor::Unknown,
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
            None,
            ExitActor::Unknown,
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
            if reason != ExitReason::Unknown {
                tl.record("closing", closing_payload(reason, &actor));
            }
            tl.record("state-change", "state=closed".to_string());
            let at = tl.slog().now();
            tl.slog_mut().close_session(
                crate::session_log::exit_out(reason),
                crate::session_log::exit_actor(&actor),
                actor.as_wire(),
                exit_code,
                at,
            );
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
        };
        if let Some((reason, t_ms)) = resolved {
            self.by_id.remove(&closed.sid);
            let seq = self.record_roster(
                closed.sid.as_str().to_string(),
                local_id,
                RosterChange::Exited,
                reason,
                exit_code,
                actor.clone(),
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

    /// Returns the highest sequence number in the roster lifecycle journal.
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
    tl.record("state-change", format!("state={}", state.as_str()));
    let _ = tl.slog_take_wake();
}

fn closing_payload(reason: ExitReason, actor: &ExitActor) -> String {
    format!(
        "reason={} by={}",
        reason.as_str(),
        crate::control::pct_encode(actor.as_wire())
    )
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
    use aterm_session::EdgeTable;
    crate::prime_test_timing();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
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
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
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
        update_parked: Default::default(),
        watchers: observed.watchers,
        frame: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
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
        ctx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_and_program_words_are_the_logs() {
        use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};
        use aterm_slog::vocab::{ASK_WORDS, STATE_WORDS};
        let sessions = [
            SessionState::Spawning,
            SessionState::Alive,
            SessionState::Exited,
        ];
        for s in sessions {
            match s {
                SessionState::Spawning | SessionState::Alive | SessionState::Exited => {}
            }
            assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
        }
        assert!(STATE_WORDS.contains(&"closed"), "the close's row");
        let states = [
            ProgramStateWord::Idle,
            ProgramStateWord::Working,
            ProgramStateWord::Blocked,
            ProgramStateWord::Done,
            ProgramStateWord::Error,
        ];
        for s in states {
            match s {
                ProgramStateWord::Idle
                | ProgramStateWord::Working
                | ProgramStateWord::Blocked
                | ProgramStateWord::Done
                | ProgramStateWord::Error => {}
            }
            assert!(STATE_WORDS.contains(&s.as_str()), "{:?}", s.as_str());
        }
        let asks = [
            ProgramBlockedKind::Permission,
            ProgramBlockedKind::Question,
            ProgramBlockedKind::Auth,
        ];
        for k in asks {
            match k {
                ProgramBlockedKind::Permission
                | ProgramBlockedKind::Question
                | ProgramBlockedKind::Auth => {}
            }
            assert!(ASK_WORDS.contains(&k.as_str()), "{:?}", k.as_str());
        }
        assert_eq!(ASK_WORDS.len(), asks.len(), "no ask word the engine lacks");
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::State, "teleporting")
                .is_err()
        );
    }

    #[test]
    fn the_hold_origins_are_the_logs() {
        use aterm_slog::vocab::ORIGIN_WORDS;
        let carried = aterm_gui_fabric::parse_holds(["person a", "fleet b", "local c"]);
        let mut origins: Vec<String> = carried.standing().map(|h| h.origin).collect();
        assert_eq!(origins.len(), 3, "every carried slot stands: {origins:?}");
        origins.push(aterm_gui_fabric::REMOTE_ORIGIN.to_string());
        for o in &origins {
            assert!(ORIGIN_WORDS.contains(&o.as_str()), "{o}");
            assert!(
                aterm_slog::Fields::new()
                    .word(aterm_slog::Key::Origin, o)
                    .is_ok(),
                "{o}"
            );
        }
        assert_eq!(
            ORIGIN_WORDS.len(),
            origins.len(),
            "no origin word the slots lack: {origins:?}"
        );
        assert!(
            aterm_slog::Fields::new()
                .word(aterm_slog::Key::Origin, "teleport")
                .is_err()
        );
    }

    #[test]
    fn a_spawn_wakes_instance_watchers() {
        use std::time::Duration;
        let mut store = SessionStore::default();
        let watcher = crate::session_watch::Subscription::single(&store.instance_watchers());
        let h = test_handle(3);
        store.register(h.clone());
        assert!(watcher.wait(Duration::ZERO), "a spawn rings the instance");
        store.register(h);
        assert!(!watcher.wait(Duration::ZERO), "a replace rings nobody");
        store.deregister_local_as(3, ExitReason::CtlClose, ExitActor::Unknown);
        assert!(watcher.wait(Duration::ZERO), "a close rings the instance");
    }

    #[test]
    fn a_state_change_wakes_the_sessions_watchers_and_a_re_mark_does_not() {
        use std::time::Duration;
        let mut store = SessionStore::default();
        let h = test_handle(4);
        let watcher = crate::session_watch::Subscription::single(&h.ctx.watchers);
        store.register(h);
        while watcher.wait(Duration::ZERO) {}
        store.set_state(4, SessionState::Alive);
        assert!(!watcher.wait(Duration::ZERO), "a re-mark rings nobody");
        store.set_state(4, SessionState::Exited);
        assert!(watcher.wait(Duration::ZERO), "the exit rings the session");
    }

    #[test]
    fn a_carried_render_debt_round_trips_and_a_bad_row_is_none() {
        let debt = |shift, pushed, cut| RenderDebt { shift, pushed, cut };
        for d in [
            debt(-1, 3, 0),
            debt(-1, 0, 2),
            debt(0, 0, 2),
            debt(4096, -4096, 4096),
        ] {
            let (row, cut) = render_debt_keys(Some(d));
            assert_eq!(parse_render_debt(row.as_deref(), cut), Some(d), "{d:?}");
        }
        assert_eq!(
            render_debt_keys(Some(debt(-1, 0, 3))),
            (Some("-1,0".to_string()), 3),
            "the rows as ever, the columns beside them"
        );
        assert_eq!(render_debt_keys(Some(debt(0, 0, 2))), (None, 2));
        assert_eq!(render_debt_keys(Some(RenderDebt::default())), (None, 0));
        assert_eq!(render_debt_keys(None), (None, 0));

        let rows = |row| parse_render_debt(Some(row), 0);
        assert_eq!(rows("-1,0"), Some(debt(-1, 0, 0)));
        assert_eq!(rows("1"), None);
        assert_eq!(rows("a,b"), None);
        assert_eq!(rows("0,0"), None, "owes nothing");
        assert_eq!(rows("4097,0"), None);
        assert_eq!(rows("-2147483648,0"), None);
        assert_eq!(rows("0,-2147483648"), None);
        assert_eq!(rows("0,0,2"), None, "three fields");
        assert_eq!(parse_render_debt(None, 0), None);
        assert_eq!(parse_render_debt(None, 4097), None);
        assert_eq!(parse_render_debt(Some("1,0"), 4097), None);
        assert_eq!(parse_render_debt(Some("bad"), 2), None, "a bad row is none");
    }

    #[test]
    fn a_carried_journal_key_round_trips_and_a_bad_one_is_none() {
        let key = (u32::MAX, u64::MAX);
        assert_eq!(parse_journal_key(&journal_key_row(key)), Some(key));
        assert_eq!(parse_journal_key("4141:7"), Some((4141, 7)));
        for bad in ["", "4141", "0:7", "a:7", "4141:-1", "4141:7:1", " 4141:7"] {
            assert_eq!(parse_journal_key(bad), None, "{bad:?}");
        }
        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 0\nsid = \"s-0123456789abcdef0123\"\nstate = \
             \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an older producer's manifest parses");
        assert_eq!(old.sessions[0].journal_key, None);
    }

    fn handle(local_id: u64, parent: Option<SessionId>) -> SessionHandle {
        handle_in_state(local_id, parent, SessionState::Alive)
    }

    fn handle_in_state(
        local_id: u64,
        parent: Option<SessionId>,
        state: SessionState,
    ) -> SessionHandle {
        let mut h = handle_alive(local_id, parent, -1);
        h.state = state;
        h
    }

    #[test]
    fn the_frozen_path_carry_reads_absent_as_predates_and_rides_per_record() {
        assert!(
            predates_path_self_heal(None),
            "absent: the old build wrote it"
        );
        assert!(
            !predates_path_self_heal(Some(crate::running_build_number())),
            "a dev build of this tree ({}) wrote the field, so it heals",
            crate::running_build_number()
        );
        assert!(
            !predates_path_self_heal(Some(1_789_468_652)),
            "a number below the anchor with the field present still heals: presence is the test"
        );
        assert!(!predates_path_self_heal(Some(1_789_584_999)));
        assert!(!predates_path_self_heal(Some(1_789_585_000)));
        assert!(
            !predates_path_self_heal(Some(0)),
            "an unnumbered build carries the field"
        );

        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.outgoing_build, None);
        assert!(predates_path_self_heal(read.outgoing_build));
        assert!(
            !read.sessions[0].frozen_path,
            "the per-record flag defaults off"
        );

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_frozen_path(1);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.outgoing_build, Some(crate::running_build_number()));
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.frozen_path)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert!(wire.contains("outgoing_build = "), "{wire}");
        assert_eq!(wire.matches("frozen_path = true").count(), 1, "{wire}");
        assert!(
            !wire.contains("frozen_path = false"),
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());

        assert_eq!(store.frozen_path_tabs(), 1);
        store.mark_frozen_path(1);
        assert_eq!(store.frozen_path_tabs(), 1, "idempotent");
        store.note_exit_code(1, Some(0));
        assert_eq!(
            store.frozen_path_tabs(),
            0,
            "an exited shell is not a tab to exec in"
        );
        assert!(store.has_frozen_path(1), "but the fact is still carried");
        store.deregister_local(1);
        assert!(!store.has_frozen_path(1));
        assert_eq!(store.frozen_path_tabs(), 0);
        store.mark_frozen_path(9);
        assert_eq!(store.frozen_path_tabs(), 0);
        store.register(handle(9, None));
        assert_eq!(store.frozen_path_tabs(), 1);
    }

    #[test]
    fn session_handoff_carries_a_hold() {
        use crate::fabric::{HOLD_DENIED, Hold, HoldIssuer, Holds, apply_hold_for_test, cmd_hold};
        let local = Hold {
            reason: "stop%20driving".to_string(),
            origin: "local".to_string(),
        };
        let fleet = Hold {
            reason: "fabric-lost".to_string(),
            origin: "fleet".to_string(),
        };
        let mut store = SessionStore::default();
        let held: [&[&Hold]; 4] = [&[&local], &[&fleet], &[], &[&local, &fleet]];
        let mut before: Vec<Holds> = Vec::new();
        for (local_id, holds) in (0..).zip(held) {
            let h = handle(local_id, None);
            for hold in holds {
                assert!(apply_hold_for_test(&h.ctx, Some((*hold).clone())));
            }
            before.push(h.ctx.fabric.holds());
            store.register(h);
        }
        let manifest = SessionHandoff::from_store(&store);
        let rows: Vec<(Option<&str>, &[String])> = manifest
            .sessions
            .iter()
            .map(|r| (r.hold.as_deref(), r.more_holds.as_slice()))
            .collect();
        let both = ["local stop%20driving".to_string()];
        assert_eq!(
            rows,
            [
                (Some("local stop%20driving"), &[][..]),
                (Some("fleet fabric-lost"), &[][..]),
                (None, &[][..]),
                (Some("fleet fabric-lost"), &both[..]),
            ],
            "every standing hold is drawn into its record, either origin, the fleet's first"
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            (
                wire.matches("hold = ").count(),
                wire.matches("more_holds = ").count()
            ),
            (3, 1),
            "no key for no hold, and the second only beside a first: {wire}"
        );
        assert!(manifest.roundtrips());

        let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
        let adopted_store: Store = new_store();
        for (rec, before) in read.sessions.iter().zip(&before) {
            let fresh = handle(rec.local_id, None);
            crate::spawn::seed_adopted_fabric(
                &fresh.ctx.fabric,
                &fresh.ctx.timeline,
                &rec.topics,
                &rec.hold_rows(),
                &rec.fabric,
            );
            assert_eq!(&fresh.ctx.fabric.holds(), before, "{rec:?}");
            let refused = crate::fabric::halt_refusal(&fresh.ctx, "send");
            assert_eq!(
                refused.is_some(),
                before.is_held(),
                "the dispatch gate answers exactly as it did before the update"
            );
            let carried_rows = fresh
                .ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == "hold" && e.payload.ends_with(" carried=1"))
                .count();
            assert_eq!(carried_rows, usize::from(before.is_held()), "{rec:?}");
            adopted_store.write().unwrap().register(fresh);
        }
        let sid_of = |local_id: u64| {
            adopted_store
                .read()
                .unwrap()
                .by_local(local_id)
                .map(|h| h.sid.as_str().to_string())
                .expect("registered")
        };
        let owner_off = |local_id: u64| {
            cmd_hold(
                &adopted_store,
                &format!("{} off", sid_of(local_id)),
                HoldIssuer::Owner,
            )
        };
        assert_eq!(
            owner_off(1),
            HOLD_DENIED,
            "a carried FLEET hold is not the owner's to lift"
        );
        assert_eq!(
            owner_off(0),
            "OK hold=0\n",
            "a carried LOCAL hold is, as it was"
        );
        assert_eq!(
            owner_off(3),
            "OK hold=1\n",
            "beside a carried fleet hold the owner lifts its own, and the fleet's stands"
        );

        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.sessions[0].hold_rows(), Vec::<String>::new());
        let one = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"local stop%20driving\"\n",
        )
        .expect("a one-hold manifest reads");
        let fresh = handle(3, None);
        crate::spawn::seed_adopted_fabric(
            &fresh.ctx.fabric,
            &fresh.ctx.timeline,
            &[],
            &one.sessions[0].hold_rows(),
            &one.sessions[0].fabric,
        );
        assert_eq!(fresh.ctx.fabric.holds(), before[0], "one hold, that origin");
    }

    #[test]
    fn an_unreadable_carried_hold_fails_closed() {
        use crate::fabric::{CARRIED_UNREADABLE, Hold, parse_hold, render_hold};
        let held = |reason: &str, origin: &str| Hold {
            reason: reason.to_string(),
            origin: origin.to_string(),
        };
        for (row, want) in [
            ("local stop%20now", held("stop%20now", "local")),
            ("fleet fabric-lost", held("fabric-lost", "fleet")),
            ("fleet -", held("-", "fleet")),
            (
                &*format!("local {}", "r".repeat(400)),
                held(&"r".repeat(128), "local"),
            ),
            ("local a%2", held("a", "local")),
            ("", held(CARRIED_UNREADABLE, "local")),
            ("fleet", held(CARRIED_UNREADABLE, "fleet")),
            ("fleet a b", held(CARRIED_UNREADABLE, "fleet")),
            ("mars now", held(CARRIED_UNREADABLE, "local")),
            ("local ", held(CARRIED_UNREADABLE, "local")),
            (" fleet x", held(CARRIED_UNREADABLE, "local")),
        ] {
            assert_eq!(parse_hold(row), want, "{row:?}");
        }
        for hold in [held("stop%20now", "local"), held("fabric-lost", "fleet")] {
            assert_eq!(parse_hold(&render_hold(&hold)), hold, "round trip");
        }
        let odd = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = 5\n",
        )
        .expect("a hold of the wrong type does not cost the manifest");
        assert_eq!(odd.sessions[0].hold.as_deref(), Some(""));
        assert_eq!(parse_hold(""), held(CARRIED_UNREADABLE, "local"));
        for (more, want) in [
            ("[\"local mine\", 5]", vec!["local mine", ""]),
            ("5", vec![""]),
            ("[]", vec![]),
        ] {
            let odd = SessionHandoff::from_toml(&format!(
                "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\ntitle = \"zsh\"\nhold = \"fleet stop\"\nmore_holds = {more}\n",
            ))
            .expect("more_holds of the wrong type does not cost the manifest");
            assert_eq!(odd.sessions[0].more_holds, want, "{more}");
        }
    }

    #[test]
    fn a_ttl_claim_and_keyed_attention_cross_the_handoff() {
        use crate::session_timeline::{
            MetaEdit, claim_supervisor, parse_carried_attention, parse_carried_supervisor,
            seed_carried_claims, write_attention_owned, write_server_attention,
        };
        let now = crate::metrics::now_us();
        let mut store = SessionStore::default();
        let s0 = handle(0, None);
        claim_supervisor(&s0.ctx, "watch-bob", None, Some(now + 60_000_000), now).unwrap();
        for (owner, text) in [
            ("-", "check the build"),
            ("supervisor", "claude box (Bash: rm -rf x) — needs you"),
            ("ci@host", "red 100%"),
        ] {
            write_attention_owned(&s0.ctx, owner, MetaEdit::Set(text), aterm_slog::Actor::Ctl)
                .unwrap();
        }
        crate::session_timeline::write_attention_kind(
            &s0.ctx,
            "ci@host",
            MetaEdit::Set("red 100%"),
            Some(aterm_types::needs::AttentionKind::Wait),
            aterm_slog::Actor::Ctl,
        )
        .unwrap();
        assert!(write_server_attention(
            &s0.ctx,
            crate::input_stall::SERVER_ATTENTION_OWNER,
            Some("claude has not read 3 keys".to_string())
        ));
        store.register(s0);
        let s1 = handle(1, None);
        claim_supervisor(&s1.ctx, "relay-x", Some(7), None, now).unwrap();
        store.register(s1);
        let s2 = handle(2, None);
        claim_supervisor(
            &s2.ctx,
            &crate::harness_host::holder(),
            None,
            Some(now + 90_000_000),
            now,
        )
        .unwrap();
        store.register(s2);
        store.register(handle(3, None));
        let s4 = handle(4, None);
        claim_supervisor(&s4.ctx, "gone", None, Some(now.saturating_sub(1)), now).unwrap();
        store.register(s4);

        let manifest = SessionHandoff::from_store(&store);
        let r = &manifest.sessions;
        let (holder, ms) = r[0]
            .supervisor
            .as_deref()
            .and_then(|row| row.split_once(' '))
            .expect("s0's lease is carried");
        let ms: u64 = ms.parse().unwrap();
        assert_eq!(holder, "watch-bob");
        assert!((50_000..=60_000).contains(&ms), "{ms}");
        assert!(r[0].claim_known);
        assert_eq!(
            r[0].attention_owners,
            [
                "- check%20the%20build",
                "supervisor claude%20box%20(Bash:%20rm%20-rf%20x)%20%E2%80%94%20needs%20you",
                "ci@host kind=wait red%20100%25",
            ],
            "every owner's line in write order, with the kind its writer named, \
             the server's own left behind"
        );
        let claims: Vec<(Option<&str>, bool, bool)> = r
            .iter()
            .map(|rec| {
                (
                    rec.supervisor.as_deref(),
                    rec.claim_known,
                    rec.claim_grace(),
                )
            })
            .collect();
        assert_eq!(
            claims[1..],
            [
                (None, false, true),
                (None, true, false),
                (None, true, false),
                (None, true, false)
            ],
            "a connection's claim is unvouched; our own host's, none and a lapsed \
             lease are vouched for"
        );
        assert!(r[0].claim_grace(), "a carried lease is waited on too");
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(wire.matches("\nsupervisor = ").count(), 1, "{wire}");
        assert_eq!(wire.matches("\nclaim_known = true").count(), 4, "{wire}");
        assert!(manifest.roundtrips());

        let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
        let later = now + 2_000_000;
        let adopted: Vec<SessionHandle> = read
            .sessions
            .iter()
            .map(|rec| {
                let fresh = handle(rec.local_id, None);
                seed_carried_claims(
                    &fresh.ctx.meta,
                    &fresh.ctx.timeline,
                    rec.supervisor.as_deref(),
                    &rec.attention_owners,
                    later,
                );
                fresh
            })
            .collect();
        {
            let meta = adopted[0].ctx.meta.lock().unwrap();
            assert_eq!(meta.live_supervisor(later), Some("watch-bob"));
            let expiry = meta.supervisor_expiry().expect("a lease");
            assert_eq!(
                expiry,
                later + ms * 1000,
                "its remaining time, from the seed"
            );
            assert_eq!(meta.attention.as_deref(), Some("red 100%"));
            assert_eq!(
                meta.attention_owners.effective_owner(),
                Some("ci@host"),
                "the most recent line is the one shown, as before the update"
            );
            assert_eq!(
                meta.attention_owners.get("supervisor"),
                Some("claude box (Bash: rm -rf x) — needs you")
            );
            assert_eq!(meta.attention_owners.get("-"), Some("check the build"));
            assert_eq!(
                meta.attention_owners.kinds_field(),
                "ask:-,-:supervisor,wait:ci@host",
                "a wait is still a wait in the successor"
            );
            assert_eq!(
                meta.attention_owners
                    .get(crate::input_stall::SERVER_ATTENTION_OWNER),
                None
            );
            assert_eq!(meta.driver_writes, 0, "a seed is no driver's write");
        }
        assert_eq!(
            claim_supervisor(
                &adopted[0].ctx,
                "aterm-harness@1",
                None,
                Some(later + 90_000_000),
                later
            ),
            Err("watch-bob".to_string())
        );
        assert_eq!(
            claim_supervisor(
                &adopted[0].ctx,
                "watch-bob",
                None,
                Some(later + 90_000_000),
                later
            ),
            Ok(false)
        );
        let seeded_rows: Vec<String> = adopted[0]
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(
            seeded_rows,
            [
                "field=attention value=red%20100%25",
                "field=supervisor value=watch-bob"
            ]
        );
        for fresh in &adopted[1..] {
            let meta = fresh.ctx.meta.lock().unwrap();
            assert_eq!(meta.live_supervisor(later), None, "nothing else is seeded");
            assert!(meta.attention_owners.is_empty());
        }

        let old = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-old\"\nstate = \"alive\"\ntitle = \"zsh\"\n",
        )
        .expect("an old manifest reads");
        assert_eq!(old.sessions[0].supervisor, None);
        assert!(old.sessions[0].attention_owners.is_empty());
        assert!(old.sessions[0].claim_grace());

        for (row, want) in [
            ("watch-bob 5000", Some(("watch-bob", 5000))),
            ("watch-bob 99999999", Some(("watch-bob", 600_000))),
            ("watch-bob 0", None),
            ("watch-bob -5", None),
            ("watch-bob soon", None),
            ("- 5000", None),
            ("bad holder! 5000", None),
            ("", None),
            ("watch-bob", None),
            (" watch-bob 5", None),
        ] {
            assert_eq!(parse_carried_supervisor(row), want, "{row:?}");
        }
        use aterm_types::needs::AttentionKind::{Ask, Wait};
        for (row, want) in [
            ("ci red", Some(("ci", None, "red".to_string()))),
            (
                "ci x%1B%5B31mred%E2%80%AEx",
                Some(("ci", None, "x[31mredx".to_string())),
            ),
            (
                "ci kind=wait limited:%20resets%207pm",
                Some(("ci", Some(Wait), "limited: resets 7pm".to_string())),
            ),
            (
                "ci kind=ask red",
                Some(("ci", Some(Ask), "red".to_string())),
            ),
            ("ci kind=later red", Some(("ci", None, "red".to_string()))),
            ("- kind=wait red", Some(("-", None, "red".to_string()))),
            ("ci kind=wait", Some(("ci", None, "kind=wait".to_string()))),
            ("aterm stalled", None),
            ("bad%20owner text", None),
            ("ci ", None),
            ("ci %0A%09", None),
            ("ci kind=wait %0A", None),
            ("ci", None),
        ] {
            assert_eq!(parse_carried_attention(row), want, "{row:?}");
        }
        let (long, bare) = (
            format!("ci {}", "a".repeat(400)),
            format!("- {}", "a".repeat(400)),
        );
        assert_eq!(
            parse_carried_attention(&long).unwrap().2.len(),
            crate::session_timeline::META_ATTENTION_KEYED_MAX
        );
        assert_eq!(
            parse_carried_attention(&bare).unwrap().2.len(),
            crate::session_timeline::META_ATTENTION_MAX
        );
        let crowded = handle(9, None);
        let rows: Vec<String> = (0..12).map(|i| format!("owner{i} line{i}")).collect();
        seed_carried_claims(&crowded.ctx.meta, &crowded.ctx.timeline, None, &rows, later);
        assert_eq!(
            crowded.ctx.meta.lock().unwrap().attention_owners.len(),
            crate::session_timeline::ATTENTION_OWNERS_MAX - 1,
            "the bare owner's slot is kept"
        );
        let odd = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
             title = \"zsh\"\nsupervisor = 5\nclaim_known = \"yes\"\nattention_owners = [1, \"ci x\"]\n",
        )
        .expect("rows of the wrong type do not cost the manifest");
        assert_eq!(odd.sessions[0].supervisor.as_deref(), Some(""));
        assert!(!odd.sessions[0].claim_known);
        assert_eq!(odd.sessions[0].attention_owners, ["ci x"]);
        assert!(odd.sessions[0].claim_grace());
        let not_a_list = SessionHandoff::from_toml(
            "schema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-odd\"\nstate = \"alive\"\n\
             title = \"zsh\"\nattention_owners = \"ci x\"\nclaim_known = true\n",
        )
        .expect("a list that is no list does not cost the manifest");
        assert!(not_a_list.sessions[0].attention_owners.is_empty());
        assert!(!not_a_list.sessions[0].claim_grace());
    }

    #[test]
    fn the_real_claim_carry_conforms_to_the_handoff_claim_model() {
        use crate::session_timeline::{claim_supervisor, seed_carried_claims};
        let model = aterm_models_harness::harness_handoff_claim_model();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let grace = model.consts.iter().find(|c| c.0 == "Grace").unwrap().1;
        let new_keys = ["supervisor = ", "claim_known = ", "attention_owners = "];
        let mut driven = 0;
        for pre in ["", "ExternalLease", "ExternalConn", "HostHeld"] {
            for older in [false, true] {
                let mut state = model.init_state();
                let mut schedule: Vec<&str> = [pre].into_iter().filter(|a| !a.is_empty()).collect();
                if older {
                    schedule.push("OlderParent");
                }
                schedule.extend(["Park", "Commit"]);
                for action in &schedule {
                    assert!(model.fire(action, &mut state), "{action}: {state:?}");
                }

                let now = crate::metrics::now_us();
                let live = handle(0, None);
                match pre {
                    "ExternalLease" => {
                        claim_supervisor(&live.ctx, "watch-bob", None, Some(now + 60_000_000), now)
                            .unwrap();
                    }
                    "ExternalConn" => {
                        claim_supervisor(&live.ctx, "relay-x", Some(7), None, now).unwrap();
                    }
                    "HostHeld" => {
                        claim_supervisor(
                            &live.ctx,
                            &crate::harness_host::holder(),
                            None,
                            Some(now + 90_000_000),
                            now,
                        )
                        .unwrap();
                    }
                    _ => {}
                }
                let mut store = SessionStore::default();
                store.register(live);
                let mut wire = SessionHandoff::from_store(&store)
                    .to_toml()
                    .expect("serializes");
                if older {
                    wire = wire
                        .lines()
                        .filter(|line| !new_keys.iter().any(|key| line.starts_with(key)))
                        .map(|line| format!("{line}\n"))
                        .collect();
                }
                let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
                let rec = &read.sessions[0];
                let fresh = handle(0, None);
                let later = now + 1_000_000;
                seed_carried_claims(
                    &fresh.ctx.meta,
                    &fresh.ctx.timeline,
                    rec.supervisor.as_deref(),
                    &rec.attention_owners,
                    later,
                );
                let shown = fresh
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .live_supervisor(later)
                    .map(str::to_owned);
                let server = match shown.as_deref() {
                    None => 0,
                    Some(holder) if holder == crate::harness_host::holder() => 2,
                    Some(_) => 1,
                };
                let real_grace = if rec.claim_grace() { grace } else { 0 };
                assert_eq!(
                    (server, real_grace),
                    (state["server"], state["grace"]),
                    "{schedule:?}: the successor's state is the model's after Commit\n{wire}"
                );
                let host_may = claim_supervisor(
                    &fresh.ctx,
                    "aterm-harness@2",
                    None,
                    Some(later + 90_000_000),
                    later,
                )
                .is_ok()
                    && !rec.claim_grace();
                assert_eq!(
                    host_may,
                    model.action_enabled("HostClaims", &state),
                    "{schedule:?}"
                );

                let mut shipped = buggy.init_state();
                for action in schedule.iter().map(|a| {
                    if *a == "Commit" {
                        "CommitUncarried"
                    } else {
                        *a
                    }
                }) {
                    assert!(buggy.fire(action, &mut shipped), "{action}: {shipped:?}");
                }
                assert_eq!((shipped["server"], shipped["grace"]), (0, 0));
                assert!(buggy.fire("HostClaims", &mut shipped));
                assert_eq!(
                    buggy.check_invariant("NoTakeover", &shipped),
                    matches!(pre, "" | "HostHeld"),
                    "{schedule:?}: the old successor takes exactly the externally held sessions"
                );
                driven += 1;
            }
        }
        assert_eq!(driven, 8, "every pre-Commit configuration of the machine");
    }

    #[test]
    fn a_retired_identity_field_is_read_past() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
identity = \"worker\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.sessions.len(), 1);
        assert_eq!(read.sessions[0].local_id, 3);
        assert_eq!(read.sessions[0].sid, "s-old");
        let wire = read.to_toml().expect("serializes");
        assert!(!wire.contains("identity ="), "{wire}");
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert_eq!(back, read);
    }

    #[cfg(unix)]
    #[test]
    fn the_task_record_rides_per_record_and_absent_reads_none() {
        use std::time::Duration;

        use crate::task_record::{
            MarkFenced, TaskMark, TaskState, mark_session_task, mark_session_task_within,
            seed_carried,
        };
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert_eq!(read.sessions[0].task, None, "absent reads None");
        let typo = SessionHandoff::from_toml(&format!("{old_wire}task = 5\n"))
            .expect("a wrong-typed row costs the row, never the manifest");
        let unread = handle(7, None);
        seed_carried(&unread.ctx, typo.sessions[0].task.as_deref());
        assert!(
            unread.ctx.timeline.lock().unwrap().task().is_none(),
            "and seeds nothing"
        );

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        let worker = handle(1, None);
        mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Working)).expect("unfenced");
        let folder = "/Users//me/work proj";
        worker
            .ctx
            .timeline
            .lock()
            .unwrap()
            .set_task_worker(aterm_phase::Program::Gemini, Some(folder.to_string()));
        let mut done = TaskMark::new(TaskState::Done);
        done.why = Some("marker");
        done.result_bytes = 5;
        let (record, _) = mark_session_task(&worker.ctx, 1, &done).expect("unfenced");
        store.register(worker.clone());
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions[0].task, None);
        let row = manifest.sessions[1].task.clone().expect("the worker's row");
        assert!(row.starts_with("rev=2 state=done from=ctl since="), "{row}");
        assert!(
            row.ends_with(&format!(
                " agent=gemini trusted_cwd={}",
                crate::control::pct_encode(folder)
            )),
            "a mark keeps the agent and the trusted folder: {row}"
        );
        assert_eq!(record.agent, Some(aterm_phase::Program::Gemini));
        assert_eq!(record.trusted_cwd.as_deref(), Some(folder));
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("\ntask = ").count(),
            1,
            "None stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert_eq!(back.sessions[1].task.as_deref(), Some(row.as_str()));

        let now = crate::metrics::now_us();
        let holder = crate::harness_host::holder();
        let drawn = CarriedClaims::of_record(&manifest.sessions[1]);
        assert_eq!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            drawn
        );

        let adopted = handle(8, None);
        seed_carried(&adopted.ctx, back.sessions[1].task.as_deref());
        let seeded = adopted.ctx.timeline.lock().unwrap().task().cloned();
        let seeded = seeded.expect("seeded");
        assert_eq!(
            crate::task_record::TaskRecord {
                since: record.since,
                ..seeded
            },
            record,
            "the same record, its since re-anchored"
        );
        let seeded_folder = |h: &SessionHandle| {
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .task()
                .and_then(|t| t.trusted_cwd.clone())
        };
        assert_eq!(seeded_folder(&adopted), Some(folder.to_string()));
        let carried = back.sessions[1].task.as_deref().expect("the row");
        let encoded = crate::control::pct_encode(folder);
        for bad in [
            "work".to_string(),
            crate::control::pct_encode("/Users//me/../etc"),
            crate::control::pct_encode("/Users//me/"),
            "/Users//me/a%0Ab".to_string(),
        ] {
            let other = handle(10, None);
            seed_carried(&other.ctx, Some(&carried.replace(&encoded, &bad)));
            let seeded = other.ctx.timeline.lock().unwrap().task().cloned();
            let seeded = seeded.unwrap_or_else(|| panic!("{bad}: the record crosses"));
            assert_eq!(seeded.trusted_cwd, None, "{bad}");
            assert_eq!(seeded.agent, Some(aterm_phase::Program::Gemini), "{bad}");
        }
        let other = handle(11, None);
        seed_carried(
            &other.ctx,
            Some(&carried.replace(&format!(" trusted_cwd={encoded}"), "")),
        );
        assert!(other.ctx.timeline.lock().unwrap().task().is_some());
        assert_eq!(seeded_folder(&other), None, "a row from before the field");
        let raised = back.sessions[1]
            .task
            .as_deref()
            .expect("the row")
            .replace(" attention=0", " attention=1");
        let unclaimed = handle(9, None);
        seed_carried(&unclaimed.ctx, Some(&raised));
        let seeded = unclaimed.ctx.timeline.lock().unwrap().task().cloned();
        assert!(!seeded.expect("seeded").attention, "no entry, no flag");

        mark_session_task(&worker.ctx, 1, &TaskMark::new(TaskState::Stalled)).expect("unfenced");
        assert_ne!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            drawn,
            "a mark after the park's draw stands the Commit down"
        );
        let fenced = CarriedClaims::of_session(1, &worker.ctx, now, &holder);
        let _ = worker.ctx.fabric.fence_hold(false);
        let done = TaskMark::new(TaskState::Done);
        assert_eq!(
            mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO),
            Err(MarkFenced)
        );
        assert_eq!(
            CarriedClaims::of_session(1, &worker.ctx, now, &holder),
            fenced
        );
        worker.ctx.fabric.lift_hold_fence();
        let landed = mark_session_task_within(&worker.ctx, 1, &done, Duration::ZERO);
        assert!(landed.is_ok_and(|(_, moved)| moved), "lifted, it lands");
    }

    #[test]
    fn the_rekey_channel_rides_per_record_and_absent_reads_false() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert!(!read.sessions[0].rekey, "absent reads false");

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_rekey_channel(1);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.rekey)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("rekey").count(),
            1,
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert!(back.sessions[1].rekey);
        store.deregister_local(1);
        assert!(!store.has_rekey_channel(1), "a closed session forgets it");
    }

    #[test]
    fn the_loader_rides_per_record_and_absent_reads_false() {
        let old_wire = "schema = 1

[[sessions]]
local_id = 3
sid = \"s-old\"
state = \"alive\"
title = \"zsh\"
";
        let read = SessionHandoff::from_toml(old_wire).expect("this build reads an old manifest");
        assert!(!read.sessions[0].loader, "absent reads false");

        let mut store = SessionStore::default();
        store.register(handle(0, None));
        store.register(handle(1, None));
        store.mark_body_loader(1);
        store.mark_integration_frozen(0);
        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(
            manifest
                .sessions
                .iter()
                .map(|r| r.loader)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
        let wire = manifest.to_toml().expect("serializes");
        assert_eq!(
            wire.matches("loader").count(),
            1,
            "false stays off the wire: {wire}"
        );
        assert!(manifest.roundtrips());
        let back = SessionHandoff::from_toml(&wire).expect("round-trips");
        assert!(back.sessions[1].loader);
        assert!(store.is_integration_frozen(0));
        store.deregister_local(1);
        store.deregister_local(0);
        assert!(!store.has_body_loader(1), "a closed session forgets it");
        assert!(!store.is_integration_frozen(0), "and the frozen mark");
    }

    #[test]
    fn register_indexes_both_key_spaces_and_deregister_clears_both() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.clone();
        store.register(h);

        assert_eq!(store.len(), 1);
        assert!(store.by_sid(&sid).is_some(), "resolvable by sid");
        assert!(store.by_local(7).is_some(), "resolvable by local id");
        assert_eq!(store.by_local(7).unwrap().sid, sid, "both keys agree");

        assert_eq!(store.deregister_local(7), Some(sid.clone()));
        assert!(store.by_sid(&sid).is_none(), "sid index cleared");
        assert!(store.by_local(7).is_none(), "local index cleared");
        assert!(store.is_empty());
        assert_eq!(store.deregister_local(7), None);
    }

    #[test]
    fn snapshot_is_sorted_by_local_id_and_carries_parent() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        store.register(handle(2, Some(root_sid.clone())));
        store.register(handle(1, Some(root_sid.clone())));

        let snap = store.snapshot();
        assert_eq!(
            snap.iter().map(|h| h.local_id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(snap[0].parent, None, "root has no parent");
        assert_eq!(
            snap[1].parent.as_ref(),
            Some(&root_sid),
            "child links to root"
        );
    }

    #[test]
    fn session_handoff_carries_additive_resource_identity_without_changing_sessions() {
        let mut store = SessionStore::default();
        store.register(handle(0, None));
        let mut manifest = SessionHandoff::from_store(&store);
        manifest.resources = Some(aterm_json::json!({
            "boot": vec![7; 16],
            "jobs": [{ "pid": 42, "start": 123456789, "session": "s-a", "ai": "codex",
                       "label": "tree:compiler", "members": [[42, 123456789], [43, 123456790]] }]
        }));
        let parsed = SessionHandoff::from_toml(&manifest.to_toml().unwrap()).unwrap();
        assert_eq!(parsed.resources, manifest.resources);
        assert_eq!(parsed.sessions, manifest.sessions);
        manifest.resources = None;
        assert!(
            SessionHandoff::from_toml(&manifest.to_toml().unwrap())
                .unwrap()
                .resources
                .is_none()
        );
    }

    #[test]
    fn session_handoff_roundtrips_the_whole_set() {
        let mut store = SessionStore::default();
        let root = handle(0, None);
        let root_sid = root.sid.clone();
        store.register(root);
        let peer = handle_in_state(2, Some(root_sid.clone()), SessionState::Alive);
        let peer_sid = peer.sid.clone();
        let peer_ctx = peer.ctx.clone();
        store.register(peer);
        store.register(handle_in_state(
            1,
            Some(root_sid.clone()),
            SessionState::Exited,
        ));
        store.set_title(1, "vim");
        {
            let mut edges = peer_ctx.edges.lock().unwrap();
            let minted = edges.grant_connection(
                &root_sid,
                &peer_sid,
                aterm_session::ConnectionKind::Both,
                &peer_ctx.nonce,
            );
            assert_eq!(minted.len(), 3, "both mints its exact op set");
        }

        let manifest = SessionHandoff::from_store(&store);
        assert_eq!(manifest.sessions.len(), 3, "every live session is packed");
        let carry = |op: &str| ConnectionCarry {
            src: root_sid.as_str().to_string(),
            dst: peer_sid.as_str().to_string(),
            op: op.to_string(),
        };
        assert_eq!(
            manifest.connections,
            vec![carry("read-screen"), carry("signal"), carry("write-input")],
            "the three rows one `both` mints, tokenless and sorted"
        );
        assert!(
            manifest.roundtrips(),
            "serialize -> parse must be the identity"
        );

        let text = manifest.to_toml().unwrap();
        assert!(
            !text
                .as_bytes()
                .windows(64)
                .any(|w| w.iter().all(u8::is_ascii_hexdigit)),
            "no bearer-token-width hex blob may ride the manifest: {text}"
        );
        let restored = SessionHandoff::from_toml(&text).expect("parse");
        assert_eq!(restored, manifest);
        assert_eq!(
            restored.connections, manifest.connections,
            "the tokenless triples round-trip verbatim"
        );
        assert_eq!(
            restored
                .sessions
                .iter()
                .map(|r| r.local_id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "tab order preserved"
        );
        assert_eq!(
            restored.sessions[1].parent.as_deref(),
            Some(root_sid.as_str())
        );
        assert_eq!(restored.sessions[1].state, "exited");
        assert_eq!(restored.sessions[1].title, "vim");
        assert_eq!(
            SessionState::from_str(&restored.sessions[1].state),
            SessionState::Exited
        );

        assert!(SessionHandoff::from_toml("not = [valid").is_none());
        assert!(SessionHandoff::from_toml("schema = 999\nsessions = []").is_none());
    }

    #[test]
    fn mark_alive_is_monotonic_idempotent_and_fail_safe() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(8, None, SessionState::Spawning));

        assert!(store.mark_alive(8));
        assert!(!store.mark_alive(8), "already Alive: no second transition");
        assert_eq!(store.by_local(8).unwrap().state, SessionState::Alive);

        store.register(handle_in_state(9, None, SessionState::Exited));
        assert!(!store.mark_alive(9), "Exited never flips back to Alive");
        assert_eq!(store.by_local(9).unwrap().state, SessionState::Exited);

        assert!(!store.mark_alive(404));
    }

    #[test]
    fn store_mutators_record_ordered_timeline_events() {
        let mut store = SessionStore::default();
        let h = handle_in_state(4, None, SessionState::Spawning);
        let timeline = h.ctx.timeline.clone();
        store.register(h.clone());
        store.register(h);
        assert!(store.mark_alive(4));
        assert!(!store.mark_alive(4), "no-op re-mark records nothing");
        store.set_title(4, "vim main.rs");
        assert!(timeline.lock().unwrap().record_title("vim main.rs"));
        assert!(
            !timeline.lock().unwrap().record_title("vim main.rs"),
            "unchanged: nothing recorded"
        );
        store.set_state(4, SessionState::Exited);
        store.set_state(4, SessionState::Exited);
        store.deregister_local(4);

        let tl = timeline.lock().unwrap();
        let got: Vec<(u64, &str, &str)> = tl
            .since(None)
            .map(|e| (e.id, e.kind, &*e.payload))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "spawned", "state=spawning"),
                (2, "state-change", "state=alive"),
                (3, "title-change", "title=vim%20main.rs"),
                (4, "state-change", "state=exited"),
                (5, "closing", "reason=shell-exit by=-"),
                (6, "state-change", "state=closed"),
            ],
            "one ordered, monotonic-id event per ACTUAL lifecycle change"
        );
    }

    #[test]
    fn roster_journal_mirrors_membership_exactly() {
        let mut store = SessionStore::default();
        assert_eq!(store.roster_seq(), 0, "a virgin store has no history");
        assert_eq!(store.roster_low_seq(), None);
        assert_eq!(store.roster_since(0).count(), 0);

        let replay = |st: &SessionStore| -> std::collections::HashSet<String> {
            let mut set = std::collections::HashSet::new();
            for r in st.roster_since(0) {
                match r.change {
                    RosterChange::Created => {
                        set.insert(r.sid.clone());
                    }
                    RosterChange::Exited => {
                        set.remove(&r.sid);
                    }
                    RosterChange::RetireRequested | RosterChange::Bye(_) => {}
                }
            }
            set
        };

        let a = handle(1, None);
        let b = handle(2, None);
        let (sid_a, sid_b) = (a.sid.as_str().to_string(), b.sid.as_str().to_string());
        store.register(a.clone());
        assert_eq!(replay(&store), store.live_sids());
        store.register(a);
        assert_eq!(store.roster_seq(), 1, "a replace journals nothing");
        store.register(b);
        assert_eq!(replay(&store), store.live_sids());
        store.deregister_local(1);
        assert_eq!(store.deregister_local(1), None);
        assert_eq!(
            store.roster_seq(),
            3,
            "the duplicate deregister journalled nothing"
        );
        assert_eq!(replay(&store), store.live_sids());

        let got: Vec<(u64, &str, RosterChange)> = store
            .roster_since(0)
            .map(|r| (r.seq, r.sid.as_str(), r.change))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, sid_a.as_str(), RosterChange::Created),
                (2, sid_b.as_str(), RosterChange::Created),
                (3, sid_a.as_str(), RosterChange::Exited),
            ]
        );
        assert_eq!(store.roster_since(1).count(), 2);
        assert_eq!(store.roster_since(3).count(), 0);
        assert_eq!(store.roster_since(99).count(), 0, "past the high = nothing");
        assert_eq!(store.roster_low_seq(), Some(1));
    }

    #[test]
    fn roster_journal_is_bounded_and_advertises_its_low_water() {
        let mut store = SessionStore::default();
        for i in 0..(ROSTER_JOURNAL_CAP as u64) {
            store.register(handle(i, None));
            store.deregister_local(i);
        }
        let total = 2 * ROSTER_JOURNAL_CAP as u64;
        assert_eq!(
            store.roster_seq(),
            total,
            "seqs keep counting past eviction"
        );
        assert_eq!(
            store.roster_since(0).count(),
            ROSTER_JOURNAL_CAP,
            "retention is capped"
        );
        assert_eq!(
            store.roster_low_seq(),
            Some(total - ROSTER_JOURNAL_CAP as u64 + 1),
            "the low-water names the oldest retained seq"
        );
        assert!(
            store.is_empty(),
            "every session registered was deregistered"
        );
    }

    fn timeline_of(h: &SessionHandle) -> Vec<(&'static str, String)> {
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .map(|e| (e.kind, e.payload.to_string()))
            .collect()
    }

    #[test]
    fn attributed_deregister_records_reason_actor_clock_and_exit_code() {
        let mut store = SessionStore::default();
        let h = handle(7, None);
        let sid = h.sid.as_str().to_string();
        store.register(h.clone());
        let before = crate::turn_ledger::now_ms();
        store.note_exit_code(7, Some(3));
        assert_eq!(
            store.deregister_local_as(7, ExitReason::CtlClose, ExitActor::Sid("s-caller".into())),
            Some(h.sid.clone())
        );
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .expect("the exit was journalled")
            .clone();
        assert_eq!(rec.sid, sid);
        assert_eq!(rec.local_id, 7);
        assert_eq!(rec.reason, ExitReason::CtlClose);
        assert_eq!(rec.actor, ExitActor::Sid("s-caller".into()));
        assert_eq!(rec.exit_code, Some(3));
        assert!(
            rec.t_ms >= before && rec.t_ms <= crate::turn_ledger::now_ms(),
            "stamped on the process-epoch clock: {} not in [{before}, now]",
            rec.t_ms
        );
        let created = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Created)
            .expect("the birth was journalled");
        assert_eq!(
            (created.reason, created.exit_code, &created.actor),
            (ExitReason::Unknown, None, &ExitActor::Unknown)
        );
        let tl = timeline_of(&h);
        assert_eq!(tl[0].0, "spawned");
        assert_eq!(
            &tl[1..],
            &[
                ("closing", "reason=ctl-close by=s-caller".to_string()),
                ("state-change", "state=closed".to_string()),
            ]
        );
    }

    #[test]
    fn unattributed_deregister_infers_shell_exit_from_the_exited_state_only() {
        let mut store = SessionStore::default();
        let exited = handle_in_state(1, None, SessionState::Exited);
        let alive = handle(2, None);
        store.register(exited.clone());
        store.register(alive.clone());
        store.deregister_local(1);
        store.deregister_local(2);
        let reasons: Vec<(String, ExitReason, ExitActor)> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| (r.sid.clone(), r.reason, r.actor.clone()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                (
                    exited.sid.as_str().to_string(),
                    ExitReason::ShellExit,
                    ExitActor::Unknown
                ),
                (
                    alive.sid.as_str().to_string(),
                    ExitReason::Unknown,
                    ExitActor::Unknown
                ),
            ]
        );
        assert!(
            timeline_of(&exited)
                .iter()
                .any(|(k, p)| *k == "closing" && p == "reason=shell-exit by=-"),
            "the inferred shell exit is a known reason: {:?}",
            timeline_of(&exited)
        );
        assert!(
            !timeline_of(&alive).iter().any(|(k, _)| *k == "closing"),
            "an unknown reason records no closing event: {:?}",
            timeline_of(&alive)
        );
    }

    #[test]
    fn an_explicit_reason_outranks_the_exited_state() {
        let mut store = SessionStore::default();
        store.register(handle_in_state(4, None, SessionState::Exited));
        store.deregister_local_as(4, ExitReason::UiClose, ExitActor::Human);
        let rec = store
            .roster_since(0)
            .find(|r| r.change == RosterChange::Exited)
            .unwrap();
        assert_eq!(
            (rec.reason, &rec.actor),
            (ExitReason::UiClose, &ExitActor::Human)
        );
    }

    #[test]
    fn a_noted_exit_code_is_consumed_exactly_once() {
        let mut store = SessionStore::default();
        store.register(handle(9, None));
        store.note_exit_code(9, None);
        store.deregister_local(9);
        store.register(handle(9, None));
        store.note_exit_code(9, Some(7));
        store.deregister_local(9);
        store.register(handle(9, None));
        store.deregister_local(9);
        let codes: Vec<Option<i32>> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.exit_code)
            .collect();
        assert_eq!(codes, vec![None, Some(7), None]);
    }

    #[test]
    fn attributed_exits_stay_bounded_and_page_by_seq() {
        let mut store = SessionStore::default();
        let n = ROSTER_JOURNAL_CAP as u64 + 5;
        for i in 0..n {
            store.register(handle(i, None));
            store.note_exit_code(i, Some(i32::try_from(i % 256).unwrap()));
            store.deregister_local_as(i, ExitReason::CtlClose, ExitActor::Sid(format!("s-{i}")));
        }
        let exits: Vec<&RosterRecord> = store
            .roster_since(0)
            .filter(|r| r.change == RosterChange::Exited)
            .collect();
        assert_eq!(exits.len(), ROSTER_JOURNAL_CAP / 2);
        let newest = exits.last().unwrap();
        assert_eq!(newest.seq, store.roster_seq());
        assert_eq!(newest.actor, ExitActor::Sid(format!("s-{}", n - 1)));
        assert_eq!(
            newest.exit_code,
            Some(i32::try_from((n - 1) % 256).unwrap())
        );
        let third = exits[exits.len() - 3].seq;
        let newer: Vec<u64> = store
            .roster_since(third)
            .filter(|r| r.change == RosterChange::Exited)
            .map(|r| r.seq)
            .collect();
        assert_eq!(newer, vec![exits[exits.len() - 2].seq, newest.seq]);
        assert_eq!(store.roster_since(newest.seq).count(), 0, "caught up");
    }

    #[test]
    fn close_attribution_scope_is_outermost_wins_and_restores() {
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown)
        );
        {
            let _outer =
                CloseAttribution::enter(ExitReason::CtlClose, ExitActor::Sid("s-a".into()));
            {
                let _inner = CloseAttribution::enter(ExitReason::UiClose, ExitActor::Human);
                assert_eq!(
                    current_close_attribution(),
                    (ExitReason::CtlClose, ExitActor::Sid("s-a".into())),
                    "the inner claim does not override the initiator"
                );
            }
            assert_eq!(
                current_close_attribution().0,
                ExitReason::CtlClose,
                "dropping the inner (non-opener) guard leaves the scope open"
            );
        }
        assert_eq!(
            current_close_attribution(),
            (ExitReason::Unknown, ExitActor::Unknown),
            "dropping the opener closes the scope"
        );
    }

    #[test]
    fn exit_reason_and_actor_wire_tokens() {
        let tokens: Vec<&str> = ExitReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            tokens,
            [
                "shell-exit",
                "ctl-close",
                "ui-close",
                "window-close",
                "app-quit",
                "unknown"
            ]
        );
        assert_eq!(ExitActor::Sid("s-z".into()).as_wire(), "s-z");
        assert_eq!(ExitActor::Human.as_wire(), "human");
        assert_eq!(ExitActor::Ctl.as_wire(), "ctl");
        assert_eq!(ExitActor::Bridge.as_wire(), "bridge");
        assert_eq!(ExitActor::Unknown.as_wire(), "-");
    }
}
