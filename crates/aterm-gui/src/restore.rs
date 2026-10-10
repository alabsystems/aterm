// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RESTORE-2: recursive terminal/native workspace restore across quit + relaunch.
//!
//! The persisted format is deliberately SEPARATE from `session_store::SessionHandoff`
//! (which hands live fds to a re-exec'd process for a seamless self-update — a
//! single-boot handoff that models no window/tab/pane tree and no cwd). This manifest is
//! the durable, human-inspectable layout written on graceful quit and applied on the
//! next cold launch: the window grid, each terminal tab's pane tree, and the stable
//! descriptors needed to reopen first-party native tabs. Durable native descriptors
//! never contain process-local ids, document bytes, config values, or secrets; bounded
//! source-addressed view selections and viewport anchors are persisted so a reopened
//! document returns to the same reading/editing position. The one exception is the
//! seamless update's attempt-bound handoff layout, and only its Settings leaves: they
//! carry the view's unsaved field drafts ([`SettingsDraftRestore`]) so the successor
//! reopens them — see [`NativeLeafRestore::settings_drafts`].
//!
//! Process-local `u64` session ids are NOT persisted — a Leaf stores the shell's cwd +
//! title so a FRESH session can be respawned in the same place, and the pane tree is
//! rebuilt with new ids in deterministic tree order (see `pane::PaneTree::rebuild`).
//!
//! Parsing is fail-safe: any I/O error, malformed TOML, or schema mismatch yields `None`
//! (fall back to a normal fresh window), and the manifest is single-use (deleted on read)
//! so a crash mid-restore can never loop.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The current on-disk schema. Bumped on any incompatible layout change; a manifest with
/// a different schema is ignored (fresh start), never mis-parsed.
pub(crate) const SCHEMA: u32 = 2;
const LEGACY_SCHEMA: u32 = 1;

pub(crate) const MAX_WINDOWS: usize = 64;
pub(crate) const MAX_TABS_PER_WINDOW: usize = 256;
const MAX_DOCUMENT_URI_BYTES: usize = 8 * 1024;
const MAX_SETTINGS_ROUTE_BYTES: usize = 128;
const MAX_RESTORE_TAG_BYTES: usize = 128;
const MAX_LEAVES_PER_TAB: usize = 256;
const MAX_SPLIT_DEPTH: usize = 32;
/// The words every cap refusal of [`RestoreManifest::why_not_whole`] ends its
/// cap with, which the update's record keys on to show that refusal as it is
/// (`update_words::capture_refused_waiting`).
pub(crate) const UPDATE_CAP_KEY: &str = "an update can carry";
const MAX_VIEW_METADATA_BYTES: usize = 16 * 1024;
const MAX_EDITOR_SELECTIONS: usize = 256;
/// The longest Settings field key a carried draft may name. Keys are the
/// `aterm.toml` paths the Settings rows edit (`sparkle_words.lexicon`), far
/// shorter than this; the bound exists so a malformed layout cannot make the
/// successor hash or compare an unbounded key.
const MAX_SETTINGS_DRAFT_KEY_BYTES: usize = 128;
/// The longest unsaved Settings field text an update carries. A field longer
/// than this is not carried; the update waits for it as it always did
/// ([`settings_draft_is_carriable`]).
pub(crate) const MAX_SETTINGS_DRAFT_TEXT_BYTES: usize = 16 * 1024;
/// The most drafts one Settings view carries: above the number of rows the
/// Settings pages edit (`prefs::editable_fields`, pinned below it by a test),
/// so a real capture never meets it and only a malformed layout is cut by it.
pub(crate) const MAX_SETTINGS_DRAFTS_PER_VIEW: usize = 256;

/// Whether one unsaved Settings field draft can ride a seamless update — the
/// ONE rule the outgoing preflight counts a draft as carried by, the handoff
/// capture writes by, and the layout's sanitize (capture and parse alike)
/// keeps by. A draft it refuses is not carried: the preflight holds the update
/// for it, exactly as every draft was held before drafts were carried.
///
/// A NUL is refused because no string a layout carries may hold one (the
/// capture must be a fixed point of the parse); the bounds keep the layout
/// sidecar far below the successor's `MAX_HANDOFF_LAYOUT_BYTES`.
#[must_use]
pub(crate) fn settings_draft_is_carriable(key: &str, text: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_SETTINGS_DRAFT_KEY_BYTES
        && !key.contains('\0')
        && text.len() <= MAX_SETTINGS_DRAFT_TEXT_BYTES
        && !text.contains('\0')
}

/// A pane's working directory as a layout may carry it — at most
/// `MAX_DOCUMENT_URI_BYTES` and no NUL — else `None`. The one rule both leaf
/// shapes (`TerminalLeafRestore`, the legacy `PaneLayout::Leaf`) sanitize with,
/// so the capture and the parse can never disagree about a directory (the
/// 2026-09-22/23 update audit, plan P0-1f).
fn storable_leaf_cwd(cwd: Option<String>) -> Option<String> {
    cwd.filter(|cwd| cwd.len() <= MAX_DOCUMENT_URI_BYTES && !cwd.contains('\0'))
}

/// Strip NULs from a pane title and cut it to `MAX_DOCUMENT_URI_BYTES` at a
/// character boundary — the title a layout may carry. Idempotent, so a sanitized
/// capture is a fixed point of `from_toml`, which is what the update worker's
/// layout round trip checks.
fn sanitize_leaf_title(title: &mut String) {
    if title.contains('\0') {
        title.retain(|character| character != '\0');
    }
    if title.len() > MAX_DOCUMENT_URI_BYTES {
        let mut end = MAX_DOCUMENT_URI_BYTES;
        while !title.is_char_boundary(end) {
            end -= 1;
        }
        title.truncate(end);
    }
}

/// serde-stable mirror of [`crate::pane::SplitDir`] — kept separate so the on-disk format
/// never couples to the internal enum's representation.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SplitKind {
    Vertical,
    Horizontal,
}

/// Stable path component used to identify focus and closed-view insertion points without
/// persisting a process-local [`crate::tab_model::ViewId`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestoreBranch {
    First,
    Second,
}

/// One source-addressed selection. Byte offsets are clamped to the reopened document at
/// apply time; the persisted value can never become an unchecked slice index.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RestoreSelection {
    pub anchor: usize,
    pub head: usize,
}

/// Stable metadata for a terminal split leaf. `local_id` is adoption-only and is ignored
/// on a cold launch; it is never treated as a durable session identity.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct TerminalLeafRestore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<u64>,
    /// USER session metadata (session-metadata stage 1; additive, absent
    /// tolerated by older manifests): the operator's `meta set` title/
    /// description/icon/role/attention, captured at quit (and at a seamless
    /// update's park) and RE-SEEDED on the way back onto the shell this leaf
    /// stands for — after a quit or a closed tab's reopen, the session that
    /// fills the leaf; after a seamless update, the handed-off shell
    /// `local_id` names, wherever it runs, and never a fresh shell standing in
    /// for it
    /// (`App::carry_restored_identity`, which also leaves alone any field a
    /// driver wrote on a live session first) — so restore keeps the
    /// operator-chosen identity (the `title` above is the engine/OSC one — a
    /// different datum).
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
    /// The session's `meta set questions` policy word (2026-09-24; additive,
    /// absent tolerated, not written when unset): carried like the five
    /// fields above, so a restored or updated worker keeps the question policy
    /// a driver gave it. Re-validated on the way in
    /// (`sanitize_user_metadata`): a hand-edited value outside the four words
    /// is dropped, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<String>,
    /// IDENTITY (session identities, 2026-09-17; additive, absent tolerated):
    /// the agent identity the session was spawned under, so a cold restore
    /// respawns the leaf's shell under it — `create = false`
    /// (`agent_identity::restorable`): a forgotten identity falls back to a
    /// default shell with one stderr line; restore never creates one. On a
    /// seamless update the adopted shell keeps its env and the label rides the
    /// handoff record instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// THE AGENT THIS LEAF HOSTED (2026-09-27; additive, absent tolerated):
    /// what a relaunch needs, read while it ran by the window's supervisor
    /// host — or, none published yet, the one a relaunch still owes the tab
    /// or an update's parent handed it running (`App::fill_desk_agents`) —
    /// carried by the crash journal ([`crate::crash_journal`]) and by the
    /// quit layout of a restart, a logout or a shutdown
    /// ([`crate::system_quit`]), never by the one a person's own quit
    /// writes, so an end they did not choose (a crash, a kill, a power loss,
    /// the system's quit) relaunches it on its conversation in its restored
    /// tab, and one they chose does not.
    ///
    /// ON A SEAMLESS UPDATE'S HANDOFF LAYOUT (round four of the 2026-09
    /// update robustness work, plan item 7) it names an agent a cold
    /// restore's relaunch queue STILL OWED this tab at the park
    /// (`HostHandle::pending_restored_for`), and nothing else: the successor
    /// relaunches it in the adopted shell the leaf names at its Commit
    /// (`App::hand_carried_restored_agents`). An older producer never writes
    /// one on a handoff leaf, and an older successor ignores it there.
    /// Boxed: the other leaf kinds are a fraction of its size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Box<AgentRestore>>,
    /// HELD BY A PROGRAM (audit #8; additive, absent tolerated, written only
    /// when true): at capture a program other than the pane's shell held its
    /// PTY (ssh, a container's shell, vim — the quit confirm's `tcgetpgrp`
    /// test), and it was not the agent aterm hosts there, which runs on this
    /// machine (`App::hosted_agent`), so `cwd` may name a folder on another
    /// machine
    /// ([`Self::folder_may_be_remote`]). The restore then takes it only when
    /// a shell can start in it here ([`Self::start_cwd`]), as a New Tab from
    /// that pane does. Only the durable capture (quit,
    /// crash journal, a closed tab) records it; the seamless update's layout
    /// never does, since its Commit compares layouts and a program may start
    /// or end in between.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
}

/// A hosted agent as its leaf's restore carries it (see
/// [`TerminalLeafRestore::agent`]): the relaunch snapshot's durable half
/// (`aterm_agent::harness::relaunch::Snapshot`), without the tab and the
/// shell, which the restored leaf provides.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct AgentRestore {
    /// The agent's pid when it ran, and its kernel start: the pid-reuse guard
    /// that tells a process still running from the one that ended.
    pub pid: u32,
    pub start: String,
    /// What it ran (an absolute path) and its argv, program first.
    pub program: String,
    pub argv: Vec<String>,
    /// Its conversation, when it had registered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The directory it was started in.
    pub cwd: String,
    /// Its version, when it had registered one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// THE CLAUDE HOME IT RAN UNDER (2026-10-09; additive, absent tolerated,
    /// written only when set): the raw `CLAUDE_CONFIG_DIR` of its own exec
    /// environment, as the host read it while it ran
    /// (`aterm_agent::harness::relaunch::Snapshot::home`). The relaunch after
    /// aterm ended puts it back on its line as every live relaunch does
    /// (`relaunch::required_env`): without it, an agent a wrapper or its own
    /// command line started under another home came back on
    /// `~/.claude.json`, in the first-run theme picker, never resuming
    /// (measured live for the explicit restart, `b16490d3d`). An older
    /// producer never writes it, and that leaf restores as it always did; an
    /// older successor ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// THE LOGIN IT RAN UNDER, the same way: the raw
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR` (`Snapshot::login`). Set and empty —
    /// the default login NAMED — is written `""`, which is not the same line
    /// as none. A leaf that names a home or a login is relaunched under
    /// them, never under the roster's proven active account, which is for an
    /// agent nothing says the login of (`harness_host::RestoredAgent::unread`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// A CODEX's run (its home, its mode, when it started): what its
    /// relaunch reads beside the thread `session` names. `None` for Claude
    /// Code — and a Codex whose run is lost is relaunched by no lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexRestore>,
}

/// A Codex TUI's run as its leaf's restore carries it
/// (`aterm_agent::harness::relaunch::CodexRun`).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct CodexRestore {
    /// The `$CODEX_HOME` it ran with.
    pub home: String,
    /// It held its thread's writer lock itself; `false`: its daemon did.
    pub embedded: bool,
    /// When it started, unix milliseconds.
    pub started_ms: u64,
}

impl AgentRestore {
    /// The process it was: its pid and kernel start. One agent is named on
    /// every leaf that showed its session (`Open Session in New Window`), so
    /// whatever relaunches it, or says it comes back, goes by this, once.
    pub(crate) fn ran_as(&self) -> (u32, &str) {
        (self.pid, &self.start)
    }

    /// The durable half of the relaunch snapshot `snap`.
    pub(crate) fn of(snap: aterm_agent::harness::relaunch::Snapshot) -> Self {
        Self {
            pid: snap.pid,
            start: snap.start,
            program: snap.program.to_string_lossy().into_owned(),
            argv: snap.argv,
            session: snap.session,
            cwd: snap.cwd,
            version: snap.version,
            home: snap.home,
            login: snap.login,
            codex: snap.codex.map(|run| CodexRestore {
                home: run.home.to_string_lossy().into_owned(),
                embedded: run.embedded,
                started_ms: run.started_ms,
            }),
        }
    }

    /// The relaunch snapshot of this agent in the reopened tab `tab`, whose
    /// shell is `shell`.
    pub(crate) fn snapshot(
        self,
        tab: String,
        shell: u32,
    ) -> aterm_agent::harness::relaunch::Snapshot {
        aterm_agent::harness::relaunch::Snapshot {
            tab,
            pid: self.pid,
            start: self.start,
            shell,
            program: std::path::PathBuf::from(self.program),
            argv: self.argv,
            session: self.session,
            cwd: self.cwd,
            version: self.version,
            codex: self
                .codex
                .map(|run| aterm_agent::harness::relaunch::CodexRun {
                    home: std::path::PathBuf::from(run.home),
                    embedded: run.embedded,
                    started_ms: run.started_ms,
                    ambiguous: false,
                    block: None,
                }),
            // Read by the frozen remedy's wording alone
            // (`relaunch::resumes_on_exit`), never by the relaunch, whose plan
            // reads the shell's dialect itself when it runs; and not read here,
            // on the event loop, from a shell that just started.
            dialect: None,
            // The login and the Claude home the agent ran under, as the
            // layout carried them off the host's snapshot (nothing is read
            // here, on the event loop): its relaunch line carries both, as a
            // live relaunch's does. A leaf that names neither (an older
            // build's, or an agent that had neither variable) adds nothing
            // of its own: the host that relaunches it knows the snapshot is
            // the layout's (`harness_host::RestoredAgent::unread`), and where
            // the owner listed two or more Claude Code accounts it carries
            // the machine's proven active one; with no roster the relaunch
            // takes the reopened shell's own, as ever.
            login: self.login,
            home: self.home,
        }
    }
}

impl AgentRestore {
    /// Whether the relaunch after aterm ended brings this agent back on its
    /// conversation ([`agent_resumes`]).
    pub(crate) fn resumes(&self) -> bool {
        agent_resumes(self.session.as_deref(), &self.argv)
    }
}

/// Whether an agent aterm hosted when it ended — its conversation `session`
/// and its argv, as the live layout carried them — is one the relaunch in
/// its reopened tab brings back (P6a, ruling 293): it names a conversation,
/// and it is no one-shot run (`-p`, `--version`), whose end is the one it was
/// launched for. `aterm_agent::harness::relaunch::after_host_ended` refuses
/// exactly these two (`ended:no-conversation`, `ended:one-shot`) before it
/// looks at the tab; what it finds there (a shell gone, a tab never ready)
/// is said when it happens ([`crate::message_reporters::restored_agents_not_resumed`]).
pub(crate) fn agent_resumes(session: Option<&str>, argv: &[String]) -> bool {
    use aterm_agent::harness::upgrade::{is_session_id, one_shot};
    session.is_some_and(is_session_id) && !one_shot(argv)
}

impl TerminalLeafRestore {
    /// Whether this leaf's folder may be another machine's: a program held
    /// the pane ([`Self::held`]), and the leaf carries no agent aterm hosted
    /// there ([`Self::agent`]), which ran on this machine in that folder.
    pub(crate) fn folder_may_be_remote(&self) -> bool {
        self.held && self.agent.is_none()
    }

    /// The folder this leaf's fresh shell is asked to start in: its `cwd`,
    /// unless that folder may be another machine's
    /// ([`Self::folder_may_be_remote`]) and no shell can start in it here.
    /// Then it is not asked for and not reported: the shell starts where one
    /// with no folder would ([`crate::spawn_folder::inherited`]).
    pub(crate) fn start_cwd(&self) -> Option<String> {
        crate::spawn_folder::inherited(self.cwd.clone()?, || self.folder_may_be_remote())
    }

    /// The six USER fields this leaf carries, as the values of the
    /// [`SessionMeta`](crate::session_timeline::SessionMeta) they were captured
    /// from.
    pub(crate) fn carried_user_meta(&self) -> crate::session_timeline::SessionMeta {
        crate::session_timeline::SessionMeta {
            user_title: self.user_title.clone(),
            description: self.description.clone(),
            icon: self.icon.clone(),
            role: self.role.clone(),
            attention: self.attention.clone(),
            questions: self.questions.clone(),
            ..Default::default()
        }
    }

    /// Make this leaf one `RestoredView::bounded` admits, whatever wrote it.
    ///
    /// The SESSION half (cwd, title, profile) is program output, not operator
    /// input: the capture copies the engine's OSC 7 directory and OSC 0/2 title
    /// straight into the leaf. A NUL there (`file:///tmp/a%00b`) used to fail
    /// `bounded()`, so `from_toml` refused the WHOLE manifest — and the update
    /// worker's layout round trip filed that as a Structural preparation
    /// failure, which latches (the 2026-09-22/23 update audit, plan P0-1f).
    /// Now an unstorable directory or profile is dropped, and the title keeps
    /// everything but its NULs, cut to the bound at a character boundary. The
    /// directory only seeds a cold respawn's cwd, so losing it costs a `cd`.
    fn sanitize(&mut self) {
        self.cwd = storable_leaf_cwd(self.cwd.take());
        sanitize_leaf_title(&mut self.title);
        self.profile = self
            .profile
            .take()
            .filter(|profile| profile.len() <= MAX_SETTINGS_ROUTE_BYTES && !profile.contains('\0'));
        self.sanitize_user_metadata();
    }

    /// Restore files survive across versions and are operator-writable. Normalize
    /// USER chrome fields before they can be re-seeded onto a live session; the
    /// control path's visible rejection cannot protect this persistence path.
    fn sanitize_user_metadata(&mut self) {
        self.user_title = self
            .user_title
            .as_deref()
            .and_then(|value| carried_user_value("title", value));
        self.description = self
            .description
            .as_deref()
            .and_then(|value| carried_user_value("description", value));
        self.icon = self
            .icon
            .as_deref()
            .and_then(|value| carried_user_value("icon", value));
        self.role = self
            .role
            .as_deref()
            .and_then(|value| carried_user_value("role", value));
        self.attention = self
            .attention
            .as_deref()
            .and_then(|value| carried_user_value("attention", value));
        self.questions = self
            .questions
            .as_deref()
            .and_then(|value| carried_user_value("questions", value));
    }

    fn user_metadata_is_canonical(&self) -> bool {
        let canonical = |field: &str, value: &Option<String>| {
            value.as_ref().is_none_or(|value| {
                carried_user_value(field, value).as_deref() == Some(value.as_str())
            })
        };
        canonical("title", &self.user_title)
            && canonical("description", &self.description)
            && canonical("icon", &self.icon)
            && canonical("role", &self.role)
            && canonical("attention", &self.attention)
            && canonical("questions", &self.questions)
    }
}

/// One Settings text field's unsaved draft: the `aterm.toml` key its row edits and
/// the text the person typed but did not save. Carried by a seamless update's
/// handoff layout only ([`NativeLeafRestore::settings_drafts`]).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct SettingsDraftRestore {
    pub key: String,
    pub text: String,
}

/// Read a layout's `settings_drafts` LENIENTLY: a carry this build cannot read
/// costs the drafts, never the layout. Every entry that is not a
/// `{ key, text }` table — and the whole field, when it is not a list — becomes
/// an entry with an empty key, which no draft can have, so the sanitize that
/// every parse runs ([`NativeLeafRestore::sanitize_settings_drafts`]) drops it
/// and COUNTS it, and the successor says how many it could not reopen.
///
/// Why not a plain `Vec`: a strict field fails the whole manifest's parse, and
/// a handoff layout that does not parse costs every native tab and the whole
/// pane arrangement (`seamless::take_incoming`'s orphan net) — to save the text
/// of a Settings field.
fn lenient_settings_drafts<'de, D>(deserializer: D) -> Result<Vec<SettingsDraftRestore>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Draft(SettingsDraftRestore),
        Unreadable(serde::de::IgnoredAny),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entries {
        List(Vec<Entry>),
        Unreadable(serde::de::IgnoredAny),
    }
    let unreadable = || SettingsDraftRestore {
        key: String::new(),
        text: String::new(),
    };
    Ok(match Entries::deserialize(deserializer)? {
        Entries::List(entries) => entries
            .into_iter()
            .map(|entry| match entry {
                Entry::Draft(draft) => draft,
                Entry::Unreadable(_) => unreadable(),
            })
            .collect(),
        Entries::Unreadable(_) => vec![unreadable()],
    })
}

/// One USER field as a restore carries it: the presentation sanitizer, and for
/// `attention` the carry's own rule as well
/// ([`crate::session_timeline::carried_attention`]). The ONE rule both
/// `sanitize_user_metadata` and `user_metadata_is_canonical` ask, so a
/// sanitized leaf is canonical by construction: a value the sanitizer kept and
/// the canonical check refused would make `from_toml` refuse the WHOLE
/// manifest — on a handoff, the layout lost to the orphan net.
fn carried_user_value(field: &str, value: &str) -> Option<String> {
    if field == "attention" {
        return crate::session_timeline::carried_attention(value);
    }
    crate::session_timeline::sanitize_metadata_value(field, value)
}

/// Per-view native state. Canonical document bytes and draft contents deliberately do not
/// appear here: Editor durability is addressed by the document journal and `durable_seq`.
/// The one exception is [`Self::settings_drafts`], carried by a seamless update's handoff
/// layout and by nothing else. Unknown app versions may put a bounded, copyable
/// description in `metadata`; it is data, never executable input.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct NativeLeafRestore {
    pub restore_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// This Editor is the canonical aterm.toml Manual surface. Additive for
    /// backward compatibility; ordinary Editor restores remain `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub config_editor: bool,
    #[serde(default)]
    pub source_anchor: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<RestoreSelection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editor_selections: Vec<RestoreSelection>,
    #[serde(default)]
    pub primary_selection: usize,
    #[serde(default)]
    pub viewport_anchor: usize,
    #[serde(default)]
    pub durable_seq: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
    /// A Settings view's unsaved field drafts, in key order — carried ONLY by a
    /// seamless update's handoff layout (`App::capture_handoff_layout`), so the
    /// successor reopens the view with the text the person typed and it stays
    /// unsaved there. A durable manifest (quit, crash journal, closed-tab
    /// recovery) never holds one: those outlive the process, and an unsaved
    /// draft was never promised to (only the handoff is a continuation of the
    /// same session).
    ///
    /// ADDITIVE AND LENIENT ON PURPOSE. Absent in every layout that carries no
    /// draft, so an older consumer's bytes are unchanged; an older consumer
    /// that meets one ignores it (this struct denies no unknown field) — which
    /// is why the outgoing process refuses to carry drafts to an OLDER build
    /// (`App::start_unix_update_handoff`). A value this build cannot read is
    /// dropped and counted ([`lenient_settings_drafts`]), never a refusal of
    /// the layout.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_settings_drafts"
    )]
    pub settings_drafts: Vec<SettingsDraftRestore>,
    /// How many carried drafts the parse had to drop — unreadable, not
    /// carriable ([`settings_draft_is_carriable`]), repeated, past
    /// [`MAX_SETTINGS_DRAFTS_PER_VIEW`], or on a leaf that is not Settings. Never
    /// written: it exists so the successor says a draft was lost out loud
    /// instead of reopening the view as if there had been none. Always zero in
    /// a capture, whose drafts are carriable by construction, so the capture is
    /// still a fixed point of the parse.
    #[serde(skip)]
    pub settings_drafts_unreadable: usize,
}

impl NativeLeafRestore {
    pub(crate) fn settings(route: String) -> Self {
        Self {
            restore_tag: "settings".to_string(),
            route: Some(route),
            uri: None,
            config_editor: false,
            source_anchor: 0,
            selection: None,
            editor_selections: Vec::new(),
            primary_selection: 0,
            viewport_anchor: 0,
            durable_seq: 0,
            metadata: String::new(),
            settings_drafts: Vec::new(),
            settings_drafts_unreadable: 0,
        }
    }

    pub(crate) fn document(restore_tag: &str, uri: String) -> Self {
        Self {
            restore_tag: restore_tag.to_string(),
            route: None,
            uri: Some(uri),
            config_editor: false,
            source_anchor: 0,
            selection: None,
            editor_selections: Vec::new(),
            primary_selection: 0,
            viewport_anchor: 0,
            durable_seq: 0,
            metadata: String::new(),
            settings_drafts: Vec::new(),
            settings_drafts_unreadable: 0,
        }
    }

    /// Whether [`Self::settings_drafts`] is what
    /// [`Self::sanitize_settings_drafts`] leaves: carriable, one per key, within
    /// the cap, and only on a Settings leaf. Part of the layout's shape check,
    /// so a manifest built around the sanitize is refused rather than carried.
    fn settings_drafts_are_canonical(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        self.settings_drafts.is_empty()
            || (self.restore_tag == "settings"
                && self.settings_drafts.len() <= MAX_SETTINGS_DRAFTS_PER_VIEW
                && self.settings_drafts.iter().all(|draft| {
                    settings_draft_is_carriable(&draft.key, &draft.text)
                        && keys.insert(draft.key.as_str())
                }))
    }

    /// Keep exactly the carried drafts this build can reopen and count the
    /// rest into [`Self::settings_drafts_unreadable`]: carriable
    /// ([`settings_draft_is_carriable`]), the first for its key, at most
    /// [`MAX_SETTINGS_DRAFTS_PER_VIEW`], and only on a Settings leaf.
    /// Idempotent, and a no-op over a capture (whose drafts pass by
    /// construction), so the capture stays a fixed point of the parse.
    fn sanitize_settings_drafts(&mut self) {
        if self.settings_drafts.is_empty() {
            return;
        }
        let on_settings = self.restore_tag == "settings";
        let mut seen = std::collections::BTreeSet::new();
        let mut kept = Vec::with_capacity(self.settings_drafts.len());
        for draft in std::mem::take(&mut self.settings_drafts) {
            if on_settings
                && kept.len() < MAX_SETTINGS_DRAFTS_PER_VIEW
                && settings_draft_is_carriable(&draft.key, &draft.text)
                && seen.insert(draft.key.clone())
            {
                kept.push(draft);
            } else {
                self.settings_drafts_unreadable = self.settings_drafts_unreadable.saturating_add(1);
            }
        }
        self.settings_drafts = kept;
    }

