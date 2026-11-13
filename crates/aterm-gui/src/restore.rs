// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Session layout persistence, crash recovery, and restore manifest serialization.
use crate::crash_journal::{Evidence, JournalId};
use crate::durable::Flush;
use aterm_agent::harness::relaunch::AgentRun;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "restore_claude_auth.rs"]
mod claude_auth;
pub(crate) use claude_auth::ClaudeAuthRestore;

/// The current on-disk schema.
pub(crate) const SCHEMA: u32 = 2;
const LEGACY_SCHEMA: u32 = 1;

pub(crate) const MAX_WINDOWS: usize = 64;
pub(crate) const MAX_TABS_PER_WINDOW: usize = 256;
const MAX_DOCUMENT_URI_BYTES: usize = 8 * 1024;
const MAX_SETTINGS_ROUTE_BYTES: usize = 128;
const MAX_RESTORE_TAG_BYTES: usize = 128;
/// The Recovery tab that stands in for a private document ([`WindowLayout::stand_in_for_private_documents`]).
pub(crate) const PRIVATE_DOCUMENT_TAG: &str = "private document";
const PRIVATE_DOCUMENT_CLOSED: &str =
    "A private document closes when aterm updates. Run the command that opened it to see it again.";
const MAX_LEAVES_PER_TAB: usize = 256;
const MAX_SPLIT_DEPTH: usize = 32;
/// The words every cap refusal of [`RestoreManifest::why_not_whole`] ends its cap with.
pub(crate) const UPDATE_CAP_KEY: &str = "an update can carry";
const MAX_VIEW_METADATA_BYTES: usize = 16 * 1024;
const MAX_EDITOR_SELECTIONS: usize = 256;
const MAX_SETTINGS_DRAFT_KEY_BYTES: usize = 128;
/// The longest unsaved Settings field text an update carries.
pub(crate) const MAX_SETTINGS_DRAFT_TEXT_BYTES: usize = 16 * 1024;
/// The most drafts one Settings view carries.
pub(crate) const MAX_SETTINGS_DRAFTS_PER_VIEW: usize = 256;

/// Whether one unsaved Settings field draft can ride a seamless update.
#[must_use]
pub(crate) fn settings_draft_is_carriable(key: &str, text: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_SETTINGS_DRAFT_KEY_BYTES
        && !key.contains('\0')
        && text.len() <= MAX_SETTINGS_DRAFT_TEXT_BYTES
        && !text.contains('\0')
}

fn storable_leaf_cwd(cwd: Option<String>) -> Option<String> {
    cwd.filter(|cwd| cwd.len() <= MAX_DOCUMENT_URI_BYTES && !cwd.contains('\0'))
}

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

/// serde-stable mirror of [`crate::pane::SplitDir`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SplitKind {
    Vertical,
    Horizontal,
}

/// Stable path component used to identify focus and closed-view insertion points without persisting a process-local [`crate::tab_model::ViewId`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestoreBranch {
    First,
    Second,
}

/// One source-addressed selection.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RestoreSelection {
    pub anchor: usize,
    pub head: usize,
}

/// Stable metadata for a terminal split leaf.
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
    pub autopilot_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Box<AgentRestore>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervised: Option<Box<AgentRestore>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost: Option<Box<LostRestore>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held: bool,
    /// The claim folder of the tab's claim worker (its task record's folder, the worker running or
    /// ended), so a restore other than a seamless update, which the record does not cross, gives
    /// the tab a record again whether or not an agent rides the leaf: `working` for a worker it
    /// brings back or a shell the PTY keeper hands back, else `exited`, a tab whose worker is not
    /// coming back (it had ended, or a quit forgot its agent), which closes once that mark has
    /// stood its time and any program run there since has had as long
    /// (`harness_host::credit_claim::note_restored_worker`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_worker: Option<String>,
    /// The tab's claim worker had ended (its task marked `exited` in [`Self::claim_worker`]), so a
    /// restore times no program found or relaunched there as the worker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claim_ended: bool,
    /// The task record of a parked worker in this leaf, as a seamless update carries one without
    /// its ages ([`crate::task_record::restore_row`]), so a restore other than a seamless update
    /// keeps the row a worker's tab is listed by ([`crate::task_record::seed_restored`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The agent this leaf carries runs in a claim folder but is no claim worker (the leaf names
    /// none, [`Self::mark_claim_bystander`]): someone's own, which a restore must not time as the
    /// worker. A writer before [`Self::claim_worker`] left both out, so a reader takes an agent in
    /// a claim folder with neither for the worker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_claim_worker: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_ep: Option<u32>,
}

/// The word that a tab's agent cannot be brought back, as a seamless update's handoff leaf carries it ([`TerminalLeafRestore::lost`]).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct LostRestore {
    pub session: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub marked: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restarted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// Whether `sid` is one a restore leaf may carry (`TerminalLeafRestore::sid`).
pub(crate) fn carried_sid(sid: &str) -> bool {
    crate::keeper_link::valid_sid(sid) && aterm_slog::ShortId::new(sid).is_some()
}

/// A hosted agent as its leaf's restore carries it (see [`TerminalLeafRestore::agent`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AgentRestore {
    pub pid: u32,
    pub start: String,
    pub program: String,
    pub argv: Vec<String>,
    pub session: Option<String>,
    pub cwd: String,
    pub version: Option<String>,
    pub home: Option<String>,
    pub login: Option<String>,
    pub claude_auth: Option<ClaudeAuthRestore>,
    pub run: Option<RunRestore>,
    pub word: String,
    pub cause: Option<String>,
    pub tab: Option<String>,
}

/// THE RUN A LEAF CARRIES FOR A LANE PAST CLAUDE CODE'S ([`AgentRestore::run`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum RunRestore {
    Codex(CodexRestore),
    Muse(MuseRestore),
    Agy(AgyRestore),
}

mod leaf {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub(super) struct AgentRestore {
        pub(super) pid: u32,
        pub(super) start: String,
        pub(super) program: String,
        pub(super) argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) session: Option<String>,
        pub(super) cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) home: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) login: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) claude_auth: Option<super::ClaudeAuthRestore>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) codex: Option<super::CodexRestore>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub(super) word: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) cause: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) tab: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) muse: Option<super::MuseRestore>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) agy: Option<super::AgyRestore>,
        /// The retired Gemini CLI's run table, which older builds wrote: read only to
        /// refuse the leaf as no agent's ([`super::AgentRestore::retired`]); every write
        /// leaves it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(super) gemini: Option<RetiredRun>,
    }

    /// A retired agent's run table, its fields ignored: that it was there is all a reader
    /// keeps.
    #[derive(Serialize, Deserialize)]
    pub(super) struct RetiredRun {}
}

impl From<leaf::AgentRestore> for AgentRestore {
    fn from(leaf: leaf::AgentRestore) -> Self {
        Self {
            pid: leaf.pid,
            start: leaf.start,
            program: leaf.program,
            argv: leaf.argv,
            session: leaf.session,
            cwd: leaf.cwd,
            version: leaf.version,
            home: leaf.home,
            login: leaf.login,
            claude_auth: leaf.claude_auth,
            run: match (leaf.codex, leaf.muse, leaf.agy, &leaf.gemini) {
                (Some(codex), None, None, None) => Some(RunRestore::Codex(codex)),
                (None, Some(muse), None, None) => Some(RunRestore::Muse(muse)),
                (None, None, Some(agy), None) => Some(RunRestore::Agy(agy)),
                _ => None,
            },
            word: if leaf.gemini.is_some() {
                RETIRED_GEMINI.to_string()
            } else {
                leaf.word
            },
            cause: leaf.cause,
            tab: leaf.tab,
        }
    }
}

impl From<AgentRestore> for leaf::AgentRestore {
    fn from(agent: AgentRestore) -> Self {
        let (codex, muse, agy) = match agent.run {
            Some(RunRestore::Codex(run)) => (Some(run), None, None),
            Some(RunRestore::Muse(run)) => (None, Some(run), None),
            Some(RunRestore::Agy(run)) => (None, None, Some(run)),
            None => (None, None, None),
        };
        Self {
            pid: agent.pid,
            start: agent.start,
            program: agent.program,
            argv: agent.argv,
            session: agent.session,
            cwd: agent.cwd,
            version: agent.version,
            home: agent.home,
            login: agent.login,
            claude_auth: agent.claude_auth,
            codex,
            word: agent.word,
            cause: agent.cause,
            tab: agent.tab,
            muse,
            agy,
            gemini: None,
        }
    }
}

impl Serialize for AgentRestore {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        leaf::AgentRestore::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AgentRestore {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let leaf = leaf::AgentRestore::deserialize(deserializer)?;
        let tables = [
            leaf.codex.is_some(),
            leaf.muse.is_some(),
            leaf.agy.is_some(),
            leaf.gemini.is_some(),
        ];
        if tables.into_iter().filter(|&table| table).count() > 1 {
            return Err(serde::de::Error::custom(
                "an agent restore has more than one run table",
            ));
        }
        let agent = Self::from(leaf);
        let claude = match agent.agent() {
            Some(aterm_agent::harness::upgrade::Agent::Claude) => true,
            Some(
                aterm_agent::harness::upgrade::Agent::Codex
                | aterm_agent::harness::upgrade::Agent::Muse
                | aterm_agent::harness::upgrade::Agent::Agy,
            )
            | None => false,
        };
        if agent.claude_auth.is_some() && !claude {
            return Err(serde::de::Error::custom(
                "Claude authentication attribution belongs only to a Claude restore",
            ));
        }
        Ok(agent)
    }
}

impl RunRestore {
    /// The agent whose run it is.
    pub(crate) const fn agent(&self) -> aterm_agent::harness::upgrade::Agent {
        match self {
            RunRestore::Codex(_) => aterm_agent::harness::upgrade::Agent::Codex,
            RunRestore::Muse(_) => aterm_agent::harness::upgrade::Agent::Muse,
            RunRestore::Agy(_) => aterm_agent::harness::upgrade::Agent::Agy,
        }
    }
}

/// A Muse Code TUI's run as its leaf's restore carries it, on disk the leaf's `muse` table.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct MuseRestore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub data: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

/// An agy TUI's run as its leaf's restore carries it (`aterm_agent::harness::relaunch::AgyRun`), on disk the leaf's `agy` table.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct AgyRestore {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default)]
    pub config: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

/// The agent word a leaf that carried the retired Gemini CLI's run table reads as:
/// no lane's ([`aterm_phase::retired_agent`]), so the leaf is relaunched by nothing
/// and never read as another agent's (an older leaf with no word would otherwise read
/// as Claude Code's).
const RETIRED_GEMINI: &str = "gemini";

/// A Codex TUI's run as its leaf's restore carries it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct CodexRestore {
    pub home: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_env: Option<String>,
    pub embedded: bool,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alab: Option<AlabRestore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<Vec<String>>,
}

