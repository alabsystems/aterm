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

/// The current on-disk schema.
pub(crate) const SCHEMA: u32 = 2;
const LEGACY_SCHEMA: u32 = 1;

pub(crate) const MAX_WINDOWS: usize = 64;
pub(crate) const MAX_TABS_PER_WINDOW: usize = 256;
const MAX_DOCUMENT_URI_BYTES: usize = 8 * 1024;
const MAX_SETTINGS_ROUTE_BYTES: usize = 128;
const MAX_RESTORE_TAG_BYTES: usize = 128;
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
        Ok(Self::from(leaf))
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

    fn into_resume(self) -> aterm_agent::harness::self_claimed::Resume {
        aterm_agent::harness::self_claimed::Resume {
            argv: self.argv,
            session: self.session,
            codex_home: self.codex_home,
            codex_auth_path: self.codex_auth_path,
            cwd: self.cwd,
        }
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
        let mut session = self.session;
        let run = match (agent, self.run) {
            (Agent::Claude, _) => AgentRun::Claude,
            (Agent::Codex, Some(RunRestore::Codex(run))) => {
                AgentRun::Codex(aterm_agent::harness::relaunch::CodexRun {
                    home: std::path::PathBuf::from(run.home),
                    home_env: run.home_env,
                    self_claimed: run.alab.map(AlabRestore::into_resume),
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
            stored_login: false,
            profile: None,
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
                Some(RunRestore::Codex(_)) | None => {
                    agent_resumes(self.session.as_deref(), &self.argv)
                }
            }
    }

    /// Whether it names an agent this build no longer supports ([`aterm_phase::retired_agent`]):
    /// relaunched by nothing ([`Self::agent`] reads no agent), the restore saying why.
    pub(crate) fn retired(&self) -> bool {
        aterm_phase::retired_agent(&self.word)
    }

    /// Whether any relaunch brings this agent back.
    pub(crate) const fn relaunched(&self) -> bool {
        match &self.run {
            None | Some(RunRestore::Muse(_) | RunRestore::Agy(_)) => true,
            Some(RunRestore::Codex(run)) => run.alab.is_some(),
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

    /// THE LINE A PERSON TYPES IN THE REOPENED TAB TO RESUME THIS AGENT ON ITS CONVERSATION.
    pub(crate) fn resume_line(&self) -> Option<String> {
        use aterm_agent::harness::accounts::{HOME_ENV, LOGIN_ENV};
        use aterm_agent::harness::upgrade::{Agent, Dialect, is_session_id, quote};
        let word = |w: &str| -> Option<String> {
            if w.chars().any(char::is_control) {
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
        };
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
        let (env, line) = match run {
            None => (
                [
                    (HOME_ENV, self.home.as_deref()),
                    (LOGIN_ENV, self.login.as_deref()),
                ]
                .into_iter()
                .filter_map(|(name, value)| value.map(|value| (name, value)))
                .collect::<Vec<_>>(),
                aterm_agent::harness::upgrade_drive::resume_command_kept(
                    None,
                    &self.argv,
                    conversation,
                )?,
            ),
            Some(RunRestore::Codex(CodexRestore {
                alab: Some(alab), ..
            })) => (
                alab.codex_home
                    .as_deref()
                    .map(|home| ("CODEX_HOME", home))
                    .into_iter()
                    .chain(
                        alab.codex_auth_path
                            .as_deref()
                            .map(|auth| ("CODEX_AUTH_PATH", auth)),
                    )
                    .collect(),
                alab.argv
                    .iter()
                    .map(|w| word(w))
                    .collect::<Option<Vec<_>>>()?
                    .join(" "),
            ),
            Some(RunRestore::Muse(_) | RunRestore::Agy(_)) => return None,
            Some(RunRestore::Codex(CodexRestore { alab: None, .. })) => return None,
        };
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
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub(crate) struct NativeLeafRestore {
    pub restore_tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
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
    pub(crate) fn about() -> Self {
        let mut descriptor = Self::settings("/".into());
        descriptor.restore_tag = "about".into();
        descriptor.route = None;
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
            "about" => (self.route.is_some() || self.uri.is_some())
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

    fn retire_unhanded_terminals(&mut self, unhanded: &[u64]) {
        match self {
            Self::Leaf { view } => {
                let RestoredView::Terminal(terminal) = &*view else {
                    return;
                };
                if !terminal.local_id.is_some_and(|id| unhanded.contains(&id)) {
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
                first.retire_unhanded_terminals(unhanded);
                second.retire_unhanded_terminals(unhanded);
            }
        }
    }

    /// Every terminal leaf of this tree names no pool id ([`TerminalLeafRestore::local_id`]).
    pub(crate) fn forget_pool_ids(&mut self) {
        match self {
            Self::Leaf {
                view: RestoredView::Terminal(terminal),
            } => terminal.local_id = None,
            Self::Leaf { .. } => {}
            Self::Split { first, second, .. } => {
                first.forget_pool_ids();
                second.forget_pool_ids();
            }
        }
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
            Self::About | Self::Settings { .. } | Self::SystemState => None,
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
            Self::About | Self::SystemState => true,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored_tabs: Vec<RestoredTab>,
}

impl WindowLayout {
    /// The handed-off shell id of the pane this window's bootstrap session fills.
    pub(crate) fn bootstrap_local_id(&self) -> Option<u64> {
        match self.bootstrap_terminal_leaf() {
            Some(leaf) => leaf.local_id,
            None => self.tabs.first()?.leaves().first()?.local_id(),
        }
    }

    /// The working directory of the pane this window's bootstrap session fills, so its session starts where that pane's shell was.
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

    fn bootstrap_terminal_leaf(&self) -> Option<&TerminalLeafRestore> {
        self.bootstrap_tab()?.root.first_terminal_leaf()
    }

    fn bootstrap_tab(&self) -> Option<&RestoredTab> {
        self.restored_tabs
            .iter()
            .find(|tab| tab.root.first_terminal_leaf().is_some())
    }

    /// Validated canonical order.
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

    fn sanitize(&mut self) {
        for tab in &mut self.tabs {
            tab.sanitize_leaf_metadata();
        }
        for tab in &mut self.restored_tabs {
            tab.sanitize();
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
        })
    }

    pub(crate) fn from_legacy_native(descriptor: &NativeTabRestore) -> Option<Self> {
        let native = match descriptor {
            NativeTabRestore::About => NativeLeafRestore::about(),
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

    /// Whether there is anything worth restoring (at least one window with one tab).
    pub(crate) fn is_empty(&self) -> bool {
        self.windows
            .iter()
            .all(|w| w.tabs.is_empty() && w.native_tabs.is_empty() && w.restored_tabs.is_empty())
    }

    /// The exact set of live terminal ids a seamless-update layout carries, sorted and distinct.
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

    /// Every canonical terminal leaf that names a handed-off shell (`local_id`), cloned, in rebuild order.
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

    /// Populate each terminal leaf with the holds reported by `holds`.
    pub(crate) fn fill_holds(&mut self, holds: &dyn Fn(u64) -> Vec<String>) {
        self.fill_terminals(&mut |terminal| {
            if let Some(id) = terminal.local_id {
                terminal.holds = holds(id);
            }
        });
    }

    fn fill_terminals(&mut self, fill: &mut dyn FnMut(&mut TerminalLeafRestore)) {
        fn walk(node: &mut RestoredSplitTree, fill: &mut dyn FnMut(&mut TerminalLeafRestore)) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => fill(terminal),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, fill);
                    walk(second, fill);
                }
            }
        }
        for window in &mut self.windows {
            for tab in &mut window.restored_tabs {
                walk(&mut tab.root, fill);
            }
        }
    }

    /// No terminal leaf carries an agent ([`TerminalLeafRestore::agent`]) or the tab's holds ([`TerminalLeafRestore::holds`]).
    pub(crate) fn forget_agents_and_holds(&mut self) {
        fn forget(node: &mut RestoredSplitTree) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => {
                    terminal.agent = None;
                    terminal.holds.clear();
                }
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    forget(first);
                    forget(second);
                }
            }
        }
        for window in &mut self.windows {
            for tab in &mut window.restored_tabs {
                forget(&mut tab.root);
            }
        }
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
                    tab.forget_local_ids(&|id| unhanded.contains(&id));
                }
            }
        }
        if self.covers_exact_seamless_ids(expected) {
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
                })),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        }])
    }

    /// The largest epoch a terminal leaf of the canonical tabs recorded for its shell's log, or `None`.
    pub(crate) fn max_log_ep(&self) -> Option<u32> {
        fn walk(node: &RestoredSplitTree, max: &mut Option<u32>) {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(terminal),
                } => *max = (*max).max(terminal.log_ep),
                RestoredSplitTree::Leaf { .. } => {}
                RestoredSplitTree::Split { first, second, .. } => {
                    walk(first, max);
                    walk(second, max);
                }
            }
        }
        let mut max = None;
        for window in &self.windows {
            for tab in &window.restored_tabs {
                walk(&tab.root, &mut max);
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
    crate::durable::finish(&File::open(path)?, flush)
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
                    let read = match File::open(&taken) {
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let lock = crate::native_document_host::HeldAdvisoryLock::try_lock(file).ok()?;
    let locked = lock.file().metadata().ok()?;
    let named = fs::symlink_metadata(path).ok()?;
    (locked.is_file() && locked.dev() == named.dev() && locked.ino() == named.ino()).then_some(lock)
}

#[cfg(unix)]
fn read_take(file: &File, path: &Path) -> Option<(String, Option<u64>)> {
    let written_unix_ms = file.metadata().ok().as_ref().and_then(modified_unix_ms);
    let mut text = String::new();
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
    let text = fs::read_to_string(path).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn the_cold_lanes_layout_write_waits_a_bounded_time_for_the_restore_lock() {
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("session.toml");
        let budget = std::time::Duration::from_millis(100);
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
        let fault = took.expect_err("the take-back is bounded too");
        assert!(fault.passing(), "{fault}");
        assert!(!path.exists(), "nothing was written");
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
        assert_eq!(
            take_from(&path).map(|manifest| manifest.windows.len()),
            Some(2)
        );
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("written");
        unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
        assert!(!path.exists(), "its own desk, taken back");
    }

    #[cfg(unix)]
    #[test]
    fn the_restore_writes_name_their_flush() {
        use aterm_durable::seam::Recording;
        const O: Flush = Flush::Ordered;
        const D: Flush = Flush::Durable;
        let dir = aterm_tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("session.toml");
        let budget = std::time::Duration::from_secs(60);
        let recording = Recording::start();
        write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
        assert_eq!(recording.take(), [D, D], "the quit's file and its entry");
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("cold lane");
        assert_eq!(
            recording.take(),
            [O, O],
            "the cold lane's file and its entry"
        );
        unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
        assert_eq!(recording.take(), [O], "the take-back's removal");
        write_quit_to(&path, &sample(), Some("1-1")).expect("the other quit");
        write_quit_to_within(&path, &sample(), Some("1-2"), budget).expect("cold lane");
        let _ = recording.take();
        unwrite_quit_to_within(&path, Some("1-2"), budget).expect("taken back");
        assert_eq!(
            recording.take(),
            [O, O],
            "the take-back's rewrite and its entry"
        );
        assert!(path.exists(), "the other instance's desk stays");
        assert!(
            take_quit_from(&path).is_some(),
            "the single-use take takes it"
        );
        assert_eq!(recording.take(), [D, O], "the claim and the cleanup");
        write_quit_to(&path, &sample(), Some("1-1")).expect("the other quit");
        let _ = recording.take();
        let run = crate::crash_journal::JournalId::now(std::process::id());
        let (taken, held) = take_holding_from(&path, run, &no_evidence());
        assert!(taken.is_some(), "the launch takes it");
        assert_eq!(
            recording.take(),
            [D],
            "the claim alone: the take stays until settled"
        );
        held.settle();
        assert!(
            recording.take().is_empty(),
            "the settle's removal syncs nothing"
        );
        let sidecar = dir.path().join("seamless-1-2.layout.toml");
        write_wire_once_to(&sidecar, &sample().to_toml().expect("wire")).expect("sidecar");
        assert_eq!(
            recording.take(),
            [O, O],
            "the handoff's sidecar and its entry"
        );
        let metrics = dir.path().join("cell-metrics.toml");
        store_cell_metrics_to(&metrics, "font", 2.0, 13.0, 8, 16).expect("metrics");
        assert_eq!(recording.take(), [O, O], "the cell-metrics cache");
    }

    #[cfg(unix)]
    fn no_evidence() -> Evidence<'static> {
        Evidence {
            log_dir: None,
            swept: &[],
        }
    }

    #[cfg(unix)]
    fn evidence_in(logs: &Path) -> Evidence<'_> {
        Evidence {
            log_dir: Some(logs),
            swept: &[],
        }
    }

    #[cfg(unix)]
    fn takes_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("the data dir")
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| parse_take_name(OsStr::new("session.toml"), name).is_some())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    fn take_of(kind: TakeKind, run: JournalId, n: usize) -> String {
        take_name(OsStr::new("session.toml"), kind, run, n)
    }

    #[cfg(unix)]
    fn dead_run() -> JournalId {
        JournalId::now(crate::crash_journal::tests::reaped_pid())
    }

    #[cfg(unix)]
    #[test]
    fn a_launch_take_is_held_until_its_run_settles_and_never_taken_while_it_lives() {
        use crate::crash_journal::tests::{marker, scratch};
        let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
        let path = dir.path().join("session.toml");
        write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
        let a = dead_run();
        let (taken, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
        let taken = taken.expect("the quit's layout");
        assert_eq!(taken.manifest, sample());
        assert_eq!(taken.run.as_deref(), Some("1-2"));
        assert!(!path.exists(), "no longer under its public name");
        assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
        marker(logs.path(), a);
        let (beside, beside_held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert!(beside.is_none(), "a live run's take is never taken");
        assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
        drop(beside_held);
        held.settle();
        assert!(takes_in(dir.path()).is_empty(), "settled, nothing is left");
        let (after, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert!(after.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_runs_take_comes_back_once_when_its_run_ended_uncleanly() {
        use crate::crash_journal::tests::{cwds, layout, marker, scratch};
        let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
        let path = dir.path().join("session.toml");
        let lost = layout(&[("/work/lost", "zsh")]);
        let quit = layout(&[("/work/quit", "zsh")]);

        write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
        assert!(take_quit_from(&path).is_some(), "a single-use take");
        assert!(
            take_holding_from(&path, dead_run(), &evidence_in(logs.path()))
                .0
                .is_none(),
            "the single-use take left nothing behind"
        );

        write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
        let a = dead_run();
        let (_, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
        marker(logs.path(), a);
        drop(held); // A dies before it settles: its lock goes with it.
        write_quit_to(&path, &quit, Some("7-8")).expect("another run's quit");
        let b = dead_run();
        let (taken, held) = take_holding_from(&path, b, &evidence_in(logs.path()));
        let taken = taken.expect("B takes both");
        assert_eq!(cwds(&taken.manifest), ["/work/quit", "/work/lost"]);
        assert_eq!(taken.run.as_deref(), Some("7-8"), "the quit's run leads");
        assert_eq!(taken.also, ["1-2"], "the run A's layout was the quit of");
        assert_eq!(
            takes_in(dir.path()),
            [
                take_of(TakeKind::Retaken, b, 0),
                take_of(TakeKind::Taken, b, 0)
            ]
        );

        marker(logs.path(), b);
        drop(held); // B dies before it settles too.
        let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        let taken = taken.expect("the quit layout B took first comes back");
        assert_eq!(
            cwds(&taken.manifest),
            ["/work/quit"],
            "and the layout B took again does not"
        );
        assert_eq!(
            takes_in(dir.path()).len(),
            1,
            "set aside: {:?}",
            takes_in(dir.path())
        );
        held.settle();
        assert!(takes_in(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_runs_take_goes_when_its_end_was_clean_or_its_desk_is_held_elsewhere() {
        use crate::crash_journal::tests::{cwds, layout, marker, scratch};
        let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
        let path = dir.path().join("session.toml");
        let lost = layout(&[("/work/lost", "zsh")]);
        let died_holding = |a: JournalId, marked: bool| {
            write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
            drop(take_holding_from(&path, a, &no_evidence()).1);
            if marked {
                marker(logs.path(), a);
            }
        };

        died_holding(dead_run(), false);
        let (taken, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert!(taken.is_none(), "a clean end's take goes");
        assert!(takes_in(dir.path()).is_empty());

        let a = dead_run();
        died_holding(a, true);
        let own = layout(&[("/work/own", "zsh")]);
        write_quit_to(&path, &own, Some(&a.stem())).expect("A's own quit, cut short");
        let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert_eq!(cwds(&taken.expect("A's quit").manifest), ["/work/own"]);
        held.settle();
        assert!(takes_in(dir.path()).is_empty(), "A's take went");

        let a = dead_run();
        died_holding(a, true);
        fs::write(dir.path().join(a.file_name()), b"").expect("A's crash journal");
        let (taken, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert!(taken.is_none(), "A's journal holds the desk");
        assert!(takes_in(dir.path()).is_empty());
        fs::remove_file(dir.path().join(a.file_name())).expect("A's crash journal");

        let a = dead_run();
        died_holding(a, true);
        let next_image = JournalId {
            pid: a.pid,
            nanos: a.nanos + 1,
        };
        let (taken, _) = take_holding_from(&path, next_image, &evidence_in(logs.path()));
        assert!(taken.is_none(), "this process's previous image's take goes");
        assert!(takes_in(dir.path()).is_empty());

        write_quit_to(&path, &lost, Some("1-2")).expect("the quit's layout");
        let y = dead_run();
        let (_, y_held) = take_holding_from(&path, y, &no_evidence());
        write_quit_to(&path, &own, Some(&y.stem())).expect("Y's quit");
        let a = dead_run();
        let (_, a_held) = take_holding_from(&path, a, &no_evidence());
        assert_eq!(takes_in(dir.path()).len(), 2, "both held");
        for (run, held) in [(y, y_held), (a, a_held)] {
            marker(logs.path(), run);
            drop(held);
        }
        let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert_eq!(
            cwds(&taken.expect("A's take").manifest),
            ["/work/own"],
            "Y's desk once, never what Y took beside it"
        );
        held.settle();
        assert!(takes_in(dir.path()).is_empty(), "Y's take went");

        died_holding(dead_run(), true);
        let (taken, held) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert_eq!(cwds(&taken.expect("comes back").manifest), ["/work/lost"]);
        held.settle();
    }

    #[cfg(unix)]
    #[test]
    fn a_take_whose_sync_fails_restores_nothing_and_holds_what_it_took() {
        use crate::crash_journal::tests::{marker, scratch};
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, logs) = (scratch("rt-dir"), scratch("rt-logs"));
        let path = dir.path().join("session.toml");
        write_quit_to(&path, &sample(), Some("1-2")).expect("the quit's layout");
        let a = dead_run();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o300)).expect("chmod");
        let (taken, held) = take_holding_from(&path, a, &evidence_in(logs.path()));
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(
            taken.is_none(),
            "nothing comes back past an indeterminate claim"
        );
        assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
        marker(logs.path(), a);
        let (beside, _) = take_holding_from(&path, dead_run(), &evidence_in(logs.path()));
        assert!(beside.is_none(), "a live run's take is never taken");
        assert_eq!(takes_in(dir.path()), [take_of(TakeKind::Taken, a, 0)]);
        held.settle();
        assert!(takes_in(dir.path()).is_empty());
    }

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
        }
    }

    fn terminal(id: Option<u64>, title: &str) -> RestoredSplitTree {
        RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
            sid: None,
            lineage: None,
            log_ep: None,
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
            autopilot: None,
            land_at: None,
            autopilot_by: None,
            theme: None,
            agent: None,
            supervised: None,
            lost: None,
            held: false,
            holds: Vec::new(),
        }))
    }

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

    #[test]
    fn the_pane_in_front_is_each_windows_active_tabs_focused_terminal() {
        let mut desk = held_desk(Some(7));
        assert_eq!(desk.windows[0].active_item, Some(1));
        assert!(desk.front_terminal_ids().is_empty());
        desk.windows[0].active_item = Some(0);
        assert_eq!(desk.front_terminal_ids(), vec![7]);
        desk.windows[0].restored_tabs[0].focused_path = vec![RestoreBranch::First];
        assert_eq!(desk.front_terminal_ids(), vec![0]);
        let mut other = held_desk(Some(9)).windows.remove(0);
        other.active_item = Some(0);
        desk.windows.push(other);
        assert_eq!(desk.front_terminal_ids(), vec![0, 9]);
        desk.windows[1].restored_tabs[0].root = RestoredSplitTree::Split {
            axis: SplitKind::Vertical,
            ratio: 0.5,
            first: Box::new(terminal(Some(8), "zsh")),
            second: Box::new(terminal(None, "build")),
        };
        desk.windows[0].active_item = None;
        assert!(desk.front_terminal_ids().is_empty());
    }

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

        let exact = RestoreManifest::from_toml(&held_desk(Some(7)).to_toml().unwrap()).unwrap();
        assert_eq!(exact.clone().placed_for_handed(&[0, 7]), Ok(exact));
    }

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

    #[test]
    fn a_layout_naming_a_session_in_two_windows_is_placed_whole() {
        let mut desk = held_desk(Some(7));
        desk.windows.push(shells_window(0..1));
        let parsed = RestoreManifest::from_toml(&desk.to_toml().unwrap())
            .expect("the successor parses a co-viewed layout");
        assert_eq!(parsed.seamless_terminal_ids(), Some(vec![0, 7]));
        let placed = parsed
            .placed_for_handed(&[0, 7])
            .expect("a co-viewed session does not unplace the layout");
        assert_eq!(placed.windows.len(), 2, "both windows are kept");
    }

    fn sample() -> RestoreManifest {
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

    fn settings_handoff_layout(drafts: Vec<SettingsDraftRestore>) -> RestoreManifest {
        let mut leaf = NativeLeafRestore::settings("/text-fonts".to_string());
        leaf.settings_drafts = drafts;
        let mut window = shells_window(0..0);
        window.restored_tabs.push(RestoredTab {
            root: RestoredSplitTree::leaf(RestoredView::Native(leaf)),
            focused_path: Vec::new(),
            zoomed: false,
        });
        RestoreManifest::new(vec![window])
    }

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

        let mut unknown = sample();
        for window in &mut unknown.windows {
            window.maximized = None;
        }
        let wire = unknown.to_toml().expect("serialize");
        assert!(
            !wire.contains("show") && !wire.contains("z_order") && !wire.contains("maximized"),
            "an unknown window state must not reach the wire:\n{wire}"
        );
        let back = RestoreManifest::from_toml(&wire).expect("parse");
        assert!(back.windows.iter().all(|w| w.show.is_unknown()));
        assert!(back.windows.iter().all(|w| w.maximized.is_none()));
    }

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
    fn mixed_native_descriptors_round_trip_in_their_order() {
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
        assert!(matches!(
            sample().windows[0].restored_tabs[1].root,
            RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ..
            }
        ));
    }

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
        let admitted = AdmittedWindows::of(manifest.windows.clone());
        assert_eq!((admitted.past_cap, admitted.unbuildable), (1, 1));
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

        assert!(!folded.windows_front_terminal_ids().contains(&313));
        assert_eq!(folded.front_terminal_ids(), folded.fold.front);
        let mut hostile = folded;
        hostile.fold.front = vec![7; MAX_FOLD_FRONT + 5];
        let read = RestoreManifest::from_toml(&hostile.to_toml().unwrap()).expect("reads");
        assert_eq!(read.fold.front.len(), MAX_FOLD_FRONT);
    }

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
        assert!(folded.windows_front_terminal_ids().contains(&255));
        assert_eq!(folded.front_terminal_ids(), vec![280, 300, 556]);
    }

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
        assert!(full.why_not_whole().is_some(), "the refusal says why");
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
        deep.restored_tabs[0].focused_path = vec![RestoreBranch::Second];
        let mut deep = RestoreManifest::new(vec![deep]);
        let before = deep.clone();
        deep.fold_into_caps();
        assert_eq!(
            deep, before,
            "the fold moves tabs whole; it does not split a tab"
        );
        assert!(!deep.admits_whole());
        assert!(deep.why_not_whole().is_some(), "the refusal says why");
    }

    #[test]
    fn system_state_restore_descriptors_round_trip_without_document_authority() {
        let native = NativeLeafRestore::system_state();
        assert_eq!(native.validation_error(), None);
        let bytes = aterm_json::to_vec(&native).unwrap();
        assert_eq!(
            aterm_json::from_slice::<NativeLeafRestore>(&bytes).unwrap(),
            native
        );
        let legacy = NativeTabRestore::SystemState;
        let bytes = aterm_json::to_vec(&legacy).unwrap();
        assert_eq!(
            aterm_json::from_slice::<NativeTabRestore>(&bytes).unwrap(),
            legacy
        );
        assert_eq!(
            RestoredTab::from_legacy_native(&legacy).unwrap().root,
            RestoredSplitTree::leaf(RestoredView::Native(native.clone()))
        );
        let mut invalid = native;
        invalid.uri = Some("file:///tmp/unrequested".into());
        assert!(invalid.validation_error().is_some());
    }

    #[test]
    fn about_restore_descriptors_round_trip_without_document_or_settings_authority() {
        let native = NativeLeafRestore::about();
        assert_eq!(native.validation_error(), None);
        let bytes = aterm_json::to_vec(&native).unwrap();
        assert_eq!(
            aterm_json::from_slice::<NativeLeafRestore>(&bytes).unwrap(),
            native
        );
        let legacy = NativeTabRestore::About;
        assert!(legacy.shape_is_valid());
        assert_eq!(legacy.document_uri(), None);
        let bytes = aterm_json::to_vec(&legacy).unwrap();
        assert_eq!(
            aterm_json::from_slice::<NativeTabRestore>(&bytes).unwrap(),
            legacy
        );
        assert_eq!(
            RestoredTab::from_legacy_native(&legacy).unwrap().root,
            RestoredSplitTree::leaf(RestoredView::Native(native.clone()))
        );
        for (route, uri) in [
            (Some("/about".to_string()), None),
            (None, Some("file:///tmp/unrequested".to_string())),
        ] {
            let mut invalid = native.clone();
            invalid.route = route;
            invalid.uri = uri;
            assert!(invalid.validation_error().is_some());
        }
        let settings = NativeTabRestore::Settings {
            route: "/about".into(),
        };
        assert!(legacy.joins_mirror(std::slice::from_ref(&settings)));
        assert!(settings.joins_mirror(std::slice::from_ref(&legacy)));
        assert!(!legacy.joins_mirror(std::slice::from_ref(&legacy)));
    }

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
            assert!(!refused.admits_whole(), "refused");
            let read = RestoreManifest::from_toml(&refused.to_toml().unwrap()).expect("one reads");
            assert_eq!(read.windows.len(), 1, "the parse leaves that window out");
        }
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
    fn unknown_native_metadata_is_cut_at_a_utf8_boundary() {
        let mut native = NativeLeafRestore::document("future.diagram", "file:///tmp/é".into());
        let prefix_len = native.copyable_metadata().len() + "\nmetadata=".len();
        native.metadata = format!("{}é", "x".repeat(MAX_VIEW_METADATA_BYTES - prefix_len - 1));
        let mut view = RestoredView::Native(native);
        view.sanitize();
        let RestoredView::Placeholder(placeholder) = view else {
            panic!("the unavailable native app must retain a placeholder");
        };
        assert_eq!(placeholder.metadata.len(), MAX_VIEW_METADATA_BYTES - 1);
        assert!(placeholder.metadata.contains("file:///tmp/é"));
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
                    metadata,
                    ..
                })
            } if restore_tag == "future.diagram" && metadata.contains("copy-me")
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
                view: RestoredView::Placeholder(_)
            }
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
            second: Box::new(terminal(None, "shell")),
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

        let mut root = terminal(None, "");
        for _ in 0..=MAX_SPLIT_DEPTH {
            root = RestoredSplitTree::Split {
                axis: SplitKind::Horizontal,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(terminal(None, "")),
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

    #[test]
    fn a_nul_cwd_leaf_round_trips_sanitized() {
        let hostile = || {
            let mut window = shells_window(7..8);
            let PaneLayout::Leaf { cwd, title, .. } = &mut window.tabs[0] else {
                unreachable!("a legacy leaf")
            };
            *cwd = Some("/tmp/a\0b".into());
            *title = "zsh\0 ~".into();
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } = &mut window.restored_tabs[0].root
            else {
                unreachable!("a terminal leaf")
            };
            leaf.cwd = Some("/tmp/a\0b".into());
            leaf.title = "zsh\0 ~".into();
            window
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

        let raw = RestoreManifest {
            schema: SCHEMA,
            fold: LayoutFold::default(),
            windows: vec![hostile()],
        };
        let wire = aterm_toml::to_string(&raw).expect("serialize the hostile fixture");
        let parsed = RestoreManifest::from_toml(&wire).expect("a NUL directory is recoverable");
        assert_clean(&parsed, "parse");

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

    #[test]
    fn a_leaf_names_its_logs_epoch_and_the_layout_says_the_largest() {
        let named = RestoreManifest::one_terminal_for_test(Some(1_792_000_123));
        let wire = named.to_toml().expect("serialize");
        assert!(wire.contains("log_ep = 1792000123"), "{wire}");
        let parsed = RestoreManifest::from_toml(&wire).expect("parse");
        assert_eq!(parsed, named, "the round trip holds");
        assert_eq!(parsed.max_log_ep(), Some(1_792_000_123));
        let older = wire.replace("log_ep = 1792000123\n", "");
        assert_ne!(older, wire, "PRECONDITION: the key is out");
        assert_eq!(
            RestoreManifest::from_toml(&older).map(|m| m.max_log_ep()),
            Some(None),
            "an older writer's leaf names none, and parses"
        );
        let plain = RestoreManifest::one_terminal_for_test(None)
            .to_toml()
            .expect("serialize");
        assert!(!plain.contains("log_ep"), "{plain}");
        let mut two = RestoreManifest::one_terminal_for_test(Some(7));
        two.windows
            .extend(RestoreManifest::one_terminal_for_test(Some(9)).windows);
        two.windows
            .extend(RestoreManifest::one_terminal_for_test(None).windows);
        assert_eq!(two.max_log_ep(), Some(9), "the largest of every window's");
        assert_eq!(
            RestoreManifest::one_terminal_for_test(None).max_log_ep(),
            None,
            "NEGATIVE CONTROL: no epoch, none said"
        );
    }

    #[test]
    fn a_leaf_names_its_session_and_lineage_and_older_peers_read_past_them() {
        let window = |sid: Option<String>, lineage: Option<String>| WindowLayout {
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
            restored_tabs: vec![RestoredTab {
                root: RestoredSplitTree::leaf(RestoredView::Terminal(TerminalLeafRestore {
                    sid,
                    lineage,
                    log_ep: None,
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
                })),
                focused_path: Vec::new(),
                zoomed: false,
            }],
        };
        let leaf_of = |m: &RestoreManifest| match &m.windows[0].restored_tabs[0].root {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(t),
            } => (t.sid.clone(), t.lineage.clone()),
            _ => panic!("a terminal leaf"),
        };
        let named = RestoreManifest::new(vec![window(
            Some("s-0123abcd".into()),
            Some("s-00ff".into()),
        )]);
        let wire = named.to_toml().expect("serialize");
        assert!(wire.contains("sid = \"s-0123abcd\""), "{wire}");
        assert!(wire.contains("lineage = \"s-00ff\""), "{wire}");
        let parsed = RestoreManifest::from_toml(&wire).expect("parse");
        assert_eq!(parsed, named, "the round trip holds");
        let plain = RestoreManifest::new(vec![window(None, None)])
            .to_toml()
            .expect("serialize");
        assert!(
            !plain.contains("sid =") && !plain.contains("lineage ="),
            "a leaf without them is written as before: {plain}"
        );
        let newer = wire.replace(
            "sid = \"s-0123abcd\"",
            "sid = \"s-0123abcd\"\nfuture_field = \"from a newer peer\"\nidentity = \"worker\"",
        );
        assert_ne!(newer, wire, "PRECONDITION: the unknown field is in");
        assert_eq!(
            RestoreManifest::from_toml(&newer).map(|m| leaf_of(&m)),
            Some(leaf_of(&named)),
            "a field this build does not know is read past"
        );
        let rewritten = RestoreManifest::from_toml(&newer)
            .expect("read past unknown fields")
            .to_toml()
            .expect("serialize");
        assert!(!rewritten.contains("identity ="), "{rewritten}");
        for (sid, lineage) in [
            ("s-ab\0cd".to_string(), format!("s-{}", "a".repeat(1 << 20))),
            (format!("s-{}", "0".repeat(48)), "s-xyz".to_string()),
            (String::new(), "-".to_string()),
        ] {
            let raw = RestoreManifest {
                schema: SCHEMA,
                fold: LayoutFold::default(),
                windows: vec![window(Some(sid.clone()), Some(lineage.clone()))],
            };
            let wire = aterm_toml::to_string(&raw).expect("serialize the hostile fixture");
            let parsed = RestoreManifest::from_toml(&wire).expect("a hostile sid is recoverable");
            assert_eq!(
                leaf_of(&parsed),
                (None, None),
                "{sid:.12?} / {lineage:.12?}"
            );
            let captured = RestoreManifest::new(vec![window(Some(sid), Some(lineage))]);
            assert_eq!(leaf_of(&captured), (None, None));
        }
    }

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
            let mut window = shells_window(1..2);
            window.tabs.clear();
            let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } = &mut window.restored_tabs[0].root
            else {
                unreachable!("a terminal leaf");
            };
            leaf.cwd = Some(cwd.to_owned());
            leaf.held = held;
            RestoreManifest::new(vec![window])
        };
        let wire = |manifest: &RestoreManifest| manifest.to_toml().expect("serializes");

        let unheld = window(&remote, false);
        assert!(!wire(&unheld).contains("held ="), "{}", wire(&unheld));
        let held = window(&remote, true);
        assert!(wire(&held).contains("held = true"), "{}", wire(&held));
        let back = RestoreManifest::from_toml(&wire(&held)).expect("parses");
        assert_eq!(back, held, "held survives the wire");

        assert_eq!(
            back.first_leaf_cwd().as_deref(),
            (!cfg!(unix)).then_some(remote.as_str()),
            "the held pane's remote folder"
        );
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
                word: String::new(),
                pid: 4242,
                start: "Sat Sep 27 01:02:03 2026".into(),
                program: "/opt/claude/bin/claude".into(),
                argv: vec!["/opt/claude/bin/claude".into()],
                session: None,
                cwd: remote.clone(),
                version: None,
                home: None,
                login: None,
                run: None,
                cause: None,
                tab: None,
            }));
        }
        assert_eq!(
            hosted.first_leaf_cwd().as_deref(),
            Some(remote.as_str()),
            "the agent aterm hosted ran on this machine: its gone folder is asked for, and \
             the spawn says it was not found"
        );
    }

    #[test]
    fn parse_is_fail_safe() {
        assert!(RestoreManifest::from_toml("this is not toml {{{").is_none());
        assert!(RestoreManifest::from_toml("").is_none());
        let mut m = sample();
        m.schema = 999;
        let toml = aterm_toml::to_string(&m).unwrap();
        assert!(
            RestoreManifest::from_toml(&toml).is_none(),
            "stale schema is ignored"
        );
    }

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

        #[cfg(unix)]
        {
            write_to(&path, &manifest).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let mut takers = Vec::new();
            for _ in 0..2 {
                let path = path.clone();
                let barrier = barrier.clone();
                let run = dead_run();
                takers.push(std::thread::spawn(move || {
                    barrier.wait();
                    take_holding_from(&path, run, &no_evidence())
                }));
            }
            barrier.wait();
            let results = takers
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                results.iter().filter(|(taken, _)| taken.is_some()).count(),
                1
            );
            assert!(
                results
                    .iter()
                    .filter_map(|(taken, _)| taken.as_ref())
                    .all(|taken| taken.manifest == manifest)
            );
            assert!(!path.exists());
            assert_eq!(takes_in(&dir).len(), 1, "the winner's take, held");
            for (_, held) in results {
                held.settle();
            }
            assert!(takes_in(&dir).is_empty());
        }

        let model = aterm_models_native::restore_manifest_single_use_model();
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
            assert!(write(&RestoreManifest::new(Vec::new())).is_err());
        });
        crate::test_env::scoped_unset("ATERM_STATE_HOME", || {
            let data = aterm_types::dirs::aterm_data_dir();
            assert_eq!(
                data,
                aterm_types::dirs::test_root().map(|root| root.join("state")),
                "never the person's folder"
            );
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
        assert_eq!(
            load_cell_metrics_from(&path, "JetBrains Mono|lh=1", 2.0, 24.0),
            None
        );

        let before = std::fs::read_to_string(&path).unwrap();
        store_cell_metrics_to(&path, "JetBrains Mono|lh=1", 1.0, 12.0, 7, 15).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

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

    #[test]
    fn cell_metrics_staleness_gates_fail_closed() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-cell-metrics-staleness-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cell-metrics.toml");

        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        std::fs::write(&path, "not toml [").unwrap();
        assert_eq!(load_cell_metrics_from(&path, "k", 1.0, 12.0), None);
        let _ = std::fs::remove_file(&path);

        store_cell_metrics_to(&path, "Cascadia|lh=1", 1.0, 12.0, 7, 15).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        store_cell_metrics_to(&path, "Fira|lh=1", 2.0, 24.0, 13, 29).unwrap();
        assert_eq!(
            load_cell_metrics_from(&path, "Cascadia|lh=1", 1.0, 12.0),
            None
        );
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0),
            Some((13, 29))
        );
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 25.0), None);
        assert_eq!(
            load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.2),
            Some((13, 29))
        );

        store_cell_metrics_to(&path, "Fira|lh=1", 3.0, 36.0, 0, 15).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 3.0, 36.0), None);
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("cell_w = 13", "cell_w = 99999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 2.0, 24.0), None);

        let _ = std::fs::remove_file(&path);
        store_cell_metrics_to(&path, "Fira|lh=1", 1.0, 12.0, 7, 15).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("schema = 1", "schema = 999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load_cell_metrics_from(&path, "Fira|lh=1", 1.0, 12.0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn numbered_sample() -> RestoreManifest {
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
        let mut m = sample();
        let mut next = 0;
        for w in &mut m.windows {
            for t in &mut w.restored_tabs {
                number(&mut t.root, &mut next);
            }
        }
        m
    }

    #[test]
    fn a_hosted_agent_rides_its_leaf_through_the_codec() {
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
            word: String::new(),
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
            run: None,
            cause: None,
            tab: Some(OLD_TAB.into()),
        };
        let mut m = numbered_sample();
        assert!(agents(&m).len() >= 2, "the sample has leaves to fill");
        assert!(agents(&m).iter().all(Option::is_none), "never filled: none");
        m.fill_agents(&|id| (id == 1).then(|| claude.clone()));
        let filled = agents(&m);
        assert_eq!(filled.iter().filter(|a| a.is_some()).count(), 1);
        assert_eq!(filled[1].as_ref(), Some(&claude));
        let text = m.to_toml().expect("toml");
        let back = RestoreManifest::from_toml(&text).expect("parse");
        assert_eq!(agents(&back), filled, "through the codec unchanged");
        let older = text.replace(&format!("tab = \"{OLD_TAB}\"\n"), "");
        assert_ne!(older, text, "the tab is written");
        let older = RestoreManifest::from_toml(&older).expect("an older layout parses");
        let older = agents(&older);
        assert_eq!(older[1].as_ref().map(|a| a.tab.as_deref()), Some(None));
    }

    #[test]
    fn a_restored_agent_names_the_tab_it_ran_in_only_for_another() {
        let snap = |tab: &str| aterm_agent::harness::relaunch::Snapshot {
            tab: tab.to_string(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            shell: 4343,
            program: std::path::PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/tmp".into(),
            version: None,
            run: aterm_agent::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
            stored_login: false,
            profile: None,
        };
        let agent = AgentRestore::of(snap(OLD_TAB));
        assert_eq!(agent.tab.as_deref(), Some(OLD_TAB));
        assert_eq!(
            agent.ran_in_another("s-00000000000000000002").as_deref(),
            Some(OLD_TAB)
        );
        assert_eq!(agent.ran_in_another(OLD_TAB), None, "a kept sid");
        assert_eq!(AgentRestore::of(snap("")).tab, None, "none to write");
    }

    const OLD_TAB: &str = "s-00000000000000000001";

    #[test]
    fn a_tabs_holds_ride_its_leaf_and_its_agent_is_not_promised() {
        use crate::fabric::{CARRIED_UNREADABLE, Hold};
        let agent = AgentRestore {
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/Users//me/.local/bin/claude".into(),
            argv: vec!["/Users//me/.local/bin/claude".into()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/Users//me/work".into(),
            version: Some("2.1.300".into()),
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        fn leaves(m: &RestoreManifest) -> Vec<TerminalLeafRestore> {
            fn walk(node: &RestoredSplitTree, out: &mut Vec<TerminalLeafRestore>) {
                match node {
                    RestoredSplitTree::Leaf {
                        view: RestoredView::Terminal(t),
                    } => out.push(t.clone()),
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
        let layout = |hold: Option<&str>| {
            let mut m = sample();
            if let RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(leaf),
            } = &mut m.windows[0].restored_tabs[0].root
            {
                leaf.local_id = Some(7);
                leaf.agent = Some(Box::new(agent.clone()));
            }
            let row = hold.map(str::to_string);
            m.fill_holds(&|id| row.iter().filter(|_| id == 7).cloned().collect());
            let text = m.to_toml().expect("toml");
            let back = RestoreManifest::from_toml(&text).expect("parse");
            assert_eq!(leaves(&back), leaves(&m), "through the codec unchanged");
            (text, leaves(&back))
        };
        let (_, back) = layout(Some("local pause"));
        assert_eq!(back[0].holds, ["local pause"]);
        assert!(
            back[1..].iter().all(|leaf| leaf.holds.is_empty()),
            "{back:?}"
        );
        assert_eq!(
            back[0].carried_holds().of_origin("local").first(),
            Some(Hold {
                reason: "pause".to_string(),
                origin: "local".to_string(),
            })
        );
        assert_eq!(
            back[0].resumes_agent(),
            None,
            "the reopened row does not promise it"
        );
        let garbled = TerminalLeafRestore {
            holds: vec!["garbled row here".to_string()],
            ..back[0].clone()
        };
        assert_eq!(
            garbled.carried_holds().first(),
            Some(Hold {
                reason: CARRIED_UNREADABLE.to_string(),
                origin: "local".to_string(),
            })
        );
        assert_eq!(garbled.resumes_agent(), None, "a garbled row holds");
        let (_, back) = layout(Some("fleet fabric-lost"));
        assert!(
            back[0].carried_holds().has("fleet") && !back[0].carried_holds().has("local"),
            "{back:?}"
        );
        assert_eq!(back[0].resumes_agent(), None, "fleet");
        let (text, back) = layout(None);
        assert!(!text.contains("holds ="), "no field written: {text}");
        assert_eq!(
            (back[0].carried_holds().is_held(), back[0].resumes_agent()),
            (false, Some(&agent)),
            "older"
        );
    }

    #[test]
    fn a_leaf_names_its_agent_and_an_unknown_one_is_relaunched_by_nothing() {
        use aterm_agent::harness::relaunch::{CodexRun, Snapshot};
        use aterm_agent::harness::upgrade::Agent;
        let claude = Snapshot {
            stored_login: false,
            profile: None,
            tab: "s-old".into(),
            pid: 5151,
            start: "Sat Sep 27 01:02:03 2026".into(),
            shell: 5150,
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some("0badf00d-1111-2222-3333-444455556666".into()),
            cwd: "/work/c".into(),
            version: Some("2.1.283".into()),
            run: aterm_agent::harness::relaunch::AgentRun::Claude,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let codex = Snapshot {
            program: "/opt/codex/bin/codex".into(),
            run: aterm_agent::harness::relaunch::AgentRun::Codex(CodexRun {
                home: "/Users//me/.codex".into(),
                home_env: None,
                self_claimed: None,
            }),
            ..claude.clone()
        };
        let wire =
            |snap: &Snapshot| aterm_toml::to_string(&AgentRestore::of(snap.clone())).unwrap();
        let read = |text: &str| aterm_toml::from_str::<AgentRestore>(text).expect("reads");
        assert!(!wire(&claude).contains("word"), "{}", wire(&claude));
        assert!(
            wire(&codex).contains("word = \"codex\""),
            "{}",
            wire(&codex)
        );
        assert_eq!(read(&wire(&claude)).agent(), Some(Agent::Claude));
        assert_eq!(read(&wire(&codex)).agent(), Some(Agent::Codex));
        let older = wire(&codex).replace("word = \"codex\"\n", "");
        assert!(!older.contains("word"), "{older}");
        assert_eq!(read(&older).agent(), Some(Agent::Codex));
        let no_run = read(wire(&codex).split("[codex]").next().unwrap());
        assert!(
            no_run.run.is_none() && no_run.word == "codex",
            "{:?}",
            no_run
        );
        assert_eq!(no_run.agent(), None, "codex without its run");
        let mut claude_with_run = read(&wire(&codex));
        claude_with_run.word = "claude".to_string();
        assert_eq!(claude_with_run.agent(), None, "claude beside a Codex run");
        let newer = read(&format!("word = \"opencode\"\n{}", wire(&claude)));
        assert_eq!(newer.agent(), None);
        assert!(
            newer.snapshot("s-new".into(), 6161).is_none(),
            "relaunched by nothing"
        );
        let muse_no_run = read(&format!("word = \"muse\"\n{}", wire(&claude)));
        assert_eq!(muse_no_run.agent(), None, "muse without its run");
        assert!(muse_no_run.resume_line().is_none());
        assert!(muse_no_run.snapshot("s-new".into(), 6161).is_none());
        for word in ["agy", "gemini"] {
            let no_run = read(&format!("word = \"{word}\"\n{}", wire(&claude)));
            assert_eq!(no_run.agent(), None, "{word} without its run");
            assert_eq!(no_run.name(), None, "{word}");
            assert!(no_run.resume_line().is_none(), "{word}");
            assert!(no_run.snapshot("s-new".into(), 6161).is_none(), "{word}");
            let mut beside_codex = read(&wire(&codex));
            beside_codex.word = word.to_string();
            assert_eq!(beside_codex.agent(), None, "{word} beside a Codex run");
        }
        assert!(
            read(&wire(&claude))
                .snapshot("s-new".into(), 6161)
                .is_some(),
            "NEGATIVE CONTROL: a Claude Code leaf comes back"
        );
    }

    fn first_agent_of(m: &RestoreManifest) -> Option<AgentRestore> {
        fn walk(node: &RestoredSplitTree) -> Option<AgentRestore> {
            match node {
                RestoredSplitTree::Leaf {
                    view: RestoredView::Terminal(t),
                } => t.agent.as_deref().cloned(),
                RestoredSplitTree::Leaf { .. } => None,
                RestoredSplitTree::Split { first, second, .. } => {
                    walk(first).or_else(|| walk(second))
                }
            }
        }
        m.windows
            .iter()
            .flat_map(|w| &w.restored_tabs)
            .find_map(|t| walk(&t.root))
    }

    #[test]
    fn a_restored_agents_home_and_login_ride_its_leaf_into_its_relaunch_line() {
        use aterm_agent::harness::relaunch::{CodexRun, Snapshot, exit_line};
        use aterm_agent::harness::upgrade::Dialect;
        const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
        const HOME: &str = if cfg!(windows) {
            "C:/Users//me/lab home/.claude"
        } else {
            "/Users//me/lab home/.claude"
        };
        const LOGIN: &str = "/Users//me/logins/work-2";

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

        let ran = |home: Option<&str>, login: Option<&str>| Snapshot {
            stored_login: false,
            profile: None,
            tab: "s-old".into(),
            pid: 4242,
            start: "Fri Oct  9 01:02:03 2026".into(),
            shell: 4241,
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(SESSION.into()),
            cwd: "/work".into(),
            version: Some("2.1.295".into()),
            run: aterm_agent::harness::relaunch::AgentRun::Claude,
            dialect: Some(Dialect::Zsh),
            login: login.map(str::to_string),
            home: home.map(str::to_string),
            claude_dir: None,
        };
        let through = |snap: Snapshot| -> Vec<(&str, Snapshot)> {
            let mut m = numbered_sample();
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
                    let agent = first_agent_of(&m).expect("the leaf carries its agent");
                    let back = agent
                        .snapshot("s-new".into(), 6161)
                        .expect("an agent this build knows");
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
                exit_line(std::path::Path::new("/Users//me"), &back),
                Some(format!(
                    " CLAUDE_CONFIG_DIR='{HOME}' CLAUDE_SECURESTORAGE_CONFIG_DIR={LOGIN} \
                     CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 {program}"
                )),
                "{format}: the home, then the login, on the line"
            );
        }
        for (format, back) in through(ran(None, Some(""))) {
            assert_eq!(
                (back.home.as_deref(), back.login.as_deref()),
                (None, Some("")),
                "{format}"
            );
            assert_eq!(
                exit_line(std::path::Path::new("/Users//me"), &back),
                Some(format!(
                    " CLAUDE_SECURESTORAGE_CONFIG_DIR='' CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 \
                     {program}"
                )),
                "{format}"
            );
        }
        for (format, back) in through(ran(None, None)) {
            assert_eq!((&back.home, &back.login), (&None, &None), "{format}");
            assert_eq!(
                exit_line(std::path::Path::new("/Users//me"), &back).as_deref(),
                Some(plain.as_str()),
                "{format}"
            );
        }
        let codex = Snapshot {
            run: AgentRun::Codex(CodexRun {
                home: "/Users//me/lab codex".into(),
                home_env: Some("/Users//me/lab codex/".into()),
                self_claimed: None,
            }),
            ..ran(None, None)
        };
        let ran_codex = codex.codex().cloned().expect("a Codex's run");
        let unchanged = |snap: &Snapshot| {
            for (format, back) in through(snap.clone()) {
                assert_eq!(
                    Snapshot {
                        tab: snap.tab.clone(),
                        shell: snap.shell,
                        ..back
                    },
                    *snap,
                    "{format}: everything but the new tab and shell, unchanged"
                );
            }
        };
        let vendor = |snap: &Snapshot| {
            let wire = aterm_toml::to_string(&AgentRestore::of(snap.clone())).expect("the leaf");
            let read: AgentRestore = aterm_toml::from_str(&wire).expect("reads");
            assert_eq!(read.name(), Some("Codex"), "{wire}");
            assert!(!read.relaunched() && !read.resumes(), "{wire}");
            assert_eq!(read.resume_line(), None, "{wire}");
            assert!(read.snapshot("s-new".into(), 6161).is_none(), "{wire}");
            wire
        };
        assert!(vendor(&codex).contains("home_env"));
        let unset = Snapshot {
            run: AgentRun::Codex(CodexRun {
                home_env: None,
                ..ran_codex.clone()
            }),
            ..codex.clone()
        };
        let wire = vendor(&unset);
        assert!(!wire.contains("home_env"), "{wire}");
        let line = aterm_agent::harness::self_claimed::Resume {
            argv: vec![
                "/Users//me/.aterm/agents/codex".into(),
                "--no-alt-screen".into(),
                "resume".into(),
                SESSION.into(),
            ],
            session: SESSION.into(),
            codex_home: Some("/Users//me/lab codex/".into()),
            codex_auth_path: Some("/Users//me/.aterm/accounts-codex/work/auth.json".into()),
            cwd: Some("/work".into()),
        };
        let alab = Snapshot {
            run: AgentRun::Codex(CodexRun {
                self_claimed: Some(line.clone()),
                ..ran_codex.clone()
            }),
            ..codex.clone()
        };
        unchanged(&alab);
        for (format, back) in through(alab.clone()) {
            assert_eq!(
                aterm_agent::harness::relaunch::self_claimed_line(&back),
                Some(&line),
                "{format}: the validated line, unchanged"
            );
        }
        let wire = aterm_toml::to_string(&AgentRestore::of(alab)).expect("the leaf");
        assert!(wire.contains("[codex.alab]"), "{wire}");
        assert_eq!(
            aterm_toml::from_str::<AgentRestore>(&wire)
                .expect("reads")
                .resume_line(),
            Some(format!(
                "CODEX_HOME='/Users//me/lab codex/' \
                 CODEX_AUTH_PATH=/Users//me/.aterm/accounts-codex/work/auth.json \
                 /Users//me/.aterm/agents/codex --no-alt-screen resume {SESSION}"
            )),
            "the person's line is the validated one"
        );
        let older = wire
            .split("[codex.alab]")
            .next()
            .expect("before the table")
            .to_string();
        assert!(
            aterm_toml::from_str::<AgentRestore>(&older)
                .expect("an older leaf reads")
                .snapshot("s-new".into(), 6161)
                .is_none(),
            "an older leaf with no line is relaunched by nobody"
        );

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
        let neither = AgentRestore {
            tab: None,
            ..AgentRestore::of(ran(None, None))
        };
        assert_eq!(
            aterm_toml::to_string(&neither).expect("this build's"),
            written,
            "an agent with neither is written as 0.101.0 writes it"
        );
        let read: AgentRestore = aterm_toml::from_str(&written).expect("this build reads it");
        assert_eq!((&read.home, &read.login), (&None, &None));
        let back = Snapshot {
            dialect: Some(Dialect::Zsh),
            ..read
                .snapshot("s-new".into(), 6161)
                .expect("an agent this build knows")
        };
        assert_eq!(
            exit_line(std::path::Path::new("/Users//me"), &back).as_deref(),
            Some(plain.as_str())
        );
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

    #[test]
    fn a_handed_tab_program_names_no_shell_and_a_shells_job_its_shell() {
        use aterm_agent::harness::relaunch::NO_SHELL;
        let agent = AgentRestore {
            word: String::new(),
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/Users//me/.local/bin/claude".into(),
            argv: vec!["/Users//me/.local/bin/claude".into()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/Users//me/work".into(),
            version: Some("2.1.283".into()),
            home: None,
            login: None,
            run: None,
            cause: None,
            tab: None,
        };
        let own = agent
            .clone()
            .handed_snapshot("s-new".into(), 4242)
            .expect("a Claude Code leaf");
        assert_eq!(
            (own.tab.as_str(), own.pid, own.shell),
            ("s-new", 4242, NO_SHELL)
        );
        let under = agent
            .handed_snapshot("s-new".into(), 4241)
            .expect("a Claude Code leaf");
        assert_eq!((under.pid, under.shell), (4242, 4241), "CONTROL: its shell");
    }

    #[test]
    fn the_agent_leaf_reads_both_ways_across_0_104_and_0_105() {
        use aterm_agent::harness::relaunch::{CodexRun, Snapshot};
        use aterm_agent::harness::upgrade::{Agent, is_session_id};
        const SESSION: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
        const THREAD: &str = "01a0e4bd-2b37-75a0-9d52-7f1e2c3a4b5c";
        const MUSE: &str = "01a148f8-9301-7752-96db-fef456f88074";
        const AGY: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";

        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct CodexRestore0104 {
            home: String,
            embedded: bool,
            started_ms: u64,
        }
        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct AgentRestore0104 {
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
            home: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            login: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            codex: Option<CodexRestore0104>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cause: Option<String>,
        }
        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct CodexRestore0105 {
            home: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            home_env: Option<String>,
            embedded: bool,
            started_ms: u64,
        }
        #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
        struct AgentRestore0105 {
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
            home: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            login: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            codex: Option<CodexRestore0105>,
            #[serde(default, skip_serializing_if = "String::is_empty")]
            word: String,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cause: Option<String>,
        }
        fn agent_0104(leaf: &AgentRestore0104) -> &'static str {
            if leaf.codex.is_some() {
                "codex"
            } else {
                "claude"
            }
        }
        fn resumes_0104(leaf: &AgentRestore0104) -> bool {
            leaf.session.as_deref().is_some_and(is_session_id)
                && !aterm_agent::harness::upgrade::one_shot(&leaf.argv)
        }
        fn agent_0105(leaf: &AgentRestore0105) -> Option<&'static str> {
            let agent = match leaf.word.as_str() {
                "" if leaf.codex.is_some() => "codex",
                "" | "claude" => "claude",
                "codex" => "codex",
                _ => return None,
            };
            (leaf.codex.is_some() == (agent == "codex")).then_some(agent)
        }

        let claude = Snapshot {
            stored_login: false,
            profile: None,
            tab: "s-old".into(),
            pid: 4242,
            start: "Fri Oct 16 01:02:03 2026".into(),
            shell: 4241,
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(SESSION.into()),
            cwd: "/work".into(),
            version: Some("2.1.301".into()),
            run: AgentRun::Claude,
            dialect: None,
            login: Some(String::new()),
            home: Some("/Users//me/.claude-work".into()),
            claude_dir: None,
        };
        let codex = Snapshot {
            program: "/opt/codex/bin/codex".into(),
            argv: vec!["/opt/codex/bin/codex".into(), "--no-daemon".into()],
            session: Some(THREAD.into()),
            version: Some("0.163.0".into()),
            run: AgentRun::Codex(CodexRun {
                home: "/Users//me/.codex".into(),
                home_env: Some("/Users//me/.codex".into()),
                self_claimed: None,
            }),
            login: None,
            home: None,
            ..claude.clone()
        };
        let wire = |snap: &Snapshot| {
            aterm_toml::to_string(&AgentRestore::of(snap.clone())).expect("this build's leaf")
        };

        for (snap, agent) in [(&claude, "claude"), (&codex, "codex")] {
            let text = wire(snap);
            let old = aterm_toml::from_str::<AgentRestore0104>(&text).expect("0.104.0 reads it");
            assert_eq!(agent_0104(&old), agent, "{text}");
            assert!(resumes_0104(&old), "{text}");
            let older = aterm_toml::from_str::<AgentRestore0105>(&text).expect("0.105.0 reads it");
            assert_eq!(agent_0105(&older), Some(agent), "{text}");
            assert!(text.contains("tab = \"s-old\"\n"), "{text}");
            assert_eq!(
                aterm_toml::to_string(&older).expect("0.105.0 writes it"),
                text.replace("tab = \"s-old\"\n", ""),
                "this build writes {agent}'s leaf as 0.105.0 does"
            );
        }

        let codex_0104 = AgentRestore0104 {
            pid: 4343,
            start: "Fri Oct 16 01:02:03 2026".into(),
            program: "/opt/codex/bin/codex".into(),
            argv: vec!["/opt/codex/bin/codex".into(), "--no-daemon".into()],
            session: Some(THREAD.into()),
            cwd: "/work".into(),
            version: Some("0.157.1".into()),
            home: None,
            login: None,
            codex: Some(CodexRestore0104 {
                home: "/Users//me/.codex".into(),
                embedded: true,
                started_ms: 1_790_543_997_000,
            }),
            cause: None,
        };
        let claude_0104 = AgentRestore0104 {
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some(SESSION.into()),
            home: Some("/Users//me/.claude-work".into()),
            codex: None,
            ..codex_0104.clone()
        };
        let text = aterm_toml::to_string(&claude_0104).expect("0.104.0's leaf");
        let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
        assert_eq!(read.agent(), Some(Agent::Claude), "{text}");
        let back = read.snapshot("s-new".into(), 6161).expect("relaunched");
        assert_eq!(back.agent(), Agent::Claude);
        assert_eq!(back.session, claude_0104.session);
        assert_eq!(back.home, claude_0104.home);
        assert!(back.codex().is_none(), "{text}");
        let text = aterm_toml::to_string(&codex_0104).expect("0.104.0's leaf");
        let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
        assert_eq!(read.agent(), Some(Agent::Codex), "{text}");
        assert!(read.snapshot("s-new".into(), 6161).is_none(), "{text}");
        let codex_0105 = AgentRestore0105 {
            pid: 4343,
            start: "Fri Oct 16 01:02:03 2026".into(),
            program: "/opt/codex/bin/codex".into(),
            argv: vec!["/opt/codex/bin/codex".into()],
            session: Some(THREAD.into()),
            cwd: "/work".into(),
            version: Some("0.163.0".into()),
            home: None,
            login: None,
            codex: Some(CodexRestore0105 {
                home: "/Users//me/lab codex".into(),
                home_env: Some("/Users//me/lab codex/".into()),
                embedded: false,
                started_ms: 1_790_543_997_000,
            }),
            word: "codex".into(),
            cause: Some("asked".into()),
        };
        let text = aterm_toml::to_string(&codex_0105).expect("0.105.0's leaf");
        let read: AgentRestore = aterm_toml::from_str(&text).expect("this build reads it");
        assert_eq!(read.agent(), Some(Agent::Codex));
        assert_eq!(
            aterm_toml::to_string(&read).expect("written back"),
            text,
            "read and written back as 0.105.0 wrote it"
        );
        let Some(RunRestore::Codex(run)) = &read.run else {
            panic!("a Codex run: {read:?}");
        };
        assert_eq!(run.home_env.as_deref(), Some("/Users//me/lab codex/"));
        assert!(
            read.snapshot("s-new".into(), 6161).is_none(),
            "a vendor `codex`'s leaf is relaunched by nobody"
        );

        let muse = Snapshot {
            program: "/opt/pkg/store/muse/2001004010006427002/bin/muse".into(),
            argv: vec![
                "/opt/pkg/store/muse/2001004010006427002/bin/muse".into(),
                "--base-url".into(),
                "http://127.0.0.1:9".into(),
            ],
            session: Some(MUSE.into()),
            version: Some("1.4.10-R6427.1".into()),
            run: AgentRun::Muse(aterm_agent::harness::relaunch::MuseRun {
                data: "/Users//me/.local/share".into(),
                env: vec![("XDG_DATA_HOME".into(), "/Users//me/.local/share".into())],
                workspace: Some("/work/proj".into()),
                lock: Some(
                    "/Users//me/.local/share/muse/sessions/2026/10/17/x/.session.lock".into(),
                ),
                started_ms: 1_790_543_997_000,
                block: Some(7),
            }),
            login: None,
            home: None,
            ..claude.clone()
        };
        let lane = wire(&muse);
        assert!(
            lane.contains("word = \"muse\"\n") && lane.contains("[muse]\n"),
            "{lane}"
        );
        let (top, table) = lane.split_once("[muse]").expect("its table");
        assert!(
            table.contains(&format!("session = \"{MUSE}\"")) && !top.contains("session = "),
            "its session in its table alone: {lane}"
        );
        let old = aterm_toml::from_str::<AgentRestore0104>(&lane).expect("0.104.0 takes it");
        assert_eq!(old.session, None, "{lane}");
        assert_eq!(agent_0104(&old), "claude", "0.104.0 has no word");
        assert!(
            !resumes_0104(&old),
            "0.104.0 finds no conversation to relaunch it on"
        );
        let older = aterm_toml::from_str::<AgentRestore0105>(&lane).expect("0.105.0 takes it");
        assert_eq!(older.word, "muse");
        assert_eq!(agent_0105(&older), None, "0.105.0: no agent's");
        let here: AgentRestore = aterm_toml::from_str(&lane).expect("this build takes it");
        assert_eq!(here.agent(), Some(Agent::Muse), "read as Muse Code's");
        assert_eq!(here.name(), Some("Muse Code"));
        assert!(here.resumes(), "its own table's session resumes");
        assert_eq!(
            here.resume_line().as_deref(),
            Some(format!("XDG_DATA_HOME=/Users//me/.local/share muse resume {MUSE}").as_str()),
            "Muse Code's own resume line, behind its data home"
        );
        let back = here
            .clone()
            .snapshot("s-new".into(), 6161)
            .expect("relaunched by its lane");
        assert_eq!(back.agent(), Agent::Muse);
        assert_eq!(back.session.as_deref(), Some(MUSE));
        assert_eq!(
            back.muse().map(|run| (
                run.data.clone(),
                run.env.clone(),
                run.workspace.clone(),
                run.lock.clone(),
                run.block
            )),
            Some((
                PathBuf::from("/Users//me/.local/share"),
                vec![(
                    "XDG_DATA_HOME".to_string(),
                    "/Users//me/.local/share".to_string()
                )],
                Some("/work/proj".to_string()),
                None,
                None
            )),
            "its run back, its lock and block never carried"
        );
        assert_eq!(
            aterm_toml::to_string(&here).expect("written back"),
            lane,
            "read and written back as written"
        );
        let unproven = wire(&Snapshot {
            session: None,
            ..muse.clone()
        });
        let unproven: AgentRestore = aterm_toml::from_str(&unproven).expect("read");
        assert_eq!(unproven.agent(), Some(Agent::Muse));
        assert!(!unproven.resumes() && unproven.resume_line().is_none());
        for word in [None, Some(""), Some("claude"), Some("muse"), Some("codex")] {
            let word = word.map_or_else(String::new, |w| format!("word = {w:?}\n"));
            let both = format!(
                "{}\n[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
                lane.replacen(
                    "word = \"muse\"\n",
                    &format!("{word}session = {SESSION:?}\n"),
                    1,
                )
                .trim_end()
            );
            assert!(
                aterm_toml::from_str::<AgentRestore>(&both).is_err(),
                "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
            );
        }
        let in_session = format!("{}word = \"muse\"\n", wire(&claude));
        let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
        assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));

        let agy = Snapshot {
            program: "/opt/pkg/store/agy/1000001000004000000/bin/agy".into(),
            argv: vec![
                "/opt/pkg/store/agy/1000001000004000000/bin/agy".into(),
                "--effort=high".into(),
                "--gemini_dir=/Users//me/alt".into(),
            ],
            session: Some(AGY.into()),
            version: Some("1.4.0".into()),
            run: AgentRun::Agy(aterm_agent::harness::relaunch::AgyRun {
                config: "/Users//me/alt/antigravity-cli".into(),
                env: vec![("HOME".into(), "/Users//me/agy-home".into())],
                lock: Some(format!("/Users//me/alt/antigravity-cli/presence/{AGY}.lock").into()),
                crash: Some("/Users//me/alt/antigravity-cli/crashes/crash_7_x.log".into()),
                started_ms: 1_790_543_997_000,
                block: Some(7),
            }),
            login: None,
            home: None,
            ..claude.clone()
        };
        let lane = wire(&agy);
        assert!(
            lane.contains("word = \"agy\"\n") && lane.contains("[agy]\n"),
            "{lane}"
        );
        let (top, table) = lane.split_once("[agy]").expect("its table");
        assert!(
            table.contains(&format!("conversation = \"{AGY}\"")) && !top.contains("session = "),
            "its conversation in its table alone: {lane}"
        );
        let old = aterm_toml::from_str::<AgentRestore0104>(&lane).expect("0.104.0 takes it");
        assert_eq!(old.session, None, "{lane}");
        assert_eq!(agent_0104(&old), "claude", "0.104.0 has no word");
        assert!(
            !resumes_0104(&old),
            "0.104.0 finds no conversation to relaunch it on"
        );
        let older = aterm_toml::from_str::<AgentRestore0105>(&lane).expect("0.105.0 takes it");
        assert_eq!(older.word, "agy");
        assert_eq!(agent_0105(&older), None, "0.105.0: no agent's");
        let here: AgentRestore = aterm_toml::from_str(&lane).expect("this build takes it");
        assert_eq!(here.agent(), Some(Agent::Agy), "read as agy's");
        assert_eq!(here.name(), Some("agy"));
        assert!(here.resumes(), "its own table's conversation resumes");
        assert_eq!(
            here.resume_line().as_deref(),
            Some(
                format!(
                    "HOME=/Users//me/agy-home agy --gemini_dir=/Users//me/alt --conversation={AGY}"
                )
                .as_str()
            ),
            "agy's own resume hint, after its config flag, behind its HOME"
        );
        let back = here
            .clone()
            .snapshot("s-new".into(), 6161)
            .expect("relaunched by its lane");
        assert_eq!(back.agent(), Agent::Agy);
        assert_eq!(back.session.as_deref(), Some(AGY));
        assert_eq!(
            back.agy().map(|run| (
                run.config.clone(),
                run.env.clone(),
                run.lock.clone(),
                run.crash.clone(),
                run.block
            )),
            Some((
                PathBuf::from("/Users//me/alt/antigravity-cli"),
                vec![("HOME".to_string(), "/Users//me/agy-home".to_string())],
                None,
                None,
                None
            )),
            "its run back, its lock, crash file and block never carried"
        );
        assert_eq!(
            aterm_toml::to_string(&here).expect("written back"),
            lane,
            "read and written back as written"
        );
        let unproven = wire(&Snapshot {
            session: None,
            ..agy.clone()
        });
        let unproven: AgentRestore = aterm_toml::from_str(&unproven).expect("read");
        assert_eq!(unproven.agent(), Some(Agent::Agy));
        assert!(!unproven.resumes() && unproven.resume_line().is_none());
        for word in [None, Some(""), Some("claude"), Some("agy"), Some("muse")] {
            let word = word.map_or_else(String::new, |w| format!("word = {w:?}\n"));
            let top = lane.replacen("word = \"agy\"\n", &word, 1);
            for other in [
                "[muse]\ndata = \"/Users//me/.local/share\"\n",
                "[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
            ] {
                let both = format!("{}\n{other}", top.trim_end());
                assert!(
                    aterm_toml::from_str::<AgentRestore>(&both).is_err(),
                    "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
                );
            }
        }
        let in_session = format!("{}word = \"agy\"\n", wire(&claude));
        let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
        assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));

        // The retired Gemini CLI's leaf exactly as v0.134.0's producer wrote it
        // (tests/fixtures/handoff/v0.134.0/carried-state): read, relaunched by
        // nothing and never another agent's, its table dropped when written back.
        let gemini_table = "\n[gemini]\nsession = \"c2d9e4f1-7a3b-4c5d-8e6f-0a1b2c3d4e5f\"\n\
                            home = \"/home/dev/.gemini\"\n\
                            chats = \"/home/dev/.gemini/tmp/7f3a9c/chats\"\n\
                            env = [\"GEMINI_CLI_HOME=/home/dev\"]\n\
                            started_ms = 1792871730000\n";
        let top = "pid = 48503\nstart = \"Mon Oct 19 09:14:02 2026\"\n\
                   program = \"/home/dev/.local/bin/gemini\"\nargv = [\"gemini\"]\n\
                   cwd = \"/home/dev/src/aterm\"\nversion = \"1.4.0\"\n";
        for (what, word) in [("its word", "word = \"gemini\"\n"), ("no word", "")] {
            let lane = format!("{top}{word}{gemini_table}");
            let here: AgentRestore = aterm_toml::from_str(&lane)
                .unwrap_or_else(|e| panic!("{what}: this build reads the retired leaf: {e}"));
            assert!(here.retired(), "{what}");
            assert_eq!(
                here.agent(),
                None,
                "{what}: no agent's, never Claude Code's"
            );
            assert_eq!(here.name(), None, "{what}");
            assert!(!here.resumes(), "{what}");
            assert_eq!(here.resume_line(), None, "{what}");
            assert!(
                here.clone().snapshot("s-new".into(), 6161).is_none(),
                "{what}: relaunched by nothing"
            );
            let back = aterm_toml::to_string(&here).expect("written back");
            assert!(
                back.contains("word = \"gemini\"\n") && !back.contains("[gemini]"),
                "{what}: its word kept, its retired table dropped: {back}"
            );
            let again: AgentRestore = aterm_toml::from_str(&back).expect("read again");
            assert!(again.retired() && again.agent().is_none(), "{what}: {back}");
            for other in [
                "[agy]\nconfig = \"/Users//me/.gemini/antigravity-cli\"\n",
                "[codex]\nhome = \"/Users//me/.codex\"\nembedded = true\nstarted_ms = 1\n",
            ] {
                let both = format!("{}\n{other}", lane.trim_end());
                assert!(
                    aterm_toml::from_str::<AgentRestore>(&both).is_err(),
                    "AMBIGUOUS_AGENT_RUN_TABLES_MUST_NOT_RESUME_AS_CLAUDE: {both}"
                );
            }
        }
        let claude_word: AgentRestore =
            aterm_toml::from_str(&format!("{}{gemini_table}", wire(&claude))).expect("read");
        assert!(
            claude_word.retired() && claude_word.agent().is_none(),
            "a retired table beside Claude Code's word is no agent's"
        );
        let claude_leaf: AgentRestore = aterm_toml::from_str(&wire(&claude)).expect("read");
        assert!(
            !claude_leaf.retired() && claude_leaf.agent() == Some(Agent::Claude),
            "NEGATIVE CONTROL: Claude Code's leaf is no retired agent's"
        );
        let in_session = format!("{}word = \"gemini\"\n", wire(&claude));
        let old = aterm_toml::from_str::<AgentRestore0104>(&in_session).expect("0.104.0");
        assert_eq!((agent_0104(&old), resumes_0104(&old)), ("claude", true));
    }

    #[cfg(unix)]
    #[test]
    fn directories_made_under_an_open_umask_are_never_group_writable() {
        use std::os::unix::fs::PermissionsExt as _;
        if !crate::test_alone::alone() {
            return;
        }
        // SAFETY: `umask(2)` sets this isolated test child's file-creation mask and cannot fail.
        unsafe { libc::umask(0o002) };
        let scratch = aterm_tempfile::tempdir().expect("scratch dir");
        let root = scratch.path();
        let mode = |rel: &str| {
            let meta = fs::symlink_metadata(root.join(rel)).expect("made");
            format!("{:o}", meta.permissions().mode() & 0o7777)
        };
        write_wire_to(&root.join("wire/.local/share/aterm/session.toml"), "").expect("wire");
        store_cell_metrics_to(
            &root.join("metrics/.local/share/aterm/cell-metrics.toml"),
            "font",
            2.0,
            13.0,
            8,
            16,
        )
        .expect("metrics");
        crate::native_document_journal::DraftJournalHost::new(
            root.join("journal/.local/state/aterm/drafts"),
        )
        .expect("journal");
        crate::control_auth::ensure_private_dir(&root.join("control/.local/state/aterm/logs"))
            .expect("private dir");
        let made = [
            ("wire/.local/share/aterm", "755"),
            ("metrics/.local/share/aterm", "755"),
            ("journal/.local/state/aterm/drafts", "700"),
            ("control/.local/state/aterm/logs", "700"),
        ];
        let mut wrong = Vec::new();
        for (leaf, want) in made {
            let mut rel = String::new();
            for name in leaf.split('/') {
                if !rel.is_empty() {
                    rel.push('/');
                }
                rel.push_str(name);
                let want = if rel == leaf { want } else { "755" };
                let got = mode(&rel);
                if got != want {
                    wrong.push(format!("{rel} {got}, not {want}"));
                }
            }
        }
        assert!(wrong.is_empty(), "made under umask 002: {wrong:#?}");
    }
}