    fn validation_error(&self) -> Option<&'static str> {
        if self.restore_tag.is_empty()
            || self.restore_tag.len() > MAX_RESTORE_TAG_BYTES
            || self.restore_tag.contains('\0')
        {
            return Some("invalid native restore tag");
        }
        if self.metadata.len() > MAX_VIEW_METADATA_BYTES || self.metadata.contains('\0') {
            return Some("invalid native restore metadata");
        }
        if self.editor_selections.len() > MAX_EDITOR_SELECTIONS
            || (!self.editor_selections.is_empty()
                && self.primary_selection >= self.editor_selections.len())
        {
            return Some("invalid editor selection state");
        }
        if self.config_editor && self.restore_tag != "editor" {
            return Some("config-editor identity requires an Editor restore");
        }
        match self.restore_tag.as_str() {
            "settings" => self
                .route
                .as_deref()
                .filter(|route| {
                    !route.is_empty()
                        && route.len() <= MAX_SETTINGS_ROUTE_BYTES
                        && route.starts_with('/')
                        && !route.contains('\0')
                })
                .is_none()
                .then_some("invalid Settings route"),
            "markdown" | "editor" => self
                .uri
                .as_deref()
                .filter(|uri| {
                    !uri.is_empty()
                        && uri.len() <= MAX_DOCUMENT_URI_BYTES
                        && uri.starts_with("file://")
                        && !uri.contains('\0')
                })
                .is_none()
                .then_some("invalid document URI"),
            _ => Some("native app is unavailable in this build"),
        }
    }

    fn copyable_metadata(&self) -> String {
        let mut metadata = format!(
            "restore_tag={:?}\nroute={:?}\nuri={:?}\nconfig_editor={}\nsource_anchor={}\nviewport_anchor={}\ndurable_seq={}",
            self.restore_tag,
            self.route,
            self.uri,
            self.config_editor,
            self.source_anchor,
            self.viewport_anchor,
            self.durable_seq
        );
        if !self.metadata.is_empty() {
            metadata.push_str("\nmetadata=");
            metadata.push_str(&self.metadata);
        }
        metadata.truncate(MAX_VIEW_METADATA_BYTES);
        metadata
    }
}

/// A recoverable leaf occupying the exact split position of an unavailable or malformed
/// app. It retains bounded metadata for diagnostics/copying and never falls back to a
/// shell command or silently deletes a healthy sibling.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct PlaceholderLeafRestore {
    pub restore_tag: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
    /// The terminal leaf this placeholder stands in for, when a seamless
    /// successor retired it because no session was handed for it
    /// ([`RestoreManifest::placed_for_handed`]): its outgoing pool id is what
    /// a held pane's carried screen is matched by (`App::restore_held_leaf`,
    /// round five, item 19), and its user identity — the `meta set` title,
    /// icon, role, description, the spawn identity — is what that pane is
    /// shown with (round six of the update audit, finding 35). NEVER ON THE
    /// WIRE: set only by the successor's own placement, so no layout a
    /// producer writes can name one.
    #[serde(skip)]
    pub unhanded: Option<Box<TerminalLeafRestore>>,
}

impl PlaceholderLeafRestore {
    /// The outgoing pool id of the retired terminal leaf this placeholder
    /// stands in for ([`Self::unhanded`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn unhanded_local_id(&self) -> Option<u64> {
        self.unhanded.as_ref().and_then(|leaf| leaf.local_id)
    }
}

/// Content descriptor at one recursive restore leaf.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RestoredView {
    Terminal(TerminalLeafRestore),
    Native(NativeLeafRestore),
    Placeholder(PlaceholderLeafRestore),
}

impl RestoredView {
    fn sanitize(&mut self) {
        match self {
            Self::Terminal(terminal) => terminal.sanitize(),
            Self::Native(native) => {
                // Before the validation: a draft this build cannot reopen costs
                // that draft (counted), never the leaf.
                native.sanitize_settings_drafts();
                let Some(reason) = native.validation_error() else {
                    return;
                };
                *self = Self::Placeholder(PlaceholderLeafRestore {
                    restore_tag: native.restore_tag.clone(),
                    reason: reason.to_string(),
                    metadata: native.copyable_metadata(),
                    unhanded: None,
                });
            }
            Self::Placeholder(_) => {}
        }
    }

    fn bounded(&self) -> bool {
        match self {
            Self::Terminal(terminal) => {
                terminal
                    .cwd
                    .as_ref()
                    .is_none_or(|cwd| cwd.len() <= MAX_DOCUMENT_URI_BYTES && !cwd.contains('\0'))
                    && terminal.title.len() <= MAX_DOCUMENT_URI_BYTES
                    && !terminal.title.contains('\0')
                    && terminal.profile.as_ref().is_none_or(|profile| {
                        profile.len() <= MAX_SETTINGS_ROUTE_BYTES && !profile.contains('\0')
                    })
                    && terminal.user_metadata_is_canonical()
            }
            Self::Native(native) => {
                native.validation_error().is_none() && native.settings_drafts_are_canonical()
            }
            Self::Placeholder(placeholder) => {
                !placeholder.restore_tag.is_empty()
                    && placeholder.restore_tag.len() <= MAX_RESTORE_TAG_BYTES
                    && !placeholder.restore_tag.contains('\0')
                    && !placeholder.reason.is_empty()
                    && placeholder.reason.len() <= MAX_SETTINGS_ROUTE_BYTES
                    && !placeholder.reason.contains('\0')
                    && placeholder.metadata.len() <= MAX_VIEW_METADATA_BYTES
                    && !placeholder.metadata.contains('\0')
            }
        }
    }
}

/// Recursive, content-agnostic persisted split tree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "node", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "the leaf IS the tree's payload (a terminal leaf grew past the lint's 200-byte \
              line with its `identity` field, 2026-09-17); every node below the root is \
              already boxed by `Split`, so the inline leaf costs one root-sized slot per \
              tab, and boxing it would put a deref on every match over a restored view"
)]
pub(crate) enum RestoredSplitTree {
    Leaf {
        view: RestoredView,
    },
    Split {
        axis: SplitKind,
        ratio: f32,
        first: Box<RestoredSplitTree>,
        second: Box<RestoredSplitTree>,
    },
}

impl RestoredSplitTree {
    pub(crate) fn leaf(view: RestoredView) -> Self {
        Self::Leaf { view }
    }

    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => 1 + first.depth().max(second.depth()),
        }
    }

    fn sanitize(&mut self) {
        match self {
            Self::Leaf { view } => view.sanitize(),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !ratio.is_finite() {
                    *ratio = 0.5;
                } else {
                    *ratio = ratio.clamp(0.05, 0.95);
                }
                first.sanitize();
                second.sanitize();
            }
        }
    }

    fn shape_is_bounded(&self) -> bool {
        self.leaf_count() <= MAX_LEAVES_PER_TAB
            && self.depth() <= MAX_SPLIT_DEPTH
            && match self {
                Self::Leaf { view } => view.bounded(),
                Self::Split {
                    ratio,
                    first,
                    second,
                    ..
                } => {
                    ratio.is_finite()
                        && (0.05..=0.95).contains(ratio)
                        && first.shape_is_bounded()
                        && second.shape_is_bounded()
                }
            }
    }

    /// Every terminal leaf of this tree whose `local_id` is in `unhanded`
    /// becomes a [`RestoredView::Placeholder`] saying no session was carried
    /// for it — the pane, its place in the split and the tab's focus path all
    /// kept. See [`RestoreManifest::placed_for_handed`].
    fn retire_unhanded_terminals(&mut self, unhanded: &[u64]) {
        match self {
            Self::Leaf { view } => {
                let RestoredView::Terminal(terminal) = &*view else {
                    return;
                };
                if !terminal.local_id.is_some_and(|id| unhanded.contains(&id)) {
                    return;
                }
                // The pane's directory and title, as the Recovery view's
                // copyable metadata — bounded in CHARACTERS so the bytes stay
                // under the leaf's own cap whatever the script.
                let metadata = format!(
                    "folder: {}\ntitle: {}",
                    terminal.cwd.as_deref().unwrap_or("none"),
                    terminal.title
                )
                .chars()
                .take(MAX_VIEW_METADATA_BYTES / 4)
                .collect();
                let unhanded = Some(Box::new(terminal.clone()));
                *view = RestoredView::Placeholder(PlaceholderLeafRestore {
                    restore_tag: "terminal".to_string(),
                    reason: crate::update_words::PANE_NOT_CARRIED.to_string(),
                    metadata,
                    unhanded,
                });
            }
            Self::Split { first, second, .. } => {
                first.retire_unhanded_terminals(unhanded);
                second.retire_unhanded_terminals(unhanded);
            }
        }
    }

    /// Every Settings leaf's carried drafts in this tree, and how many the
    /// parse had to drop — see [`RestoreManifest::carried_settings_drafts`].
    fn collect_settings_drafts(
        &self,
        drafts: &mut Vec<SettingsDraftRestore>,
        unreadable: &mut usize,
    ) {
        match self {
            Self::Leaf {
                view: RestoredView::Native(native),
            } => {
                drafts.extend(native.settings_drafts.iter().cloned());
                *unreadable = unreadable.saturating_add(native.settings_drafts_unreadable);
            }
            Self::Leaf { .. } => {}
            Self::Split { first, second, .. } => {
                first.collect_settings_drafts(drafts, unreadable);
                second.collect_settings_drafts(drafts, unreadable);
            }
        }
    }

    /// The first TERMINAL leaf in the order a rebuild walks this tree (first
    /// branch before second) — see [`WindowLayout::bootstrap_local_id`].
    pub(crate) fn first_terminal_leaf(&self) -> Option<&TerminalLeafRestore> {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => Some(terminal),
            Self::Leaf { .. } => None,
            Self::Split { first, second, .. } => first
                .first_terminal_leaf()
                .or_else(|| second.first_terminal_leaf()),
        }
    }

    /// The first TERMINAL leaf of this tree that recorded a directory, in the
    /// same walk as [`Self::first_terminal_leaf`]: that leaf when it has one,
    /// else the next terminal pane that does — see
    /// [`WindowLayout::bootstrap_cwd`].
    fn first_terminal_with_cwd(&self) -> Option<&TerminalLeafRestore> {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => terminal.cwd.is_some().then_some(terminal),
            Self::Leaf { .. } => None,
            Self::Split { first, second, .. } => first
                .first_terminal_with_cwd()
                .or_else(|| second.first_terminal_with_cwd()),
        }
    }
}

/// One complete tab recovery record. Focus and zoom are properties of the container,
/// never duplicated into each leaf.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoredTab {
    pub root: RestoredSplitTree,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focused_path: Vec<RestoreBranch>,
    #[serde(default)]
    pub zoomed: bool,
}

impl RestoredTab {
    fn sanitize(&mut self) {
        self.root.sanitize();
    }

    fn shape_is_valid(&self) -> bool {
        self.focused_path.len() <= MAX_SPLIT_DEPTH
            && self.root.shape_is_bounded()
            && self.path_names_a_leaf()
    }

    fn path_names_a_leaf(&self) -> bool {
        self.focused_leaf().is_some()
    }

    /// The leaf [`Self::focused_path`] names, or `None` when the path runs off
    /// the tree (a shape [`Self::shape_is_valid`] refuses).
    fn focused_leaf(&self) -> Option<&RestoredView> {
        let mut node = &self.root;
        for branch in &self.focused_path {
            let RestoredSplitTree::Split { first, second, .. } = node else {
                return None;
            };
            node = match branch {
                RestoreBranch::First => first,
                RestoreBranch::Second => second,
            };
        }
        match node {
            RestoredSplitTree::Leaf { view } => Some(view),
            RestoredSplitTree::Split { .. } => None,
        }
    }
}

/// One node of a persisted tab pane tree. A `Leaf` drops the process-local session id and
/// keeps the shell's `cwd` + `title` (so the session can be respawned there); a `Split`
/// preserves the divider direction + ratio.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PaneLayout {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        title: String,
        /// The one leaf that held keyboard focus in its tab.
        #[serde(default)]
        focused: bool,
        /// The outgoing session's pool id, captured ONLY so a SEAMLESS-update boot can
        /// re-adopt the live shell for this pane (matched to its handed-off fd by id)
        /// instead of forking a fresh one. A durable cold-quit restore ignores it (there
        /// is no live shell to adopt, so every leaf forks fresh) — it is purely the
        /// layout↔live-fd bridge for the self-update handoff. Additive/optional: absent
        /// in older manifests, harmless when unmatched.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_id: Option<u64>,
    },
    Split {
        dir: SplitKind,
        ratio: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl PaneLayout {
    /// A plain single-pane leaf — a test-fixture convenience (production capture goes
    /// through [`crate::pane::PaneTree::to_layout`], which builds variants directly).
    #[cfg(test)]
    pub(crate) fn leaf(cwd: Option<String>, title: String, focused: bool) -> Self {
        PaneLayout::Leaf {
            cwd,
            title,
            focused,
            local_id: None,
        }
    }

    /// The legacy mirror's leaves under the same session-metadata rule as
    /// `TerminalLeafRestore::sanitize`: an older consumer that still reads this
    /// projection must not be handed the NUL directory the canonical tree
    /// dropped (the 2026-09-22/23 update audit, plan P0-1f).
    fn sanitize_leaf_metadata(&mut self) {
        match self {
            PaneLayout::Leaf { cwd, title, .. } => {
                *cwd = storable_leaf_cwd(cwd.take());
                sanitize_leaf_title(title);
            }
            PaneLayout::Split { first, second, .. } => {
                first.sanitize_leaf_metadata();
                second.sanitize_leaf_metadata();
            }
        }
    }

    /// The legacy mirror's half of [`RestoreManifest::placed_for_handed`]: a
    /// leaf naming a session that was not handed names none, so no reader of
    /// this projection (`WindowLayout::bootstrap_local_id`'s fallback) can pick
    /// it as a shell to adopt.
    fn forget_local_ids(&mut self, unhanded: &[u64]) {
        match self {
            PaneLayout::Leaf { local_id, .. } => {
                if local_id.is_some_and(|id| unhanded.contains(&id)) {
                    *local_id = None;
                }
            }
            PaneLayout::Split { first, second, .. } => {
                first.forget_local_ids(unhanded);
                second.forget_local_ids(unhanded);
            }
        }
    }

    /// The number of Leaf panes in this subtree (== the sessions to respawn).
    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            PaneLayout::Leaf { .. } => 1,
            PaneLayout::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    /// Every `Leaf` in this subtree, in tree order (left-to-right / top-to-bottom) —
    /// the SAME order [`crate::pane::PaneTree::rebuild`] assigns fresh session ids, so
    /// `leaves()[i]`'s cwd is the spawn cwd for `fresh[i]`.
    pub(crate) fn leaves(&self) -> Vec<&PaneLayout> {
        let mut out = Vec::with_capacity(self.leaf_count());
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves<'a>(&'a self, out: &mut Vec<&'a PaneLayout>) {
        match self {
            PaneLayout::Leaf { .. } => out.push(self),
            PaneLayout::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    /// This node's persisted cwd — `None` for a `Split` (only leaves carry one).
    pub(crate) fn cwd(&self) -> Option<&str> {
        match self {
            PaneLayout::Leaf { cwd, .. } => cwd.as_deref(),
            PaneLayout::Split { .. } => None,
        }
    }

    /// This leaf's outgoing session pool id, when captured for a seamless-update handoff
    /// (`None` for a `Split`, and for any cold-quit leaf). The boot adoption matches this
    /// to a handed-off live fd to re-adopt the running shell into this exact pane.
    pub(crate) fn local_id(&self) -> Option<u64> {
        match self {
            PaneLayout::Leaf { local_id, .. } => *local_id,
            PaneLayout::Split { .. } => None,
        }
    }
}

/// Durable identity of one native tab. Titles and indicators are deliberately absent:
/// the live app derives them again from the Settings route or canonical document grant.
/// Likewise, no per-view selection, scroll position, editor draft bytes, or async target
/// identity crosses a process boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NativeTabRestore {
    Settings { route: String },
    Markdown { uri: String },
    Editor { uri: String },
}

impl NativeTabRestore {
    #[cfg(test)]
    pub(crate) fn document_uri(&self) -> Option<&str> {
        match self {
            Self::Settings { .. } => None,
            Self::Markdown { uri } | Self::Editor { uri } => Some(uri),
        }
    }

    /// Whether this descriptor may join a window's legacy mirror that already
    /// holds `mirror`: no second Settings descriptor and no second copy of an
    /// equal one. THE ONE RULE for the mirror, read and written: every build's
    /// parse refuses a window whose `native_tabs` breaks it
    /// ([`WindowLayout::shape_is_valid`], whatever `restored_tabs` says), and the
    /// capture leaves out of the mirror the descriptor that would break it, as it
    /// leaves out a heterogeneous split tab — the canonical `restored_tabs` keep
    /// that tab whole.
    ///
    /// Why a live window can hold such a pair (round eight of the update audit,
    /// D5 review): an update's fold ([`RestoreManifest::fold_into_caps`]) moves
    /// tabs whole, so a window past the cap that had Settings open, or the same
    /// document, joins the last window beside that window's own, and the
    /// successor reopens both. Its next capture wrote both into the mirror, which
    /// every build refuses: the next update refused its own layout, and the quit
    /// layout and every crash journal left that window out.
    pub(crate) fn joins_mirror(&self, mirror: &[Self]) -> bool {
        let is_settings = |descriptor: &Self| matches!(descriptor, Self::Settings { .. });
        !mirror.contains(self) && !(is_settings(self) && mirror.iter().any(is_settings))
    }

    fn shape_is_valid(&self) -> bool {
        match self {
            Self::Settings { route } => {
                !route.is_empty()
                    && route.len() <= MAX_SETTINGS_ROUTE_BYTES
                    && route.starts_with('/')
                    && !route.contains('\0')
            }
            Self::Markdown { uri } | Self::Editor { uri } => {
                !uri.is_empty()
                    && uri.len() <= MAX_DOCUMENT_URI_BYTES
                    && uri.starts_with("file://")
                    && !uri.contains('\0')
            }
        }
    }
}

/// Canonical chrome order without persisting any live [`crate::tab_model::TabId`]. Each
/// entry is an index into the matching compatibility list in [`WindowLayout`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TabOrderEntry {
    Terminal { index: usize },
    Native { index: usize },
}

/// One window's macOS show state at capture time (gap #29). Every field is
/// `Option`: `None` is "not captured", and restore treats it exactly like the
/// window's ordinary state — never a forced change off a manifest that did not
/// say. See `crate::window_show` for what the capture reads and what a restore
/// does with it.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct WindowShow {
    /// The window was in native full screen (its own Space).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// The window was miniaturized into the Dock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimized: Option<bool>,
    /// Its place in the app's window stack, FRONT TO BACK: 0 is the frontmost.
    /// A rank, not a count — only the order between windows means anything, and
    /// restore tolerates gaps and ties (a hand-edited manifest is data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_order: Option<u32>,
    /// It was the key window — the one typing went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<bool>,
}

impl WindowShow {
    /// Nothing captured: the value every non-macOS capture, headless capture and
    /// older manifest carries.
    pub(crate) const UNKNOWN: Self = Self {
        fullscreen: None,
        minimized: None,
        z_order: None,
        key: None,
    };

    /// Whether nothing was captured — the `skip_serializing_if` that keeps such
    /// a window's wire byte-identical to an older build's.
    pub(crate) fn is_unknown(&self) -> bool {
        *self == Self::UNKNOWN
    }
}

/// One window's persisted state: its size, position, the active tab index, and one pane
/// tree per tab.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct WindowLayout {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub active_tab: usize,
    /// Outer window position (physical px), when known — so a reopened window (cold
    /// restore or a seamless update) comes back exactly where it was instead of at the OS
    /// cascade default. Additive/optional: absent in older manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_y: Option<i32>,
    /// Whether the window was MAXIMIZED at capture time, so quitting maximized
    /// reopens maximized instead of as a floating default-size frame. When this is
    /// `Some(true)` the Windows capture stores `rcNormalPosition` — the OS's own
    /// restore-down rect — in `outer_x`/`outer_y` rather than the maximized frame's
    /// origin (see `capture_restore_manifest`), so un-maximizing after a restore
    /// lands where un-maximizing before the quit would have. Additive/optional
    /// (`Option`, not `bool`): older manifests parse as `None` = "unknown", which
    /// restore treats exactly like `Some(false)` — never force a state change off
    /// a manifest that predates the field. Currently written on Windows only; the
    /// macOS strip keeps its zoom semantics untouched and the Unix seamless
    /// commit's topology equality never sees a `Some` (belt-and-braces normalized
    /// there anyway, next to `outer_x`/`outer_y`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
    /// THE WINDOW'S RAW INNER SIZE, `[width, height]` in physical px
    /// (`WindowState::win_px`), never a maximized window's (its row holds the
    /// normal grid), and in the durable capture (quit, crash journal) never a
    /// full-screen one's. Additive and wire-neutral when absent, like `show`:
    /// an older reader ignores it, and a row without it (an older writer's)
    /// restores as it always has, inheriting the front window's grid.
    ///
    /// A row that carries it is recreated at ITS OWN grid (`rows`×`cols`), an
    /// update's successor's window and a cold restore's alike, and at this
    /// size when the size holds exactly that grid at the launch's metrics
    /// (`App::attach_frame_px`), else at the grid's exact frame. A cold launch
    /// starts at the configured font, so after a zoomed quit that is the
    /// frame; its window 0 is sized from its grid alone, as it always was.
    ///
    /// Why an update carries it (P6 finding 4, windows 1..N): a font zoom
    /// re-grids INSIDE the window it has, so a zoomed window is its grid's
    /// frame plus a remainder of under one cell per axis, and the zoom back out
    /// re-grids that same window to the grid it had. A successor that recreates
    /// the window as the exact frame of `rows`×`cols` drops the remainder, and
    /// the zoom back out fits a row and a column fewer. Window 0's size rides
    /// the window carry (`WindowCarry::inner_px`, which outranks this row for
    /// the bootstrap window, as its grid does); every further window's rides
    /// here, by the rule window 0 takes.
    ///
    /// On the wire, so the layout digest covers it: the child hashes the bytes
    /// it read, never a re-serialization (`seamless::layout_digest`), so parent
    /// and child agree whatever either build knows of the field, and the sizes
    /// the successor recreates its windows at are the ones the adoption proof
    /// commits to. Live, like the position: a sub-cell resize while the
    /// successor boots changes it, so `commit_layout_topology` normalizes it
    /// out of the Commit comparison, where the grid stays compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_px: Option<[u32; 2]>,
    /// Whether the window had its PRESENCE ROW committed under its chrome, in
    /// a seamless update's layout only, beside [`Self::inner_px`] and for the
    /// same reason: that size and `rows` hold the row, which the successor
    /// reserves before its first attach (`WindowState::take_carried_frame`;
    /// window 0's rides `WindowCarry::presence_row`). Written only while a
    /// row stands, so every other layout's wire is unchanged; a sizing fact
    /// like `inner_px`, so `commit_layout_topology` normalizes it out too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presence_row: bool,
    /// The window's macOS SHOW STATE — full screen, minimized, its place in the
    /// stack and whether it was the key window (gap #29: a seamless update pulled
    /// a full-screen window out of its Space, and with two or more windows the
    /// successor keyed whichever it revealed last, so typing landed in the wrong
    /// one). Captured on macOS only (`App::capture_restore_manifest`); every other
    /// platform, a headless capture and every older manifest leave it
    /// [`WindowShow::UNKNOWN`], which restore treats as "change nothing".
    /// Additive and wire-neutral when unknown: `skip_serializing_if` keeps such a
    /// window byte-identical to what an older build writes, an older reader
    /// ignores the `show` table (no `deny_unknown_fields` on this struct), and a
    /// child's layout digest hashes the WIRE it read, never a re-serialization of
    /// its own parse (`seamless::layout_digest`), so an older successor re-proves
    /// a layout that carries it. Live show state, like the position above, so
    /// `commit_layout_topology` normalizes it out of the Commit comparison.
    #[serde(default, skip_serializing_if = "WindowShow::is_unknown")]
    pub show: WindowShow,
    /// Terminal-only compatibility projection. Kept byte-compatible with RESTORE-1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PaneLayout>,
    /// Stable native app descriptors. No title or process-local identity is persisted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tabs: Vec<NativeTabRestore>,
    /// Canonical interleaving of `tabs` and `native_tabs`. An absent list is the legacy
    /// terminal-only order, allowing pre-native manifests to decode unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_order: Vec<TabOrderEntry>,
    /// Canonical active chrome index. `active_tab` remains the legacy terminal-projection
    /// active index; this additive field disambiguates mixed windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_item: Option<usize>,
    /// RESTORE-2 canonical tabs. The v1 compatibility projections above remain during
    /// the terminal-host migration and allow a rolling seamless update to decode the
    /// layout; new runtime code must prefer this recursive representation when present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored_tabs: Vec<RestoredTab>,
}

impl WindowLayout {
    /// The handed-off shell id of the pane this window's BOOTSTRAP session
    /// fills. The deferred restore grafts the session already running in the
    /// window onto the first terminal leaf of the canonical tree, walked the
    /// way it rebuilds it (tabs in order, each tree first branch before
    /// second), so the shell adopted as that session must be the one that
    /// leaf names — `main_entry` picks window 0's by this, and `apply_restore_manifest`
    /// every further window's. The legacy `tabs` mirror speaks only for a
    /// layout with no canonical terminal leaf at all: it lists all-terminal
    /// tabs only, so it names a different shell whenever a tab split between a
    /// native view and a terminal comes first.
    pub(crate) fn bootstrap_local_id(&self) -> Option<u64> {
        match self.bootstrap_terminal_leaf() {
            Some(leaf) => leaf.local_id,
            None => self.tabs.first()?.leaves().first()?.local_id(),
        }
    }

    /// The working directory of the pane this window's BOOTSTRAP session
    /// fills — the leaf [`Self::bootstrap_local_id`] names, by the same pick —
    /// so the session spawned for it starts where that pane's shell was.
    ///
    /// That leaf's own cwd when it recorded one. When it recorded none — its
    /// program sent no OSC 7, or the quit-time capture found its terminal
    /// locked and left the directory out (`App::restore_session_meta`) — the
    /// first directory another terminal pane of the SAME tab recorded, in
    /// rebuild order, since panes split off one another usually share a
    /// directory. Never another tab's: when no pane of this tab recorded a
    /// directory, the bootstrap starts in the default one. The leaf's
    /// [`TerminalLeafRestore::start_cwd`] decides it, so a folder a program's
    /// pane reported that is not here (ssh's remote one) is not asked for;
    /// the legacy mirror records no program and passes its folder as it is.
    pub(crate) fn bootstrap_cwd(&self) -> Option<String> {
        match self.bootstrap_tab() {
            Some(tab) => tab.root.first_terminal_with_cwd()?.start_cwd(),
            None => self
                .tabs
                .first()?
                .leaves()
                .first()?
                .cwd()
                .map(str::to_owned),
        }
    }

    /// The agent identity of the pane this window's BOOTSTRAP session fills —
    /// the leaf [`Self::bootstrap_local_id`] names, by the same pick as
    /// [`Self::bootstrap_cwd`] — so the session spawned for it runs under that
    /// identity from its first fork: a shell's env cannot be re-injected
    /// afterwards, and the graft (`App::restore_terminal_leaf`) refuses a
    /// bootstrap that does not wear what the pane names. Measured 2026-09-17
    /// (review): before this pick, window 0's first leaf came back under the
    /// human's login, labeled `identity=-`. Only the canonical tree carries
    /// the field; the legacy `tabs` mirror names none.
    pub(crate) fn bootstrap_identity(&self) -> Option<&str> {
        self.bootstrap_terminal_leaf()?.identity.as_deref()
    }

    /// The canonical tree's first terminal leaf, in rebuild order: the leaf
    /// the deferred restore grafts this window's bootstrap session onto.
    fn bootstrap_terminal_leaf(&self) -> Option<&TerminalLeafRestore> {
        self.bootstrap_tab()?.root.first_terminal_leaf()
    }

