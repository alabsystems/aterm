// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Fabric posts (design §21.5): one broadcast through the session's own control socket after
//! an event is observed or a write settles `Done`, opt-in (`--notify`, `How::notify(true)`),
//! and silent when it fails.

#[cfg(any(test, debug_assertions))]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::client::Github;
use crate::fx::Fx;
use crate::github::How;
use crate::identity::EnvSource;
use crate::journal::IntentId;
use crate::slug::Slug;

/// The two topics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topic {
    /// `agh.release`: a wait's outcome.
    Release,
    /// `agh.write`: a settled write.
    Write,
}

impl Topic {
    /// `agh.release`, `agh.write`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Topic::Release => "agh.release",
            Topic::Write => "agh.write",
        }
    }
}

/// One post.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Notice {
    /// The topic.
    pub topic: Topic,
    /// The repository.
    pub repo: Slug,
    /// The tag.
    pub tag: String,
    /// On `agh.release`, the held conditions joined by `+`, each condition's slug written
    /// `aterm_codec::pct::encode`d with each `+` then written `%2B`; on `agh.write`, `created`,
    /// `uploaded`, `published` or `deleted`.
    pub event: String,
    /// The asset count, when the event has one.
    pub n: Option<u32>,
    /// The release id, when known.
    pub id: Option<u64>,
    /// The broker's dedup key.
    pub key: String,
}

impl Notice {
    /// A wait's notice: key `agh-` and the first 16 hex of
    /// `sha256(repo ‖ 0x00 ‖ tag ‖ 0x00 ‖ event ‖ 0x00 ‖ n or "")`, the repo lowercased.
    #[must_use]
    pub fn wait(repo: &Slug, tag: &str, held: &[&str], n: Option<u32>, id: Option<u64>) -> Notice {
        // WP9 writes the encoding and the key (plan §13).
        Notice {
            topic: Topic::Release,
            repo: repo.clone(),
            tag: tag.to_string(),
            event: held.join("+"),
            n,
            id,
            key: String::new(),
        }
    }

    /// A one-shot write's notice: key `agh-{intent}`.
    #[must_use]
    pub fn intent(
        repo: &Slug,
        tag: &str,
        event: &str,
        intent: &IntentId,
        id: Option<u64>,
    ) -> Notice {
        // WP9 writes the key (plan §13).
        let _ = intent;
        Notice {
            topic: Topic::Write,
            repo: repo.clone(),
            tag: tag.to_string(),
            event: event.to_string(),
            n: None,
            id,
            key: String::new(),
        }
    }

    /// A Converging write's notice, which has no intent: key `agh-` and the first 16 hex of
    /// `sha256(repo ‖ 0x00 ‖ tag ‖ 0x00 ‖ event ‖ 0x00 ‖ n or "" ‖ 0x00 ‖ id or "")`, so two
    /// uploads to one release, whose counts differ, are two posts.
    #[must_use]
    pub fn converged(
        repo: &Slug,
        tag: &str,
        event: &str,
        n: Option<u32>,
        id: Option<u64>,
    ) -> Notice {
        // WP9 writes the key (plan §13).
        Notice {
            topic: Topic::Write,
            repo: repo.clone(),
            tag: tag.to_string(),
            event: event.to_string(),
            n,
            id,
            key: String::new(),
        }
    }

    /// `v=1 repo={owner}/{repo} tag={tag} event={event}`, then ` n={n}` and ` id={id}` when
    /// set. A tag holding a byte that is not ASCII graphic (a space, a control byte, any
    /// non-ASCII byte), `=` or `%` is written `aterm_codec::pct::encode`d, with each `=` then
    /// written `%3D` (`pct::encode` keeps `=`), so the body stays one line of `key=value` words.
    /// The tag is caller text, so the body then passes `Redactor::shapes`: a token-shaped tag
    /// is written `[redacted]`, which keeps the body one line.
    #[must_use]
    pub fn body(&self) -> String {
        // WP9 writes the body (plan §13).
        String::new()
    }

    /// `@{sid} post to=say:{topic} kind=note key={key} -- {body}`, built from `body()`, so it
    /// is redacted as `body()` is.
    #[must_use]
    pub fn line(&self, sid: &str) -> String {
        // WP9 writes the line (plan §13).
        let _ = sid;
        String::new()
    }
}

/// How one post ended, for the receipt's `notify=` field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Posted {
    /// `notify=sent:{id}`.
    Sent(String),
    /// `notify=skipped:{why}`: no session (`no-session`), a nested multiplexer (`mux`),
    /// a fabric that is not connected (`fabric={state}`), or a tape (`taped`).
    Skipped(String),
    /// `notify=failed:{why}`: no socket, a refused AUTH, an `ERR` reply, a timeout, or
    /// `unsupported` off unix. `why` is one redacted word or short phrase.
    Failed(String),
}