/// The crash rule's validated resume line as a leaf carries it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct AlabRestore {
    pub argv: Vec<String>,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_auth_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl AlabRestore {
    fn of(resume: aterm_agent::harness::self_claimed::Resume) -> Self {
        Self {
            argv: resume.argv,
            session: resume.session,
            codex_home: resume.codex_home,
            codex_auth_path: resume.codex_auth_path,
            cwd: resume.cwd,
        }
    }

    fn into_valid_resume(self) -> Option<aterm_agent::harness::self_claimed::Resume> {
        let resume = aterm_agent::harness::self_claimed::Resume {
            argv: self.argv,
            session: self.session,
            codex_home: self.codex_home,
            codex_auth_path: self.codex_auth_path,
            cwd: self.cwd,
        };
        resume.validate_structural().ok().map(|()| resume)
    }

    fn is_valid(&self) -> bool {
        self.clone().into_valid_resume().is_some()
    }
}

impl AgentRestore {
    /// The process it was.
    pub(crate) fn ran_as(&self) -> (u32, &str) {
        (self.pid, &self.start)
    }

    /// A relaunch owed on `snap`, its act's `asked` cause with it.
    pub(crate) fn owed(
        snap: aterm_agent::harness::relaunch::Snapshot,
        asked: Option<aterm_agent::harness::restart_stamp::Cause>,
    ) -> Self {
        Self {
            cause: asked.map(|cause| cause.word().to_string()),
            ..Self::of(snap)
        }
    }

    /// The tab it ran in ([`Self::tab`]) where that is not `now`, the sid of the tab it is reopened in.
    pub(crate) fn ran_in_another(&self, now: &str) -> Option<String> {
        self.tab.clone().filter(|t| t != now)
    }

    /// The explicit restart's cause it carries ([`Self::cause`]).
    pub(crate) fn asked(&self) -> Option<aterm_agent::harness::restart_stamp::Cause> {
        self.cause
            .as_deref()
            .and_then(aterm_agent::harness::restart_stamp::Cause::parse)
    }

    /// The durable half of the relaunch snapshot `snap`.
    pub(crate) fn of(snap: aterm_agent::harness::relaunch::Snapshot) -> Self {
        let word = snap.agent().word().to_string();
        let claude_auth = (matches!(snap.run, AgentRun::Claude) && snap.auth_observed).then(|| {
            ClaudeAuthRestore::new(
                snap.route,
                snap.launch_env.map(|env| env.entries().to_vec()),
                snap.profile.map(Into::into),
            )
        });
        let (session, lane_session) = match &snap.run {
            AgentRun::Muse(_) | AgentRun::Agy(_) => (None, snap.session),
            AgentRun::Claude | AgentRun::Codex(_) => (snap.session, None),
        };
        Self {
            word,
            tab: Some(snap.tab).filter(|t| !t.is_empty()),
            pid: snap.pid,
            start: snap.start,
            program: snap.program.to_string_lossy().into_owned(),
            argv: snap.argv,
            session,
            cwd: snap.cwd,
            version: snap.version,
            home: snap.home,
            login: snap.login,
            claude_auth,
            run: match snap.run {
                AgentRun::Claude => None,
                AgentRun::Codex(run) => Some(RunRestore::Codex(CodexRestore {
                    home: run.home.to_string_lossy().into_owned(),
                    home_env: run.home_env,
                    embedded: true,
                    started_ms: 0,
                    alab: run.self_claimed.map(AlabRestore::of),
                    carried: None,
                })),
                AgentRun::Muse(run) => Some(RunRestore::Muse(MuseRestore {
                    session: lane_session,
                    workspace: run.workspace,
                    data: run.data.to_string_lossy().into_owned(),
                    env: aterm_agent::harness::upgrade_muse::env_words(&run.env),
                })),
                AgentRun::Agy(run) => Some(RunRestore::Agy(AgyRestore {
                    conversation: lane_session,
                    config: run.config.to_string_lossy().into_owned(),
                    env: aterm_agent::harness::upgrade_agy::env_words(&run.env),
                })),
            },
            cause: None,
        }
    }

    /// The relaunch snapshot of this agent in the reopened tab `tab`, whose shell is `shell`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ClaudeRestoreAuth",
            action = "NextCase",
            reason = "Choose captured, legacy, or wrong-provider restore metadata."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ClaudeRestoreAuth",
            action = "Restore",
            project = "restore::claude_auth::tests::restore_transition"
        )
    )]
    pub(crate) fn snapshot(
        self,
        tab: String,
        shell: u32,
    ) -> Option<aterm_agent::harness::relaunch::Snapshot> {
        use aterm_agent::harness::upgrade::Agent;
        if !self.relaunched() {
            return None;
        }
        let agent = self.agent()?;
        let auth_observed = self.claude_auth.is_some();
        let (route, profile, launch_env) = self
            .claude_auth
            .map(|auth| {
                (
                    auth.route(),
                    auth.profile.map(Into::into),
                    auth.launch_env
                        .as_deref()
                        .and_then(aterm_agent::harness::accounts::LaunchEnv::of_record),
                )
            })
            .unwrap_or_default();
        let mut session = self.session;
        let run = match (agent, self.run) {
            (Agent::Claude, _) => AgentRun::Claude,
            (Agent::Codex, Some(RunRestore::Codex(run))) => {
                let alab = run.alab.and_then(AlabRestore::into_valid_resume)?;
                AgentRun::Codex(aterm_agent::harness::relaunch::CodexRun {
                    home: std::path::PathBuf::from(run.home),
                    home_env: run.home_env,
                    self_claimed: Some(alab),
                })
            }
            (Agent::Codex, None) => return None,
            (Agent::Muse, Some(RunRestore::Muse(run))) => {
                session = run.session;
                AgentRun::Muse(aterm_agent::harness::relaunch::MuseRun {
                    data: std::path::PathBuf::from(run.data),
                    env: aterm_agent::harness::upgrade_muse::env_of_words(&run.env),
                    workspace: run.workspace,
                    lock: None,
                    started_ms: 0,
                    block: None,
                })
            }
            (Agent::Muse, _) => return None,
            (Agent::Agy, Some(RunRestore::Agy(run))) => {
                session = run.conversation;
                AgentRun::Agy(aterm_agent::harness::relaunch::AgyRun {
                    config: std::path::PathBuf::from(run.config),
                    env: aterm_agent::harness::upgrade_agy::env_of_words(&run.env),
                    lock: None,
                    crash: None,
                    started_ms: 0,
                    block: None,
                })
            }
            (Agent::Agy, _) => return None,
            (Agent::Codex, Some(RunRestore::Muse(_) | RunRestore::Agy(_))) => return None,
        };
        Some(aterm_agent::harness::relaunch::Snapshot {
            tab,
            pid: self.pid,
            start: self.start,
            shell,
            program: std::path::PathBuf::from(self.program),
            argv: self.argv,
            session,
            cwd: self.cwd,
            version: self.version,
            run,
            dialect: None,
            claude_dir: None,
            login: self.login,
            home: self.home,
            route,
            auth_observed,
            profile,
            launch_env,
        })
    }
}

impl AgentRestore {
    /// The relaunch snapshot of this agent.
    #[cfg(any(unix, test))]
    pub(crate) fn handed_snapshot(
        self,
        tab: String,
        adopted: u32,
    ) -> Option<aterm_agent::harness::relaunch::Snapshot> {
        let shell = if adopted == self.pid {
            aterm_agent::harness::relaunch::NO_SHELL
        } else {
            adopted
        };
        self.snapshot(tab, shell)
    }

    /// Whether the relaunch after aterm ended brings this agent back on its conversation ([`agent_resumes`]).
    pub(crate) fn resumes(&self) -> bool {
        self.relaunched()
            && match &self.run {
                Some(RunRestore::Muse(run)) => {
                    run.session
                        .as_deref()
                        .is_some_and(aterm_agent::harness::upgrade_muse::is_session_uuid)
                        && aterm_agent::harness::upgrade_muse::is_tui_argv(&self.argv)
                }
                Some(RunRestore::Agy(run)) => {
                    run.conversation
                        .as_deref()
                        .is_some_and(aterm_agent::harness::upgrade_agy::is_conversation_uuid)
                        && aterm_agent::harness::upgrade_agy::is_tui_argv(&self.argv)
                }
                Some(RunRestore::Codex(_)) => {
                    self.session
                        .as_deref()
                        .is_some_and(aterm_agent::harness::upgrade::is_session_id)
                        && aterm_agent::harness::codex_alab::is_tui_argv(&self.argv)
                }
                None => agent_resumes(self.session.as_deref(), &self.argv),
            }
    }

    /// Whether it names an agent this build no longer supports ([`aterm_phase::retired_agent`]):
    /// relaunched by nothing ([`Self::agent`] reads no agent), the restore saying why.
    pub(crate) fn retired(&self) -> bool {
        aterm_phase::retired_agent(&self.word)
    }

    /// Whether any relaunch brings this agent back.
    pub(crate) fn relaunched(&self) -> bool {
        match &self.run {
            None | Some(RunRestore::Muse(_) | RunRestore::Agy(_)) => true,
            Some(RunRestore::Codex(run)) => run.alab.as_ref().is_some_and(AlabRestore::is_valid),
        }
    }