    /// The first canonical tab with a terminal leaf: the tab holding
    /// [`Self::bootstrap_terminal_leaf`].
    fn bootstrap_tab(&self) -> Option<&RestoredTab> {
        self.restored_tabs
            .iter()
            .find(|tab| tab.root.first_terminal_leaf().is_some())
    }

    /// Validated canonical order. Legacy terminal-only manifests synthesize the identity
    /// order. Duplicate/out-of-range indices fail closed rather than aliasing descriptors.
    pub(crate) fn canonical_order(&self) -> Option<Vec<TabOrderEntry>> {
        if self.tab_order.is_empty() {
            return self.native_tabs.is_empty().then(|| {
                (0..self.tabs.len())
                    .map(|index| TabOrderEntry::Terminal { index })
                    .collect()
            });
        }
        if self.tab_order.len() != self.tabs.len().saturating_add(self.native_tabs.len()) {
            return None;
        }
        let mut terminals = vec![false; self.tabs.len()];
        let mut natives = vec![false; self.native_tabs.len()];
        let mut next_terminal = 0usize;
        let mut next_native = 0usize;
        for entry in &self.tab_order {
            let (seen, index) = match *entry {
                TabOrderEntry::Terminal { index } => {
                    if index != next_terminal {
                        return None;
                    }
                    next_terminal = next_terminal.saturating_add(1);
                    (&mut terminals, index)
                }
                TabOrderEntry::Native { index } => {
                    if index != next_native {
                        return None;
                    }
                    next_native = next_native.saturating_add(1);
                    (&mut natives, index)
                }
            };
            let slot = seen.get_mut(index)?;
            if std::mem::replace(slot, true) {
                return None;
            }
        }
        (terminals.into_iter().all(|seen| seen) && natives.into_iter().all(|seen| seen))
            .then(|| self.tab_order.clone())
    }

    /// Active canonical index, migrating the legacy terminal projection when needed.
    pub(crate) fn canonical_active(&self, order: &[TabOrderEntry]) -> usize {
        self.active_item
            .filter(|index| *index < order.len())
            .or_else(|| {
                order.iter().position(|entry| {
                    matches!(entry, TabOrderEntry::Terminal { index } if *index == self.active_tab)
                })
            })
            .unwrap_or(0)
    }

    fn shape_is_valid(&self) -> bool {
        // Unique descriptors, at most one of them Settings: each joins the
        // mirror ahead of it (`NativeTabRestore::joins_mirror`, the rule the
        // capture writes by).
        let native_mirror_is_valid = self
            .native_tabs
            .iter()
            .enumerate()
            .all(|(index, descriptor)| descriptor.joins_mirror(&self.native_tabs[..index]));
        let canonical_count = if self.restored_tabs.is_empty() {
            self.tabs.len().saturating_add(self.native_tabs.len())
        } else {
            self.restored_tabs.len()
        };
        canonical_count <= MAX_TABS_PER_WINDOW
            && self
                .native_tabs
                .iter()
                .all(NativeTabRestore::shape_is_valid)
            && native_mirror_is_valid
            && self.canonical_order().is_some()
            && self.active_item.is_none_or(|index| index < canonical_count)
            && self.restored_tabs.iter().all(RestoredTab::shape_is_valid)
    }

    /// Every tab of this window, both projections, made one `from_toml` admits:
    /// the ONE manifest-wide sanitize, run by `RestoreManifest::new` (so every
    /// capture — the quit's and the update's, before its `layout_digest` — is
    /// already a fixed point) and by `from_toml` (so an older writer's layout
    /// is recovered rather than refused).
    fn sanitize(&mut self) {
        for tab in &mut self.tabs {
            tab.sanitize_leaf_metadata();
        }
        for tab in &mut self.restored_tabs {
            tab.sanitize();
        }
    }

    /// A window [`RestoreManifest::fold_into_caps`] made for a spill from
    /// `donor`: the donor's grid and the size that holds it (with the
    /// presence row that size reserves), so the shells moved into it keep
    /// their grid; no position, show state or maximize, so the window system
    /// places it; and its first tab in front. No tabs yet: the fold moves
    /// them in.
    fn spill_of(donor: &Self) -> Self {
        Self {
            rows: donor.rows,
            cols: donor.cols,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: donor.inner_px,
            presence_row: donor.presence_row,
            show: WindowShow::UNKNOWN,
            tabs: Vec::new(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: Vec::new(),
        }
    }

    /// Drop the legacy mirror (`tabs`, `native_tabs`, `tab_order` and the
    /// mirror's `active_tab`) of a window [`RestoreManifest::fold_into_caps`]
    /// moved tabs out of. The canonical `restored_tabs` stay, and the empty
    /// mirror is one every reader admits (`canonical_order` of an empty
    /// mirror is the empty order); see the fold for why the mirror goes.
    fn forget_legacy_mirror(&mut self) {
        self.tabs.clear();
        self.native_tabs.clear();
        self.tab_order.clear();
        self.active_tab = 0;
    }

    fn migrate_legacy_tabs(&mut self) -> Option<()> {
        if !self.restored_tabs.is_empty() {
            return Some(());
        }
        let order = self.canonical_order()?;
        self.restored_tabs = order
            .iter()
            .map(|entry| match *entry {
                TabOrderEntry::Terminal { index } => {
                    RestoredTab::from_legacy_terminal(self.tabs.get(index)?)
                }
                TabOrderEntry::Native { index } => {
                    RestoredTab::from_legacy_native(self.native_tabs.get(index)?)
                }
            })
            .collect::<Option<Vec<_>>>()?;
        Some(())
    }
}

impl RestoredTab {
    fn from_legacy_terminal(layout: &PaneLayout) -> Option<Self> {
        fn convert(
            layout: &PaneLayout,
            path: &mut Vec<RestoreBranch>,
            focused: &mut Option<Vec<RestoreBranch>>,
        ) -> RestoredSplitTree {
            match layout {
                PaneLayout::Leaf {
                    cwd,
                    title,
                    focused: is_focused,
                    local_id,
                } => {
                    if *is_focused && focused.is_none() {
                        *focused = Some(path.clone());
                    }
                    RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                        cwd: cwd.clone(),
                        title: title.clone(),
                        profile: None,
                        local_id: *local_id,
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                        questions: None,
                        identity: None,
                        agent: None,
                        held: false,
                    }))
                }
                PaneLayout::Split {
                    dir,
                    ratio,
                    first,
                    second,
                } => {
                    path.push(RestoreBranch::First);
                    let first = convert(first, path, focused);
                    path.pop();
                    path.push(RestoreBranch::Second);
                    let second = convert(second, path, focused);
                    path.pop();
                    RestoredSplitTree::Split {
                        // Legacy PaneLayout names the divider orientation, while the
                        // recursive model names the child-placement axis.
                        axis: match dir {
                            SplitKind::Vertical => SplitKind::Horizontal,
                            SplitKind::Horizontal => SplitKind::Vertical,
                        },
                        ratio: *ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }
                }
            }
        }

        let mut focused = None;
        let root = convert(layout, &mut Vec::new(), &mut focused);
        Some(Self {
            root,
            focused_path: focused.unwrap_or_default(),
            zoomed: false,
        })
    }

    pub(crate) fn from_legacy_native(descriptor: &NativeTabRestore) -> Option<Self> {
        let native = match descriptor {
            NativeTabRestore::Settings { route } => NativeLeafRestore::settings(route.clone()),
            NativeTabRestore::Markdown { uri } => {
                NativeLeafRestore::document("markdown", uri.clone())
            }
            NativeTabRestore::Editor { uri } => NativeLeafRestore::document("editor", uri.clone()),
        };
        Some(Self {
            root: RestoredSplitTree::leaf(RestoredView::Native(native)),
            focused_path: Vec::new(),
            zoomed: false,
        })
    }
}

/// What a seamless update's producer FOLDED to bring its desk within the
/// layout's caps ([`RestoreManifest::fold_into_caps`]): how many windows past
/// [`MAX_WINDOWS`] became tabs of the windows that fit, how many tabs past a
/// window's [`MAX_TABS_PER_WINDOW`] moved to another window, and which panes
/// were in front before anything moved. The successor says it once on glass
/// (`update_words::layout_folded`). Additive and wire-neutral when nothing was
/// folded: `skip_serializing_if` keeps every layout within the caps
/// byte-identical to what an older build writes, and an older reader ignores
/// the table (no `deny_unknown_fields` on [`RestoreManifest`]) and places the
/// folded layout as it is.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct LayoutFold {
    /// Windows past [`MAX_WINDOWS`] whose tabs went into the windows that fit.
    #[serde(default)]
    pub windows: u32,
    /// Tabs past a window's [`MAX_TABS_PER_WINDOW`] moved into another window.
    #[serde(default)]
    pub tabs: u32,
    /// THE PANES IN FRONT BEFORE THE FOLD (round eight of the update audit, D5
    /// review): each captured window's active tab's focused terminal, as
    /// [`RestoreManifest::front_terminal_ids`] read the desk before a tab moved.
    /// Those are the panes whose titles the outgoing build kept current in their
    /// session records (`publish_active_terminal_title`), which is what the
    /// seamless adoption asks of that set (`seamless::take_incoming`, U47). The
    /// fold changes which tab each window shows — a window past the cap is tabs
    /// of another, a donor whose front tab moved shows another — so the folded
    /// windows' `active_item` no longer say it; a folded layout answers by this
    /// list instead. Written only by a fold that moved something; at most one
    /// entry per captured window, each the focus of a distinct tab, so a layout
    /// within the caps holds at most [`MAX_FOLD_FRONT`] of them and the parse
    /// keeps no more.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub front: Vec<u64>,
}

/// The most [`LayoutFold::front`] entries a parse keeps: one per tab of a
/// layout at both caps, since each entry is the focus of a distinct tab and
/// tabs move whole. Every fold the park admits is within it, so the parse is a
/// fixed point of the producer's layout.
const MAX_FOLD_FRONT: usize = MAX_WINDOWS * MAX_TABS_PER_WINDOW;

impl LayoutFold {
    /// The default value: what every layout within the caps carries, and the
    /// one the wire leaves out.
    pub(crate) fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the fold moved anything: a window merged into tabs, or a tab
    /// moved to another window. What the successor says, and when
    /// [`Self::front`] (not the windows' `active_item`) names the panes in
    /// front.
    pub(crate) fn folded(&self) -> bool {
        self.windows > 0 || self.tabs > 0
    }
}

/// The persisted session-restore manifest.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoreManifest {
    pub schema: u32,
    /// What the producer folded to fit the caps; only a seamless update's
    /// layout ever carries one ([`Self::fold_into_caps`]).
    #[serde(default, skip_serializing_if = "LayoutFold::is_none")]
    pub fold: LayoutFold,
    pub windows: Vec<WindowLayout>,
}

impl RestoreManifest {
    pub(crate) fn new(mut windows: Vec<WindowLayout>) -> Self {
        for window in &mut windows {
            let _ = window.migrate_legacy_tabs();
            window.sanitize();
        }
        Self {
            schema: SCHEMA,
            fold: LayoutFold::default(),
            windows,
        }
    }

    /// THE CAPS, APPLIED BY THE WRITER (round eight of the update audit, D5:
    /// #505 and the lead U33). Nothing at runtime limits how many windows a
    /// desk has or how many tabs a window holds, but a layout holds at most
    /// [`MAX_WINDOWS`] windows of [`MAX_TABS_PER_WINDOW`] tabs, and every
    /// build's parse leaves out what is past them ([`Self::from_toml`]). A
    /// seamless update's layout must name every shell it hands
    /// ([`Self::placed_for_handed`]), so its producer cannot leave windows out:
    /// the 65-window desk's capture failed its own round trip, and the worker
    /// filed each attempt as a transient failure that froze every tab again.
    /// So the handoff capture FOLDS what is past the caps into what fits
    /// (decided 2026-10-02 for the owner):
    ///
    /// * the windows past the [`MAX_WINDOWS`]th become tabs of the LAST window
    ///   that fits, in order, after its own;
    /// * a window's tabs past [`MAX_TABS_PER_WINDOW`] move, in order, to the
    ///   NEXT window with room (after it, then from the first), or to a window
    ///   made for them — at the donor's grid and size, placed by the window
    ///   system — when no window has room and the desk is under the window
    ///   cap. When every window is full, what is left stays where it is: such
    ///   a desk is past what one layout can hold, and the park refuses it typed
    ///   ([`Self::why_not_whole`]);
    /// * a window that LOST tabs keeps its canonical tabs only: its legacy
    ///   mirror (`tabs`, `native_tabs`, `tab_order`) would name a moved tab's
    ///   shell in two windows, and no build since v0.21.0 reads it while
    ///   `restored_tabs` is present but to pick a bootstrap shell for a window
    ///   whose tree has no terminal pane. A window that only RECEIVED tabs
    ///   keeps its mirror as captured: it names its own tabs, all still there;
    /// * a donor whose front tab moved shows its last remaining tab, and a
    ///   window made for a spill shows the moved front tab when it holds it.
    ///
    /// Every tab moves whole, so every handed shell is still named exactly as
    /// often as before. A layout within the caps is left exactly as it was —
    /// byte-identical on the wire — and [`Self::fold`] records what was folded
    /// for the successor to say once.
    pub(crate) fn fold_into_caps(&mut self) {
        let over_tab_cap = |window: &WindowLayout| window.restored_tabs.len() > MAX_TABS_PER_WINDOW;
        if self.windows.len() <= MAX_WINDOWS && !self.windows.iter().any(over_tab_cap) {
            return;
        }
        // The panes in front as the outgoing build shows them, before a tab
        // moves: `LayoutFold::front`.
        let front = self.windows_front_terminal_ids();
        let mut fold = LayoutFold::default();
        if self.windows.len() > MAX_WINDOWS {
            let past = self.windows.split_off(MAX_WINDOWS);
            fold.windows = u32::try_from(past.len()).unwrap_or(u32::MAX);
            let last = self
                .windows
                .last_mut()
                .expect("the windows that fit are MAX_WINDOWS, never none");
            for window in past {
                last.restored_tabs.extend(window.restored_tabs);
            }
        }
        let mut lost_tabs = vec![false; self.windows.len()];
        let mut moved = 0usize;
        let mut donor = 0;
        while donor < self.windows.len() {
            while self.windows[donor].restored_tabs.len() > MAX_TABS_PER_WINDOW {
                let Some(host) = self.spill_host_for(donor) else {
                    break;
                };
                let made = host == self.windows.len();
                if made {
                    let spill = WindowLayout::spill_of(&self.windows[donor]);
                    self.windows.push(spill);
                    lost_tabs.push(false);
                }
                let room = MAX_TABS_PER_WINDOW - self.windows[host].restored_tabs.len();
                let over = self.windows[donor].restored_tabs.len() - MAX_TABS_PER_WINDOW;
                let count = room.min(over);
                let spilled = MAX_TABS_PER_WINDOW..MAX_TABS_PER_WINDOW + count;
                let at = self.windows[host].restored_tabs.len();
                let front = self.windows[donor].active_item;
                let tabs = self.windows[donor]
                    .restored_tabs
                    .drain(spilled.clone())
                    .collect::<Vec<_>>();
                self.windows[host].restored_tabs.extend(tabs);
                match front {
                    Some(active) if spilled.contains(&active) => {
                        self.windows[donor].active_item = Some(MAX_TABS_PER_WINDOW - 1);
                        if made {
                            self.windows[host].active_item = Some(at + active - spilled.start);
                        }
                    }
                    Some(active) if active >= spilled.end => {
                        self.windows[donor].active_item = Some(active - count);
                    }
                    _ => {}
                }
                moved = moved.saturating_add(count);
                lost_tabs[donor] = true;
            }
            donor += 1;
        }
        fold.tabs = u32::try_from(moved).unwrap_or(u32::MAX);
        for (window, lost_tabs) in self.windows.iter_mut().zip(lost_tabs) {
            if lost_tabs {
                window.forget_legacy_mirror();
            }
        }
        if fold.folded() {
            fold.front = front;
        }
        self.fold = fold;
    }

    /// Where [`Self::fold_into_caps`] spills `donor`'s tabs past the cap: the
    /// next window with room after it, then from the first; else a window
    /// made for them (`Some(self.windows.len())`) while the desk is under
    /// the window cap; else nowhere.
    fn spill_host_for(&self, donor: usize) -> Option<usize> {
        let count = self.windows.len();
        (donor + 1..count)
            .chain(0..donor)
            .find(|&index| self.windows[index].restored_tabs.len() < MAX_TABS_PER_WINDOW)
            .or_else(|| (count < MAX_WINDOWS).then_some(count))
    }

    /// Whether [`Self::from_toml`] reads every window of this layout back as
    /// it is ([`Self::why_not_whole`] says nothing): the tests' shorthand.
    #[cfg(test)]
    pub(crate) fn admits_whole(&self) -> bool {
        self.why_not_whole().is_none()
    }

    /// Why [`Self::from_toml`] would NOT read every window of this layout back
    /// as it is — past the window cap, or a window in a shape this build does
    /// not rebuild — or `None` when it would. What a seamless update's park
    /// asks of its layout before it commits to it
    /// (`app_update_handoff::commit_handoff_layout`): a layout that fails it
    /// fails the worker's round trip on every attempt, so it is refused at the
    /// park as a fact about the desk, retried when the desk changes, never as
    /// the worker's own transient trouble. The fold ([`Self::fold_into_caps`])
    /// leaves only a desk past what any layout holds failing it: more than 64
    /// windows of 256 tabs, or a tab past 256 panes or 31 nested splits.
    ///
    /// In the words the park's refusal carries, which the update's record
    /// shows as they are ([`UPDATE_CAP_KEY`] in each): what is past a cap, by
    /// how much, and where — or, for a window in another shape this build does
    /// not read back, that window, never a cap it is within (the D5 review: a
    /// window holding two Settings descriptors in its legacy mirror was refused
    /// as "past what one handoff layout holds").
    pub(crate) fn why_not_whole(&self) -> Option<String> {
        if self.windows.len() > MAX_WINDOWS {
            return Some(format!(
                "{} windows are open, more than the {MAX_WINDOWS} {UPDATE_CAP_KEY}",
                self.windows.len()
            ));
        }
        let (index, window) = self
            .windows
            .iter()
            .enumerate()
            .find(|(_, window)| !window.shape_is_valid())?;
        let number = index + 1;
        let past_split_caps = |tab: &RestoredTab| {
            tab.root.leaf_count() > MAX_LEAVES_PER_TAB || tab.root.depth() > MAX_SPLIT_DEPTH
        };
        Some(if window.restored_tabs.len() > MAX_TABS_PER_WINDOW {
            // The park sees the layout after the fold, which moves whole tabs
            // and drops none: a window still past the tab cap means the total
            // is past every window's, and the total is what the person has.
            let total: usize = self.windows.iter().map(|w| w.restored_tabs.len()).sum();
            format!(
                "{total} tabs are open, more than the {} ({MAX_WINDOWS} windows of \
                 {MAX_TABS_PER_WINDOW}) {UPDATE_CAP_KEY}",
                MAX_WINDOWS * MAX_TABS_PER_WINDOW
            )
        } else if let Some(index) = window.restored_tabs.iter().position(past_split_caps) {
            let root = &window.restored_tabs[index].root;
            let tab = index + 1;
            if root.leaf_count() > MAX_LEAVES_PER_TAB {
                format!(
                    "tab {tab} of window {number} has {} panes, more than the \
                     {MAX_LEAVES_PER_TAB} in one tab {UPDATE_CAP_KEY}",
                    root.leaf_count()
                )
            } else {
                // `depth` counts the panes' own level: the splits are one fewer.
                format!(
                    "tab {tab} of window {number} is split {} levels deep, more than the {} \
                     {UPDATE_CAP_KEY}",
                    root.depth() - 1,
                    MAX_SPLIT_DEPTH - 1
                )
            }
        } else {
            format!(
                "window {number} of the handoff layout is in a shape this build does not read back"
            )
        })
    }

    /// Whether there is anything worth restoring (at least one window with one tab).
    pub(crate) fn is_empty(&self) -> bool {
        self.windows
            .iter()
            .all(|w| w.tabs.is_empty() && w.native_tabs.is_empty() && w.restored_tabs.is_empty())
    }

    /// Return the exact set of live terminal ids carried by a seamless-update
    /// layout, sorted and distinct. Durable cold-restore manifests
    /// intentionally omit these ids, so they are not valid handoff authority.
    /// A terminal leaf with NO id is rejected: it would be backed by a newly
    /// spawned shell during the overlap.
    ///
    /// AN ID NAMED TWICE IS ONE SESSION SHOWN TWICE (round six, finding 10).
    /// `Open Session in New Window` (Cmd-Shift-O) puts one pooled session in
    /// two views, and every build's capture stamps that session's id on both
    /// leaves. Rejecting the repeat dropped the WHOLE layout on every update
    /// for as long as the co-view stayed open: every window folded into tabs
    /// of the first. The successor adopts the shell at the first leaf it
    /// rebuilds that names it and shows it again at every later one
    /// (`App::restore_terminal_leaf`), so a repeat is not ambiguous.
    pub(crate) fn seamless_terminal_ids(&self) -> Option<Vec<u64>> {
        fn collect(node: &RestoredSplitTree, ids: &mut Vec<u64>) -> Option<()> {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => ids.push(terminal.local_id?),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    collect(first, ids)?;
                    collect(second, ids)?;
                }
            }
            Some(())
        }

        let mut ids = Vec::new();
        for window in &self.windows {
            for tab in &window.restored_tabs {
                collect(&tab.root, &mut ids)?;
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// Every canonical terminal leaf that names a handed-off shell (`local_id`),
    /// cloned, in rebuild order. The orphan net (`App::adopt_orphan_shells_as_tabs`)
    /// runs after the rebuild has consumed the manifest, and puts on each shell
    /// the rebuild left over the USER identity of the leaf that names it.
    pub(crate) fn handed_off_leaves(&self) -> Vec<TerminalLeafRestore> {
        fn collect(node: &RestoredSplitTree, leaves: &mut Vec<TerminalLeafRestore>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } if terminal.local_id.is_some() => leaves.push(terminal.clone()),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    collect(first, leaves);
                    collect(second, leaves);
                }
            }
        }

        let mut leaves = Vec::new();
        for window in &self.windows {
            for tab in &window.restored_tabs {
                collect(&tab.root, &mut leaves);
            }
        }
        leaves
    }

    /// The handed-off shells (`local_id`) of the panes IN FRONT: in each
    /// window, the terminal leaf that held focus in its active tab, as the
    /// capture recorded them (`active_item` into `restored_tabs`, then that
    /// tab's `focused_path`). Exactly the pane whose title an outgoing build
    /// kept current in its session record (`publish_active_terminal_title`
    /// publishes the active tab's FOCUSED terminal, and nothing for a native
    /// one), which is what the seamless adoption needs it for
    /// (`seamless::take_incoming`, round eight of the update audit, U47).
    ///
    /// A window that recorded no active item (its active tab had no descriptor,
    /// or the layout was migrated from the legacy projection, whose migration
    /// sets none) names no pane, and neither does one whose focused leaf is
    /// native or names no handed-off shell.
    ///
    /// A FOLDED layout ([`Self::fold_into_caps`]) answers by what its producer
    /// recorded before a tab moved ([`LayoutFold::front`]): its windows show
    /// other tabs than the outgoing build's did, so their `active_item` would
    /// leave out a merged window's front pane and name a donor's new one, whose
    /// record copy is the stale one.
    pub(crate) fn front_terminal_ids(&self) -> Vec<u64> {
        if self.fold.folded() {
            return self.fold.front.clone();
        }
        self.windows_front_terminal_ids()
    }

    /// [`Self::front_terminal_ids`] read off the windows as they are.
    fn windows_front_terminal_ids(&self) -> Vec<u64> {
        self.windows
            .iter()
            .filter_map(|window| {
                match window
                    .restored_tabs
                    .get(window.active_item?)?
                    .focused_leaf()?
                {
                    RestoredView::Terminal(terminal) => terminal.local_id,
                    RestoredView::Native(_) | RestoredView::Placeholder(_) => None,
                }
            })
            .collect()
    }

    /// Put on every terminal leaf the agent `hosted` says its session
    /// (`local_id`) runs — the crash journal's capture ([`crate::crash_journal`]);
    /// a leaf whose session hosts none carries none.
    pub(crate) fn fill_agents(&mut self, hosted: &dyn Fn(u64) -> Option<AgentRestore>) {
        fn fill(node: &mut RestoredSplitTree, hosted: &dyn Fn(u64) -> Option<AgentRestore>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => terminal.agent = terminal.local_id.and_then(hosted).map(Box::new),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    fill(first, hosted);
                    fill(second, hosted);
                }
            }
        }
        for window in &mut self.windows {
            for tab in &mut window.restored_tabs {
                fill(&mut tab.root, hosted);
            }
        }
    }

    /// The layout a seamless successor PLACES, given the sessions the outgoing
    /// process actually handed (`expected`) — or the layout back, unplaced,
    /// when it cannot be made to name exactly them.
    ///
    /// WHY A LAYOUT MAY NAME MORE THAN WAS HANDED (round three of the 2026-09
    /// update robustness work). The outgoing process hands only its LIVE
    /// sessions (`App::handoff_live_sessions`): an exited pane kept open under
    /// `--hold` has no process to carry. But its layout capture stamps a
    /// `local_id` on every terminal view, exited ones included, so the layout
    /// names one id more than the handoff — and a successor that demanded an
    /// exact match dropped the WHOLE layout for it: every window collapsed
    /// into tabs of the first, and every unsaved Settings draft (which rides
    /// only in this layout, plan P2-2) was gone with a log line. Every build
    /// that shipped the capture does this, so it is answered here, where the
    /// layout is placed, rather than only in a capture a newer build writes.
    ///
    /// A leaf naming a session that was NOT handed becomes a placeholder
    /// saying no session was carried for it; its split, its tab and every
    /// other leaf are placed as they were. That keeps both guarantees the
    /// exact check exists for: each handed PTY is still named exactly once
    /// (nothing ambiguous), and no terminal leaf is left to be backed by a
    /// shell spawned during the overlap (the unhanded one is no terminal any
    /// more). A layout that MISSES a handed session or has a terminal leaf
    /// with no id at all is still unplaced: those are not a pane the outgoing
    /// process chose not to hand, and the orphan net answers them. A session
    /// NAMED twice is a co-view, and is placed (see
    /// [`Self::seamless_terminal_ids`]); one HANDED twice is not placed.
    pub(crate) fn placed_for_handed(mut self, expected: &[u64]) -> Result<Self, Self> {
        let Some(named) = self.seamless_terminal_ids() else {
            return Err(self);
        };
        let mut handed = expected.to_vec();
        handed.sort_unstable();
        if handed.windows(2).any(|pair| pair[0] == pair[1])
            || !handed.iter().all(|id| named.binary_search(id).is_ok())
        {
            return Err(self);
        }
        let unhanded = named
            .into_iter()
            .filter(|id| handed.binary_search(id).is_err())
            .collect::<Vec<_>>();
        if !unhanded.is_empty() {
            for window in &mut self.windows {
                for tab in &mut window.restored_tabs {
                    tab.root.retire_unhanded_terminals(&unhanded);
                }
                for tab in &mut window.tabs {
                    tab.forget_local_ids(&unhanded);
                }
            }
        }
        if self.covers_exact_seamless_ids(expected) {
            Ok(self)
        } else {
            Err(self)
        }
    }

    /// Every unsaved Settings draft this layout carries, in window, tab and
    /// tree order, with how many the parse had to drop — what a successor that
    /// cannot PLACE the layout says it lost, with the text, rather than
    /// dropping it with the layout (round three).
    pub(crate) fn carried_settings_drafts(&self) -> (Vec<SettingsDraftRestore>, usize) {
        let mut drafts = Vec::new();
        let mut unreadable = 0usize;
        for window in &self.windows {
            for tab in &window.restored_tabs {
                tab.root
                    .collect_settings_drafts(&mut drafts, &mut unreadable);
            }
        }
        (drafts, unreadable)
    }

    /// True only when this layout names every authenticated inherited terminal
    /// exactly once and names no additional terminal. Native leaves are allowed:
    /// they have no PTY and are independently restored from bounded descriptors.
    pub(crate) fn covers_exact_seamless_ids(&self, expected: &[u64]) -> bool {
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        !expected.windows(2).any(|pair| pair[0] == pair[1])
            && self.seamless_terminal_ids().as_deref() == Some(expected.as_slice())
    }

    /// The cwd of the pane the bootstrap session 0 becomes — the FIRST window's
    /// [`WindowLayout::bootstrap_cwd`] — so its spawn can start in the right
    /// directory (every other leaf is spawned later by `apply_pending_restore`
    /// with its own cwd).
    pub(crate) fn first_leaf_cwd(&self) -> Option<String> {
        self.windows.first()?.bootstrap_cwd()
    }

    /// The agent identity of the pane the bootstrap session 0 becomes — the
    /// FIRST window's [`WindowLayout::bootstrap_identity`] — so its spawn can
    /// run under it (`main_entry`, beside [`Self::first_leaf_cwd`]).
    pub(crate) fn first_leaf_identity(&self) -> Option<&str> {
        self.windows.first()?.bootstrap_identity()
    }

    pub(crate) fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize restore manifest: {e}"))
    }

    /// FAIL-SAFE parse: any TOML error or schema mismatch yields `None`, so a corrupt or
    /// old-version manifest never blocks launch — the app just starts fresh.
    ///
    /// THE CAPS DEGRADE PER WINDOW (round seven of the update audit, finding
    /// 12). Nothing at runtime limits how many windows a desk has or how many
    /// tabs a window holds, and no writer applies these caps, so a desk of 65
    /// windows (or one window of 257 tabs) wrote a layout this parse refused
    /// WHOLE: the quit's `session.toml` and every crash-journal image came
    /// back as one fresh window, with nothing said. Now a window this build
    /// cannot rebuild — a legacy projection that does not migrate, a shape
    /// past a cap — is left out on its own, the windows after the
    /// [`MAX_WINDOWS`]th are not read, and the rest come back, with a log
    /// line saying how many did not. Only a layout none of whose windows
    /// reads is refused. A layout within the caps parses exactly as before,
    /// so every writer's round trip is unchanged.
    pub(crate) fn from_toml(s: &str) -> Option<Self> {
        let mut manifest: Self = aterm_toml::from_str(s).ok()?;
        if manifest.schema != LEGACY_SCHEMA && manifest.schema != SCHEMA {
            return None;
        }
        let offered = manifest.windows.len();
        let admitted = AdmittedWindows::of(std::mem::take(&mut manifest.windows));
        if admitted.kept.is_empty() && offered > 0 {
            return None;
        }
        if let Some(line) = admitted.left_out_line(offered) {
            aterm_log::warn!("restore: {line}");
        }
        manifest.windows = admitted.kept;
        manifest.fold.front.truncate(MAX_FOLD_FRONT);
        manifest.schema = SCHEMA;
        Some(manifest)
    }
}