impl Posted {
    /// The receipt's value: `sent:{id}`, `skipped:{why}` or `failed:{why}`.
    #[must_use]
    pub fn receipt(&self) -> String {
        match self {
            Posted::Sent(id) => format!("sent:{id}"),
            Posted::Skipped(why) => format!("skipped:{why}"),
            Posted::Failed(why) => format!("failed:{why}"),
        }
    }
}

mod sealed {
    /// Only agh's notifiers.
    pub trait Sealed {}
}

/// Where posts go. Sealed: `SessionNotifier`, `NoNotifier` and the tests' `RecordingNotifier`
/// are the only ones, so no caller can route a post elsewhere.
pub trait Notifier: sealed::Sealed + Send + Sync {
    /// Post `notice` once, through `fx` (`Fx::emit_fabric`), and say how it ended. Never
    /// an error: a failed post changes no exit code (design §21.5).
    fn post(&self, fx: &Fx, notice: &Notice) -> Posted;
}

/// Posts nothing: `Posted::Skipped("off")`.
pub struct NoNotifier;

impl sealed::Sealed for NoNotifier {}

impl Notifier for NoNotifier {
    fn post(&self, _fx: &Fx, _notice: &Notice) -> Posted {
        Posted::Skipped("off".to_string())
    }
}

/// The bound on each phase of a post: 10 s.
const PHASE_BOUND: Duration = Duration::from_secs(10);

/// The session's own socket (design §21.5): the six steps of `SessionNotifier::post`.
pub struct SessionNotifier {
    /// The session, `s-<hex>`; `None` when there is none.
    sid: Option<String>,
    /// The socket, when a test pinned it; else resolved per post.
    sock: Option<std::path::PathBuf>,
    /// The bound on each phase.
    bound: Duration,
}

impl SessionNotifier {
    /// The notifier of the session `env` names: `ATERM_PARENT_SESSION_ID` of shape
    /// `s-<hex>`, unless `ATERM_MUX` or `ATERM_MUX_BASE` is set (a nested multiplexer,
    /// `Skipped("mux")`, as `aterm ctl @self post` refuses there). No session:
    /// `Skipped("no-session")` on every post.
    #[must_use]
    pub fn from_env(env: &dyn EnvSource) -> SessionNotifier {
        // WP9 reads the session (plan §13); until then there is none.
        let _ = env;
        SessionNotifier {
            sid: None,
            sock: None,
            bound: PHASE_BOUND,
        }
    }

    /// Tests: the notifier of session `sid` on the socket at `sock`, no resolution.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn at_socket(sock: &std::path::Path, sid: &str) -> SessionNotifier {
        SessionNotifier {
            sid: Some(sid.to_string()),
            sock: Some(sock.to_path_buf()),
            bound: PHASE_BOUND,
        }
    }

    /// Tests: the same notifier with `bound` in place of the 10 s bound per phase, so a test
    /// of a server that never answers ends in a second.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_bound_for_tests(mut self, bound: Duration) -> SessionNotifier {
        self.bound = bound;
        self
    }
}

impl sealed::Sealed for SessionNotifier {}

impl Notifier for SessionNotifier {
    fn post(&self, fx: &Fx, notice: &Notice) -> Posted {
        // WP9 writes the six steps (plan §13).
        let _ = (fx, notice, &self.sid, &self.sock, self.bound);
        Posted::Failed("not yet".to_string())
    }
}

/// Tests: keeps every notice and posts nothing; `Posted::Sent("recorded")`.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    /// Every notice so far, in order.
    posted: Mutex<Vec<Notice>>,
}

#[cfg(any(test, debug_assertions))]
impl RecordingNotifier {
    /// Empty: `RecordingNotifier::default()`.
    #[must_use]
    pub fn new() -> RecordingNotifier {
        RecordingNotifier::default()
    }

    /// Every notice so far, in order.
    #[must_use]
    pub fn posted(&self) -> Vec<Notice> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(any(test, debug_assertions))]
impl sealed::Sealed for RecordingNotifier {}

#[cfg(any(test, debug_assertions))]
impl Notifier for RecordingNotifier {
    fn post(&self, _fx: &Fx, notice: &Notice) -> Posted {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(notice.clone());
        Posted::Sent("recorded".to_string())
    }
}

/// Post `notice` through `gh.notifier()` when `how.notify`, and record its
/// `Posted::receipt` with `Github::note_notify`, so the verb's receipt prints
/// `notify=sent:…`, `notify=skipped:…` or `notify=failed:…`. When `how.notify` is false it
/// posts nothing and notes nothing. Under any tape it asks no notifier: it emits
/// `notice.line("self")` at the fabric door itself (`Fx::emit_fabric`, which answers
/// `Skipped("taped")`), so a recording and its replay emit the same bytes whatever notifier
/// each run holds. A resource write method calls it once, after its write is `Done`;
/// `release wait --notify` calls it with the wait's notice. Never an error.
pub(crate) fn settled(gh: &Github, how: &How, notice: &Notice) {
    // WP9 writes the body (plan §13).
    let _ = (gh, how, notice);
}