    /// The agent as a person names it, the one its relaunch would read ([`Self::agent`]).
    pub(crate) fn name(&self) -> Option<&'static str> {
        use aterm_agent::harness::upgrade::Agent;
        self.agent().map(|agent| match agent {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Muse => agent.product(),
            Agent::Agy => aterm_agent::harness::upgrade_agy::PROGRAM,
        })
    }

    #[cfg(test)]
    pub(crate) fn resume_line(&self) -> Option<String> {
        self.resume_line_at(None)
    }

    /// One word of [`Self::resume_line_at`]'s line: as written when it holds only letters, digits
    /// and `-_./:=@%+,`, else quoted for zsh; `None` for a word with a control or line-separator
    /// character, which no typed line carries.
    fn resume_word(w: &str) -> Option<String> {
        use aterm_agent::harness::upgrade::{Dialect, quote};
        if w.chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        {
            return None;
        }
        let plain = !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b));
        Some(if plain {
            w.to_string()
        } else {
            quote(Dialect::Zsh, w)
        })
    }

    /// THE LINE A PERSON TYPES IN THE REOPENED TAB TO RESUME THIS AGENT ON ITS CONVERSATION.
    ///
    /// Routes a Codex conversation off a pool row `seat`'s records mark limited, signed out or not
    /// signed in here ([`aterm_agent::harness::accounts_pool::cold_resume_env`]). Called off the
    /// main thread when `seat` is present: the recorded executable's capability is read there.
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ColdResumeEnvironment",
            action = "NextCase",
            reason = "Select a healthy account or either direction of a held account reroute."
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::spec_unmodeled(
            machine = "ColdResumeEnvironment",
            action = "Contaminate",
            reason = "Run the native command in a shell with inherited account selectors."
        )
    )]
    #[cfg_attr(
        all(unix, any(test, debug_assertions)),
        aterm_spec_anchor::refines(
            machine = "ColdResumeEnvironment",
            action = "Render",
            project = "aterm_gui::restore::cold_resume_environment_tests::environment_transition"
        )
    )]
    pub(crate) fn resume_line_at(
        &self,
        seat: Option<&aterm_agent::harness::accounts_pool::Seat<'_>>,
    ) -> Option<String> {
        use aterm_agent::harness::accounts::{Cli, HOME_ENV, LOGIN_ENV};
        use aterm_agent::harness::codex_alab::{AUTH_PATH_ENV, HOME_ENV as CODEX_HOME_ENV};
        use aterm_agent::harness::upgrade::{Agent, Dialect, is_session_id};
        let word = Self::resume_word;
        let run = match self.agent()? {
            Agent::Claude | Agent::Codex => &self.run,
            Agent::Muse => {
                let Some(RunRestore::Muse(run)) = &self.run else {
                    return None;
                };
                return aterm_agent::harness::upgrade_muse::told_line(
                    run.session.as_deref()?,
                    &aterm_agent::harness::upgrade_muse::env_of_words(&run.env),
                );
            }
            Agent::Agy => {
                let Some(RunRestore::Agy(run)) = &self.run else {
                    return None;
                };
                return aterm_agent::harness::upgrade_agy::told_line(
                    run.conversation.as_deref()?,
                    &aterm_agent::harness::upgrade_agy::env_of_words(&run.env),
                    &self.argv,
                );
            }
        };
        let conversation = self.session.as_deref().filter(|id| is_session_id(id))?;
        let mut routed_alab: Option<AlabRestore> = None;
        let mut routed_unset = Vec::new();
        let (env, line) = match run {
            None => (
                [
                    (HOME_ENV, self.home.as_deref()),
                    (LOGIN_ENV, self.login.as_deref()),
                ]
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect::<Vec<_>>(),
                aterm_agent::harness::upgrade_drive::resume_command_kept(&self.argv, conversation)?,
            ),
            Some(RunRestore::Codex(CodexRestore {
                alab: Some(alab), ..
            })) if alab.is_valid() => {
                if let Some(seat) = seat {
                    let ran_env: Vec<(&str, &str)> = alab
                        .codex_home
                        .as_deref()
                        .map(|home| (CODEX_HOME_ENV, home))
                        .into_iter()
                        .chain(
                            alab.codex_auth_path
                                .as_deref()
                                .map(|auth| (AUTH_PATH_ENV, auth)),
                        )
                        .collect();
                    if let Some(pick) = aterm_agent::harness::accounts_pool::cold_resume_env(
                        seat,
                        Cli::Codex,
                        &ran_env,
                        std::path::Path::new(alab.argv.first()?),
                    )
                    .ok()?
                    {
                        let mut unset =
                            aterm_agent::harness::relaunch::pool_args(Cli::Codex, &pick, None, &[])
                                .ok()?
                                .unset;
                        let mut candidate = alab.clone();
                        candidate.codex_auth_path = pick.value;
                        if !candidate.is_valid() {
                            return None;
                        }
                        if candidate.codex_home.as_deref().is_none_or(str::is_empty)
                            && !unset.contains(&CODEX_HOME_ENV)
                        {
                            unset.push(CODEX_HOME_ENV);
                        }
                        routed_unset = unset;
                        routed_alab = Some(candidate);
                    }
                }
                let effective = routed_alab.as_ref().unwrap_or(alab);
                (
                    effective
                        .codex_home
                        .as_deref()
                        .filter(|home| !(routed_alab.is_some() && home.is_empty()))
                        .map(|home| (CODEX_HOME_ENV, home))
                        .into_iter()
                        .chain(
                            effective
                                .codex_auth_path
                                .as_deref()
                                .map(|auth| (AUTH_PATH_ENV, auth)),
                        )
                        .collect(),
                    effective
                        .argv
                        .iter()
                        .map(|w| word(w))
                        .collect::<Option<Vec<_>>>()?
                        .join(" "),
                )
            }
            Some(RunRestore::Muse(_) | RunRestore::Agy(_) | RunRestore::Codex(_)) => return None,
        };
        if let Some(alab) = &routed_alab {
            let (exe, args) = alab.argv.split_first()?;
            // A routed suggestion must carry its removals even when the original
            // shell has acquired another login or home since the session ended.
            return aterm_agent::harness::upgrade::relaunch_line_env_clear(
                Dialect::Zsh,
                None,
                None,
                &env,
                &routed_unset,
                Path::new(exe),
                args,
            )
            .ok();
        }
        let mut said = String::new();
        for (name, value) in env {
            said.push_str(name);
            said.push('=');
            said.push_str(&word(value)?);
            said.push(' ');
        }
        said.push_str(&line);
        Some(said)
    }

    /// The agent it was.
    pub(crate) fn agent(&self) -> Option<aterm_agent::harness::upgrade::Agent> {
        use aterm_agent::harness::upgrade::Agent;
        let ran = self.run.as_ref().map(RunRestore::agent);
        let agent = if self.word.is_empty() {
            ran.unwrap_or(Agent::Claude)
        } else {
            Agent::parse(&self.word)?
        };
        let agrees = match agent {
            Agent::Claude => ran.is_none(),
            Agent::Codex | Agent::Muse | Agent::Agy => ran == Some(agent),
        };
        agrees.then_some(agent)
    }
}

/// Return whether an agent session hosted before shutdown should be resumed on restore.
pub(crate) fn agent_resumes(session: Option<&str>, argv: &[String]) -> bool {
    use aterm_agent::harness::upgrade::{is_session_id, one_shot};
    session.is_some_and(is_session_id) && !one_shot(argv)
}

impl TerminalLeafRestore {
    /// Set [`Self::not_claim_worker`] from the agent this leaf carries now: one in a claim folder
    /// on a leaf that names no claim worker is someone's own, where this run wrote the leaf from
    /// its live session (this writer names every claim worker it has a record of). Run once the
    /// leaf carries its last agent ([`RestoreManifest::mark_claim_bystanders`]), never on a leaf
    /// carried from an older layout, which may name a worker only by its agent.
    pub(crate) fn mark_claim_bystander(&mut self) {
        self.not_claim_worker = self.claim_worker.is_none()
            && self.agent.as_ref().is_some_and(|agent| {
                aterm_agent::harness::credit_offers::is_claim_folder(std::path::Path::new(
                    &agent.cwd,
                ))
            });
    }

    /// The standing holds this leaf's tab carried, read fail-closed.
    pub(crate) fn carried_holds(&self) -> crate::fabric::Holds {
        crate::fabric::parse_holds(self.holds.iter().map(String::as_str))
    }

    /// The agent this leaf names that the relaunch after aterm ended brings back on its conversation ([`AgentRestore::resumes`]).
    pub(crate) fn resumes_agent(&self) -> Option<&AgentRestore> {
        self.agent
            .as_deref()
            .filter(|agent| agent.resumes() && self.holds.is_empty())
    }

    /// Whether this leaf's folder may be another machine's.
    pub(crate) const fn folder_may_be_remote(&self) -> bool {
        self.held && self.agent.is_none()
    }

    /// The folder this leaf's fresh shell is asked to start in.
    pub(crate) fn start_cwd(&self) -> Option<String> {
        crate::spawn_folder::inherited(self.cwd.clone()?, || self.folder_may_be_remote())
    }

    /// The seven USER fields this leaf carries.
    pub(crate) fn carried_user_meta(&self) -> crate::session_timeline::SessionMeta {
        crate::session_timeline::SessionMeta {
            user_title: self.user_title.clone(),
            description: self.description.clone(),
            icon: self.icon.clone(),
            role: self.role.clone(),
            attention: self.attention.clone(),
            questions: self.questions.clone(),
            autopilot: self.autopilot.clone(),
            land_at: self.carried_land_at(),
            autopilot_set_by: self.carried_set_by(),
            ..Default::default()
        }
    }

    /// The setter this leaf carries, kept only beside a word and only where it parses.
    pub(crate) fn carried_set_by(&self) -> Option<crate::session_timeline::AutopilotSetBy> {
        self.autopilot.as_ref()?;
        self.autopilot_by
            .as_deref()
            .and_then(crate::session_timeline::AutopilotSetBy::parse_carried)
    }

    /// The timer this leaf carries.
    pub(crate) fn carried_land_at(&self) -> Option<i64> {
        self.land_at.filter(|_| {
            self.autopilot
                .as_deref()
                .and_then(crate::session_timeline::normalize_autopilot_value)
                == Some(aterm_agent::supervise::Autopilot::Cruise.word())
        })
    }

    fn sanitize(&mut self) {
        self.cwd = storable_leaf_cwd(self.cwd.take());
        self.task = storable_leaf_cwd(self.task.take());
        sanitize_leaf_title(&mut self.title);
        self.profile = self
            .profile
            .take()
            .filter(|profile| profile.len() <= MAX_SETTINGS_ROUTE_BYTES && !profile.contains('\0'));
        self.sid = self.sid.take().filter(|sid| carried_sid(sid));
        self.lineage = self.lineage.take().filter(|sid| carried_sid(sid));
        self.log_ep = self.log_ep.and_then(crate::session_log::told_epoch);
        self.holds = crate::fabric::render_holds(&self.carried_holds());
        self.sanitize_user_metadata();
    }

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
        self.autopilot = self
            .autopilot
            .as_deref()
            .and_then(|value| carried_user_value("autopilot", value));
        self.land_at = self.carried_land_at();
        self.autopilot_by = self.carried_set_by().map(|set_by| set_by.carried());
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
            && canonical("autopilot", &self.autopilot)
            && self.land_at == self.carried_land_at()
            && self.autopilot_by == self.carried_set_by().map(|set_by| set_by.carried())
    }
}

/// One Settings text field's unsaved draft.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct SettingsDraftRestore {
    pub key: String,
    pub text: String,
}

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

fn carried_user_value(field: &str, value: &str) -> Option<String> {
    if field == "attention" {
        return crate::session_timeline::carried_attention(value);
    }
    crate::session_timeline::sanitize_metadata_value(field, value)
}

/// Per-view native state.
/// Presentation state belonging to the same Editor document and source buffer.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct MarkdownWorkspaceRestore {
    pub mode: String,
    pub focus: String,
    pub source_anchor: usize,
    pub visual_row: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reader_entry_anchor: Option<usize>,
    pub selection: Option<RestoreSelection>,
    pub split_per_mille: u16,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct NativeLeafRestore {
    pub restore_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub config_editor: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<MarkdownWorkspaceRestore>,
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
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "lenient_settings_drafts"
    )]
    pub settings_drafts: Vec<SettingsDraftRestore>,
    #[serde(skip)]
    pub settings_drafts_unreadable: usize,
}