/// The saved windows [`RestoreManifest::from_toml`] reopens, and how many it
/// leaves out BY CAUSE — counted apart, so the log names each, never one
/// hedged over the other.
struct AdmittedWindows {
    kept: Vec<WindowLayout>,
    /// Left out after the [`MAX_WINDOWS`]th kept window.
    past_cap: usize,
    /// Left out in a shape this build cannot rebuild: more than
    /// [`MAX_TABS_PER_WINDOW`] tabs, a legacy layout that does not migrate, a
    /// split past its bounds.
    unbuildable: usize,
}

impl AdmittedWindows {
    fn of(windows: Vec<WindowLayout>) -> Self {
        let mut admitted = Self {
            kept: Vec::with_capacity(windows.len().min(MAX_WINDOWS)),
            past_cap: 0,
            unbuildable: 0,
        };
        for mut window in windows {
            if admitted.kept.len() == MAX_WINDOWS {
                admitted.past_cap += 1;
                continue;
            }
            if window.migrate_legacy_tabs().is_none() {
                admitted.unbuildable += 1;
                continue;
            }
            window.sanitize();
            if window.shape_is_valid() {
                admitted.kept.push(window);
            } else {
                admitted.unbuildable += 1;
            }
        }
        admitted
    }

    /// The log's words for the windows left out of `offered`, or `None` when
    /// every one came back.
    fn left_out_line(&self, offered: usize) -> Option<String> {
        let left_out = self.past_cap + self.unbuildable;
        if left_out == 0 {
            return None;
        }
        let mut causes = Vec::with_capacity(2);
        if self.past_cap > 0 {
            causes.push(format!(
                "{} past the {MAX_WINDOWS}-window limit",
                self.past_cap
            ));
        }
        if self.unbuildable > 0 {
            causes.push(format!(
                "{} in a shape this build cannot rebuild",
                self.unbuildable
            ));
        }
        Some(format!(
            "{left_out} of {offered} saved windows not reopened: {}",
            causes.join(", ")
        ))
    }
}

/// The manifest path: `<aterm data dir>/session.toml` — [`aterm_types::dirs::aterm_data_dir`],
/// `<data_dir>/aterm` unless a development build's `ATERM_STATE_HOME` gives the instance
/// its own root (ruling 406), which the crash journals beside it then follow too. `None`
/// when nothing resolves (e.g. wasm / a stripped environment, or a relative seam).
pub(crate) fn manifest_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_data_dir().map(|dir| dir.join("session.toml"))
}

/// Durably write `manifest` under the process-shared restore lock. Publication
/// uses a unique create-new sibling, file sync, atomic replacement, and directory
/// sync; a fixed `.tmp` name can neither collide nor be truncated.
///
/// The shipped writers stamp a run ([`write_quit_to`]) or publish a serialized
/// wire ([`write_wire_once_to`]); this spelling is the tests'.
#[cfg(test)]
pub(crate) fn write_to(path: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    write_quit_to(path, manifest, None)
}

/// THE QUIT'S OWN RUN (round seven of the update audit, finding 48's
/// review): the `[quit]` table a quit appends to its `session.toml`, naming
/// the run that wrote it by its crash journal's stem (`<pid>-<nanos>`,
/// `crash_journal::JournalId::stem`). A quit writes this file first and
/// retires its journal after; a quit cut short between the two (a Force Quit
/// of a slow quit, a logout's kill, a power loss) leaves both, and the next
/// claim must not reopen that same desk a second time behind its own quit
/// layout. Additive (L3): a build that does not know the table ignores it,
/// and a file without it (an older quit's) names no run.
///
/// AND EVERY OTHER INSTANCE'S DESK (restore audit, 2026-10-02): `also`, the
/// desks a stamped quit found waiting and kept behind its own
/// ([`write_quit_to`]), each by its run. Additive too: an older reader
/// reopens every window and sets aside only `run`'s journal.
#[derive(Serialize, Deserialize, Default)]
struct QuitDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quit: Option<QuitTable>,
}

#[derive(Serialize, Deserialize, Default)]
struct QuitTable {
    #[serde(default)]
    run: String,
    /// The other desks behind the quit's own, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    also: Vec<KeptDesk>,
}

/// Another instance's desk a `session.toml` holds ([`QuitTable::also`]): the
/// run whose quit wrote it, and how many of the file's windows are its.
#[derive(Serialize, Deserialize)]
struct KeptDesk {
    #[serde(default)]
    run: String,
    #[serde(default)]
    windows: usize,
}

/// The run a `session.toml`'s text names ([`QuitDoc`]); `None` when it names
/// none (an older quit's, or a layout no quit wrote).
pub(crate) fn quit_run_of(text: &str) -> Option<String> {
    aterm_toml::from_str::<QuitDoc>(text)
        .ok()?
        .quit
        .map(|quit| quit.run)
        .filter(|run| !run.is_empty())
}

/// The runs whose desks a `session.toml`'s text holds behind its own quit's
/// ([`QuitTable::also`]): a journal of one of them is that quit's own too.
pub(crate) fn quit_also_of(text: &str) -> Vec<String> {
    aterm_toml::from_str::<QuitDoc>(text)
        .ok()
        .and_then(|doc| doc.quit)
        .map_or_else(Vec::new, |quit| {
            quit.also
                .into_iter()
                .map(|desk| desk.run)
                .filter(|run| !run.is_empty())
                .collect()
        })
}

/// One quit's desk in a `session.toml`: its run, and its windows.
struct Desk {
    run: String,
    windows: Vec<WindowLayout>,
}

/// The desks a stamped `session.toml`'s `text` holds, its own quit's first;
/// `None` for one no stamping quit wrote (an older build's, an unstamped
/// write) or this build cannot read whole. Every window is read as written —
/// a desk is counted by position, and the parse's caps leave windows out
/// ([`RestoreManifest::from_toml`]; they apply when the file is taken).
fn desks_of(text: &str) -> Option<Vec<Desk>> {
    let quit = aterm_toml::from_str::<QuitDoc>(text).ok()?.quit?;
    let manifest = aterm_toml::from_str::<RestoreManifest>(text).ok()?;
    if quit.run.is_empty() || manifest.schema != SCHEMA {
        return None;
    }
    let behind: usize = quit.also.iter().map(|desk| desk.windows).sum();
    let mut windows = manifest.windows.into_iter();
    let lead = windows.len().saturating_sub(behind);
    let mut desks = vec![Desk {
        run: quit.run,
        windows: windows.by_ref().take(lead).collect(),
    }];
    for kept in quit.also {
        desks.push(Desk {
            run: kept.run,
            windows: windows.by_ref().take(kept.windows).collect(),
        });
    }
    Some(desks)
}

/// The text of a quit layout: `manifest` as `run`'s desk, then as much of
/// each of `kept` as fits within [`MAX_WINDOWS`] (a desk left no room is left
/// out whole, its run with it, so a journal of its quit cut short still
/// reopens it), and the `[quit]` table naming them. With nothing kept it is
/// exactly `manifest`'s text and the table naming `run`.
fn quit_wire(
    manifest: &RestoreManifest,
    run: &str,
    kept: impl IntoIterator<Item = Desk>,
) -> Result<String, String> {
    let mut room = MAX_WINDOWS.saturating_sub(manifest.windows.len());
    let mut behind = Vec::new();
    let mut also = Vec::new();
    for desk in kept {
        let windows = desk.windows.len().min(room);
        if windows == 0 {
            continue;
        }
        room -= windows;
        behind.extend(desk.windows.into_iter().take(windows));
        also.push(KeptDesk {
            run: desk.run,
            windows,
        });
    }
    let mut toml = if behind.is_empty() {
        manifest.to_toml()?
    } else {
        RestoreManifest {
            schema: SCHEMA,
            fold: manifest.fold.clone(),
            windows: manifest.windows.iter().cloned().chain(behind).collect(),
        }
        .to_toml()?
    };
    let table = aterm_toml::to_string(&QuitDoc {
        quit: Some(QuitTable {
            run: run.to_string(),
            also,
        }),
    })
    .map_err(|e| format!("serialize the quit's run: {e}"))?;
    if !toml.is_empty() && !toml.ends_with('\n') {
        toml.push('\n');
    }
    toml.push('\n');
    toml.push_str(&table);
    Ok(toml)
}

/// The desks of the `session.toml` at `path` that are not `run`'s — read
/// under the restore lock its caller holds; none when there is no file, or
/// one no stamping quit wrote ([`desks_of`]).
fn other_desks(path: &Path, run: &str) -> Vec<Desk> {
    fs::read_to_string(path)
        .ok()
        .as_deref()
        .and_then(desks_of)
        .unwrap_or_default()
        .into_iter()
        .filter(|desk| desk.run != run)
        .collect()
}

/// [`write_to`], the text followed by the `[quit]` table naming `run`, the
/// quit's own run ([`QuitDoc`]); `None` writes exactly what [`write_to`] does.
///
/// ANOTHER INSTANCE'S QUIT, WAITING (restore audit, 2026-10-02). Two
/// windowed instances are a supported configuration, and every windowed
/// launch takes `session.toml` at boot: of two running, the first to quit
/// wrote it and the second replaced it, the first one's desk gone with its
/// journal retired by its own quit. A stamped quit now reads the file under
/// the lock it replaces it under and keeps every other run's desk behind its
/// own ([`quit_wire`]), replacing only its own run's — so each desk opens
/// once, and a journal of either quit cut short is set aside by its run
/// (`crash_journal::QuitLayout::names`). An unstamped write (the tests'
/// [`write_to`], Windows' end-session flush) replaces as before, and so does
/// a stamped one over a file no stamping quit wrote: an older build's quit,
/// or that flush — this run's own snapshot when the session end it was
/// written for was cancelled and the person quits later.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "CrashJournalClaim",
        action = "WriteQuitLayout",
        project = "aterm_gui::crash_journal_conformance::Rig::project"
    )
)]
pub(crate) fn write_quit_to(
    path: &Path,
    manifest: &RestoreManifest,
    run: Option<&str>,
) -> Result<(), String> {
    write_quit_to_waiting(path, manifest, run, LockWait::Unbounded).map_err(String::from)
}

/// [`write_quit_to`] for the cold update lane, which runs on the GUI main
/// thread: the restore lock is waited for at most `budget`, and a holder past
/// it (another aterm reading or writing its windows, or stopped while it does)
/// is a PASSING refusal, never a wait with no end (round eight of the update
/// audit, U49). Every refusal says whether it passes ([`RestoreFault`]).
#[cfg(any(unix, test))]
pub(crate) fn write_quit_to_within(
    path: &Path,
    manifest: &RestoreManifest,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    write_quit_to_waiting(path, manifest, run, LockWait::Within(budget))
}

fn write_quit_to_waiting(
    path: &Path,
    manifest: &RestoreManifest,
    run: Option<&str>,
    wait: LockWait,
) -> Result<(), RestoreFault> {
    made_parent(path)?;
    with_restore_lock_waiting(path, wait, || {
        let wire = match run {
            Some(run) => quit_wire(manifest, run, other_desks(path, run))?,
            None => manifest.to_toml()?,
        };
        write_restore_locked(path, wire.as_bytes())
    })
}

/// TAKE BACK A QUIT LAYOUT NO EXEC USED: the cold update lane's, written for
/// a successor that did not start ([`write_quit_to`]), so that no later
/// launch reopens a desk that stayed up. Only `run`'s desk goes: every other
/// instance's desk the write kept stays, the file's lead passing to the
/// first of them. Taking the whole file lost them all. A file that holds no
/// desk of `run`'s is left as it is; with `run` `None` (an unstamped write,
/// which replaced the file) the file is taken.
#[cfg(test)]
pub(crate) fn unwrite_quit_to(path: &Path, run: Option<&str>) -> Result<(), String> {
    unwrite_quit_to_waiting(path, run, LockWait::Unbounded).map_err(String::from)
}

/// [`unwrite_quit_to`] for the cold update lane on the GUI main thread: the
/// restore lock is waited for at most `budget`, as its write waits
/// ([`write_quit_to_within`]; round eight of the update audit, U49).
#[cfg(any(unix, test))]
pub(crate) fn unwrite_quit_to_within(
    path: &Path,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    unwrite_quit_to_waiting(path, run, LockWait::Within(budget))
}

#[cfg(any(unix, test))]
fn unwrite_quit_to_waiting(
    path: &Path,
    run: Option<&str>,
    wait: LockWait,
) -> Result<(), RestoreFault> {
    let Some(run) = run else {
        if let Err(error) = take_from_result(path, wait) {
            crate::logging::stderr_line!("aterm-gui: restore manifest not consumed: {error}");
        }
        return Ok(());
    };
    let Some(parent) = path.parent() else {
        return Err(RestoreFault::settled(format!(
            "restore manifest {} has no parent",
            path.display()
        )));
    };
    if !parent.exists() {
        return Ok(());
    }
    with_restore_lock_waiting(path, wait, || {
        let Some(desks) = fs::read_to_string(path).ok().as_deref().and_then(desks_of) else {
            return Ok(());
        };
        if !desks.iter().any(|desk| desk.run == run) {
            return Ok(());
        }
        let mut others = desks.into_iter().filter(|desk| desk.run != run);
        match others.next() {
            Some(lead) => {
                let lead_desk = RestoreManifest {
                    schema: SCHEMA,
                    fold: LayoutFold::default(),
                    windows: lead.windows,
                };
                let wire = quit_wire(&lead_desk, &lead.run, others)?;
                write_restore_locked(path, wire.as_bytes())
            }
            None => fs::remove_file(path)
                .and_then(|()| sync_restore_directory(parent))
                .map_err(|error| {
                    RestoreFault::io(format_args!("take back {}", path.display()), &error)
                }),
        }
    })
}

/// `path`'s directory, made when it is not there.
fn made_parent(path: &Path) -> Result<&Path, RestoreFault> {
    let parent = path.parent().ok_or_else(|| {
        RestoreFault::settled(format!("restore manifest {} has no parent", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| RestoreFault::io(format_args!("mkdir {}", parent.display()), &error))?;
    Ok(parent)
}

/// [`write_to`] of an already serialized manifest: `wire` is published byte for
/// byte, under the same lock, sync, atomic replacement and directory sync.
#[cfg(any(unix, test))]
fn write_wire_to(path: &Path, wire: &str) -> Result<(), String> {
    made_parent(path)?;
    with_restore_lock(path, || write_restore_locked(path, wire.as_bytes())).map_err(String::from)
}

/// [`write_to`] for a ONE-SHOT name no other process will ever lock — an update
/// handoff's attempt-bound layout (`seamless-<pid>-<nonce>.layout.toml`), which its
/// successor reads without the lock — and then remove the lock file the write took.
/// Only such a name may drop its lock: for a shared one (`session.toml`) a lock
/// unlinked while another process waits on it lets a third lock a new file beside
/// it. Kept, these locks piled up in the control directory, one per update.
/// The handoff itself writes its layout through [`write_wire_once_to`]; this
/// spelling is the tests'.
#[cfg(test)]
pub(crate) fn write_once_to(path: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    write_wire_once_to(path, &manifest.to_toml()?)
}

/// [`write_once_to`] of an already serialized layout, published byte for byte:
/// the handoff's layout sidecar is written from the exact wire its digest and
/// its byte-equality check were taken over (`handoff_diet`), never from a
/// second serialization.
#[cfg(any(unix, test))]
pub(crate) fn write_wire_once_to(path: &Path, wire: &str) -> Result<(), String> {
    let written = write_wire_to(path, wire);
    if let Some(lock) = restore_lock_path(path) {
        let _ = fs::remove_file(lock);
    }
    written
}

/// A quit's `session.toml` as a launch takes it ([`take_quit_from`]).
#[derive(Debug)]
pub(crate) struct TakenQuit {
    pub(crate) manifest: RestoreManifest,
    /// The run its quit names ([`quit_run_of`]); `None` for an older build's
    /// quit, which names none.
    pub(crate) run: Option<String>,
    /// The runs of the other instances' desks it holds behind its quit's own
    /// ([`quit_also_of`]).
    pub(crate) also: Vec<String>,
    /// When the quit wrote it: the file's modification time, in Unix
    /// milliseconds ([`modified_unix_ms`]; `None` when the file system did
    /// not say). A quit writes its layout after its crash journal's last
    /// image, so a journal image written later is another run's
    /// (`crash_journal::QuitLayout::written_unix_ms`).
    pub(crate) written_unix_ms: Option<u64>,
}

/// When the file `meta` describes was last written, in Unix milliseconds;
/// `None` when the file system does not say.
pub(crate) fn modified_unix_ms(meta: &fs::Metadata) -> Option<u64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// Claim the manifest atomically, durably remove its public name, then parse the
/// claimed bytes. The rename is the single-use commit point: a crash during
/// parsing/apply cannot expose the same manifest to the next launch.
///
/// `None` means absent, consumed-but-invalid, or an I/O failure. Failures are
/// logged because this compatibility API intentionally falls back to a fresh
/// window rather than blocking launch. With the manifest come the run its
/// quit names ([`quit_run_of`]) and when it was written ([`TakenQuit`]).
pub(crate) fn take_quit_from(path: &Path) -> Option<TakenQuit> {
    match take_from_result(path, LockWait::Unbounded) {
        Ok(manifest) => manifest,
        Err(error) => {
            crate::logging::stderr_line!("aterm-gui: restore manifest not consumed: {error}");
            None
        }
    }
}

fn write_restore_locked(path: &Path, bytes: &[u8]) -> Result<(), RestoreFault> {
    let parent = path.parent().ok_or_else(|| {
        RestoreFault::settled(format!("restore manifest {} has no parent", path.display()))
    })?;
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("session.toml"));
    let (temporary, mut file) = create_unique_restore_sibling(parent, name, "write")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                RestoreFault::io(format_args!("protect {}", temporary.display()), &error)
            })?;
    }
    let result = (|| {
        file.write_all(bytes).map_err(|error| {
            RestoreFault::io(format_args!("write {}", temporary.display()), &error)
        })?;
        crate::durable::sync_all(&file).map_err(|error| {
            RestoreFault::io(format_args!("sync {}", temporary.display()), &error)
        })?;
        drop(file);
        replace_restore_file(&temporary, path).map_err(|error| {
            RestoreFault::io(format_args!("publish {}", path.display()), &error)
        })?;
        sync_restore_directory(parent).map_err(|error| {
            RestoreFault::published_but(
                format_args!(
                    "{} was published but directory durability is indeterminate",
                    path.display()
                ),
                &error,
            )
        })
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn take_from_result(path: &Path, wait: LockWait) -> Result<Option<TakenQuit>, String> {
    let Some(parent) = path.parent() else {
        return Err(format!("restore manifest {} has no parent", path.display()));
    };
    if !parent.exists() {
        return Ok(None);
    }
    with_restore_lock_waiting(path, wait, || {
        if !path.exists() {
            return Ok(None);
        }
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("session.toml"));
        let claim = unique_restore_sibling_path(parent, name, "claimed")?;
        claim_restore_file(path, &claim)
            .map_err(|error| format!("claim {}: {error}", path.display()))?;
        sync_restore_directory(parent).map_err(|error| {
            format!(
                "{} was claimed but single-use durability is indeterminate: {error}",
                path.display()
            )
        })?;

        // The claim is a rename: the file keeps the time the quit wrote it.
        let read = File::open(&claim).and_then(|mut file| {
            let written_unix_ms = file.metadata().ok().as_ref().and_then(modified_unix_ms);
            let mut text = String::new();
            std::io::Read::read_to_string(&mut file, &mut text)?;
            Ok((text, written_unix_ms))
        });
        let parsed = match read {
            Ok((text, written_unix_ms)) => {
                RestoreManifest::from_toml(&text).map(|manifest| TakenQuit {
                    manifest,
                    run: quit_run_of(&text),
                    also: quit_also_of(&text),
                    written_unix_ms,
                })
            }
            Err(error) => {
                crate::logging::stderr_line!(
                    "aterm-gui: claimed restore read {} failed: {error}",
                    claim.display()
                );
                None
            }
        };
        if let Err(error) = fs::remove_file(&claim) {
            crate::logging::stderr_line!(
                "aterm-gui: claimed restore cleanup {} failed: {error}",
                claim.display()
            );
        } else if let Err(error) = sync_restore_directory(parent) {
            crate::logging::stderr_line!(
                "aterm-gui: claimed restore cleanup sync {} failed: {error}",
                parent.display()
            );
        }
        Ok(parsed)
    })
}

/// A restore-file operation that did not land: what failed, and whether its
/// cause PASSES — another aterm holding the restore lock past a bounded wait,
/// a full disk or quota, an I/O error, no descriptor or memory just then — or
/// answers the same on every try: no data dir, a folder or lock this user
/// cannot write, a read-only volume, a volume without the locks or syncs the
/// write needs. Typed for the cold update lane, which retries a passing one and
/// goes on without its layout past a settled one (round eight of the update
/// audit, U48), where it had refused the update every minute with no end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoreFault {
    message: String,
    passing: bool,
    /// The file was published before the failure: only its directory's
    /// durability is unknown ([`Self::published_but`]).
    published: bool,
}

impl RestoreFault {
    /// A failure that answers the same on every try.
    pub(crate) fn settled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            passing: false,
            published: false,
        }
    }

    /// `context: error`, passing as the error is ([`io_error_passes`]).
    pub(crate) fn io(context: impl std::fmt::Display, error: &std::io::Error) -> Self {
        Self {
            message: format!("{context}: {error}"),
            passing: io_error_passes(error),
            published: false,
        }
    }

    /// [`Self::io`] for a failure AFTER the file was published (its
    /// directory's sync): a reader already sees it, so a caller that does not
    /// go on takes it back ([`Self::published`]).
    pub(crate) fn published_but(context: impl std::fmt::Display, error: &std::io::Error) -> Self {
        Self {
            published: true,
            ..Self::io(context, error)
        }
    }

    /// The restore lock at `lock_path`, held by another description past
    /// `budget`: passing — its holder lets go, or is resumed.
    #[cfg(any(unix, test))]
    fn busy(lock_path: &Path, budget: std::time::Duration) -> Self {
        Self {
            message: format!(
                "another aterm is reading or saving its windows (the restore lock {} was held \
                 past {} ms)",
                lock_path.display(),
                budget.as_millis()
            ),
            passing: true,
            published: false,
        }
    }

    /// Whether the cause passes (see the type).
    #[cfg_attr(
        not(unix),
        allow(
            dead_code,
            reason = "the cold update lane, which reads it, is unix-only"
        )
    )]
    pub(crate) fn passing(&self) -> bool {
        self.passing
    }

    /// Whether the file was published before the failure
    /// ([`Self::published_but`]).
    #[cfg_attr(
        not(unix),
        allow(
            dead_code,
            reason = "the cold update lane, which reads it, is unix-only"
        )
    )]
    pub(crate) fn published(&self) -> bool {
        self.published
    }
}

impl std::fmt::Display for RestoreFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<RestoreFault> for String {
    fn from(fault: RestoreFault) -> Self {
        fault.message
    }
}

/// A failure spelled as words alone (a layout that does not serialize): it
/// answers the same on every try.
impl From<String> for RestoreFault {
    fn from(message: String) -> Self {
        Self::settled(message)
    }
}

/// Whether `error` is a MOMENT: interrupted, timed out or would block; out of
/// memory or descriptors; a full disk or quota; an I/O error; a busy resource.
/// Everything else — `EACCES`, `EPERM`, `EROFS`, `ENOTSUP`, `ENOLCK`, a missing
/// folder — comes back the same every time. The install-location probe's split
/// (`aterm_update::bundle::classify_probe_error`), for this module's writes.
fn io_error_passes(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(
        error.kind(),
        ErrorKind::Interrupted
            | ErrorKind::TimedOut
            | ErrorKind::WouldBlock
            | ErrorKind::OutOfMemory
            | ErrorKind::StorageFull
            | ErrorKind::QuotaExceeded
            | ErrorKind::ResourceBusy
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(
                libc::ENOSPC
                    | libc::EDQUOT
                    | libc::EIO
                    | libc::EINTR
                    | libc::EAGAIN
                    | libc::ENOMEM
                    | libc::EMFILE
                    | libc::ENFILE
                    | libc::EBUSY
            )
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The lock file that serializes every write and claim of `path`: the hidden
/// `.<name>.aterm-restore.lock` beside it. `None` for a path with no parent.
fn restore_lock_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("session.toml"));
    Some(parent.join(format!(".{}.aterm-restore.lock", name.to_string_lossy())))
}