impl NativeLeafRestore {
    pub(crate) fn mail() -> Self {
        let mut descriptor = Self::about();
        descriptor.restore_tag = "mail".into();
        descriptor
    }
    pub(crate) fn about() -> Self {
        let mut descriptor = Self::settings("/".into());
        descriptor.restore_tag = "about".into();
        descriptor.route = None;
        descriptor
    }

    pub(crate) fn allocation() -> Self {
        let mut descriptor = Self::about();
        descriptor.restore_tag = "allocation".into();
        descriptor
    }

    pub(crate) fn system_state() -> Self {
        let mut descriptor = Self::settings("/".into());
        descriptor.restore_tag = "system-state".into();
        descriptor.route = None;
        descriptor
    }

    pub(crate) fn settings(route: String) -> Self {
        Self {
            restore_tag: "settings".to_string(),
            route: Some(route),
            uri: None,
            config_editor: false,
            markdown: None,
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
            markdown: None,
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
        if let Some(markdown) = &self.markdown
            && (self.restore_tag != "editor"
                || !matches!(markdown.mode.as_str(), "read" | "edit" | "split")
                || !matches!(markdown.focus.as_str(), "editor" | "reader")
                || !(200..=800).contains(&markdown.split_per_mille))
        {
            return Some("invalid Markdown workspace state");
        }
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
            "mail" => (self.route.is_some() || self.uri.is_some())
                .then_some("Mail has no document or Settings route"),
            "about" | "allocation" => (self.route.is_some() || self.uri.is_some())
                .then_some("About has no document or Settings route"),
            "system-state" => (self.route.is_some() || self.uri.is_some())
                .then_some("System State has no document or Settings route"),
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
        metadata.truncate(metadata.floor_char_boundary(MAX_VIEW_METADATA_BYTES));
        metadata
    }
}

/// A recoverable leaf occupying the exact split position of an unavailable or malformed app.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct PlaceholderLeafRestore {
    pub restore_tag: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub metadata: String,
    #[serde(skip)]
    pub unhanded: Option<Box<TerminalLeafRestore>>,
}

impl PlaceholderLeafRestore {
    /// The outgoing pool id of the retired terminal leaf this placeholder stands in for.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn unhanded_local_id(&self) -> Option<u64> {
        self.unhanded.as_ref().and_then(|leaf| leaf.local_id)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "the terminal leaf is the common view (it passed the lint's 200-byte line over \
              the native leaf with its handoff-only `lost` word, 2026-10-17); a layout holds \
              one view per leaf, its tree already boxed by `Split`, and boxing the terminal \
              leaf would put a deref on every match over a restored view, as \
              `RestoredSplitTree` says"
)]
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
                    && terminal.sid.as_deref().is_none_or(carried_sid)
                    && terminal.lineage.as_deref().is_none_or(carried_sid)
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

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "node", rename_all = "snake_case")]
#[allow(
    clippy::large_enum_variant,
    reason = "the leaf IS the tree's payload; every node below the root is \
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
    pub(crate) const fn leaf(view: RestoredView) -> Self {
        Self::Leaf { view }
    }

    pub(crate) fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf { .. } => 1,
            Self::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    #[cfg(any(unix, test))]
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

    fn bounded_shape(&self, remaining_depth: usize, remaining_leaves: &mut usize) -> bool {
        if remaining_depth == 0 {
            return false;
        }
        match self {
            Self::Leaf { view } => {
                let Some(next) = remaining_leaves.checked_sub(1) else {
                    return false;
                };
                *remaining_leaves = next;
                view.bounded()
            }
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ratio.is_finite()
                    && (0.05..=0.95).contains(ratio)
                    && first.bounded_shape(remaining_depth - 1, remaining_leaves)
                    && second.bounded_shape(remaining_depth - 1, remaining_leaves)
            }
        }
    }

    fn shape_is_bounded(&self) -> bool {
        let mut remaining_leaves = MAX_LEAVES_PER_TAB;
        self.bounded_shape(MAX_SPLIT_DEPTH, &mut remaining_leaves)
    }

    fn retire_unhanded_terminals(&mut self, unhanded_sorted: &[u64]) {
        match self {
            Self::Leaf { view } => {
                let RestoredView::Terminal(terminal) = &*view else {
                    return;
                };
                if !terminal
                    .local_id
                    .is_some_and(|id| unhanded_sorted.binary_search(&id).is_ok())
                {
                    return;
                }
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
                first.retire_unhanded_terminals(unhanded_sorted);
                second.retire_unhanded_terminals(unhanded_sorted);
            }
        }
    }

    /// Every terminal leaf of this tree names no pool id ([`TerminalLeafRestore::local_id`]).
    pub(crate) fn forget_pool_ids(&mut self) {
        self.visit_terminals_mut(&mut |terminal| terminal.local_id = None);
    }

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

    /// Visit every terminal leaf in rebuild order without allocating a `Vec`.
    pub(crate) fn visit_terminals<'a>(&'a self, f: &mut impl FnMut(&'a TerminalLeafRestore)) {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => f(terminal),
            Self::Leaf { .. } => {}
            Self::Split { first, second, .. } => {
                first.visit_terminals(f);
                second.visit_terminals(f);
            }
        }
    }

    /// Mutably visit every terminal leaf in rebuild order without allocating a `Vec`.
    pub(crate) fn visit_terminals_mut(&mut self, f: &mut impl FnMut(&mut TerminalLeafRestore)) {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => f(terminal),
            Self::Leaf { .. } => {}
            Self::Split { first, second, .. } => {
                first.visit_terminals_mut(f);
                second.visit_terminals_mut(f);
            }
        }
    }

    /// Test whether every terminal leaf in rebuild order satisfies `f`, short-circuiting on `false`.
    pub(crate) fn all_terminals<'a>(
        &'a self,
        f: &mut impl FnMut(&'a TerminalLeafRestore) -> bool,
    ) -> bool {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => f(terminal),
            Self::Leaf { .. } => true,
            Self::Split { first, second, .. } => first.all_terminals(f) && second.all_terminals(f),
        }
    }

    /// The first terminal leaf in rebuild order (first branch before second).
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

/// One complete tab recovery record.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoredTab {
    pub root: RestoredSplitTree,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focused_path: Vec<RestoreBranch>,
    #[serde(default)]
    pub zoomed: bool,
    /// A worker's tab ([`crate::tab_model::Tab::parked`]). An older manifest has no such field and
    /// an older build ignores it, losing only the grouping.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked: bool,
    /// A tab its task opened for itself ([`crate::tab_model::Tab::ephemeral`]), whose pane remains
    /// after the task concludes. A parked tab is task-owned even where this bit is absent.
    /// An older build ignores this bit and leaves the tab as a person's there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// A worker's tab another Mac's fleet drive asked for ([`crate::tab_model::Tab::fleet`]): it
    /// stays a worker's with no task record, as a drive's plain shell has none. An older build
    /// ignores it, and brings such a tab back as a person's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fleet: bool,
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