fn with_restore_lock<T, E: From<RestoreFault>>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    with_restore_lock_waiting(path, LockWait::Unbounded, operation)
}

/// How long a restore-lock take may wait for another holder.
#[derive(Clone, Copy, Debug)]
enum LockWait {
    /// Until the holder lets go: a launch's take, a person's quit, a worker.
    Unbounded,
    /// At most this long, then a passing refusal ([`RestoreFault::busy`]): the
    /// GUI main thread's, which may not wait with no end (round eight of the
    /// update audit, U49).
    #[cfg(any(unix, test))]
    Within(std::time::Duration),
}

/// [`with_restore_lock`], waiting for the lock as `wait` says.
fn with_restore_lock_waiting<T, E: From<RestoreFault>>(
    path: &Path,
    wait: LockWait,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let lock_path = restore_lock_path(path).ok_or_else(|| {
        RestoreFault::settled(format!("restore manifest {} has no parent", path.display()))
    })?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            RestoreFault::io(
                format_args!("open restore lock {}", lock_path.display()),
                &error,
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                RestoreFault::io(
                    format_args!("protect restore lock {}", lock_path.display()),
                    &error,
                )
            })?;
    }
    // Released by `LOCK_UN` when the guard drops, on every return — not by the
    // close alone. The take WAITS, and a child any thread forks while the lock is
    // held (a tab's shell) keeps a copy of its description until it execs: a lock
    // released by the close stayed taken in that copy, and the next manifest write
    // — this instance's next capture, another instance's — sat waiting for a
    // shell's exec instead of for a writer.
    let locking = |error: &std::io::Error| {
        RestoreFault::io(
            format_args!("lock restore manifest {}", path.display()),
            error,
        )
    };
    let _held = match wait {
        LockWait::Unbounded => {
            crate::native_document_host::HeldAdvisoryLock::lock(lock).map_err(|e| locking(&e))?
        }
        #[cfg(any(unix, test))]
        LockWait::Within(budget) => {
            match crate::native_document_host::acquire_advisory_lock_within(lock, budget) {
                Ok(held) => held,
                Err(crate::native_document_host::AdvisoryLockRefusal::Busy) => {
                    return Err(RestoreFault::busy(&lock_path, budget).into());
                }
                Err(crate::native_document_host::AdvisoryLockRefusal::Failed(error)) => {
                    return Err(locking(&error).into());
                }
            }
        }
    };
    #[cfg(all(test, unix))]
    crate::parked_fork::fork_if_armed();
    sweep_dead_restore_siblings(path);
    operation()
}

/// Remove the write temporaries and claims ([`restore_sibling_path`]) that a
/// writer or claimer of `path` left when it was killed mid-operation (or, a
/// claim, when its directory sync failed): each a whole layout that nothing
/// reads again. Run under `path`'s restore lock, which every write and claim of
/// it holds; a live pid's is left all the same, this process's included. Only
/// regular files go, so a link is never followed.
fn sweep_dead_restore_siblings(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("session.toml"));
    let prefix = format!(".{}.aterm-", name.to_string_lossy());
    let own = std::process::id();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(rest) = file_name
            .to_str()
            .and_then(|sibling| sibling.strip_prefix(&prefix))
            .filter(|rest| rest.ends_with(".tmp"))
        else {
            continue;
        };
        let pid = ["write-", "claimed-"]
            .iter()
            .find_map(|purpose| rest.strip_prefix(purpose))
            .and_then(|tail| tail.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok());
        if pid.is_some_and(|pid| pid != own && !aterm_uds::process::pid_alive(pid))
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn create_unique_restore_sibling(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
) -> Result<(PathBuf, File), RestoreFault> {
    let seed = next_restore_seed();
    for attempt in 0_u32..64 {
        let candidate = restore_sibling_path(parent, name, purpose, seed, attempt);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(RestoreFault::io(
                    format_args!("create {}", candidate.display()),
                    &error,
                ));
            }
        }
    }
    // Every name of one seed taken: the next try draws another.
    Err(RestoreFault {
        message: "could not allocate a unique restore temporary".to_string(),
        passing: true,
        published: false,
    })
}

fn unique_restore_sibling_path(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
) -> Result<PathBuf, String> {
    let seed = next_restore_seed();
    for attempt in 0_u32..64 {
        let candidate = restore_sibling_path(parent, name, purpose, seed, attempt);
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => return Err(format!("inspect {}: {error}", candidate.display())),
        }
    }
    Err("could not allocate a unique restore claim".to_string())
}

fn restore_sibling_path(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
    seed: u64,
    attempt: u32,
) -> PathBuf {
    parent.join(format!(
        ".{}.aterm-{purpose}-{}-{seed}-{attempt}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ))
}

fn next_restore_seed() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = aterm_time::since_epoch().map_or(0, |duration| duration.as_nanos() as u64);
    ordinal ^ nanos.rotate_left(19)
}

#[cfg(not(windows))]
fn replace_restore_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(not(windows))]
fn claim_restore_file(source: &Path, claim: &Path) -> std::io::Result<()> {
    fs::rename(source, claim)
}

#[cfg(windows)]
fn replace_restore_file(temporary: &Path, target: &Path) -> std::io::Result<()> {
    move_restore_file(temporary, target, true)
}

#[cfg(windows)]
fn claim_restore_file(source: &Path, claim: &Path) -> std::io::Result<()> {
    move_restore_file(source, claim, false)
}

#[cfg(windows)]
fn move_restore_file(source: &Path, target: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both UTF-16 buffers are NUL-terminated and remain live for the call.
    let moved = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), flags) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_restore_directory(path: &Path) -> std::io::Result<()> {
    crate::durable::sync_all(&File::open(path)?)
}

#[cfg(not(unix))]
fn sync_restore_directory(_path: &Path) -> std::io::Result<()> {
    // Windows publication/claim uses MOVEFILE_WRITE_THROUGH above.
    Ok(())
}

/// Write the manifest to the standard [`manifest_path`] (graceful-quit hook). With
/// no path, the refusal names its cause: a development build's relative (or empty)
/// `ATERM_STATE_HOME` resolves none on purpose (ruling 406), else no data dir
/// resolves (ruling 408). The quit's own write is [`write_quit`], which names
/// its run.
#[cfg(any(windows, test))]
pub(crate) fn write(manifest: &RestoreManifest) -> Result<(), String> {
    write_quit(manifest, None)
}

/// [`write`] for the quit: the layout stamped with the quit's own run
/// ([`write_quit_to`]).
pub(crate) fn write_quit(manifest: &RestoreManifest, run: Option<&str>) -> Result<(), String> {
    let path = manifest_path().ok_or_else(no_manifest_path)?;
    write_quit_to(&path, manifest, run)
}

/// Why no [`manifest_path`] resolves.
fn no_manifest_path() -> String {
    if aterm_types::dev_seam!("ATERM_STATE_HOME")
        .is_some_and(|root| !Path::new(&root).is_absolute())
    {
        "ATERM_STATE_HOME is not an absolute path; session restore is off".to_string()
    } else {
        "no data dir for session restore".to_string()
    }
}

/// [`write_quit_to_within`] at the standard [`manifest_path`]: the cold update
/// lane's layout. No path is a settled refusal.
#[cfg(unix)]
pub(crate) fn write_quit_within(
    manifest: &RestoreManifest,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    let path = manifest_path().ok_or_else(|| RestoreFault::settled(no_manifest_path()))?;
    write_quit_to_within(&path, manifest, run, budget)
}

/// [`unwrite_quit_to_within`] at the standard [`manifest_path`] (the cold
/// update lane's take-back).
#[cfg(unix)]
pub(crate) fn unwrite_quit_within(
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    let path = manifest_path().ok_or_else(|| RestoreFault::settled(no_manifest_path()))?;
    unwrite_quit_to_within(&path, run, budget)
}

/// [`take_quit_from`]'s layout alone.
#[cfg(test)]
pub(crate) fn take_from(path: &Path) -> Option<RestoreManifest> {
    take_quit_from(path).map(|taken| taken.manifest)
}

/// Read + delete the standard [`manifest_path`] (launch hook), with the run
/// its quit names ([`quit_run_of`]) and when it was written ([`TakenQuit`]).
pub(crate) fn take() -> Option<TakenQuit> {
    take_quit_from(&manifest_path()?)
}

// ---------------------------------------------------------------------------
// L1 (show the window early): the per-scale REAL cell-metrics cache.
//
// A windowed cold launch sizes its first — hidden — OS window from
// `seed_cell_px`, a 0.6×/1.2× em heuristic, and keeps it hidden until the
// backend build joins — a wait long enough to be worth hiding on WINDOWS, the
// one platform that takes the early-reveal path, and 0.01 ms on macOS per the
// 2026-08-23 arena drill-down; see the platform split in `app_window.rs`'s
// early-reveal note — because revealing the heuristic size would
// trade blankness for a visibly JUMPING window. This cache removes the
// heuristic on every launch after the first: `attach_os_window` persists the
// REAL cell metrics the finished backend derived, keyed by display scale
// factor, so the NEXT launch can size the pre-backend window exactly and
// reveal it before the join.
//
// Deliberately NOT part of [`RestoreManifest`]: the manifest is single-use
// (deleted on read) and written only on graceful quit, so folding the metrics
// in would lose them to every crash and every restore-less boot. This is a
// durable many-use sibling in the same data dir, sharing the manifest's lock +
// unique-temp publication machinery so the two persistence lanes cannot round
// atomicity differently.
//
// Staleness is SAFE, not correct-by-construction: a font-file update that
// changes advance widths without touching any config key yields a wrong entry.
// The consumer (`attach_os_window`) treats the cache as a size *prediction*,
// re-derives the truth after the backend joins, logs when they disagree, and
// re-persists — one launch with a one-off post-reveal resize, then converged.

/// On-disk schema for [`CellMetricsCache`]. Bumped on incompatible layout
/// change; a mismatched file is ignored wholesale (cold-launch behaviour),
/// never mis-parsed.
const CELL_METRICS_SCHEMA: u32 = 1;

/// Scales worth remembering. A desktop has a handful of distinct DPI factors;
/// dropping the OLDEST entry beyond this bounds the file against a pathological
/// `--scale` sweep writing unbounded rows.
const MAX_CELL_METRICS_ENTRIES: usize = 16;

/// Sanity bound for one persisted cell edge in device px. A corrupted or
/// hand-edited file must never size a window to absurdity; out-of-band entries
/// read as absent (cold launch), and degenerate measurements are never written.
const MAX_CELL_EDGE_PX: usize = 4096;

/// One scale factor's measured cell geometry.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub(crate) struct CellMetricsEntry {
    /// The display scale factor × 1000, rounded — an integer key so lookup is
    /// exact equality, immune to f64 formatting drift through TOML.
    pub scale_milli: u32,
    /// The font size (physical px) the cells were measured at — the launch
    /// default for this scale at write time. A reader predicts its own target
    /// px and rejects the entry on mismatch, so an explicit `font_px` config
    /// change invalidates without a schema bump.
    pub font_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// The whole cache: one font-selection fingerprint, then per-scale entries.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub(crate) struct CellMetricsCache {
    pub schema: u32,
    /// The cell-GEOMETRY fingerprint (font selection + line height) every entry
    /// was measured under. One key for the whole file, not per entry: a font
    /// change invalidates every scale's metrics at once, which is exactly true.
    pub font_key: String,
    #[serde(default)]
    pub entries: Vec<CellMetricsEntry>,
}

/// The cache path: `<aterm data dir>/cell-metrics.toml`, beside `session.toml` under the
/// same rule ([`aterm_types::dirs::aterm_data_dir`], ruling 406).
fn cell_metrics_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_data_dir().map(|dir| dir.join("cell-metrics.toml"))
}

/// `scale` → the integer entry key. Clamped positive so a hostile/broken scale
/// can neither alias 0 nor overflow.
fn cell_metrics_scale_milli(scale: f64) -> u32 {
    (scale * 1000.0).round().clamp(1.0, 1_000_000.0) as u32
}

/// Fail-safe read: any I/O error, malformed TOML, or schema mismatch is `None`
/// (the caller cold-launches), mirroring the manifest's parse contract.
fn read_cell_metrics(path: &Path) -> Option<CellMetricsCache> {
    let text = fs::read_to_string(path).ok()?;
    let cache: CellMetricsCache = aterm_toml::from_str(&text).ok()?;
    (cache.schema == CELL_METRICS_SCHEMA).then_some(cache)
}

/// The persisted cell size for `scale`, iff it was measured under the same
/// font fingerprint at (within rounding) the same font px the caller predicts
/// for this launch. `None` = cold launch (keep the hidden-until-ready path).
/// Unlocked read on the launch critical path: publication is whole-file
/// atomic (unique temp + rename), so a torn read cannot be observed.
#[cfg(windows)]
pub(crate) fn load_cell_metrics(
    font_key: &str,
    scale: f64,
    expected_font_px: f32,
) -> Option<(usize, usize)> {
    load_cell_metrics_from(&cell_metrics_path()?, font_key, scale, expected_font_px)
}

#[cfg(any(windows, test))]
fn load_cell_metrics_from(
    path: &Path,
    font_key: &str,
    scale: f64,
    expected_font_px: f32,
) -> Option<(usize, usize)> {
    let cache = read_cell_metrics(path)?;
    if cache.font_key != font_key {
        return None;
    }
    let key = cell_metrics_scale_milli(scale);
    let entry = cache.entries.iter().find(|e| e.scale_milli == key)?;
    // Same tolerance class as the attach path's own "backend already at the
    // target size" comparison (0.5 px): a real font-px change always exceeds
    // it, while a bit-identical re-derivation always passes.
    if (entry.font_px - expected_font_px).abs() >= 0.5 {
        return None;
    }
    let (cell_w, cell_h) = (entry.cell_w as usize, entry.cell_h as usize);
    ((1..=MAX_CELL_EDGE_PX).contains(&cell_w) && (1..=MAX_CELL_EDGE_PX).contains(&cell_h))
        .then_some((cell_w, cell_h))
}

/// The ONE display scale this machine has measured cells at — the cache's sole
/// entry — for `predicted_first_window_scale` on a host with no pre-window
/// display query. Two or more entries (a laptop that also drives an external
/// display) are `None`: choosing between them would be a guess dressed as a
/// record, and the caller's 1× fallback is the honest unknown. A missing or
/// malformed cache is `None` the same way `load_cell_metrics` reads it.
pub(crate) fn recorded_cell_metrics_scale() -> Option<f64> {
    recorded_cell_metrics_scale_from(&cell_metrics_path()?)
}

fn recorded_cell_metrics_scale_from(path: &Path) -> Option<f64> {
    let cache = read_cell_metrics(path)?;
    match cache.entries.as_slice() {
        [only] => Some(f64::from(only.scale_milli) / 1000.0),
        _ => None,
    }
}

/// Persist one scale's measured cell metrics. Fire-and-forget on a spawned
/// thread: the caller sits inside the window-attach bracket (squarely between
/// launch and first paint), and a lost write costs exactly one future cold
/// launch — self-healing, so neither the write nor its fsync belongs on the
/// critical path. Owned arguments because the thread outlives the borrow.
pub(crate) fn store_cell_metrics(
    font_key: String,
    scale: f64,
    font_px: f32,
    cell_w: usize,
    cell_h: usize,
) {
    let Some(path) = cell_metrics_path() else {
        return;
    };
    std::thread::spawn(move || {
        if let Err(error) = store_cell_metrics_to(&path, &font_key, scale, font_px, cell_w, cell_h)
        {
            crate::logging::stderr_line!("aterm-gui: cell-metrics cache not written: {error}");
        }
    });
}