/// One node of a persisted tab pane tree.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PaneLayout {
    Leaf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default)]
        title: String,
        #[serde(default)]
        focused: bool,
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
    /// A plain single-pane leaf.
    #[cfg(test)]
    pub(crate) fn leaf(cwd: Option<String>, title: String, focused: bool) -> Self {
        PaneLayout::Leaf {
            cwd,
            title,
            focused,
            local_id: None,
        }
    }

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

    fn forget_local_ids(&mut self, forget: &dyn Fn(u64) -> bool) {
        match self {
            PaneLayout::Leaf { local_id, .. } => {
                if local_id.is_some_and(forget) {
                    *local_id = None;
                }
            }
            PaneLayout::Split { first, second, .. } => {
                first.forget_local_ids(forget);
                second.forget_local_ids(forget);
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

    /// Every `Leaf` in this subtree in tree order (left-to-right / top-to-bottom).
    #[cfg(test)]
    pub(crate) fn leaves(&self) -> Vec<&PaneLayout> {
        let mut out = Vec::with_capacity(self.leaf_count());
        self.collect_leaves(&mut out);
        out
    }

    /// Visit every `Leaf` in this subtree in tree order without allocating a `Vec`.
    pub(crate) fn visit_leaves<'a>(&'a self, f: &mut impl FnMut(&'a PaneLayout)) {
        let _ = self.all_leaves(&mut |leaf| {
            f(leaf);
            true
        });
    }

    /// Test `f` against every `Leaf` in this subtree in tree order without allocating a `Vec`.
    ///
    /// Stops at the first leaf where `f` returns `false`.
    pub(crate) fn all_leaves<'a>(&'a self, f: &mut impl FnMut(&'a PaneLayout) -> bool) -> bool {
        match self {
            PaneLayout::Leaf { .. } => f(self),
            PaneLayout::Split { first, second, .. } => first.all_leaves(f) && second.all_leaves(f),
        }
    }

    /// The first `Leaf` in this subtree in tree order without allocating a `Vec`.
    pub(crate) fn first_leaf(&self) -> &PaneLayout {
        match self {
            PaneLayout::Leaf { .. } => self,
            PaneLayout::Split { first, .. } => first.first_leaf(),
        }
    }

    #[cfg(test)]
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

    /// This leaf's outgoing session pool id, when captured for a seamless-update handoff (`None` for a `Split`, and for any cold-quit leaf).
    pub(crate) const fn local_id(&self) -> Option<u64> {
        match self {
            PaneLayout::Leaf { local_id, .. } => *local_id,
            PaneLayout::Split { .. } => None,
        }
    }
}

/// Durable identity of one native tab.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum NativeTabRestore {
    Allocation,
    Mail,
    About,
    #[serde(rename = "system-state")]
    SystemState,
    Settings {
        route: String,
    },
    Markdown {
        uri: String,
    },
    Editor {
        uri: String,
    },
}

impl NativeTabRestore {
    #[cfg(test)]
    pub(crate) fn document_uri(&self) -> Option<&str> {
        match self {
            Self::Mail
            | Self::About
            | Self::Allocation
            | Self::Settings { .. }
            | Self::SystemState => None,
            Self::Markdown { uri } | Self::Editor { uri } => Some(uri),
        }
    }

    /// Whether this descriptor may join a window's legacy mirror that already holds `mirror`.
    pub(crate) fn joins_mirror(&self, mirror: &[Self]) -> bool {
        let is_settings = |descriptor: &Self| matches!(descriptor, Self::Settings { .. });
        !mirror.contains(self) && !(is_settings(self) && mirror.iter().any(is_settings))
    }

    fn shape_is_valid(&self) -> bool {
        match self {
            Self::Mail | Self::About | Self::Allocation | Self::SystemState => true,
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

/// Canonical chrome order without persisting any live [`crate::tab_model::TabId`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TabOrderEntry {
    Terminal { index: usize },
    Native { index: usize },
}

/// One window's macOS show state at capture time.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct WindowShow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<bool>,
}

impl WindowShow {
    /// Nothing captured.
    pub(crate) const UNKNOWN: Self = Self {
        fullscreen: None,
        minimized: None,
        z_order: None,
        key: None,
    };

    /// Whether nothing was captured.
    pub(crate) fn is_unknown(&self) -> bool {
        *self == Self::UNKNOWN
    }
}

/// One window's persisted state.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct WindowLayout {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_px: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presence_row: bool,
    #[serde(default, skip_serializing_if = "WindowShow::is_unknown")]
    pub show: WindowShow,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PaneLayout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tabs: Vec<NativeTabRestore>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_order: Vec<TabOrderEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_item: Option<usize>,
    /// The window showed its Workers list (`docs/DESIGN-workers-tab-2026-11-02.md`), which has no
    /// item of its own: restore opens the list again and brings it to the front. With no tab
    /// beside it, it is a lone list ([`Self::is_lone_workers_list`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workers_in_front: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored_tabs: Vec<RestoredTab>,
}

impl WindowLayout {
    /// The row names no tab of any kind.
    pub(crate) fn holds_no_tab(&self) -> bool {
        self.tabs.is_empty() && self.native_tabs.is_empty() && self.restored_tabs.is_empty()
    }

    /// All a capture describes of the window is a Workers list with no worker in it: the list stood
    /// alone, or beside a private document's tab, which no capture describes. Only a handoff to a
    /// target that declares `desk1` carries such a row ([`Self::kept_in_layout`]), and a restore
    /// rebuilds it as that list with no shell.
    pub(crate) fn is_lone_workers_list(&self) -> bool {
        self.workers_in_front && self.holds_no_tab()
    }

    /// Stand a Recovery tab in for the private documents this row could not describe, which is
    /// what a handoff carries when no window has anything else ([`crate::App::capture_handoff_layout_for`]).
    /// No path or value crosses, and an empty list behind them does not come back. Neither a private
    /// document nor this tab is described by any other capture ([`crate::App::view_has_no_capture`]),
    /// so a next launch opens a new shell as ever.
    pub(crate) fn stand_in_for_private_documents(&mut self) {
        self.workers_in_front = false;
        self.active_item = Some(0);
        self.restored_tabs = vec![RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Placeholder(PlaceholderLeafRestore {
                restore_tag: PRIVATE_DOCUMENT_TAG.to_string(),
                reason: PRIVATE_DOCUMENT_CLOSED.to_string(),
                metadata: String::new(),
                unhanded: None,
            })),
            focused_path: Vec::new(),
            zoomed: false,
            parked: false,
            ephemeral: false,
            fleet: false,
        }];
    }

    /// Restoring this row leaves no terminal in the window.
    pub(crate) fn holds_no_terminal(&self) -> bool {
        if self.restored_tabs.is_empty() {
            self.tabs.is_empty()
        } else {
            self.restored_tabs
                .iter()
                .all(|tab| tab.root.first_terminal_leaf().is_none())
        }
    }

    /// Whether a capture keeps this row: a window with nothing to bring back would come back as a
    /// new shell the person never had, unless the capture carries lone lists for a target that
    /// rebuilds them.
    pub(crate) fn kept_in_layout(&self, carry_lone_lists: bool) -> bool {
        !self.holds_no_tab() || (carry_lone_lists && self.workers_in_front)
    }

    /// The handed-off shell id of the pane this window's bootstrap session fills.
    pub(crate) fn bootstrap_local_id(&self) -> Option<u64> {
        match self.bootstrap_terminal_leaf() {
            Some(leaf) => leaf.local_id,
            None => self.tabs.first()?.first_leaf().local_id(),
        }
    }

    /// The working directory of the pane this window's bootstrap session fills, so its session starts where that pane's shell was.
    pub(crate) fn bootstrap_cwd(&self) -> Option<String> {
        match self.bootstrap_tab() {
            Some(tab) => tab.root.first_terminal_with_cwd()?.start_cwd(),
            None => self.tabs.first()?.first_leaf().cwd().map(str::to_owned),
        }
    }

    fn bootstrap_terminal_leaf(&self) -> Option<&TerminalLeafRestore> {
        self.bootstrap_tab()?.root.first_terminal_leaf()
    }

    fn bootstrap_tab(&self) -> Option<&RestoredTab> {
        self.restored_tabs
            .iter()
            .find(|tab| tab.root.first_terminal_leaf().is_some())
    }

    /// Whether [`Self::tab_order`] is a valid canonical permutation of `tabs` and `native_tabs`.
    fn canonical_order_is_valid(&self) -> bool {
        const INLINE_BITS: usize = MAX_TABS_PER_WINDOW;
        const INLINE_WORDS: usize = INLINE_BITS / 64;

        fn mark_seen(
            bits: &mut [u64; INLINE_WORDS],
            overflow: &mut Vec<bool>,
            index: usize,
            len: usize,
        ) -> bool {
            if index >= len {
                return false;
            }
            if let Some(word) = bits.get_mut(index / 64) {
                let mask = 1u64 << (index % 64);
                if (*word & mask) != 0 {
                    return false;
                }
                *word |= mask;
                true
            } else {
                if overflow.is_empty() {
                    overflow.resize(len - INLINE_BITS, false);
                }
                !std::mem::replace(&mut overflow[index - INLINE_BITS], true)
            }
        }

        if self.tab_order.is_empty() {
            return self.native_tabs.is_empty();
        }
        if self.tabs.len().checked_add(self.native_tabs.len()) != Some(self.tab_order.len()) {
            return false;
        }
        let mut terminal_bits = [0u64; INLINE_WORDS];
        let mut terminal_overflow = Vec::new();
        let mut native_bits = [0u64; INLINE_WORDS];
        let mut native_overflow = Vec::new();
        for entry in &self.tab_order {
            let ok = match *entry {
                TabOrderEntry::Terminal { index } => mark_seen(
                    &mut terminal_bits,
                    &mut terminal_overflow,
                    index,
                    self.tabs.len(),
                ),
                TabOrderEntry::Native { index } => mark_seen(
                    &mut native_bits,
                    &mut native_overflow,
                    index,
                    self.native_tabs.len(),
                ),
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Validated canonical order.
    pub(crate) fn canonical_order(&self) -> Option<Vec<TabOrderEntry>> {
        if !self.canonical_order_is_valid() {
            return None;
        }
        if self.tab_order.is_empty() {
            Some(
                (0..self.tabs.len())
                    .map(|index| TabOrderEntry::Terminal { index })
                    .collect(),
            )
        } else {
            Some(self.tab_order.clone())
        }
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
            && self.canonical_order_is_valid()
            && self.active_item.is_none_or(|index| index < canonical_count)
            && self.restored_tabs.iter().all(RestoredTab::shape_is_valid)
    }

    fn sanitize(&mut self) {
        for tab in &mut self.tabs {
            tab.sanitize_leaf_metadata();
        }
        for tab in &mut self.restored_tabs {
            tab.sanitize();
        }
    }

    /// The row a capture writes for a window that holds only an empty Workers list.
    #[cfg(test)]
    pub(crate) const fn lone_workers_list(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
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
            workers_in_front: true,
            restored_tabs: Vec::new(),
        }
    }

    const fn spill_of(donor: &Self) -> Self {
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
            workers_in_front: false,
            restored_tabs: Vec::new(),
        }
    }

    /// Clear the legacy flat tab mirror fields from the manifest.
    pub(crate) fn forget_legacy_mirror(&mut self) {
        self.tabs.clear();
        self.native_tabs.clear();
        self.tab_order.clear();
        self.active_tab = 0;
    }

    /// NO POOL ID OF THE LAUNCH THAT WROTE IT on any leaf of this window.
    pub(crate) fn forget_pool_ids(&mut self) {
        for tab in &mut self.restored_tabs {
            tab.root.forget_pool_ids();
        }
        for tab in &mut self.tabs {
            tab.forget_local_ids(&|_| true);
        }
    }

    fn migrate_legacy_tabs(&mut self) -> Option<()> {
        if !self.restored_tabs.is_empty() {
            return Some(());
        }
        if !self.canonical_order_is_valid() {
            return None;
        }
        self.restored_tabs = if self.tab_order.is_empty() {
            self.tabs
                .iter()
                .map(RestoredTab::from_legacy_terminal)
                .collect::<Option<Vec<_>>>()?
        } else {
            self.tab_order
                .iter()
                .map(|entry| match *entry {
                    TabOrderEntry::Terminal { index } => {
                        RestoredTab::from_legacy_terminal(self.tabs.get(index)?)
                    }
                    TabOrderEntry::Native { index } => {
                        RestoredTab::from_legacy_native(self.native_tabs.get(index)?)
                    }
                })
                .collect::<Option<Vec<_>>>()?
        };
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
                        sid: None,
                        lineage: None,
                        log_ep: None,
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
                        autopilot: None,
                        land_at: None,
                        autopilot_by: None,
                        theme: None,
                        agent: None,
                        supervised: None,
                        lost: None,
                        held: false,
                        holds: Vec::new(),
                        claim_ended: false,
                        not_claim_worker: false,
                        claim_worker: None,
                        task: None,
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
            parked: false,
            ephemeral: false,
            fleet: false,
        })
    }

    pub(crate) fn from_legacy_native(descriptor: &NativeTabRestore) -> Option<Self> {
        let native = match descriptor {
            NativeTabRestore::Allocation => NativeLeafRestore::allocation(),
            NativeTabRestore::About => NativeLeafRestore::about(),
            NativeTabRestore::Mail => NativeLeafRestore::mail(),
            NativeTabRestore::SystemState => NativeLeafRestore::system_state(),
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
            parked: false,
            ephemeral: false,
            fleet: false,
        })
    }
}

/// What a seamless update's producer folded to bring its desk within the layout's caps ([`RestoreManifest::fold_into_caps`]).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct LayoutFold {
    #[serde(default)]
    pub windows: u32,
    #[serde(default)]
    pub tabs: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub front: Vec<u64>,
}

const MAX_FOLD_FRONT: usize = MAX_WINDOWS * MAX_TABS_PER_WINDOW;

impl LayoutFold {
    /// The default value.
    pub(crate) fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the fold moved anything.
    pub(crate) const fn folded(&self) -> bool {
        self.windows > 0 || self.tabs > 0
    }
}

/// The persisted session-restore manifest.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub(crate) struct RestoreManifest {
    pub schema: u32,
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

    /// Apply the layout caps in the writer.
    pub(crate) fn fold_into_caps(&mut self) {
        self.lone_lists_give_up_their_slots();
        let over_tab_cap = |window: &WindowLayout| window.restored_tabs.len() > MAX_TABS_PER_WINDOW;
        if self.windows.len() <= MAX_WINDOWS && !self.windows.iter().any(over_tab_cap) {
            return;
        }
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
                let [donor_win, host_win] = self
                    .windows
                    .get_disjoint_mut([donor, host])
                    .expect("spill_host_for never returns donor");
                host_win
                    .restored_tabs
                    .extend(donor_win.restored_tabs.drain(spilled.clone()));
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

    /// A lone Workers list holds nothing, so no window folds for one: only as many lists as fit
    /// beside the windows with tabs stay, the earliest first, and one left out counts in no fold.
    fn lone_lists_give_up_their_slots(&mut self) {
        let lists = self
            .windows
            .iter()
            .filter(|window| window.is_lone_workers_list())
            .count();
        let mut room = MAX_WINDOWS.saturating_sub(self.windows.len() - lists);
        self.windows.retain(|window| {
            let stays = !window.is_lone_workers_list() || room > 0;
            if window.is_lone_workers_list() && stays {
                room -= 1;
            }
            stays
        });
    }

    fn spill_host_for(&self, donor: usize) -> Option<usize> {
        let count = self.windows.len();
        (donor + 1..count)
            .chain(0..donor)
            .find(|&index| self.windows[index].restored_tabs.len() < MAX_TABS_PER_WINDOW)
            .or_else(|| (count < MAX_WINDOWS).then_some(count))
    }

    /// Whether [`Self::from_toml`] reads every window of this layout back as it is ([`Self::why_not_whole`] says nothing).
    #[cfg(test)]
    pub(crate) fn admits_whole(&self) -> bool {
        self.why_not_whole().is_none()
    }

    /// Return why [`Self::from_toml`] would reject any window in this layout.
    #[cfg(any(unix, test))]
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

    /// Whether there is nothing worth restoring: no window with a tab or a lone Workers list.
    pub(crate) fn is_empty(&self) -> bool {
        self.windows
            .iter()
            .all(|w| w.holds_no_tab() && !w.is_lone_workers_list())
    }

    /// Whether a window's row is a lone Workers list ([`WindowLayout::is_lone_workers_list`]).
    #[cfg(unix)]
    pub(crate) fn carries_lone_workers_list(&self) -> bool {
        self.windows.iter().any(WindowLayout::is_lone_workers_list)
    }

    /// The exact set of live terminal ids a seamless-update layout carries, sorted and distinct.
    pub(crate) fn seamless_terminal_ids(&self) -> Option<Vec<u64>> {
        let mut ids = Vec::new();
        for window in &self.windows {
            for tab in &window.restored_tabs {
                if !tab.root.all_terminals(&mut |terminal| {
                    let Some(id) = terminal.local_id else {
                        return false;
                    };
                    ids.push(id);
                    true
                }) {
                    return None;
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// Every canonical terminal leaf that names a handed-off shell (`local_id`), cloned, in rebuild order.
    pub(crate) fn handed_off_leaves(&self) -> Vec<TerminalLeafRestore> {
        let mut leaves = Vec::new();
        for window in &self.windows {
            for tab in &window.restored_tabs {
                tab.root.visit_terminals(&mut |terminal| {
                    if terminal.local_id.is_some() {
                        leaves.push(terminal.clone());
                    }
                });
            }
        }
        leaves
    }

    /// The handed-off shells (`local_id`) of the panes in front.
    pub(crate) fn front_terminal_ids(&self) -> Vec<u64> {
        if self.fold.folded() {
            return self.fold.front.clone();
        }
        self.windows_front_terminal_ids()
    }

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

    /// Put on every terminal leaf the agent `hosted` says its session (`local_id`) runs.
    pub(crate) fn fill_agents(&mut self, hosted: &dyn Fn(u64) -> Option<AgentRestore>) {
        self.fill_terminals(&mut |terminal| {
            if let Some(id) = terminal.local_id {
                terminal.agent = hosted(id).map(Box::new);
            }
        });
    }

    /// [`TerminalLeafRestore::mark_claim_bystander`] on every terminal leaf this run wrote from a
    /// live session, once the agents are filled and carried forward.
    pub(crate) fn mark_claim_bystanders(&mut self) {
        self.fill_terminals(&mut |terminal| {
            // A leaf with no session of this run is one a restore still owed, carried as its
            // own writer left it (`RestoreDrain::carry_owed`), whose verdict stands: a layout
            // before `claim_worker` names a running worker only by its agent.
            if terminal.local_id.is_some() {
                terminal.mark_claim_bystander();
            }
        });
    }

    /// Populate each terminal leaf with the holds reported by `holds`.
    pub(crate) fn fill_holds(&mut self, holds: &dyn Fn(u64) -> Vec<String>) {
        self.fill_terminals(&mut |terminal| {
            if let Some(id) = terminal.local_id {
                terminal.holds = holds(id);
            }
        });
    }

    fn fill_terminals(&mut self, fill: &mut impl FnMut(&mut TerminalLeafRestore)) {
        for window in &mut self.windows {
            for tab in &mut window.restored_tabs {
                tab.root.visit_terminals_mut(fill);
            }
        }
    }

    /// No terminal leaf carries an agent ([`TerminalLeafRestore::agent`]) or the tab's holds ([`TerminalLeafRestore::holds`]).
    pub(crate) fn forget_agents_and_holds(&mut self) {
        self.fill_terminals(&mut |terminal| {
            terminal.agent = None;
            terminal.holds.clear();
        });
    }

    /// The layout a seamless successor places.
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
                    tab.forget_local_ids(&|id| unhanded.binary_search(&id).is_ok());
                }
            }
        }
        if self.seamless_terminal_ids().as_deref() == Some(handed.as_slice()) {
            Ok(self)
        } else {
            Err(self)
        }
    }

    /// Every unsaved Settings draft this layout carries, in window, tab and tree order, with how many the parse dropped.
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

    /// True only when this layout names every authenticated inherited terminal exactly once and names no additional terminal.
    #[cfg(test)]
    pub(crate) fn covers_exact_seamless_ids(&self, expected: &[u64]) -> bool {
        let mut expected = expected.to_vec();
        expected.sort_unstable();
        !expected.windows(2).any(|pair| pair[0] == pair[1])
            && self.seamless_terminal_ids().as_deref() == Some(expected.as_slice())
    }

    /// The cwd of the pane the bootstrap session 0 becomes (the first window's [`WindowLayout::bootstrap_cwd`]), so its spawn starts there.
    pub(crate) fn first_leaf_cwd(&self) -> Option<String> {
        self.windows.first()?.bootstrap_cwd()
    }

    /// A layout of one window whose one terminal leaf records `log_ep`, as a capture writes it.
    #[cfg(test)]
    pub(crate) fn one_terminal_for_test(log_ep: Option<u32>) -> RestoreManifest {
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
            tabs: vec![PaneLayout::Leaf {
                cwd: None,
                title: "zsh".into(),
                focused: true,
                local_id: Some(7),
            }],
            native_tabs: Vec::new(),
            tab_order: Vec::new(),
            active_item: None,
            workers_in_front: false,
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    sid: Some("s-0123abcd".into()),
                    lineage: None,
                    log_ep,
                    cwd: None,
                    title: "zsh".into(),
                    profile: None,
                    local_id: Some(7),
                    user_title: None,
                    description: None,
                    icon: None,
                    role: None,
                    attention: None,
                    theme: None,
                    questions: None,
                    autopilot: None,
                    land_at: None,
                    autopilot_by: None,
                    agent: None,
                    supervised: None,
                    lost: None,
                    held: false,
                    holds: Vec::new(),
                    claim_ended: false,
                    not_claim_worker: false,
                    claim_worker: None,
                    task: None,
                })),
                focused_path: Vec::new(),
                zoomed: false,
                parked: false,
                ephemeral: false,
                fleet: false,
            }],
        }])
    }

    /// The largest epoch a terminal leaf of the canonical tabs recorded for its shell's log, or `None`.
    pub(crate) fn max_log_ep(&self) -> Option<u32> {
        let mut max = None;
        for window in &self.windows {
            for tab in &window.restored_tabs {
                tab.root.visit_terminals(&mut |terminal| {
                    max = max.max(terminal.log_ep);
                });
            }
        }
        max
    }

    pub(crate) fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| format!("serialize restore manifest: {e}"))
    }

    /// Fail-safe parse.
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

struct AdmittedWindows {
    kept: Vec<WindowLayout>,
    past_cap: usize,
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

/// The manifest path.
pub(crate) fn manifest_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_data_dir().map(|dir| dir.join("session.toml"))
}

/// Durably write `manifest` under the process-shared restore lock.
#[cfg(test)]
pub(crate) fn write_to(path: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    write_quit_to(path, manifest, None)
}

#[derive(Serialize, Deserialize, Default)]
struct QuitDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quit: Option<QuitTable>,
}

#[derive(Serialize, Deserialize, Default)]
struct QuitTable {
    #[serde(default)]
    run: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    also: Vec<KeptDesk>,
}

#[derive(Serialize, Deserialize)]
struct KeptDesk {
    #[serde(default)]
    run: String,
    #[serde(default)]
    windows: usize,
}

/// The run a `session.toml`'s text names ([`QuitDoc`]).
pub(crate) fn quit_run_of(text: &str) -> Option<String> {
    aterm_toml::from_str::<QuitDoc>(text)
        .ok()?
        .quit
        .map(|quit| quit.run)
        .filter(|run| !run.is_empty())
}

/// The runs whose desks a `session.toml`'s text holds behind its own quit's ([`QuitTable::also`]).
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

struct Desk {
    run: String,
    windows: Vec<WindowLayout>,
}

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

fn read_restore_text(path: &Path, max_bytes: u64) -> std::io::Result<(String, Option<u64>)> {
    use std::io::Read as _;
    let (file, meta) = aterm_uds::regular_file::open(path)?;
    if meta.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "restore file exceeds size limit",
        ));
    }
    let written_unix_ms = modified_unix_ms(&meta);
    let cap = usize::try_from(meta.len().min(max_bytes))
        .unwrap_or(0)
        .saturating_add(1);
    let mut text = String::with_capacity(cap);
    file.take(max_bytes + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "restore file exceeds size limit",
        ));
    }
    Ok((text, written_unix_ms))
}

fn other_desks(path: &Path, run: &str) -> Vec<Desk> {
    read_restore_text(path, 64 << 20)
        .ok()
        .map(|(text, _)| text)
        .as_deref()
        .and_then(desks_of)
        .unwrap_or_default()
        .into_iter()
        .filter(|desk| desk.run != run)
        .collect()
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
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
    write_quit_to_waiting(path, manifest, run, LockWait::Unbounded, Flush::Durable)
        .map_err(String::from)
}

/// [`write_quit_to`] for the cold update lane, which runs on the GUI main thread.
#[cfg(unix)]
pub(crate) fn write_quit_to_within(
    path: &Path,
    manifest: &RestoreManifest,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    write_quit_to_waiting(
        path,
        manifest,
        run,
        LockWait::Within(budget),
        Flush::Ordered,
    )
}

fn write_quit_to_waiting(
    path: &Path,
    manifest: &RestoreManifest,
    run: Option<&str>,
    wait: LockWait,
    flush: Flush,
) -> Result<(), RestoreFault> {
    made_parent(path)?;
    with_restore_lock_waiting(path, wait, || {
        let wire = match run {
            Some(run) => quit_wire(manifest, run, other_desks(path, run))?,
            None => manifest.to_toml()?,
        };
        write_restore_locked(path, wire.as_bytes(), flush)
    })
}