fn store_cell_metrics_to(
    path: &Path,
    font_key: &str,
    scale: f64,
    font_px: f32,
    cell_w: usize,
    cell_h: usize,
) -> Result<(), String> {
    // Never persist a degenerate measurement: a 0-px or absurd cell would make
    // the NEXT launch reveal a broken frame. Skipping is correct — the cache
    // simply stays cold for this scale.
    if !(1..=MAX_CELL_EDGE_PX).contains(&cell_w) || !(1..=MAX_CELL_EDGE_PX).contains(&cell_h) {
        return Ok(());
    }
    let (Ok(cell_w), Ok(cell_h)) = (u32::try_from(cell_w), u32::try_from(cell_h)) else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| format!("cell-metrics cache {} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    // Read-modify-write UNDER the shared restore lock (keyed per file name, so
    // this never contends with `session.toml` traffic): two live aterm
    // instances finishing backends concurrently must not lose each other's
    // scale entries by racing the whole-file rewrite.
    with_restore_lock(path, || {
        let mut cache = read_cell_metrics(path)
            .filter(|c| c.font_key == font_key)
            .unwrap_or_else(|| CellMetricsCache {
                schema: CELL_METRICS_SCHEMA,
                font_key: font_key.to_string(),
                entries: Vec::new(),
            });
        let entry = CellMetricsEntry {
            scale_milli: cell_metrics_scale_milli(scale),
            font_px,
            cell_w,
            cell_h,
        };
        match cache
            .entries
            .iter_mut()
            .find(|e| e.scale_milli == entry.scale_milli)
        {
            // Steady state (every attach on an already-cached scale): byte-equal
            // entry, no disk write at all.
            Some(existing) if *existing == entry => return Ok(()),
            Some(existing) => *existing = entry,
            None => {
                if cache.entries.len() >= MAX_CELL_METRICS_ENTRIES {
                    cache.entries.remove(0);
                }
                cache.entries.push(entry);
            }
        }
        let toml =
            aterm_toml::to_string(&cache).map_err(|e| format!("serialize cell metrics: {e}"))?;
        write_restore_locked(path, toml.as_bytes())
    })
    .map_err(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE RESTORE LOCK IS FREE THE MOMENT ITS OPERATION RETURNS, even while a
    /// child forked under it has yet to exec: released by `LOCK_UN`, not by the
    /// close alone. The take waits, so a close-only release made the next manifest
    /// write wait out the exec of every shell forked while a write held the lock.
    /// The fork is made for real inside the hold ([`crate::parked_fork`]).
    #[cfg(unix)]
    #[test]
    fn the_restore_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("session.toml");
        let armed = crate::parked_fork::arm_at(1);
        with_restore_lock::<_, String>(&path, || Ok(())).expect("an uncontended restore lock");
        let child = armed.child();
        let lock_path = restore_lock_path(&path).expect("a lock path");
        let next = crate::native_document_host::HeldAdvisoryLock::try_lock(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("the lock file"),
        );
        assert!(
            next.is_ok(),
            "the restore lock stayed taken in the child forked under it: released by \
             the close alone"
        );
        drop(next);
        assert!(child.still_parked(), "…while the child had yet to exec");
        child.release();
    }

    /// ROUND EIGHT, U49: THE COLD UPDATE LANE'S LAYOUT WRITE, ON THE GUI MAIN
    /// THREAD, WAITS A BOUNDED TIME FOR THE RESTORE LOCK. Every instance takes
    /// that lock at its boot and its quit; one stopped while it holds it
    /// (Ctrl-Z on a shell-started window, a debugger) froze the window whose
    /// automatic cold lane came next, with no notice, until it let go. Now the
    /// write — and its take-back — refuse past the budget, as a passing
    /// refusal the lane retries; nothing is written. CONTROL: once the holder
    /// lets go, the same write lands.
    ///
    /// RED before the fix: the write waited for the holder (the bound below
    /// ran out with no answer).
    #[cfg(unix)]
    #[test]
    fn the_cold_lanes_layout_write_waits_a_bounded_time_for_the_restore_lock() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("session.toml");
        let budget = std::time::Duration::from_millis(100);
        // Another aterm's boot or quit: the lock held on its own description.
        let holder = crate::native_document_host::HeldAdvisoryLock::lock(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(restore_lock_path(&path).expect("a lock path"))
                .expect("the lock file"),
        )
        .expect("held");
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = {
            let path = path.clone();
            std::thread::spawn(move || {
                let wrote = write_quit_to_within(&path, &sample(), Some("1-2"), budget);
                let took = unwrite_quit_to_within(&path, Some("1-2"), budget);
                let _ = tx.send((wrote, took));
            })
        };
        let answered = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        drop(holder);
        let (wrote, took) = answered.expect("THE WRITE ANSWERED WITHIN ITS BOUND");
        writer.join().expect("the writer");
        let fault = wrote.expect_err("refused while another aterm holds the lock");
        assert!(fault.passing(), "a passing refusal, retried: {fault}");
        assert!(fault.to_string().contains("restore lock"), "{fault}");
        let fault = took.expect_err("the take-back is bounded too");
        assert!(fault.passing(), "{fault}");
        assert!(!path.exists(), "nothing was written");
        // CONTROL: free, it lands, and its take-back takes it.
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
        assert_eq!(
            take_from(&path).map(|manifest| manifest.windows.len()),
            Some(2)
        );
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
        unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
        assert!(!path.exists(), "its own desk, taken back");
    }

    /// ROUND EIGHT, U48: A LAYOUT WRITE SAYS WHETHER ITS CAUSE PASSES. A full
    /// disk, a quota, an I/O error or a busy lock is a moment the cold lane
    /// retries; a data folder this user cannot write, a read-only volume or a
    /// volume without the locks or syncs the write needs answers the same on
    /// every try, and the lane goes on without its layout rather than refuse
    /// the update every minute with no end.
    #[cfg(unix)]
    #[test]
    fn a_restore_write_fault_says_whether_its_cause_passes() {
        for errno in [
            libc::ENOSPC,
            libc::EDQUOT,
            libc::EIO,
            libc::EINTR,
            libc::EAGAIN,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOMEM,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(RestoreFault::io("write", &error).passing(), "{error}");
        }
        for errno in [
            libc::EACCES,
            libc::EPERM,
            libc::EROFS,
            libc::ENOTSUP,
            libc::ENOLCK,
            libc::ENOENT,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(!RestoreFault::io("write", &error).passing(), "{error}");
        }
        assert!(!RestoreFault::from("serialize".to_string()).passing());
        // A real one: a data folder this user cannot write.
        // SAFETY: `geteuid` has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = aterm_tempfile::tempdir().expect("scratch dir");
            let locked = dir.path().join("aterm");
            fs::create_dir(&locked).expect("the data folder");
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).expect("read-only");
            let fault = write_quit_to_within(
                &locked.join("session.toml"),
                &sample(),
                Some("1-2"),
                std::time::Duration::from_millis(100),
            )
            .expect_err("refused");
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("restored");
            assert!(!fault.passing(), "settled: {fault}");
            assert!(fault.to_string().contains("Permission denied"), "{fault}");
        }
    }

    /// A terminal leaf naming session `id`.
    fn terminal(id: Option<u64>, title: &str) -> RestoredSplitTree {
        RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
            cwd: Some("/tmp/work".to_string()),
            title: title.to_string(),
            profile: None,
            local_id: id,
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            identity: None,
            agent: None,
            held: false,
        }))
    }

    /// The `--hold` desk of the round-three review, as the outgoing capture
    /// writes it: a live shell (0) split with an exited pane (7) the handoff
    /// does not hand, beside a Settings tab carrying an unsaved draft — in both
    /// projections, the canonical tree and the legacy terminal mirror.
    fn held_desk(exited: Option<u64>) -> RestoreManifest {
        let mut settings = NativeLeafRestore::settings("/text-fonts".to_string());
        settings.settings_drafts = vec![SettingsDraftRestore {
            key: "font_family".to_string(),
            text: "Carried Mono".to_string(),
        }];
        RestoreManifest::new(vec![WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![PaneLayout::Split {
                dir: SplitKind::Vertical,
                ratio: 0.5,
                first: Box::new(PaneLayout::Leaf {
                    cwd: Some("/tmp/work".to_string()),
                    title: "zsh".to_string(),
                    focused: false,
                    local_id: Some(0),
                }),
                second: Box::new(PaneLayout::Leaf {
                    cwd: Some("/tmp/work".to_string()),
                    title: "build".to_string(),
                    focused: true,
                    local_id: exited,
                }),
            }],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(1),
            restored_tabs: vec![
                RestoredTab {
                    root: RestoredSplitTree::Split {
                        axis: SplitKind::Vertical,
                        ratio: 0.5,
                        first: Box::new(terminal(Some(0), "zsh")),
                        second: Box::new(terminal(exited, "build")),
                    },
                    focused_path: vec![RestoreBranch::Second],
                    zoomed: false,
                },
                RestoredTab {
                    root: RestoredSplitTree::leaf(RestoredView::Native(settings)),
                    focused_path: Vec::new(),
                    zoomed: false,
                },
            ],
        }])
    }

    /// THE PANE IN FRONT is each window's active tab's FOCUSED terminal, by
    /// `active_item` and `focused_path` (round eight of the update audit, U47):
    /// the one pane whose title an outgoing build kept current in its session
    /// record, so the seamless adoption trusts that copy for it alone.
    #[test]
    fn the_pane_in_front_is_each_windows_active_tabs_focused_terminal() {
        let mut desk = held_desk(Some(7));
        // The Settings tab in front: no terminal is.
        assert_eq!(desk.windows[0].active_item, Some(1));
        assert!(desk.front_terminal_ids().is_empty());
        // The split in front: its focused SECOND pane, never the first.
        desk.windows[0].active_item = Some(0);
        assert_eq!(desk.front_terminal_ids(), vec![7]);
        desk.windows[0].restored_tabs[0].focused_path = vec![RestoreBranch::First];
        assert_eq!(desk.front_terminal_ids(), vec![0]);
        // A second window answers for its own front pane.
        let mut other = held_desk(Some(9)).windows.remove(0);
        other.active_item = Some(0);
        desk.windows.push(other);
        assert_eq!(desk.front_terminal_ids(), vec![0, 9]);
        // A focused pane that names no handed shell, and a window that
        // recorded no active item, name none.
        desk.windows[1].restored_tabs[0].root = RestoredSplitTree::Split {
            axis: SplitKind::Vertical,
            ratio: 0.5,
            first: Box::new(terminal(Some(8), "zsh")),
            second: Box::new(terminal(None, "build")),
        };
        desk.windows[0].active_item = None;
        assert!(desk.front_terminal_ids().is_empty());
    }

    /// A LAYOUT THAT NAMES A PANE THE HANDOFF DID NOT HAND IS PLACED (round
    /// three of the 2026-09 update robustness work). The outgoing process
    /// hands only live sessions, but stamps every terminal view's id on the
    /// layout, so an exited `--hold` pane made the layout name one session more
    /// than was handed — and the successor, demanding an exact match, dropped
    /// the whole layout: the split, the tab order and the carried Settings
    /// draft with it. Now that one pane becomes a placeholder, and everything
    /// else is placed as it was.
    ///
    /// The CONTROL is the old acceptance, `covers_exact_seamless_ids`, which
    /// refuses this very layout.
    #[test]
    fn a_layout_naming_an_unhanded_pane_is_placed_with_that_pane_retired() {
        let wire = held_desk(Some(7))
            .to_toml()
            .expect("the outgoing capture serializes");
        let parsed = RestoreManifest::from_toml(&wire).expect("the successor parses it");
        assert!(
            !parsed.covers_exact_seamless_ids(&[0]),
            "CONTROL: the exact check refuses the layout for the exited pane"
        );
        let placed = parsed
            .clone()
            .placed_for_handed(&[0])
            .expect("placed, with the unhanded pane retired");
        assert_eq!(placed.seamless_terminal_ids(), Some(vec![0]));
        assert!(placed.covers_exact_seamless_ids(&[0]));
        let window = &placed.windows[0];
        assert!(
            window.shape_is_valid(),
            "the placed layout is one from_toml admits"
        );
        let tab = &window.restored_tabs[0];
        assert_eq!(
            tab.focused_path,
            vec![RestoreBranch::Second],
            "the focus is kept"
        );
        let RestoredSplitTree::Split { first, second, .. } = &tab.root else {
            panic!("the split is kept: {:?}", tab.root);
        };
        assert!(matches!(
            &**first,
            RestoredSplitTree::Leaf { view: RestoredView::Terminal(live) } if live.local_id == Some(0)
        ));
        let RestoredSplitTree::Leaf {
            view: RestoredView::Placeholder(retired),
        } = &**second
        else {
            panic!("the unhanded pane is a placeholder: {second:?}");
        };
        assert_eq!(retired.restore_tag, "terminal");
        assert_eq!(retired.reason, crate::update_words::PANE_NOT_CARRIED);
        assert!(retired.metadata.contains("build"), "{}", retired.metadata);
        assert_eq!(
            retired.unhanded_local_id(),
            Some(7),
            "the placeholder names the pane a held screen is matched by"
        );
        assert!(
            !placed
                .to_toml()
                .expect("the placed layout serializes")
                .contains("unhanded"),
            "and that id never reaches a wire"
        );
        assert_eq!(
            placed.carried_settings_drafts(),
            (
                vec![SettingsDraftRestore {
                    key: "font_family".to_string(),
                    text: "Carried Mono".to_string(),
                }],
                0
            ),
            "the Settings draft is placed with its tab"
        );
        let PaneLayout::Split { first, second, .. } = &window.tabs[0] else {
            panic!("the legacy mirror is kept");
        };
        assert_eq!(
            (first.local_id(), second.local_id()),
            (Some(0), None),
            "the legacy mirror names no session that was not handed"
        );
        assert_eq!(
            window.bootstrap_local_id(),
            Some(0),
            "the bootstrap pick is the handed shell"
        );

        // An exact layout is placed unchanged.
        let exact = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
        assert_eq!(exact.clone().placed_for_handed(&[0, 7]), Ok(exact));
    }

    /// What is NOT a pane the outgoing process chose not to hand stays
    /// unplaced, for the orphan net: a handed session the layout does not
    /// name, a handed id listed twice, or a terminal leaf with no id.
    #[test]
    fn a_layout_missing_a_handed_session_stays_unplaced() {
        let parsed = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
        assert!(
            parsed.clone().placed_for_handed(&[0, 5]).is_err(),
            "5 is not named"
        );
        assert!(
            parsed.clone().placed_for_handed(&[0, 0]).is_err(),
            "0 twice"
        );
        let unnamed = RestoreManifest::from_toml(&held_desk(None).to_toml().unwrap()).unwrap();
        assert!(
            unnamed.placed_for_handed(&[0]).is_err(),
            "a leaf with no id"
        );
    }

    /// ROUND SIX, FINDING 10: a layout that names one session in two leaves —
    /// `Open Session in New Window` — is PLACED, whole: the repeat is one
    /// session shown twice, not an ambiguous PTY. Before this, every update
    /// taken while a co-view was open dropped the layout, and every window
    /// folded into tabs of the first.
    #[test]
    fn a_layout_naming_a_session_in_two_windows_is_placed_whole() {
        let mut desk = held_desk(Some(7));
        desk.windows.push(WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![PaneLayout::Leaf {
                cwd: Some("/tmp/work".to_string()),
                title: "zsh".to_string(),
                focused: true,
                local_id: Some(0),
            }],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: vec![RestoredTab {
                root: terminal(Some(0), "zsh"),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        });
        let parsed = RestoreManifest::from_toml(&desk.to_toml().unwrap())
            .expect("the successor parses a co-viewed layout");
        assert_eq!(parsed.seamless_terminal_ids(), Some(vec![0, 7]));
        let placed = parsed
            .placed_for_handed(&[0, 7])
            .expect("a co-viewed session does not unplace the layout");
        assert_eq!(placed.windows.len(), 2, "both windows are kept");
    }

    /// The placeholder's reason fits the leaf it is written into: a reason over
    /// the cap would be a layout `from_toml` refuses.
    #[test]
    fn the_unhanded_pane_reason_fits_a_placeholder() {
        let reason = crate::update_words::PANE_NOT_CARRIED;
        assert!(
            !reason.is_empty()
                && reason.len() <= MAX_SETTINGS_ROUTE_BYTES
                && !reason.contains('\0')
        );
    }

    fn sample() -> RestoreManifest {
        // A two-window manifest: window 0 has one plain tab + one split tab; window 1 a
        // single tab. Exercises every variant + per-leaf cwd/title/focus + active_tab.
        RestoreManifest::new(vec![
            WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 1,
                outer_x: Some(120),
                outer_y: Some(64),
                maximized: Some(true),
                inner_px: None,
                presence_row: false,
                show: WindowShow::UNKNOWN,
                tabs: vec![
                    PaneLayout::leaf(Some("/home/a".into()), "zsh".into(), true),
                    PaneLayout::Split {
                        dir: SplitKind::Vertical,
                        ratio: 0.6,
                        first: Box::new(PaneLayout::leaf(Some("/tmp".into()), "vim".into(), true)),
                        second: Box::new(PaneLayout::leaf(None, "top".into(), false)),
                    },
                ],
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: None,
                restored_tabs: Vec::new(),
            },
            WindowLayout {
                rows: 40,
                cols: 132,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                inner_px: None,
                presence_row: false,
                show: WindowShow::UNKNOWN,
                tabs: vec![PaneLayout::leaf(Some("/".into()), "sh".into(), true)],
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: None,
                restored_tabs: Vec::new(),
            },
        ])
    }

    fn mixed_sample(uri: &str) -> RestoreManifest {
        RestoreManifest::new(vec![WindowLayout {
            rows: 32,
            cols: 110,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![PaneLayout::leaf(Some("/tmp".into()), "shell".into(), true)],
            native_tabs: vec![
                NativeTabRestore::Settings {
                    route: "/about".to_string(),
                },
                NativeTabRestore::Editor {
                    uri: uri.to_string(),
                },
            ],
            tab_order: vec![
                TabOrderEntry::Native { index: 0 },
                TabOrderEntry::Terminal { index: 0 },
                TabOrderEntry::Native { index: 1 },
            ],
            active_item: Some(2),
            restored_tabs: Vec::new(),
        }])
    }

    fn draft(key: &str, text: &str) -> SettingsDraftRestore {
        SettingsDraftRestore {
            key: key.to_string(),
            text: text.to_string(),
        }
    }

    /// A handoff layout whose Settings leaf carries `drafts`, as
    /// `App::capture_handoff_layout` builds it (through `RestoreManifest::new`,
    /// the capture's sanitize).
    fn settings_handoff_layout(drafts: Vec<SettingsDraftRestore>) -> RestoreManifest {
        let mut leaf = NativeLeafRestore::settings("/text-fonts".to_string());
        leaf.settings_drafts = drafts;
        RestoreManifest::new(vec![WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: Vec::new(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Native(leaf)),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        }])
    }

    /// A Settings leaf's carried drafts are part of the handoff layout's wire and
    /// the capture stays a FIXED POINT of the parse — the property the update
    /// worker checks (`from_toml(to_toml(layout)) == layout`) and the layout
    /// digest is taken over. Text with quotes, newlines and non-ASCII survives.
    #[test]
    fn settings_drafts_ride_the_handoff_layout_as_a_fixed_point() {
        let layout = settings_handoff_layout(vec![
            draft("font_family", "Menlo \"Nerd\"\nFont"),
            draft("shell", "/bin/zsh -l \u{3042}"),
        ]);
        let wire = layout.to_toml().expect("serialize");
        let parsed = RestoreManifest::from_toml(&wire).expect("the layout parses");
        assert_eq!(parsed, layout, "the capture is a fixed point of the parse");
        assert_eq!(parsed.to_toml().unwrap(), wire);
        let RestoredSplitTree::Leaf {
            view: RestoredView::Native(native),
        } = &parsed.windows[0].restored_tabs[0].root
        else {
            panic!("the Settings leaf");
        };
        assert_eq!(native.settings_drafts.len(), 2);
        assert_eq!(native.settings_drafts_unreadable, 0);
        assert!(
            !settings_handoff_layout(Vec::new())
                .to_toml()
                .unwrap()
                .contains("settings_drafts"),
            "a layout with no draft is byte-identical to what an older consumer reads"
        );
    }

    /// THE LENIENT CONSUMER: a carried draft this build cannot read or reopen
    /// costs that draft — counted, so the successor says so — never the leaf,
    /// the layout, or the handoff.
    #[test]
    fn an_unreadable_settings_draft_costs_the_draft_not_the_layout() {
        let parse = |tag: &str, extra: &str| -> NativeLeafRestore {
            let route = if tag == "settings" {
                "route = \"/home\"\n"
            } else {
                "uri = \"file:///tmp/notes.md\"\n"
            };
            let wire = format!("kind = \"native\"\nrestore_tag = \"{tag}\"\n{route}{extra}");
            let mut view: RestoredView = aterm_toml::from_str(&wire)
                .unwrap_or_else(|error| panic!("a draft never fails the leaf: {error:?}\n{wire}"));
            view.sanitize();
            let RestoredView::Native(native) = view else {
                panic!("the leaf stays native");
            };
            assert!(view_is_bounded(&native), "{native:?}");
            native
        };
        let garbage = parse("settings", "settings_drafts = \"garbage\"\n");
        assert!(garbage.settings_drafts.is_empty());
        assert_eq!(garbage.settings_drafts_unreadable, 1);

        let long = "x".repeat(MAX_SETTINGS_DRAFT_TEXT_BYTES + 1);
        let mixed = parse(
            "settings",
            &format!(
                "settings_drafts = [{{ key = \"font_family\", text = \"Mono\" }}, {{ key = 7, \
                 text = \"x\" }}, \"loose\", {{ key = \"font_family\", text = \"again\" }}, \
                 {{ key = \"shell\", text = \"a\\u0000b\" }}, {{ key = \"padding\", text = \
                 \"{long}\" }}, {{ key = \"\", text = \"keyless\" }}]\n"
            ),
        );
        assert_eq!(mixed.settings_drafts, vec![draft("font_family", "Mono")]);
        assert_eq!(
            mixed.settings_drafts_unreadable, 6,
            "a wrong type, a stray value, a repeat, a NUL, an overlong text and an empty key"
        );

        let editor = parse(
            "editor",
            "settings_drafts = [{ key = \"font_family\", text = \"Mono\" }]\n",
        );
        assert!(
            editor.settings_drafts.is_empty(),
            "only a Settings leaf carries drafts"
        );
        assert_eq!(editor.settings_drafts_unreadable, 1);

        let over = parse(
            "settings",
            &format!(
                "settings_drafts = [{}]\n",
                (0..=MAX_SETTINGS_DRAFTS_PER_VIEW)
                    .map(|index| format!("{{ key = \"k{index}\", text = \"v\" }}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        assert_eq!(over.settings_drafts.len(), MAX_SETTINGS_DRAFTS_PER_VIEW);
        assert_eq!(over.settings_drafts_unreadable, 1);
    }

    fn view_is_bounded(native: &NativeLeafRestore) -> bool {
        RestoredView::Native(native.clone()).bounded()
    }

    /// The manifest survives a TOML round-trip byte-for-byte (structure + order + cwd +
    /// title + focus + ratio), the persistence contract restore depends on.
    #[test]
    fn manifest_round_trips_through_toml() {
        let m = sample();
        let toml = m.to_toml().expect("serialize");
        let back = RestoreManifest::from_toml(&toml).expect("parse");
        assert_eq!(m, back);
        // leaf_count sanity: window 0 has 1 + 2 = 3 leaves across its 2 tabs.
        assert_eq!(m.windows[0].tabs[1].leaf_count(), 2);
        assert!(matches!(
            m.windows[0].restored_tabs[1].root,
            RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ..
            }
        ));
        assert!(!m.is_empty());
    }

    /// W3 schema compatibility both ways. FORWARD: a pre-`maximized` manifest (the
    /// key simply absent — byte-for-byte what an older build wrote, thanks to
    /// `skip_serializing_if`) parses with `maximized: None`, so an upgrade never
    /// rejects the user's saved session. BACKWARD: a `None` never emits the key,
    /// so a manifest written by this build and read by an older one carries
    /// nothing new unless a window really was maximized (and serde's default
    /// unknown-field tolerance absorbs it there).
    #[test]
    fn manifest_maximized_is_schema_compatible() {
        // sample() has window 0 maximized: the flag must survive the round trip
        // (covered structurally by `manifest_round_trips_through_toml`, asserted
        // by name here so a serde-attr regression fails with a legible message).
        let toml = sample().to_toml().expect("serialize");
        assert!(toml.contains("maximized = true"));
        let back = RestoreManifest::from_toml(&toml).expect("parse");
        assert_eq!(back.windows[0].maximized, Some(true));
        assert_eq!(back.windows[1].maximized, None);

        // The legacy shape: strip the flag and re-serialize — the key vanishes
        // entirely (an old manifest, byte-for-byte in this respect) and parsing
        // yields "unknown", never a forced state.
        let mut legacy = sample();
        for window in &mut legacy.windows {
            window.maximized = None;
        }
        let legacy_toml = legacy.to_toml().expect("serialize legacy");
        assert!(!legacy_toml.contains("maximized"));
        let back = RestoreManifest::from_toml(&legacy_toml).expect("parse legacy");
        assert!(back.windows.iter().all(|w| w.maximized.is_none()));
    }

    /// Gap #29, the wire half. A window's show state round-trips exactly — the
    /// parent's own pre-flight (`from_toml(to_toml(layout)) == layout`) and the
    /// byte fixed point a same-build child re-proves — and an UNKNOWN one emits
    /// nothing at all, so every capture that records none (every platform but
    /// macOS, every headless capture) writes the wire an older build writes.
    #[test]
    fn window_show_state_round_trips_and_unknown_emits_nothing() {
        let mut shown = sample();
        shown.windows[0].show = WindowShow {
            fullscreen: Some(true),
            minimized: Some(false),
            z_order: Some(0),
            key: Some(true),
        };
        shown.windows[1].show = WindowShow {
            fullscreen: Some(false),
            minimized: Some(true),
            z_order: Some(1),
            key: Some(false),
        };
        let wire = shown.to_toml().expect("serialize");
        let back = RestoreManifest::from_toml(&wire).expect("parse");
        assert_eq!(
            back, shown,
            "the show state survives the round trip:\n{wire}"
        );
        assert_eq!(
            back.to_toml().expect("reserialize"),
            wire,
            "a manifest carrying show state is a byte fixed point of the codec"
        );

        let unknown = sample();
        let wire = unknown.to_toml().expect("serialize");
        assert!(
            !wire.contains("show") && !wire.contains("z_order"),
            "an unknown show state must not reach the wire:\n{wire}"
        );
        let back = RestoreManifest::from_toml(&wire).expect("parse");
        assert!(back.windows.iter().all(|w| w.show.is_unknown()));
    }

    /// Gap #29, compatibility with an OLDER peer in both directions. FORWARD:
    /// every checked-in layout a parent wrote before the show state parses with
    /// it unknown (the successor then changes nothing), and one a parent that
    /// writes it (v0.100.0's producer on macOS, `[windows.show]`) parses with
    /// exactly the windows it carried known. BACKWARD: an older reader's
    /// `WindowLayout` has no `show` field and no `deny_unknown_fields`, so it
    /// skips the table — shown here with a reader that knows only the grid,
    /// the narrowest older shape. The child's layout digest hashes the wire it
    /// read (`seamless::layout_wire_digest`), so skipping a key it does not know
    /// cannot move the proof. And a key a LATER build adds inside `show` is
    /// skipped by this one.
    #[test]
    fn window_show_state_is_compatible_with_older_and_newer_peers() {
        let mut shown = sample();
        shown.windows[0].show = WindowShow {
            fullscreen: Some(true),
            minimized: Some(false),
            z_order: Some(0),
            key: Some(true),
        };
        let wire = shown.to_toml().expect("serialize");
        assert!(wire.contains("[windows.show]"), "wire: {wire}");

        #[derive(Deserialize)]
        struct OlderWindow {
            rows: u16,
            cols: u16,
        }
        #[derive(Deserialize)]
        struct OlderManifest {
            schema: u32,
            windows: Vec<OlderWindow>,
        }
        let older: OlderManifest =
            aterm_toml::from_str(&wire).expect("an older reader skips the show table");
        assert_eq!(older.schema, SCHEMA);
        assert_eq!(
            older
                .windows
                .iter()
                .map(|w| (w.rows, w.cols))
                .collect::<Vec<_>>(),
            vec![(24, 80), (40, 132)]
        );

        let later = wire.replacen("[windows.show]\n", "[windows.show]\nspace_index = 3\n", 1);
        assert_ne!(later, wire, "the later key was spliced in:\n{later}");
        let back = RestoreManifest::from_toml(&later).expect("a later show key is skipped");
        assert_eq!(back.windows[0].show, shown.windows[0].show);

        // Every layout an older release's parent really wrote (the checked-in
        // handoff fixtures, one directory per release and desk).
        let fixtures = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/handoff"
        ));
        let mut layouts = 0;
        let (mut without_show, mut with_show) = (0, 0);
        for release in fs::read_dir(fixtures).expect("the handoff fixtures") {
            let release = release.expect("a release directory").path();
            if !release.is_dir() {
                continue;
            }
            for desk in fs::read_dir(&release).expect("a release's desks") {
                let desk = desk.expect("a desk").path();
                // A release directory also holds files of its own (its
                // `rendezvous.toml`); only its directories are desks.
                if !desk.is_dir() {
                    continue;
                }
                for file in fs::read_dir(&desk).expect("a desk's files") {
                    let path = file.expect("a fixture file").path();
                    if !path.to_string_lossy().ends_with(".layout.toml") {
                        continue;
                    }
                    let wire = fs::read_to_string(&path).expect("a fixture layout");
                    let parsed = RestoreManifest::from_toml(&wire)
                        .unwrap_or_else(|| panic!("{} parses", path.display()));
                    // A window's `[windows.show]` table is written only
                    // when something was captured (`WindowShow::is_unknown`),
                    // so the tables a parent wrote are the windows it carried.
                    let carried = wire.matches("[windows.show]\n").count();
                    let known = parsed
                        .windows
                        .iter()
                        .filter(|w| !w.show.is_unknown())
                        .count();
                    assert_eq!(
                        known,
                        carried,
                        "{}: a parent's show state is read as it carried it, and none is \
                         invented where it carried none",
                        path.display()
                    );
                    if carried == 0 {
                        without_show += 1;
                    } else {
                        with_show += 1;
                    }
                    layouts += 1;
                }
            }
        }
        assert!(layouts >= 2, "the fixture walk read {layouts} layouts");
        assert!(
            without_show > 0,
            "the walk read a layout of a parent older than the show state"
        );
        assert!(with_show > 0, "and one of a parent that writes it");
    }

    #[test]
    fn restore_parse_canonicalizes_hostile_user_chrome_metadata() {
        let mut manifest = sample();
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &mut manifest.windows[0].restored_tabs[0].root
        else {
            panic!("sample first tab is a terminal leaf");
        };
        terminal.user_title = Some("  build\n\u{202e}agent  ".to_string());
        terminal.description = Some(format!("purpose\u{2029}{}", "x".repeat(1100)));
        terminal.icon = Some("\u{2066}👨‍👩‍👧‍👦\u{2069}".to_string());

        // Serialize the deliberately noncanonical structure directly: the
        // ordinary constructor already sanitizes and would not exercise parse.
        let wire = aterm_toml::to_string(&manifest).expect("serialize hostile fixture");
        let decoded = RestoreManifest::from_toml(&wire).expect("bounded restore parses");
        let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(terminal),
        } = &decoded.windows[0].restored_tabs[0].root
        else {
            panic!("decoded first tab is a terminal leaf");
        };
        assert_eq!(terminal.user_title.as_deref(), Some("buildagent"));
        assert_eq!(terminal.icon.as_deref(), Some("👨‍👩‍👧‍👦"));
        let description = terminal
            .description
            .as_deref()
            .expect("description retained");
        assert!(description.starts_with("purpose"));
        assert!(description.len() <= crate::session_timeline::META_DESCRIPTION_MAX);
        assert!(!crate::session_timeline::metadata_has_forbidden_formatting(
            description
        ));
        assert!(terminal.user_metadata_is_canonical());
    }

    /// The retired Claude Code hooks' bare badge does not survive the parse —
    /// the half that clears one an older parent still writes into the wire —
    /// and dropping it keeps the rest of the leaf and the round trip's fixed
    /// point. NEGATIVE CONTROL: an attention without the prefix arrives intact.
    #[test]
    fn restore_parse_drops_the_retired_hook_badge_and_keeps_the_rest() {
        let parse = |attention: &str| {
            let mut manifest = sample();
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } = &mut manifest.windows[0].restored_tabs[0].root
            else {
                panic!("sample first tab is a terminal leaf");
            };
            terminal.user_title = Some("worker".to_string());
            terminal.attention = Some(attention.to_string());
            // Serialized directly, as an unfixed parent writes it: the
            // ordinary constructor would drop the badge before the parse.
            let wire = aterm_toml::to_string(&manifest).expect("serialize legacy fixture");
            assert!(wire.contains(attention), "the wire carries it: {wire}");
            let decoded = RestoreManifest::from_toml(&wire).expect("the manifest still parses");
            let again = RestoreManifest::from_toml(&decoded.to_toml().expect("serialize"))
                .expect("reparse");
            assert_eq!(again, decoded, "the parse is a fixed point");
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } = &decoded.windows[0].restored_tabs[0].root
            else {
                panic!("decoded first tab is a terminal leaf");
            };
            assert_eq!(terminal.user_title.as_deref(), Some("worker"));
            assert!(terminal.user_metadata_is_canonical());
            terminal.attention.clone()
        };
        assert_eq!(
            parse("claude needs approval: Claude needs your permission - permission_prompt"),
            None
        );
        assert_eq!(
            parse("waiting on a human review").as_deref(),
            Some("waiting on a human review")
        );
    }

    #[test]
    fn mixed_native_descriptors_round_trip_without_live_or_transient_state() {
        let manifest = mixed_sample("file:///tmp/readme.md");
        let toml = manifest.to_toml().expect("serialize");
        let decoded = RestoreManifest::from_toml(&toml).expect("decode mixed manifest");
        assert_eq!(decoded, manifest);
        let window = &decoded.windows[0];
        let order = window.canonical_order().expect("validated order");
        assert_eq!(window.canonical_active(&order), 2);
        assert_eq!(
            window.native_tabs[1].document_uri(),
            Some("file:///tmp/readme.md")
        );
        for forbidden in [
            "tab_id",
            "view_id",
            "instance_id",
            "search_input",
            "draft",
            "unsaved",
        ] {
            assert!(
                !toml.contains(forbidden),
                "transient field {forbidden:?} leaked into restore TOML"
            );
        }
    }

    #[test]
    fn legacy_terminal_only_manifest_migrates_to_identity_order() {
        let legacy = r#"
schema = 1

[[windows]]
rows = 24
cols = 80
active_tab = 0

[[windows.tabs]]
kind = "leaf"
cwd = "/tmp"
title = "shell"
focused = true
"#;
        let decoded = RestoreManifest::from_toml(legacy).expect("legacy manifest");
        assert!(decoded.windows[0].native_tabs.is_empty());
        assert_eq!(
            decoded.windows[0].canonical_order(),
            Some(vec![TabOrderEntry::Terminal { index: 0 }])
        );
        assert_eq!(
            decoded.windows[0]
                .canonical_active(&decoded.windows[0].canonical_order().expect("legacy order")),
            0
        );
    }

    /// ROUND SEVEN OF THE UPDATE AUDIT, FINDING 12: THE CAPS DEGRADE PER
    /// WINDOW. A desk of 65 windows, one of which holds 257 tabs, wrote a
    /// layout the parse refused whole — the quit's and every crash journal's —
    /// and the next launch opened one fresh window. The window past a cap is
    /// left out alone, the windows after the 64th are not read, and the rest
    /// come back in order. NEGATIVE CONTROL: a layout none of whose windows
    /// reads is still refused.
    ///
    /// RED before the fix: `from_toml` answered `None`.
    #[test]
    fn a_layout_past_the_caps_keeps_every_window_it_can() {
        use crate::crash_journal::fixtures::layout;
        let window = |cwd: String| layout(&[(cwd.as_str(), "zsh")]).windows.remove(0);
        let crowded: Vec<(String, String)> = (0..=MAX_TABS_PER_WINDOW)
            .map(|i| (format!("/t{i}"), "zsh".to_string()))
            .collect();
        let crowded: Vec<(&str, &str)> = crowded
            .iter()
            .map(|(cwd, title)| (cwd.as_str(), title.as_str()))
            .collect();
        let mut windows: Vec<WindowLayout> = (0..=MAX_WINDOWS)
            .map(|i| window(format!("/w{i}")))
            .collect();
        windows.insert(1, layout(&crowded).windows.remove(0));
        let manifest = RestoreManifest::new(windows);
        assert_eq!(manifest.windows.len(), MAX_WINDOWS + 2, "PRECONDITION");
        // The log names each cause apart: the crowded window, and the one
        // after the 64th.
        let admitted = AdmittedWindows::of(manifest.windows.clone());
        assert_eq!(
            admitted.left_out_line(manifest.windows.len()).as_deref(),
            Some(
                "2 of 66 saved windows not reopened: 1 past the 64-window limit, 1 in a shape \
                 this build cannot rebuild"
            )
        );
        let only_past = AdmittedWindows {
            kept: Vec::new(),
            past_cap: 3,
            unbuildable: 0,
        };
        assert_eq!(
            only_past.left_out_line(67).as_deref(),
            Some("3 of 67 saved windows not reopened: 3 past the 64-window limit"),
            "one cause, no hedge"
        );
        let read = RestoreManifest::from_toml(&manifest.to_toml().unwrap())
            .expect("the windows that fit come back");
        assert_eq!(read.windows.len(), MAX_WINDOWS);
        let firsts: Vec<Option<String>> = read.windows.iter().map(|w| w.bootstrap_cwd()).collect();
        let expected: Vec<Option<String>> =
            (0..MAX_WINDOWS).map(|i| Some(format!("/w{i}"))).collect();
        assert_eq!(firsts, expected, "in order, the crowded window left out");

        let only_crowded = RestoreManifest::new(layout(&crowded).windows);
        assert!(
            RestoreManifest::from_toml(&only_crowded.to_toml().unwrap()).is_none(),
            "nothing reads: refused, as before"
        );
    }

    /// A window of one terminal tab per shell in `shells`, in both projections
    /// (the canonical tree and the legacy terminal mirror), as the handoff
    /// capture writes it.
    fn shells_window(shells: std::ops::Range<u64>) -> WindowLayout {
        WindowLayout {
            rows: 30,
            cols: 100,
            active_tab: 0,
            outer_x: Some(40),
            outer_y: Some(60),
            maximized: None,
            inner_px: Some([1000, 640]),
            presence_row: true,
            show: WindowShow::UNKNOWN,
            tabs: shells
                .clone()
                .map(|shell| PaneLayout::Leaf {
                    cwd: Some("/tmp/work".to_string()),
                    title: "zsh".to_string(),
                    focused: true,
                    local_id: Some(shell),
                })
                .collect(),
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: Some(0),
            restored_tabs: shells
                .map(|shell| RestoredTab {
                    root: terminal(Some(shell), "zsh"),
                    focused_path: Vec::new(),
                    zoomed: false,
                })
                .collect(),
        }
    }

    /// The shells each window's tabs name, in order.
    fn shells_of(manifest: &RestoreManifest) -> Vec<Vec<u64>> {
        manifest
            .windows
            .iter()
            .map(|window| {
                window
                    .restored_tabs
                    .iter()
                    .filter_map(|tab| tab.root.first_terminal_leaf()?.local_id)
                    .collect()
            })
            .collect()
    }

    /// What every fold must keep: the layout is a fixed point of the parse
    /// (the update worker's round trip), within the caps, and names every
    /// handed shell — nothing is left to the orphan net.
    fn assert_folded_whole(folded: &RestoreManifest, shells: u64) {
        assert!(folded.admits_whole(), "within the caps");
        assert_eq!(
            RestoreManifest::from_toml(&folded.to_toml().unwrap()).as_ref(),
            Some(folded),
            "the producer's round trip holds"
        );
        let handed = (0..shells).collect::<Vec<_>>();
        assert!(
            folded.clone().placed_for_handed(&handed).is_ok(),
            "every handed shell is placed"
        );
    }

    /// D5 (round eight of the update audit, #505): A DESK WITHIN THE CAPS IS
    /// NOT FOLDED — exactly at them (64 windows, one of 256 tabs), the layout
    /// and its wire are what they were, and no `fold` table is written.
    #[test]
    fn a_desk_within_the_caps_is_left_byte_identical() {
        let mut windows = vec![shells_window(0..256)];
        windows.extend(
            (256..256 + MAX_WINDOWS as u64 - 1).map(|shell| shells_window(shell..shell + 1)),
        );
        let before = RestoreManifest::new(windows);
        assert_eq!(
            before.windows.len(),
            MAX_WINDOWS,
            "PRECONDITION: at the window cap"
        );
        let mut folded = before.clone();
        folded.fold_into_caps();
        assert_eq!(folded, before);
        let wire = folded.to_toml().unwrap();
        assert_eq!(
            wire,
            before.to_toml().unwrap(),
            "byte-identical on the wire"
        );
        assert!(!wire.contains("fold"), "no fold table: {}", &wire[..200]);
        assert!(folded.admits_whole());
    }

    /// D5: THE WINDOWS PAST THE CAP ARE TABS OF THE LAST WINDOW THAT FITS,
    /// in order after its own; when that window overflows its own cap, the
    /// tabs past it go to the NEXT window with room — after it there is none,
    /// so the first. Every window the fold changed drops its legacy mirror,
    /// which would name a moved shell in two windows; every other window is
    /// exactly as captured.
    #[test]
    fn the_windows_past_the_cap_fold_into_the_last_window_that_fits() {
        let mut windows = (0..MAX_WINDOWS as u64 - 1)
            .map(|shell| shells_window(shell..shell + 1))
            .collect::<Vec<_>>();
        windows.push(shells_window(63..313)); // the last that fits: 250 tabs
        windows.push(shells_window(313..318)); // past the cap: 5 tabs…
        windows.push(shells_window(318..323)); // …and 5 more
        let before = RestoreManifest::new(windows);
        let mut folded = before.clone();
        folded.fold_into_caps();

        assert_eq!(
            folded.fold,
            LayoutFold {
                windows: 2,
                tabs: 4,
                front: (0..=63).chain([313, 318]).collect(),
            },
            "every captured window's front pane, before a tab moved"
        );
        let shells = shells_of(&folded);
        assert_eq!(shells.len(), MAX_WINDOWS);
        assert_eq!(
            shells[MAX_WINDOWS - 1],
            (63..319).collect::<Vec<_>>(),
            "its own 250, then the folded windows' tabs up to the cap"
        );
        assert_eq!(
            shells[0],
            vec![0, 319, 320, 321, 322],
            "the 4 past the last window's cap: into the next window with room"
        );
        assert!(
            folded.windows[MAX_WINDOWS - 1].tabs.is_empty(),
            "the last window lost tabs: no legacy mirror naming a moved shell"
        );
        assert_eq!(
            folded.windows[0].tabs, before.windows[0].tabs,
            "the first only received tabs: its mirror names its own, as captured"
        );
        for window in [0, MAX_WINDOWS - 1] {
            assert_eq!(
                folded.windows[window].active_item,
                Some(0),
                "its own front tab"
            );
        }
        for window in 1..MAX_WINDOWS - 1 {
            assert_eq!(
                folded.windows[window], before.windows[window],
                "window {window}, untouched, is as captured"
            );
        }
        assert_folded_whole(&folded, 323);
    }

    /// D5: A WINDOW PAST THE TAB CAP SPILLS INTO THE NEXT WINDOW WITH ROOM,
    /// skipping a full one; its front tab, when it moves to a window that
    /// has its own, leaves the donor showing its last remaining tab.
    #[test]
    fn a_window_past_the_tab_cap_spills_into_the_next_window_with_room() {
        let mut crowded = shells_window(0..300);
        crowded.active_item = Some(280);
        let before = RestoreManifest::new(vec![
            crowded,
            shells_window(300..556), // full
            shells_window(556..566),
        ]);
        let mut folded = before.clone();
        folded.fold_into_caps();
        assert_eq!(
            folded.fold,
            LayoutFold {
                windows: 0,
                tabs: 44,
                front: vec![280, 300, 556],
            }
        );
        let shells = shells_of(&folded);
        assert_eq!(shells[0], (0..256).collect::<Vec<_>>());
        assert_eq!(shells[1], (300..556).collect::<Vec<_>>(), "full: skipped");
        assert_eq!(
            shells[2],
            (556..566).chain(256..300).collect::<Vec<_>>(),
            "its own, then the spilled tabs in order"
        );
        assert_eq!(folded.windows[0].active_item, Some(255));
        assert_eq!(
            folded.windows[2].active_item,
            Some(0),
            "the host's own front tab"
        );
        assert!(folded.windows[0].tabs.is_empty(), "the donor: no mirror");
        assert_eq!(
            folded.windows[2].tabs, before.windows[2].tabs,
            "the host: as captured"
        );
        assert_eq!(folded.windows[1], before.windows[1]);
        assert_folded_whole(&folded, 566);
    }

    /// D5: A LONE WINDOW PAST THE TAB CAP SPILLS INTO WINDOWS MADE FOR IT —
    /// at its grid and size, with its presence row, placed by the window
    /// system — the first of which shows its front tab, which moved there.
    #[test]
    fn a_lone_window_past_the_tab_cap_spills_into_windows_made_for_it() {
        let mut crowded = shells_window(0..600);
        crowded.active_item = Some(300);
        let mut folded = RestoreManifest::new(vec![crowded]);
        folded.fold_into_caps();
        assert_eq!(
            folded.fold,
            LayoutFold {
                windows: 0,
                tabs: 344,
                front: vec![300],
            }
        );
        assert_eq!(
            shells_of(&folded),
            vec![
                (0..256).collect::<Vec<_>>(),
                (256..512).collect(),
                (512..600).collect()
            ]
        );
        assert_eq!(
            folded
                .windows
                .iter()
                .map(|window| window.active_item)
                .collect::<Vec<_>>(),
            vec![Some(255), Some(44), Some(0)],
            "the front tab (300) is the first made window's 45th"
        );
        for made in &folded.windows[1..] {
            assert_eq!(
                (made.rows, made.cols, made.inner_px, made.presence_row),
                (30, 100, Some([1000, 640]), true)
            );
            assert_eq!((made.outer_x, made.outer_y), (None, None));
        }
        assert_folded_whole(&folded, 600);
    }

    /// D5's limit: a desk past what any layout holds — every one of 64
    /// windows full and a tab to spare, or a tab past its split depth — is
    /// left as it is by the fold and not admitted whole, which the park
    /// refuses typed (`app_update_handoff::commit_handoff_layout`).
    #[test]
    fn a_desk_past_what_any_layout_holds_is_not_admitted_whole() {
        let mut windows = (0..MAX_WINDOWS as u64)
            .map(|window| {
                let first = window * MAX_TABS_PER_WINDOW as u64;
                shells_window(first..first + MAX_TABS_PER_WINDOW as u64)
            })
            .collect::<Vec<_>>();
        let spare = windows[1].restored_tabs[0].clone();
        windows[0].restored_tabs.push(spare);
        let mut full = RestoreManifest::new(windows);
        full.fold_into_caps();
        assert_eq!(
            full.windows[0].restored_tabs.len(),
            MAX_TABS_PER_WINDOW + 1,
            "nowhere to go"
        );
        assert!(!full.admits_whole());
        assert_eq!(
            full.why_not_whole().as_deref(),
            Some(
                "16385 tabs are open, more than the 16384 (64 windows of 256) an update can carry"
            ),
            "the refusal names the cap and where"
        );
        assert!(full.fold.is_none(), "nothing moved, nothing recorded");

        let mut root = terminal(Some(0), "zsh");
        for _ in 0..=MAX_SPLIT_DEPTH {
            root = RestoredSplitTree::Split {
                axis: SplitKind::Vertical,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(terminal(None, "deep")),
            };
        }
        let mut deep = shells_window(0..1);
        deep.restored_tabs[0].root = root;
        // Focused on a leaf at the top: the depth alone is past the cap.
        deep.restored_tabs[0].focused_path = vec![RestoreBranch::Second];
        let mut deep = RestoreManifest::new(vec![deep]);
        let before = deep.clone();
        deep.fold_into_caps();
        assert_eq!(
            deep, before,
            "the fold moves tabs whole; it does not split a tab"
        );
        assert!(!deep.admits_whole());
        assert_eq!(
            deep.why_not_whole().as_deref(),
            Some("tab 1 of window 1 is split 33 levels deep, more than the 31 an update can carry")
        );
    }

    /// THE MIRROR RULE, ONE FOR THE READ AND THE WRITE (the D5 review). The
    /// fold moves tabs whole, so a window past the cap with Settings open, or
    /// the same document, puts a second one into the last window; its
    /// successor reopens both, and its next capture wrote both into the
    /// window's legacy mirror — which every build's parse refuses, so the next
    /// update refused its own layout ("past what one handoff layout holds",
    /// which it was not) and the quit layout and crash journals left the window
    /// out. The capture now leaves out of the mirror what
    /// `NativeTabRestore::joins_mirror` refuses, the very rule the parse reads
    /// the mirror by; and a window the parse refuses for anything but a cap is
    /// named as that, not as a cap.
    #[test]
    fn the_legacy_mirror_is_written_by_the_rule_the_parse_reads_it_by() {
        let settings = |route: &str| NativeTabRestore::Settings {
            route: route.to_string(),
        };
        let markdown = |uri: &str| NativeTabRestore::Markdown {
            uri: uri.to_string(),
        };
        let editor = |uri: &str| NativeTabRestore::Editor {
            uri: uri.to_string(),
        };
        let live = [
            settings("/about"),
            markdown("file:///tmp/notes.md"),
            settings("/updates"),
            markdown("file:///tmp/notes.md"),
            editor("file:///tmp/notes.md"),
            markdown("file:///tmp/other.md"),
        ];
        let mut mirror = Vec::new();
        for descriptor in live {
            if descriptor.joins_mirror(&mirror) {
                mirror.push(descriptor);
            }
        }
        assert_eq!(
            mirror,
            vec![
                settings("/about"),
                markdown("file:///tmp/notes.md"),
                editor("file:///tmp/notes.md"),
                markdown("file:///tmp/other.md"),
            ],
            "a second Settings and a second equal descriptor stay out; another view of the \
             same file does not"
        );

        // The parse admits the mirror the rule wrote, and refuses the one it
        // would not have: a window whose canonical tabs are whole either way.
        let window_with = |native_tabs: Vec<NativeTabRestore>| {
            let mut window = shells_window(0..1);
            window.tab_order = std::iter::once(TabOrderEntry::Terminal { index: 0 })
                .chain((0..native_tabs.len()).map(|index| TabOrderEntry::Native { index }))
                .collect();
            window.native_tabs = native_tabs;
            window.restored_tabs.extend(
                std::iter::repeat_with(|| RestoredTab {
                    root: RestoredSplitTree::leaf(RestoredView::Native(
                        NativeLeafRestore::settings("/about".to_string()),
                    )),
                    focused_path: Vec::new(),
                    zoomed: false,
                })
                .take(2),
            );
            RestoreManifest::new(vec![shells_window(1..2), window])
        };
        let written = window_with(mirror);
        assert!(
            written.admits_whole(),
            "two Settings leaves, one in the mirror"
        );
        assert_eq!(
            RestoreManifest::from_toml(&written.to_toml().unwrap()).as_ref(),
            Some(&written)
        );
        for doubled in [
            vec![settings("/about"), settings("/updates")],
            vec![
                markdown("file:///tmp/notes.md"),
                markdown("file:///tmp/notes.md"),
            ],
        ] {
            let refused = window_with(doubled);
            assert_eq!(
                refused.why_not_whole().as_deref(),
                Some("window 2 of the handoff layout is in a shape this build does not read back"),
                "named as the window it is, never as a cap"
            );
            let read = RestoreManifest::from_toml(&refused.to_toml().unwrap()).expect("one reads");
            assert_eq!(read.windows.len(), 1, "the parse leaves that window out");
        }
    }

    /// THE PANES IN FRONT OF A FOLDED LAYOUT ARE THE OUTGOING BUILD'S (the D5
    /// review, P3). The seamless adoption trusts a session record's title
    /// copy only for a pane that was in front, the one the outgoing build kept
    /// current (U47). The fold changes what each window shows: a window past
    /// the cap becomes tabs behind the last window's front tab, and a donor
    /// whose front tab moved shows its last remaining one. Read off the
    /// folded windows, the merged window's front pane was missing (a missed
    /// park read then installed an empty title over the record's current one)
    /// and the donor's new front pane was named (its record copy is the stale
    /// one U47 refuses). The fold records the panes in front before a tab
    /// moved, and a folded layout answers by them; the record survives the
    /// round trip, and a parse keeps no more of it than a layout has tabs.
    #[test]
    fn a_folded_layout_names_the_panes_that_were_in_front_before_the_fold() {
        // Windows past the cap: their front panes (313, 318) are behind the
        // last window's own front tab once folded.
        let mut windows = (0..MAX_WINDOWS as u64 - 1)
            .map(|shell| shells_window(shell..shell + 1))
            .collect::<Vec<_>>();
        windows.push(shells_window(63..313));
        windows.push(shells_window(313..318));
        windows.push(shells_window(318..323));
        let mut merged = RestoreManifest::new(windows);
        let before = merged.front_terminal_ids();
        merged.fold_into_caps();
        assert!(
            !merged.windows_front_terminal_ids().contains(&313),
            "PRECONDITION: read off the folded windows, a merged window's front pane is missing"
        );
        assert_eq!(merged.front_terminal_ids(), before);
        assert!(merged.front_terminal_ids().contains(&313));
        assert!(merged.front_terminal_ids().contains(&318));
        assert_folded_whole(&merged, 323);

        // A donor whose front tab moved: it shows its last remaining tab
        // (shell 255), which was behind; the moved front pane (280) was not.
        let mut crowded = shells_window(0..300);
        crowded.active_item = Some(280);
        let mut donor = RestoreManifest::new(vec![
            crowded,
            shells_window(300..556),
            shells_window(556..566),
        ]);
        donor.fold_into_caps();
        assert!(
            donor.windows_front_terminal_ids().contains(&255),
            "PRECONDITION: read off the folded windows, the donor's new front pane is named"
        );
        assert_eq!(donor.front_terminal_ids(), vec![280, 300, 556]);
        assert_folded_whole(&donor, 566);

        // A layout that folded nothing answers off its windows, as before.
        let within = RestoreManifest::new(vec![shells_window(0..3)]);
        assert_eq!(within.front_terminal_ids(), vec![0]);

        // A parse keeps at most one front pane per tab a layout holds.
        let mut hostile = donor;
        hostile.fold.front = vec![7; MAX_FOLD_FRONT + 5];
        let read = RestoreManifest::from_toml(&hostile.to_toml().unwrap()).expect("reads");
        assert_eq!(read.fold.front.len(), MAX_FOLD_FRONT);
    }

    #[test]
    fn malformed_native_metadata_and_aliasing_order_fail_closed() {
        let mut malformed_uri = mixed_sample("https://example.invalid/not-local.md");
        let toml = aterm_toml::to_string(&malformed_uri).unwrap();
        assert!(RestoreManifest::from_toml(&toml).is_none());

        malformed_uri.windows[0].native_tabs[1] = NativeTabRestore::Editor {
            uri: "file:///tmp/readme.md".to_string(),
        };
        malformed_uri.windows[0].tab_order = vec![
            TabOrderEntry::Native { index: 0 },
            TabOrderEntry::Terminal { index: 0 },
            TabOrderEntry::Native { index: 0 },
        ];
        let toml = aterm_toml::to_string(&malformed_uri).unwrap();
        assert!(
            RestoreManifest::from_toml(&toml).is_none(),
            "duplicate descriptor indices may not alias one live tab"
        );

        let mut duplicate_settings = mixed_sample("file:///tmp/readme.md");
        duplicate_settings.windows[0].native_tabs[1] = NativeTabRestore::Settings {
            route: "/updates".to_string(),
        };
        assert!(
            RestoreManifest::from_toml(&duplicate_settings.to_toml().unwrap()).is_none(),
            "one window cannot restore two aliases of its Settings singleton"
        );

        let missing_uri = r#"
schema = 1
[[windows]]
rows = 24
cols = 80
active_tab = 0
[[windows.native_tabs]]
kind = "editor"
[[windows.tab_order]]
kind = "native"
index = 0
active_item = 0
"#;
        assert!(RestoreManifest::from_toml(missing_uri).is_none());
    }

    #[test]
    fn v2_unknown_and_corrupt_native_leaves_become_in_place_placeholders() {
        let input = r#"
schema = 2

[[windows]]
rows = 30
cols = 100
active_item = 0

[[windows.restored_tabs]]
focused_path = ["second"]

[windows.restored_tabs.root]
node = "split"
axis = "vertical"
ratio = 0.61

[windows.restored_tabs.root.first]
node = "leaf"
[windows.restored_tabs.root.first.view]
kind = "terminal"
cwd = "/tmp"
title = "healthy shell"

[windows.restored_tabs.root.second]
node = "leaf"
[windows.restored_tabs.root.second.view]
kind = "native"
restore_tag = "future.diagram"
metadata = "opaque=copy-me"
"#;
        let manifest = RestoreManifest::from_toml(input).expect("v2 manifest remains usable");
        let tab = &manifest.windows[0].restored_tabs[0];
        let RestoredSplitTree::Split { first, second, .. } = &tab.root else {
            panic!("split topology was not preserved");
        };
        assert!(matches!(
            &**first,
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(TerminalLeafRestore { title, .. })
            } if title == "healthy shell"
        ));
        assert!(matches!(
            &**second,
            RestoredSplitTree::Leaf {
                view: RestoredView::Placeholder(PlaceholderLeafRestore {
                    restore_tag,
                    reason,
                    metadata,
                    ..
                })
            } if restore_tag == "future.diagram"
                && reason.contains("unavailable")
                && metadata.contains("copy-me")
        ));

        let corrupt_known = input.replace("future.diagram", "editor").replace(
            "metadata = \"opaque=copy-me\"",
            "uri = \"https://invalid.example\"",
        );
        let recovered = RestoreManifest::from_toml(&corrupt_known)
            .expect("one corrupt known leaf must not discard its sibling");
        let RestoredSplitTree::Split { second, .. } = &recovered.windows[0].restored_tabs[0].root
        else {
            unreachable!()
        };
        assert!(matches!(
            &**second,
            RestoredSplitTree::Leaf {
                view: RestoredView::Placeholder(PlaceholderLeafRestore { reason, .. })
            } if reason == "invalid document URI"
        ));
    }

    #[test]
    fn v2_sanitizes_geometry_but_rejects_resource_exhaustion_shapes() {
        let mut manifest = mixed_sample("file:///tmp/readme.md");
        let mut tab = RestoredTab::from_legacy_native(&NativeTabRestore::Editor {
            uri: "file:///tmp/readme.md".to_string(),
        })
        .unwrap();
        tab.root = RestoredSplitTree::Split {
            axis: SplitKind::Horizontal,
            ratio: f32::NAN,
            first: Box::new(tab.root.clone()),
            second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                TerminalLeafRestore {
                    cwd: None,
                    title: "shell".to_string(),
                    profile: None,
                    local_id: None,
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                    questions: None,
                    identity: None,
                    agent: None,
                    held: false,
                },
            ))),
        };
        tab.focused_path = vec![RestoreBranch::First];
        manifest.windows[0].restored_tabs = vec![tab];
        manifest.windows[0].active_item = Some(0);
        let encoded = aterm_toml::to_string(&manifest).unwrap();
        let decoded = RestoreManifest::from_toml(&encoded).expect("NaN ratio is recoverable");
        assert!(matches!(
            decoded.windows[0].restored_tabs[0].root,
            RestoredSplitTree::Split { ratio, .. } if ratio == 0.5
        ));

        let mut root = RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
            cwd: None,
            title: String::new(),
            profile: None,
            local_id: None,
            user_title: None,
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            identity: None,
            agent: None,
            held: false,
        }));
        for _ in 0..=MAX_SPLIT_DEPTH {
            root = RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(RestoredSplitTree::leaf(RestoredView::Terminal(
                    TerminalLeafRestore {
                        cwd: None,
                        title: String::new(),
                        profile: None,
                        local_id: None,
                        user_title: None,
                        description: None,
                        icon: None,
                        role: None,
                        attention: None,
                        questions: None,
                        identity: None,
                        agent: None,
                        held: false,
                    },
                ))),
            };
        }
        manifest.windows[0].restored_tabs = vec![RestoredTab {
            root,
            focused_path: Vec::new(),
            zoomed: false,
        }];
        assert!(
            RestoreManifest::from_toml(&aterm_toml::to_string(&manifest).unwrap()).is_none(),
            "adversarial depth must fail before runtime allocation"
        );
    }

    /// A NUL IN A PANE'S DIRECTORY COSTS THE DIRECTORY, NEVER THE LAYOUT (the
    /// 2026-09-22/23 update audit, plan P0-1f). A program can report a working
    /// directory holding a NUL (OSC 7 `file:///tmp/a%00b`), and the capture
    /// copies the engine's cwd into the pane's leaf. `bounded()` refuses a NUL,
    /// so `from_toml` answered `None` for the WHOLE manifest — and the update
    /// worker's layout round trip filed that as a Structural preparation
    /// failure, which latches. The leaf's sanitize now drops the directory (and
    /// strips NULs from the title) on BOTH sides: the capture, through
    /// `RestoreManifest::new`, and the parse, so an older writer's layout is
    /// adopted too.
    #[test]
    fn a_nul_cwd_leaf_round_trips_sanitized() {
        let hostile = || WindowLayout {
            rows: 24,
            cols: 80,
            active_tab: 0,
            outer_x: None,
            outer_y: None,
            maximized: None,
            inner_px: None,
            presence_row: false,
            show: WindowShow::UNKNOWN,
            tabs: vec![PaneLayout::Leaf {
                cwd: Some("/tmp/a\0b".into()),
                title: "zsh\0 ~".into(),
                focused: true,
                local_id: Some(7),
            }],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: None,
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    cwd: Some("/tmp/a\0b".into()),
                    title: "zsh\0 ~".into(),
                    profile: None,
                    local_id: Some(7),
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                    identity: None,
                    questions: None,
                    agent: None,
                    held: false,
                })),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        };
        let assert_clean = |manifest: &RestoreManifest, what: &str| {
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } = &manifest.windows[0].restored_tabs[0].root
            else {
                panic!("{what}: the tab is still a terminal leaf");
            };
            assert_eq!(terminal.cwd, None, "{what}: the NUL directory is dropped");
            assert_eq!(
                terminal.title, "zsh ~",
                "{what}: the title loses only its NUL"
            );
            assert_eq!(
                terminal.local_id,
                Some(7),
                "{what}: the adoption id survives"
            );
            let PaneLayout::Leaf { cwd, title, .. } = &manifest.windows[0].tabs[0] else {
                panic!("{what}: the legacy mirror is still a leaf");
            };
            assert_eq!(cwd, &None, "{what}: the legacy mirror drops it too");
            assert_eq!(title, "zsh ~", "{what}: and strips the title the same way");
        };

        // THE PARSE: an older writer's raw layout, serialized without passing
        // through the sanitizing constructor.
        let raw = RestoreManifest {
            schema: SCHEMA,
            fold: LayoutFold::default(),
            windows: vec![hostile()],
        };
        let wire = aterm_toml::to_string(&raw).expect("serialize the hostile fixture");
        let parsed = RestoreManifest::from_toml(&wire).expect("a NUL directory is recoverable");
        assert_clean(&parsed, "parse");

        // THE CAPTURE: what `capture_restore_manifest` builds, and the update
        // worker's exact check — `from_toml(to_toml(layout)) == layout`.
        let captured = RestoreManifest::new(vec![hostile()]);
        assert_clean(&captured, "capture");
        let round_trip = captured
            .to_toml()
            .ok()
            .and_then(|wire| RestoreManifest::from_toml(&wire));
        assert_eq!(
            round_trip.as_ref(),
            Some(&captured),
            "the worker's layout round trip holds on a sanitized capture"
        );
    }

    /// AUDIT #8 — A LEAF A PROGRAM HELD (ssh) is written `held = true`, a
    /// leaf that was not is written as it always was (so an older writer's
    /// layout and today's unheld one are the same bytes), and the bootstrap
    /// session does not ask for a folder a held pane reported that is not
    /// here. NEGATIVE CONTROLS: the same folder from an unheld pane is asked
    /// for (its spawn falls back and says so), a held pane's folder that is
    /// here is kept, and a pane the agent aterm hosted held ran on this
    /// machine, so its folder is asked for.
    #[test]
    fn a_held_leaf_round_trips_and_its_folder_that_is_not_here_is_not_asked_for() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-restore-remote")
            .tempdir()
            .expect("scratch dir");
        let here = scratch
            .path()
            .to_str()
            .expect("utf-8 scratch path")
            .to_owned();
        let remote = format!("{here}/on-the-remote-host/proj");
        let window = |cwd: &str, held: bool| {
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(mut leaf),
            } = terminal(Some(1), "ssh")
            else {
                unreachable!("a terminal leaf");
            };
            leaf.cwd = Some(cwd.to_owned());
            leaf.held = held;
            RestoreManifest::new(vec![WindowLayout {
                rows: 24,
                cols: 80,
                active_tab: 0,
                outer_x: None,
                outer_y: None,
                maximized: None,
                inner_px: None,
                presence_row: false,
                show: WindowShow::UNKNOWN,
                tabs: Vec::new(),
                native_tabs: Vec::new(),
                tab_order: Vec::new(),
                active_item: None,
                restored_tabs: vec![RestoredTab {
                    root: RestoredSplitTree::leaf(RestoredView::Terminal(leaf)),
                    focused_path: Vec::new(),
                    zoomed: false,
                }],
            }])
        };
        let wire = |manifest: &RestoreManifest| manifest.to_toml().expect("serializes");

        let unheld = window(&remote, false);
        assert!(!wire(&unheld).contains("held ="), "{}", wire(&unheld));
        let held = window(&remote, true);
        assert!(wire(&held).contains("held = true"), "{}", wire(&held));
        let back = RestoreManifest::from_toml(&wire(&held)).expect("parses");
        assert_eq!(back, held, "held survives the wire");

        assert_eq!(back.first_leaf_cwd(), None, "the held pane's remote folder");
        assert_eq!(
            unheld.first_leaf_cwd().as_deref(),
            Some(remote.as_str()),
            "NEGATIVE CONTROL: the shell's own folder"
        );
        assert_eq!(
            window(&here, true).first_leaf_cwd().as_deref(),
            Some(here.as_str()),
            "NEGATIVE CONTROL: a held pane's folder that is here"
        );
        let mut hosted = window(&remote, true);
        if let RestoredSplitTree::Leaf {
            view: RestoredView::Terminal(leaf),
        } = &mut hosted.windows[0].restored_tabs[0].root
        {
            leaf.agent = Some(Box::new(AgentRestore {
                pid: 4242,
                start: "Sat Sep 27 01:02:03 2026".into(),
                program: "/opt/claude/bin/claude".into(),
                argv: vec!["/opt/claude/bin/claude".into()],
                session: None,
                cwd: remote.clone(),
                version: None,
                home: None,
                login: None,
                codex: None,
            }));
        }
        assert_eq!(
            hosted.first_leaf_cwd().as_deref(),
            Some(remote.as_str()),
            "the agent aterm hosted ran on this machine: its gone folder is asked for, and \
             the spawn says it was not found"
        );
    }

    /// FAIL-SAFE: corrupt TOML, a wrong schema, and empty input all yield `None` (start
    /// fresh) rather than a partial/misparsed layout.
    #[test]
    fn parse_is_fail_safe() {
        assert!(RestoreManifest::from_toml("this is not toml {{{").is_none());
        assert!(RestoreManifest::from_toml("").is_none());
        // Valid TOML, wrong schema → ignored.
        let mut m = sample();
        m.schema = 999;
        let toml = aterm_toml::to_string(&m).unwrap();
        assert!(
            RestoreManifest::from_toml(&toml).is_none(),
            "stale schema is ignored"
        );
    }

    /// `write_to` then `take_from` round-trips through a real file, and the read is
    /// SINGLE-USE — the manifest is deleted, so a second take yields `None`.
    #[test]
    fn write_then_take_is_single_use() {
        let dir = std::env::temp_dir().join(format!("aterm-restore-test-{}", std::process::id()));
        let path = dir.join("session.toml");
        let m = sample();
        write_to(&path, &m).expect("write");
        assert!(path.exists(), "manifest written");
        let back = take_from(&path).expect("take");
        assert_eq!(m, back);
        assert!(!path.exists(), "single-use: manifest deleted on read");
        assert!(take_from(&path).is_none(), "a second take finds nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A one-shot name — the handoff's attempt-bound layout — keeps nothing but the
    /// manifest: its lock goes with the write (they used to pile up in the control
    /// directory, one per update), while a shared name keeps the lock every writer
    /// and claimer of it serializes on. The bytes are exactly `to_toml()`, which is
    /// what the handoff's layout digest commits to.
    #[test]
    fn a_one_shot_write_leaves_only_its_manifest() {
        let dir =
            std::env::temp_dir().join(format!("aterm-restore-once-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let once = dir.join("seamless-4242-abcd.layout.toml");
        let manifest = sample();
        write_once_to(&once, &manifest).unwrap();
        let names = |dir: &Path| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(names(&dir), ["seamless-4242-abcd.layout.toml"]);
        assert_eq!(
            std::fs::read_to_string(&once).unwrap(),
            manifest.to_toml().unwrap()
        );
        let shared = dir.join("session.toml");
        write_to(&shared, &manifest).unwrap();
        assert_eq!(
            names(&dir),
            [
                ".session.toml.aterm-restore.lock",
                "seamless-4242-abcd.layout.toml",
                "session.toml"
            ],
            "a shared name keeps its lock"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A write temporary or a claim that a killed writer or claimer left beside
    /// a name, each a whole layout, is swept under that name's lock once its pid
    /// is gone. A live pid's is left, as is another name's, and a link is never
    /// followed.
    #[cfg(unix)]
    #[test]
    fn a_dead_pids_restore_temporaries_go_under_the_lock() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let elsewhere = aterm_tempfile::tempdir().expect("scratch dir");
        let target = elsewhere.path().join("layout.toml");
        std::fs::write(&target, "not ours").unwrap();
        let (dead, live) = (
            crate::crash_journal::tests::reaped_pid(),
            std::os::unix::process::parent_id(),
        );
        let swept = [
            format!(".session.toml.aterm-write-{dead}-1-0.tmp"),
            format!(".session.toml.aterm-claimed-{dead}-1-0.tmp"),
        ];
        let left = [
            format!(".session.toml.aterm-write-{live}-1-0.tmp"),
            format!(".cell-metrics.toml.aterm-write-{dead}-1-0.tmp"),
        ];
        for name in swept.iter().chain(&left) {
            std::fs::write(dir.path().join(name), "a layout").unwrap();
        }
        let link = dir
            .path()
            .join(format!(".session.toml.aterm-claimed-{dead}-2-0.tmp"));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let path = dir.path().join("session.toml");
        write_to(&path, &sample()).expect("write");
        for name in &swept {
            assert!(!dir.path().join(name).exists(), "{name} is swept");
        }
        for name in &left {
            assert!(dir.path().join(name).exists(), "{name} is left");
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "not ours");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "only regular files are swept, never a link"
        );
        assert_eq!(
            take_from(&path),
            Some(sample()),
            "the layout itself is whole"
        );
    }

    #[test]
    fn unique_durable_write_replaces_existing_without_touching_fixed_temp_alias() {
        let dir =
            std::env::temp_dir().join(format!("aterm-restore-replace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        let fixed = path.with_extension("toml.tmp");
        std::fs::write(&fixed, "unrelated").unwrap();
        std::fs::write(&path, "stale").unwrap();

        let manifest = sample();
        write_to(&path, &manifest).unwrap();
        assert_eq!(
            RestoreManifest::from_toml(&std::fs::read_to_string(&path).unwrap()),
            Some(manifest)
        );
        assert_eq!(std::fs::read_to_string(&fixed).unwrap(), "unrelated");
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.contains("aterm-write")
        }));

        // Tier-1 projection of the genuine locked unique-temp publication.
        let model = aterm_spec::derive::restore_manifest_single_use_model();
        let locked = model.successors("LockWriter", &model.init_state())[0].clone();
        let temporary = model.successors("CreateUniqueTemporary", &locked)[0].clone();
        let published = model.successors("PublishManifest", &temporary)[0].clone();
        assert_eq!(published["visible"], 1);
        assert!(model.check_invariant("UniqueTemporaryNeverAliases", &published));
        // The fixed alias is no step of the healthy writer; it is the mutant's.
        assert!(model.successors("ReuseFixedTemporary", &locked).is_empty());
        let alias = aterm_spec::interp::with_buggy(&model, 1)
            .successors("ReuseFixedTemporary", &locked)[0]
            .clone();
        assert!(!model.check_invariant("UniqueTemporaryNeverAliases", &alias));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_takers_have_exactly_one_committed_consumer() {
        let dir =
            std::env::temp_dir().join(format!("aterm-restore-consume-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("session.toml");
        let manifest = sample();
        write_to(&path, &manifest).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut takers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = barrier.clone();
            takers.push(std::thread::spawn(move || {
                barrier.wait();
                take_from(&path)
            }));
        }
        barrier.wait();
        let results = takers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
        assert!(results.into_iter().flatten().all(|value| value == manifest));
        assert!(!path.exists());
        assert!(take_from(&path).is_none());

        // Tier-1 projection of the one real winner and absent loser. Returning
        // before SyncClaim is the explicit negative control.
        let model = aterm_spec::derive::restore_manifest_single_use_model();
        let locked = model.successors("LockTakeA", &model.init_state())[0].clone();
        let claimed = model.successors("ClaimA", &locked)[0].clone();
        assert!(model.successors("ReturnA", &claimed).is_empty());
        let synced = model.successors("SyncClaim", &claimed)[0].clone();
        let returned = model.successors("ReturnA", &synced)[0].clone();
        let loser_locked = model.successors("LockTakeB", &returned)[0].clone();
        let absent = model.successors("ObserveAbsentB", &loser_locked)[0].clone();
        assert_eq!(absent["returned"], 1);
        assert!(model.check_invariant("AtMostOneConsumer", &absent));
        assert!(model.check_invariant("ReturnOnlyAfterDurableClaim", &absent));

        // Negative control: the take 3473ced57 replaced, replayed on real files
        // in its losing interleaving — no lock; A reads, B reads while the name
        // survives A's read, A removes, B's remove fails (ignored, as it was),
        // and both return the manifest. Each step is the mutant's.
        write_to(&path, &manifest).unwrap();
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let initial = buggy.init_state();
        let read_a = std::fs::read_to_string(&path).ok();
        let a_read = buggy.successors("HistoricalTakeA", &initial)[0].clone();
        assert!(path.exists(), "the name survives the read");
        assert_eq!(a_read["visible"], 1);
        assert_eq!(aterm_spec::interp::admits(&model, &initial, &a_read), None);
        assert!(!model.check_invariant("ClaimRemovesVisibleName", &a_read));
        let read_b = std::fs::read_to_string(&path).ok();
        let both_read = buggy.successors("HistoricalTakeB", &a_read)[0].clone();
        assert!(std::fs::remove_file(&path).is_ok());
        let a_returned = buggy.successors("HistoricalTakeA", &both_read)[0].clone();
        assert!(std::fs::remove_file(&path).is_err());
        let b_returned = buggy.successors("HistoricalTakeB", &a_returned)[0].clone();
        let consumed = [read_a, read_b]
            .into_iter()
            .filter_map(|text| RestoreManifest::from_toml(&text?))
            .collect::<Vec<_>>();
        assert_eq!(consumed, [manifest.clone(), manifest]);
        assert_eq!(b_returned["returned"], 2);
        assert!(!model.check_invariant("AtMostOneConsumer", &b_returned));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PRIVATE INSTANCE'S LAYOUT STAYS ITS OWN (ruling 406; day ten's D1): under a
    /// development build's `ATERM_STATE_HOME` the manifest, the crash journals beside
    /// it and the cell-metrics cache live in that root, beside the logs folder whose
    /// crash markers the journal claim reads — before, an instance on the owner's
    /// `$HOME` with its own state root took the owner's `session.toml` and claimed,
    /// then deleted, the owner's dead journals. A relative seam resolves nothing
    /// (never the owner's folder), and unset they are `<data_dir>/aterm` exactly as
    /// before. NEGATIVE CONTROL: the unset arm names the OS data dir, not the scratch
    /// root, so the scoped arm's pass is the seam's doing.
    #[test]
    fn the_manifest_journals_and_cell_metrics_follow_the_state_home_seam() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-restore-state-home")
            .tempdir()
            .expect("scratch dir");
        let root = scratch.path().to_path_buf();
        crate::test_env::scoped("ATERM_STATE_HOME", &root, || {
            assert_eq!(manifest_path(), Some(root.join("session.toml")));
            assert_eq!(crate::crash_journal::journal_dir(), Some(root.clone()));
            assert_eq!(cell_metrics_path(), Some(root.join("cell-metrics.toml")));
            assert_eq!(
                aterm_types::dirs::logs_dir(),
                Some(root.join("logs")),
                "the crash evidence the claim reads sits under the same root"
            );
        });
        crate::test_env::scoped("ATERM_STATE_HOME", "relative/state", || {
            assert_eq!(manifest_path(), None, "a relative seam is refused");
            assert_eq!(crate::crash_journal::journal_dir(), None);
            assert_eq!(cell_metrics_path(), None);
            // The quit's refusal names that cause, not a missing data dir
            // (ruling 408). Nothing is written: no path resolves.
            assert_eq!(
                write(&RestoreManifest::new(Vec::new())),
                Err("ATERM_STATE_HOME is not an absolute path; session restore is off".to_string())
            );
        });
        crate::test_env::scoped_unset("ATERM_STATE_HOME", || {
            let data = aterm_types::dirs::data_dir().map(|dir| dir.join("aterm"));
            assert_eq!(
                manifest_path(),
                data.as_ref().map(|dir| dir.join("session.toml"))
            );
            assert_eq!(crate::crash_journal::journal_dir(), data.clone());
            assert_eq!(
                cell_metrics_path(),
                data.as_ref().map(|dir| dir.join("cell-metrics.toml"))
            );
            assert_ne!(manifest_path(), Some(root.join("session.toml")));
        });
    }

    /// The recorded scale is the cache's SOLE entry: none when the file is
    /// absent, the one scale after one store, and — the case a guess would
    /// get wrong half the time — none again once a second scale is recorded.
    #[test]
    fn the_recorded_scale_is_the_sole_cached_scale_or_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-recorded-scale-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");
        assert_eq!(recorded_cell_metrics_scale_from(&path), None, "no cache");
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 2.0, 24.0, 14, 30).unwrap();
        assert_eq!(recorded_cell_metrics_scale_from(&path), Some(2.0));
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        assert_eq!(
            recorded_cell_metrics_scale_from(&path),
            None,
            "two recorded scales are not a prediction"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 cell-metrics cache: store→load round-trips per scale, a second scale
    /// coexists with the first, and re-storing the identical entry is the
    /// steady-state no-op (file bytes untouched).
    #[test]
    fn cell_metrics_round_trip_per_scale() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-roundtrip-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");

        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.5, 18.0, 11, 22).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
            Some((7, 15))
        );
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
            Some((11, 22))
        );
        // An uncached scale misses without disturbing the cached ones.
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 2.0, 24.0),
            None
        );

        // Steady state: the identical re-store leaves the published file's
        // mtime-bearing BYTES untouched (compare content — mtimes are too
        // coarse on some filesystems to prove a skipped write).
        let before = std::fs::read_to_string(&path).unwrap();
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A REAL change on one scale updates that entry and keeps the other.
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 8, 16).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.0, 12.0),
            Some((8, 16))
        );
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 1.5, 18.0),
            Some((11, 22))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// L1 cell-metrics cache: every staleness gate fails CLOSED (a miss, never a
    /// wrong hit) — font-key change, font-px change, degenerate persisted cells,
    /// schema mismatch, and absent/corrupt files.
    #[test]
    fn cell_metrics_staleness_gates_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-staleness-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");

        // Absent + corrupt files read as cold launches.
        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        std::fs::write(&path, "not toml [").unwrap();
        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        let _ = std::fs::remove_file(&path);

        store_cell_metrics_to(&path, "Cascadia|lh=1", 1.0, 12.0, 7, 15).unwrap();
        // A font change (new fingerprint) misses...
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        // ...and the next STORE under the new key drops the stale entries
        // wholesale rather than mixing fingerprints.
        store_cell_metrics_to(&path, "Fira|lh=1", 2.0, 24.0, 13, 29).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "Cascadia|lh=1", 1.0, 12.0),
            None
        );
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0),
            Some((13, 29))
        );
        // A predicted-font-px drift ≥ the tolerance misses (an explicit
        // `font_px` config change), a sub-tolerance one hits (f32 noise).
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 25.0), None);
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.2),
            Some((13, 29))
        );

        // Degenerate measurements are never persisted...
        store_cell_metrics_to(&path, "Fira|lh=1", 3.0, 36.0, 0, 15).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 3.0, 36.0), None);
        // ...and a hand-corrupted absurd entry is rejected at load.
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("cell_w = 13", "cell_w = 99999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0), None);

        // A future schema is ignored wholesale, never mis-parsed.
        let _ = std::fs::remove_file(&path);
        store_cell_metrics_to(&path, "Fira|lh=1", 1.0, 12.0, 7, 15).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("schema = 1", "schema = 999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE AGENT RIDES ITS LEAF (2026-09-27): the crash journal's capture puts on
    /// each terminal leaf the agent its session hosts, and the manifest
    /// carries it through its own codec unchanged. NEGATIVE CONTROL: a leaf
    /// whose session hosts none carries none, and a manifest never filled
    /// (a person's own quit) carries none at all.
    #[test]
    fn a_hosted_agent_rides_its_leaf_through_the_codec() {
        fn number(node: &mut RestoredSplitTree, next: &mut u64) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => {
                    t.local_id = Some(*next);
                    *next += 1;
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    number(first, next);
                    number(second, next);
                }
            }
        }
        fn agents(m: &RestoreManifest) -> Vec<Option<AgentRestore>> {
            fn walk(node: &RestoredSplitTree, out: &mut Vec<Option<AgentRestore>>) {
                match node {
                    RestoredSplitTree::Leaf {
                        view: RestoredView::Terminal(t),
                    } => out.push(t.agent.as_deref().cloned()),
                    RestoredSplitTree::Leaf { .. } => {}
                    RestoredSplitTree::Split { first, second, .. } => {
                        walk(first, out);
                        walk(second, out);
                    }
                }
            }
            let mut out = Vec::new();
            for w in &m.windows {
                for t in &w.restored_tabs {
                    walk(&t.root, &mut out);
                }
            }
            out
        }
        let claude = AgentRestore {
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/Users//me/.local/bin/claude".into(),
            argv: vec![
                "/Users//me/.local/bin/claude".into(),
                "--dangerously-skip-permissions".into(),
            ],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/Users//me/work".into(),
            version: Some("2.1.283".into()),
            home: None,
            login: None,
            codex: None,
        };
        let mut m = sample();
        let mut next = 0;
        for w in &mut m.windows {
            for t in &mut w.restored_tabs {
                number(&mut t.root, &mut next);
            }
        }
        assert!(next >= 2, "the sample has leaves to fill");
        assert!(agents(&m).iter().all(Option::is_none), "never filled: none");
        m.fill_agents(&|id| (id == 1).then(|| claude.clone()));
        let filled = agents(&m);
        assert_eq!(filled.iter().filter(|a| a.is_some()).count(), 1);
        assert_eq!(filled[1].as_ref(), Some(&claude));
        let back = RestoreManifest::from_toml(&m.to_toml().expect("toml")).expect("parse");
        assert_eq!(agents(&back), filled, "through the codec unchanged");
    }

    /// A CODEX rides its leaf too (2026-09-27, the harness's Codex parity):
    /// the run its relaunch reads — its home, its mode, when it started —
    /// goes from the host's snapshot into the leaf, through the codec, and
    /// back into the reopened tab's snapshot, so the next launch relaunches it
    /// on its Codex lane, never as a Claude Code. NEGATIVE CONTROL: a Claude
    /// Code's snapshot comes back with no Codex run.
    #[test]
    fn a_codex_run_rides_its_leaf_back_into_its_relaunch() {
        use aterm_agent::harness::relaunch::{CodexRun, Snapshot};
        let codex = Snapshot {
            tab: "s-old".into(),
            pid: 5151,
            start: "Sat Sep 27 01:02:03 2026".into(),
            shell: 5150,
            program: "/opt/codex/bin/codex".into(),
            argv: vec!["/opt/codex/bin/codex".into(), "--no-daemon".into()],
            session: Some("01a0e4bd-2b37-75a0-9d52-7f1e2c3a4b5c".into()),
            cwd: "/work/c".into(),
            version: Some("0.157.1".into()),
            codex: Some(CodexRun {
                home: "/Users//me/.codex".into(),
                embedded: true,
                started_ms: 1_790_543_997_000,
                ambiguous: false,
                block: None,
            }),
            dialect: None,
            login: None,
            home: None,
        };
        let mut m = sample();
        let mut next = 0;
        for w in &mut m.windows {
            for t in &mut w.restored_tabs {
                number_leaves(&mut t.root, &mut next);
            }
        }
        let carried = AgentRestore::of(codex.clone());
        m.fill_agents(&|id| (id == 0).then(|| carried.clone()));
        let back = RestoreManifest::from_toml(&m.to_toml().expect("toml")).expect("parse");
        let mut found = None;
        for w in &back.windows {
            for t in &w.restored_tabs {
                first_agent(&t.root, &mut found);
            }
        }
        let snap = found
            .expect("the leaf carries it")
            .snapshot("s-new".into(), 6161);
        assert_eq!(snap.codex, codex.codex, "the Codex run, unchanged");
        assert_eq!((snap.tab.as_str(), snap.shell), ("s-new", 6161));
        assert_eq!(
            Snapshot {
                tab: codex.tab.clone(),
                shell: codex.shell,
                ..snap
            },
            codex
        );
        let claude = Snapshot {
            codex: None,
            ..codex
        };
        assert_eq!(
            AgentRestore::of(claude)
                .snapshot("s-new".into(), 6161)
                .codex,
            None
        );

        fn number_leaves(node: &mut RestoredSplitTree, next: &mut u64) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => {
                    t.local_id = Some(*next);
                    *next += 1;
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    number_leaves(first, next);
                    number_leaves(second, next);
                }
            }
        }
        fn first_agent(node: &RestoredSplitTree, out: &mut Option<AgentRestore>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => {
                    if out.is_none() {
                        out.clone_from(&t.agent.as_deref().cloned());
                    }
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    first_agent(first, out);
                    first_agent(second, out);
                }
            }
        }
    }

    /// THE CLAUDE HOME AND THE LOGIN AN AGENT RAN UNDER RIDE ITS LEAF INTO
    /// THE RELAUNCH AFTER ATERM ENDED (2026-10-09): what the host read off
    /// its process (`Snapshot::home`, `::login`) goes onto the leaf
    /// ([`AgentRestore::of`]), through every on-disk format a cold restore
    /// reads — the layout's own codec (the quit's `session.toml`, the
    /// seamless update's handoff layout) and the crash journal's image —
    /// back into the reopened tab's snapshot ([`AgentRestore::snapshot`]),
    /// and onto the line its relaunch plans
    /// (`aterm_agent::harness::relaunch::exit_line`: `required_env`, as every
    /// live relaunch's). The default login NAMED (set and empty) stays named.
    /// NEGATIVE CONTROL: a leaf naming neither plans today's line, byte for
    /// byte.
    ///
    /// AND BOTH WAYS ACROSS AN UPDATE: a leaf as 0.101.0 writes it (its
    /// `AgentRestore`, frozen below) is what this build writes for an agent
    /// with neither, and reads back as today — no home, no login, today's
    /// line; and 0.101.0's reader takes a leaf naming both, ignoring them.
    ///
    /// RED with the fields left off the leaf (`of` and `snapshot` as before):
    /// every format gives back neither, and the line is the plain one.
    #[test]
    fn a_restored_agents_home_and_login_ride_its_leaf_into_its_relaunch_line() {
        use aterm_agent::harness::relaunch::{Snapshot, exit_line};
        use aterm_agent::harness::upgrade::Dialect;
        const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
        const HOME: &str = "/Users//me/lab home/.claude";
        const LOGIN: &str = "/Users//me/logins/work-2";

        /// `AgentRestore` as 0.101.0 declares it
        /// (`git show v0.101.0:crates/aterm-gui/src/restore.rs`): no home,
        /// no login, and no `deny_unknown_fields`.
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct AgentRestore0101 {
            pid: u32,
            start: String,
            program: String,
            argv: Vec<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            session: Option<String>,
            cwd: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            version: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            codex: Option<CodexRestore>,
        }
        fn first_agent(node: &RestoredSplitTree) -> Option<AgentRestore> {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => t.agent.as_deref().cloned(),
                RestoredSplitTree::Leaf { .. } => None,
                RestoredSplitTree::Split { first, second, .. } => {
                    first_agent(first).or_else(|| first_agent(second))
                }
            }
        }
        fn number(node: &mut RestoredSplitTree, next: &mut u64) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => {
                    t.local_id = Some(*next);
                    *next += 1;
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    number(first, next);
                    number(second, next);
                }
            }
        }

        let ran = |home: Option<&str>, login: Option<&str>| Snapshot {
            tab: "s-old".into(),
            pid: 4242,
            start: "Fri Oct  9 01:02:03 2026".into(),
            shell: 4241,
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(SESSION.into()),
            cwd: "/work".into(),
            version: Some("2.1.295".into()),
            codex: None,
            dialect: Some(Dialect::Zsh),
            login: login.map(str::to_string),
            home: home.map(str::to_string),
        };
        // The reopened tab's snapshot, read back from each on-disk format,
        // in a zsh as the relaunch's plan reads the reopened shell's dialect.
        let through = |snap: Snapshot| -> Vec<(&str, Snapshot)> {
            let mut m = sample();
            let mut next = 0;
            for w in &mut m.windows {
                for t in &mut w.restored_tabs {
                    number(&mut t.root, &mut next);
                }
            }
            let carried = AgentRestore::of(snap);
            m.fill_agents(&|id| (id == 0).then(|| carried.clone()));
            let layout = RestoreManifest::from_toml(&m.to_toml().expect("toml")).expect("parse");
            let header = crate::crash_journal::JournalHeader::now(7, false);
            let (_, journal) = crate::crash_journal::decode(
                &crate::crash_journal::encode(&m, &header).expect("the image"),
            )
            .expect("the journal reads");
            [("layout", layout), ("crash journal", journal)]
                .into_iter()
                .map(|(format, m)| {
                    let agent = m
                        .windows
                        .iter()
                        .flat_map(|w| &w.restored_tabs)
                        .find_map(|t| first_agent(&t.root))
                        .expect("the leaf carries its agent");
                    let back = agent.snapshot("s-new".into(), 6161);
                    let back = Snapshot {
                        dialect: Some(Dialect::Zsh),
                        ..back
                    };
                    (format, back)
                })
                .collect()
        };
        let program = format!("'/opt/claude/bin/claude' '--resume' '{SESSION}'");
        let plain = format!(" CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}");
        for (format, back) in through(ran(Some(HOME), Some(LOGIN))) {
            assert_eq!(
                (back.home.as_deref(), back.login.as_deref()),
                (Some(HOME), Some(LOGIN)),
                "{format}"
            );
            assert_eq!(
                exit_line(&back),
                Some(format!(
                    " CLAUDE_CONFIG_DIR='{HOME}' CLAUDE_SECURESTORAGE_CONFIG_DIR={LOGIN} \
                     CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}"
                )),
                "{format}: the home, then the login, on the line"
            );
        }
        // The default login NAMED is carried named: a different line from none.
        for (format, back) in through(ran(None, Some(""))) {
            assert_eq!(
                (back.home.as_deref(), back.login.as_deref()),
                (None, Some("")),
                "{format}"
            );
            assert_eq!(
                exit_line(&back),
                Some(format!(
                    " CLAUDE_SECURESTORAGE_CONFIG_DIR='' CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 \
                     {program}"
                )),
                "{format}"
            );
        }
        // NEGATIVE CONTROL: neither — today's line, byte for byte.
        for (format, back) in through(ran(None, None)) {
            assert_eq!((&back.home, &back.login), (&None, &None), "{format}");
            assert_eq!(
                exit_line(&back).as_deref(),
                Some(plain.as_str()),
                "{format}"
            );
        }

        // AS 0.101.0 WRITES IT: what this build writes for an agent with
        // neither variable, byte for byte, and read back as today.
        let old = AgentRestore0101 {
            pid: 4242,
            start: "Fri Oct  9 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(SESSION.into()),
            cwd: "/work".into(),
            version: Some("2.1.295".into()),
            codex: None,
        };
        let written = aterm_toml::to_string(&old).expect("0.101.0's record");
        assert_eq!(
            aterm_toml::to_string(&AgentRestore::of(ran(None, None))).expect("this build's"),
            written,
            "an agent with neither is written as 0.101.0 writes it"
        );
        let read: AgentRestore = aterm_toml::from_str(&written).expect("this build reads it");
        assert_eq!((&read.home, &read.login), (&None, &None));
        let back = Snapshot {
            dialect: Some(Dialect::Zsh),
            ..read.snapshot("s-new".into(), 6161)
        };
        assert_eq!(exit_line(&back).as_deref(), Some(plain.as_str()));
        // …and 0.101.0 reads this build's leaf naming both, ignoring them.
        let both = aterm_toml::to_string(&AgentRestore::of(ran(Some(HOME), Some(LOGIN))))
            .expect("this build's record");
        assert!(
            both.contains("home = ") && both.contains("login = "),
            "{both}"
        );
        assert_eq!(
            aterm_toml::from_str::<AgentRestore0101>(&both).expect("0.101.0 reads it"),
            old
        );
    }
}