/// TAKE BACK A QUIT LAYOUT NO EXEC USED.
#[cfg(all(test, unix))]
pub(crate) fn unwrite_quit_to(path: &Path, run: Option<&str>) -> Result<(), String> {
    unwrite_quit_to_waiting(path, run, LockWait::Unbounded).map_err(String::from)
}

/// `unwrite_quit_to` for the cold update lane on the GUI main thread.
#[cfg(unix)]
pub(crate) fn unwrite_quit_to_within(
    path: &Path,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    unwrite_quit_to_waiting(path, run, LockWait::Within(budget))
}

#[cfg(unix)]
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
        let Some(desks) = read_restore_text(path, 64 << 20)
            .ok()
            .map(|(text, _)| text)
            .as_deref()
            .and_then(desks_of)
        else {
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
                write_restore_locked(path, wire.as_bytes(), Flush::Ordered)
            }
            None => fs::remove_file(path)
                .and_then(|()| sync_restore_directory(parent, Flush::Ordered))
                .map_err(|error| {
                    RestoreFault::io(format_args!("take back {}", path.display()), &error)
                }),
        }
    })
}

fn made_parent(path: &Path) -> Result<&Path, RestoreFault> {
    let parent = path.parent().ok_or_else(|| {
        RestoreFault::settled(format!("restore manifest {} has no parent", path.display()))
    })?;
    aterm_types::fs_restricted::create_dir_all_unshared(parent, 0o755)
        .map_err(|error| RestoreFault::io(format_args!("mkdir {}", parent.display()), &error))?;
    Ok(parent)
}

#[cfg(any(unix, test))]
fn write_wire_to(path: &Path, wire: &str) -> Result<(), String> {
    made_parent(path)?;
    with_restore_lock(path, || {
        write_restore_locked(path, wire.as_bytes(), Flush::Ordered)
    })
    .map_err(String::from)
}

/// Write `manifest` atomically to a one-shot path not shared with concurrent writers.
#[cfg(test)]
pub(crate) fn write_once_to(path: &Path, manifest: &RestoreManifest) -> Result<(), String> {
    write_wire_once_to(path, &manifest.to_toml()?)
}

/// `write_once_to` of an already serialized layout, published byte for byte.
#[cfg(any(unix, test))]
pub(crate) fn write_wire_once_to(path: &Path, wire: &str) -> Result<(), String> {
    let written = write_wire_to(path, wire);
    if let Some(lock) = restore_lock_path(path) {
        let _ = fs::remove_file(lock);
    }
    written
}

/// A quit's `session.toml` as a launch takes it ([`take_for_launch`]).
#[derive(Debug)]
pub(crate) struct TakenQuit {
    pub(crate) manifest: RestoreManifest,
    pub(crate) run: Option<String>,
    pub(crate) also: Vec<String>,
    pub(crate) written_unix_ms: Option<u64>,
}

/// When the file `meta` describes was last written, in Unix milliseconds.
pub(crate) fn modified_unix_ms(meta: &fs::Metadata) -> Option<u64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// Claim the manifest atomically, durably remove its public name, then parse the claimed bytes.
#[cfg(any(test, not(unix)))]
pub(crate) fn take_quit_from(path: &Path) -> Option<TakenQuit> {
    match take_from_result(path, LockWait::Unbounded) {
        Ok(manifest) => manifest,
        Err(error) => {
            crate::logging::stderr_line!("aterm-gui: restore manifest not consumed: {error}");
            None
        }
    }
}

fn write_restore_locked(path: &Path, bytes: &[u8], flush: Flush) -> Result<(), RestoreFault> {
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
        crate::durable::finish(&file, flush).map_err(|error| {
            RestoreFault::io(format_args!("sync {}", temporary.display()), &error)
        })?;
        drop(file);
        replace_restore_file(&temporary, path).map_err(|error| {
            RestoreFault::io(format_args!("publish {}", path.display()), &error)
        })?;
        sync_restore_directory(parent, flush).map_err(|error| {
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
        sync_restore_directory(parent, Flush::Durable).map_err(|error| {
            format!(
                "{} was claimed but single-use durability is indeterminate: {error}",
                path.display()
            )
        })?;

        let read = read_restore_text(&claim, 64 << 20);
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
        } else if let Err(error) = sync_restore_directory(parent, Flush::Ordered) {
            crate::logging::stderr_line!(
                "aterm-gui: claimed restore cleanup sync {} failed: {error}",
                parent.display()
            );
        }
        Ok(parsed)
    })
}

/// A restore-file operation that did not land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoreFault {
    message: String,
    passing: bool,
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

    /// [`Self::io`] for a failure after the file was published (its directory's sync).
    pub(crate) fn published_but(context: impl std::fmt::Display, error: &std::io::Error) -> Self {
        Self {
            published: true,
            ..Self::io(context, error)
        }
    }

    #[cfg(unix)]
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

    #[cfg_attr(
        not(unix),
        allow(
            dead_code,
            reason = "the cold update lane, which reads it, is unix-only"
        )
    )]
    pub(crate) const fn passing(&self) -> bool {
        self.passing
    }

    #[cfg_attr(
        not(unix),
        allow(
            dead_code,
            reason = "the cold update lane, which reads it, is unix-only"
        )
    )]
    pub(crate) const fn published(&self) -> bool {
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

impl From<String> for RestoreFault {
    fn from(message: String) -> Self {
        Self::settled(message)
    }
}

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

#[derive(Clone, Copy, Debug)]
enum LockWait {
    Unbounded,
    #[cfg(unix)]
    Within(std::time::Duration),
}

fn with_restore_lock_waiting<T, E: From<RestoreFault>>(
    path: &Path,
    wait: LockWait,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    with_restore_lock_seeing(path, wait, |_| operation())
}

fn with_restore_lock_seeing<T, E: From<RestoreFault>>(
    path: &Path,
    wait: LockWait,
    operation: impl FnOnce(Vec<PathBuf>) -> Result<T, E>,
) -> Result<T, E> {
    let lock_path = restore_lock_path(path).ok_or_else(|| {
        RestoreFault::settled(format!("restore manifest {} has no parent", path.display()))
    })?;
    let mut lock_opts = OpenOptions::new();
    lock_opts
        .read(true)
        .write(true)
        .create(true)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        lock_opts
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let lock = lock_opts.open(&lock_path).map_err(|error| {
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
        #[cfg(unix)]
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
    let takes = sweep_dead_restore_siblings(path);
    operation(takes)
}

fn sweep_dead_restore_siblings(path: &Path) -> Vec<PathBuf> {
    let mut takes = Vec::new();
    let Some(parent) = path.parent() else {
        return takes;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return takes;
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
        else {
            continue;
        };
        if !rest.ends_with(".tmp") {
            if rest.starts_with("taken-") || rest.starts_with("retaken-") {
                takes.push(entry.path());
            }
            continue;
        }
        let pid = ["write-", "claimed-"]
            .iter()
            .find_map(|purpose| rest.strip_prefix(purpose))
            .and_then(|tail| tail.split('-').next())
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|pid| pid.parse::<u32>().ok());
        if pid.is_some_and(|pid| pid != own && !aterm_uds::process::pid_alive(pid))
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    takes
}

fn create_unique_restore_sibling(
    parent: &Path,
    name: &OsStr,
    purpose: &str,
) -> Result<(PathBuf, File), RestoreFault> {
    let seed = next_restore_seed();
    for attempt in 0_u32..64 {
        let candidate = restore_sibling_path(parent, name, purpose, seed, attempt);
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        match opts.open(&candidate) {
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
fn sync_restore_directory(path: &Path, flush: Flush) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    crate::durable::finish(&handle, flush)
}

#[cfg(not(unix))]
fn sync_restore_directory(_path: &Path, _flush: Flush) -> std::io::Result<()> {
    Ok(())
}

/// Write the manifest to the standard [`manifest_path`] (graceful-quit hook).
#[cfg(any(windows, test))]
pub(crate) fn write(manifest: &RestoreManifest) -> Result<(), String> {
    write_quit(manifest, None)
}

/// [`write`] for the quit.
pub(crate) fn write_quit(manifest: &RestoreManifest, run: Option<&str>) -> Result<(), String> {
    let path = manifest_path().ok_or_else(no_manifest_path)?;
    write_quit_to(&path, manifest, run)
}

fn no_manifest_path() -> String {
    if aterm_types::dev_seam!("ATERM_STATE_HOME")
        .is_some_and(|root| !Path::new(&root).is_absolute())
    {
        "ATERM_STATE_HOME is not an absolute path; session restore is off".to_string()
    } else {
        "no data dir for session restore".to_string()
    }
}

/// [`write_quit_to_within`] at the standard [`manifest_path`].
#[cfg(unix)]
pub(crate) fn write_quit_within(
    manifest: &RestoreManifest,
    run: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), RestoreFault> {
    let path = manifest_path().ok_or_else(|| RestoreFault::settled(no_manifest_path()))?;
    write_quit_to_within(&path, manifest, run, budget)
}

/// [`unwrite_quit_to_within`] at the standard [`manifest_path`].
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

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TakeKind {
    Taken,
    Retaken,
}

#[cfg(unix)]
fn take_name(name: &OsStr, kind: TakeKind, run: JournalId, n: usize) -> String {
    let name = name.to_string_lossy();
    match kind {
        TakeKind::Taken => format!(".{name}.aterm-taken-{}", run.stem()),
        TakeKind::Retaken => format!(".{name}.aterm-retaken-{}-{n}", run.stem()),
    }
}

#[cfg(unix)]
fn parse_take_name(name: &OsStr, file: &str) -> Option<(TakeKind, JournalId)> {
    let rest = file.strip_prefix(&format!(".{}.aterm-", name.to_string_lossy()))?;
    if let Some(stem) = rest.strip_prefix("taken-") {
        return Some((TakeKind::Taken, JournalId::parse_stem(stem)?));
    }
    let (stem, n) = rest.strip_prefix("retaken-")?.rsplit_once('-')?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    n.parse::<usize>().ok()?;
    Some((TakeKind::Retaken, JournalId::parse_stem(stem)?))
}

/// What a launch's take holds until its run keeps the desk itself ([`settle_take`]).
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct HeldTake {
    files: Vec<(PathBuf, crate::native_document_host::HeldAdvisoryLock)>,
}

#[cfg(unix)]
impl HeldTake {
    /// Phase two.
    pub(crate) fn settle(self) {
        for (path, lock) in self.files {
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                crate::logging::stderr_line!(
                    "aterm-gui: taken restore layout {} not removed: {error}",
                    path.display()
                );
            }
            drop(lock);
        }
    }
}

#[cfg(unix)]
static HELD_TAKE: std::sync::Mutex<Option<HeldTake>> = std::sync::Mutex::new(None);

/// PHASE TWO of this run's take (`HeldTake::settle`).
pub(crate) fn settle_take() {
    #[cfg(unix)]
    if let Some(held) = HELD_TAKE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        held.settle();
    }
}

/// THE LAUNCH'S TAKE of the standard [`manifest_path`] (launch hook).
pub(crate) fn take_for_launch(run: JournalId, evidence: &Evidence<'_>) -> Option<TakenQuit> {
    let path = manifest_path()?;
    #[cfg(unix)]
    {
        let (taken, held) = take_holding_from(&path, run, evidence);
        *HELD_TAKE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(held);
        taken
    }
    #[cfg(not(unix))]
    {
        let _ = (run, evidence);
        take_quit_from(&path)
    }
}

/// `take_quit_from` in two phases, for a launch.
#[cfg(unix)]
pub(crate) fn take_holding_from(
    path: &Path,
    run: JournalId,
    evidence: &Evidence<'_>,
) -> (Option<TakenQuit>, HeldTake) {
    match take_holding_result(path, run, evidence) {
        Ok(taken) => taken,
        Err(error) => {
            crate::logging::stderr_line!("aterm-gui: restore manifest not consumed: {error}");
            (None, HeldTake::default())
        }
    }
}

#[cfg(unix)]
fn take_holding_result(
    path: &Path,
    run: JournalId,
    evidence: &Evidence<'_>,
) -> Result<(Option<TakenQuit>, HeldTake), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("restore manifest {} has no parent", path.display()));
    };
    if !parent.exists() {
        return Ok((None, HeldTake::default()));
    }
    with_restore_lock_seeing(path, LockWait::Unbounded, |left| {
        let name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("session.toml"));
        let mut held = HeldTake::default();
        let mut texts: Vec<(String, Option<u64>)> = Vec::new();
        let mut kept_by_quit: Vec<String> = Vec::new();
        let mut unheld = None;
        let mut claimed = false;
        if path.exists() {
            let taken = parent.join(take_name(name, TakeKind::Taken, run, 0));
            claim_restore_file(path, &taken)
                .map_err(|error| format!("claim {}: {error}", path.display()))?;
            claimed = true;
            let read = match hold_take(&taken) {
                Some(lock) => {
                    let read = read_take(lock.file(), &taken);
                    held.files.push((taken, lock));
                    read
                }
                None => {
                    let read = match crate::logging::open_regular_nonblock(&taken) {
                        Ok(file) => read_take(&file, &taken),
                        Err(error) => {
                            crate::logging::stderr_line!(
                                "aterm-gui: claimed restore read {} failed: {error}",
                                taken.display()
                            );
                            None
                        }
                    };
                    unheld = Some(taken);
                    read
                }
            };
            if let Some((text, written)) = read {
                kept_by_quit = quit_run_of(&text)
                    .into_iter()
                    .chain(quit_also_of(&text))
                    .collect();
                texts.push((text, written));
            }
        }
        let mut dead = Vec::new();
        for from in left {
            let Some((kind, left_by)) = from
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(|file| parse_take_name(name, file))
            else {
                continue;
            };
            let Some(lock) = hold_take(&from) else {
                continue;
            };
            let why = take_set_aside(kind, left_by, run, &kept_by_quit, parent, evidence);
            let read = read_take(lock.file(), &from);
            dead.push((from, left_by, lock, why, read));
        }
        for (_, _, _, why, read) in &dead {
            if why.is_none()
                && let Some((text, _)) = read
            {
                kept_by_quit.extend(quit_run_of(text).into_iter().chain(quit_also_of(text)));
            }
        }
        let mut retaken = 0;
        for (from, left_by, lock, why, read) in dead {
            if let Some(why) = why.or_else(|| {
                kept_by_quit
                    .contains(&left_by.stem())
                    .then_some("a layout this launch takes holds its run's desk")
            }) {
                let _ = fs::remove_file(&from);
                aterm_log::info!("restore: {} set aside: {why}", from.display());
                continue;
            }
            let to = parent.join(take_name(name, TakeKind::Retaken, run, retaken));
            if let Err(error) = fs::rename(&from, &to) {
                crate::logging::stderr_line!(
                    "aterm-gui: restore layout {} not taken again: {error}",
                    from.display()
                );
                continue;
            }
            retaken += 1;
            aterm_log::info!(
                "restore: the layout run {} took comes back ({}): it ended uncleanly before it \
                 kept its desk",
                left_by.stem(),
                to.display()
            );
            texts.extend(read);
            held.files.push((to, lock));
        }
        if (claimed || retaken > 0)
            && let Err(error) = sync_restore_directory(parent, Flush::Durable)
        {
            // Nothing comes back, as from the single-use claim: a claim that may not
            // outlive a crash must not bring back a layout that brings the machine
            // down. What this run took stays HELD — never a live run's take with its
            // lock let go for another launch to take — and goes when it settles.
            if let Some(unheld) = unheld {
                let _ = fs::remove_file(&unheld);
            }
            crate::logging::stderr_line!(
                "aterm-gui: restore manifest not consumed: {} was claimed but single-use \
                 durability is indeterminate: {error}",
                path.display()
            );
            return Ok((None, held));
        }
        if let Some(unheld) = unheld
            && fs::remove_file(&unheld).is_ok()
        {
            let _ = sync_restore_directory(parent, Flush::Ordered);
        }
        Ok((taken_together(texts), held))
    })
}

#[cfg(unix)]
fn hold_take(path: &Path) -> Option<crate::native_document_host::HeldAdvisoryLock> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let lock = crate::native_document_host::HeldAdvisoryLock::try_lock(file).ok()?;
    let locked = lock.file().metadata().ok()?;
    let named = fs::symlink_metadata(path).ok()?;
    (locked.is_file() && locked.dev() == named.dev() && locked.ino() == named.ino()).then_some(lock)
}

#[cfg(unix)]
fn read_take(file: &File, path: &Path) -> Option<(String, Option<u64>)> {
    let meta = file.metadata().ok();
    let written_unix_ms = meta.as_ref().and_then(modified_unix_ms);
    let cap = meta
        .as_ref()
        .and_then(|m| usize::try_from(m.len().min(64 << 20).saturating_add(1)).ok())
        .unwrap_or(0);
    let mut text = String::with_capacity(cap);
    let mut reader = file;
    match std::io::Read::read_to_string(&mut reader, &mut text) {
        Ok(_) => Some((text, written_unix_ms)),
        Err(error) => {
            crate::logging::stderr_line!(
                "aterm-gui: claimed restore read {} failed: {error}",
                path.display()
            );
            None
        }
    }
}

#[cfg(unix)]
fn take_set_aside(
    kind: TakeKind,
    left_by: JournalId,
    run: JournalId,
    kept_by_quit: &[String],
    dir: &Path,
    evidence: &Evidence<'_>,
) -> Option<&'static str> {
    if left_by.pid == run.pid {
        Some("this process's previous image left it, which an exec replaced")
    } else if kept_by_quit.contains(&left_by.stem()) {
        Some("its run's own quit layout holds the desk")
    } else if dir.join(left_by.file_name()).exists() {
        Some("its run's crash journal holds the desk")
    } else if crate::crash_journal::classify_death(evidence, left_by, None).is_none() {
        Some("its run ended cleanly")
    } else if kind == TakeKind::Retaken {
        Some(
            "its run had taken it again after a crash and ended uncleanly too before it kept the \
             desk: a layout that stops aterm comes back once",
        )
    } else {
        None
    }
}

#[cfg(unix)]
fn taken_together(texts: Vec<(String, Option<u64>)>) -> Option<TakenQuit> {
    let mut parsed = texts.into_iter().filter_map(|(text, written_unix_ms)| {
        RestoreManifest::from_toml(&text).map(|manifest| TakenQuit {
            manifest,
            run: quit_run_of(&text),
            also: quit_also_of(&text),
            written_unix_ms,
        })
    });
    let mut taken = parsed.next()?;
    for later in parsed {
        taken.also.extend(later.run.into_iter().chain(later.also));
        let room = MAX_WINDOWS.saturating_sub(taken.manifest.windows.len());
        taken
            .manifest
            .windows
            .extend(later.manifest.windows.into_iter().take(room));
    }
    Some(taken)
}

const CELL_METRICS_SCHEMA: u32 = 1;

const MAX_CELL_METRICS_ENTRIES: usize = 16;

const MAX_CELL_EDGE_PX: usize = 4096;

/// One scale factor's measured cell geometry.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub(crate) struct CellMetricsEntry {
    pub scale_milli: u32,
    pub font_px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// The whole cache: one font-selection fingerprint, then per-scale entries.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub(crate) struct CellMetricsCache {
    pub schema: u32,
    pub font_key: String,
    #[serde(default)]
    pub entries: Vec<CellMetricsEntry>,
}

fn cell_metrics_path() -> Option<PathBuf> {
    aterm_types::dirs::aterm_data_dir().map(|dir| dir.join("cell-metrics.toml"))
}

fn cell_metrics_scale_milli(scale: f64) -> u32 {
    (scale * 1000.0).round().clamp(1.0, 1_000_000.0) as u32
}

fn read_cell_metrics(path: &Path) -> Option<CellMetricsCache> {
    let (text, _) = read_restore_text(path, 1 << 20).ok()?;
    let cache: CellMetricsCache = aterm_toml::from_str(&text).ok()?;
    (cache.schema == CELL_METRICS_SCHEMA).then_some(cache)
}

/// The persisted cell size for `scale`.
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
    if (entry.font_px - expected_font_px).abs() >= 0.5 {
        return None;
    }
    let (cell_w, cell_h) = (entry.cell_w as usize, entry.cell_h as usize);
    ((1..=MAX_CELL_EDGE_PX).contains(&cell_w) && (1..=MAX_CELL_EDGE_PX).contains(&cell_h))
        .then_some((cell_w, cell_h))
}

/// The one display scale this machine has measured cells at (the cache's sole entry).
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

/// Persist one scale's measured cell metrics, fire-and-forget on a spawned thread.
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
    // Never persist a degenerate measurement: a 0-px or absurd cell would make the next
    // launch reveal a broken frame. The cache stays cold for this scale.
    if !(1..=MAX_CELL_EDGE_PX).contains(&cell_w) || !(1..=MAX_CELL_EDGE_PX).contains(&cell_h) {
        return Ok(());
    }
    let (Ok(cell_w), Ok(cell_h)) = (u32::try_from(cell_w), u32::try_from(cell_h)) else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| format!("cell-metrics cache {} has no parent", path.display()))?;
    aterm_types::fs_restricted::create_dir_all_unshared(parent, 0o755)
        .map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
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
        write_restore_locked(path, toml.as_bytes(), Flush::Ordered)
    })
    .map_err(String::from)
}

#[cfg(all(test, unix))]
#[path = "cold_resume_environment_tests.rs"]
mod cold_resume_environment_tests;

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
