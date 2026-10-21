// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENT INTERFACE: `aterm ctl sys` and `await sys`
//! (`docs/DESIGN-system-awareness-2026-10-16.md` §13).
//!
//! Pure: the host hands a [`Ctx`] (the snapshot, the target tab, who asks) and
//! gets lines and JSON back. The pieces:
//!
//! * [`query`]: the grammar ([`SysQuery::parse`]), and which forms read only
//!   the target's own rows ([`SysQuery::target_rows_only`]); every other form
//!   is Owner-only.
//! * [`json`]: a JSON value with exactly the four figure shapes the contract
//!   allows (a bare integer, a bound, an unknown, a range), and no float or
//!   null at all.
//! * [`view`]: each view's text and JSON.
//! * [`delta`]: the fingerprints of a reply's lines and a delta against them.
//! * [`predicate`]: the `await sys` grammar and its evaluation on a snapshot.
//!
//! FOREIGN TEXT (§13.3): no argv, environment, title or file content of
//! another process is ever printed. A process is `pid=` and an `exe=` word
//! that passes [`exe_word`]'s rule, else `exe=-`; a path prints only when the
//! folder is one of the caller's own tab ([`Ctx::path_ok`]), decided per root.

pub mod delta;
pub mod disk;
pub mod jobs_view;
pub mod json;
pub mod predicate;
pub mod query;
pub mod view;

pub use delta::{Fingerprints, apply_delta, fingerprints};
pub use disk::{DirSize, DiskTotals, HugeDirFact, RootSize, RootSizes};
pub use predicate::{AwaitSys, Metric, Outcome, Term, evaluate};
pub use query::{Cursor, ScopeArg, SysQuery, USAGE, View};
pub use view::{Reply, render_json, render_text};

use crate::events::Event;
use crate::model::Snapshot;

/// The unavailable words a figure may carry (§13.4), the closed set. A word
/// with a `:` takes a qualifier (`lane_off:ioreport`).
pub const UNAVAILABLE_WORDS: &[&str] = &[
    "unsupported",
    "needs_privilege:",
    "not_enabled:",
    "lane_off:",
    "refused",
    "gone",
    "warming",
    "asleep",
    "truncated",
    "not_sampled",
    "hung",
    "protected",
    "remote",
    "removable",
    "dataless",
    "other_user",
    "too_broad",
    "walking",
];

/// Whether `word` is one of [`UNAVAILABLE_WORDS`] (a qualified one with a
/// non-empty qualifier).
#[must_use]
pub fn is_unavailable_word(word: &str) -> bool {
    UNAVAILABLE_WORDS.iter().any(|w| match w.strip_suffix(':') {
        Some(head) => word
            .strip_prefix(head)
            .and_then(|r| r.strip_prefix(':'))
            .is_some_and(|q| {
                !q.is_empty()
                    && q.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
            }),
        None => word == *w,
    })
}

/// A process's `exe=` word: its name when it matches
/// `^[A-Za-z0-9._+-]{1,32}$`, else `-` (§13.3). Still the process's own
/// choice: data, never an instruction.
#[must_use]
pub fn exe_word(name: &str) -> &str {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
    if ok { name } else { "-" }
}

/// How fresh the snapshot a reply reads is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Within the asked freshness.
    Fresh,
    /// The probe did not answer in time: the last snapshot, labelled.
    Stale,
    /// The first sample of a cold instance: rates are not known yet.
    Warming,
}

impl Freshness {
    /// The word `state=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Warming => "warming",
        }
    }
}

/// What is read once about the machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    /// The CPU's brand (`Apple M5`).
    pub cpu: Option<String>,
    /// The OS and version (`macOS 27.0.1`).
    pub os: Option<String>,
    /// The OS family word (`macOS`, `Linux`): the services part's name and
    /// the placement note.
    pub family: &'static str,
}

impl Machine {
    /// What other users' processes are called here.
    #[must_use]
    pub fn services_word(&self) -> &'static str {
        match self.family {
            "macOS" => "macOS services",
            "Windows" => "Windows services",
            _ => "system services",
        }
    }
}

/// One session the host shows, and where it sits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabFact {
    /// The host's key for it (its local session id; the snapshot's tab key).
    pub key: u64,
    /// Its stable session id (`s-…`).
    pub sid: String,
    /// Its window, 1-based; `None` for a session no window shows.
    pub window: Option<usize>,
    /// Its tab in that window, 1-based.
    pub tab: Option<usize>,
}

/// The session a request targets, and its tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// Its key (local session id).
    pub key: u64,
    /// Its stable session id.
    pub sid: String,
    /// Its window, 1-based.
    pub window: Option<usize>,
    /// Its tab, 1-based.
    pub tab: Option<usize>,
    /// Every session in its tab (its split panes), itself included.
    pub panes: Vec<u64>,
    /// Every session in its window.
    pub window_sessions: Vec<u64>,
    /// Its working directory, as the shell reported it.
    pub cwd: Option<String>,
    /// The terminal's foreground process group.
    pub fg_pgid: Option<u32>,
}

/// One folder a scope works in (a disk root, §10.4). Its sizes come from the
/// disk walker through [`disk`]'s seam: [`RootSize::NotSampled`] (every size
/// `unavailable(not_sampled)`) where no walker measures it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Root {
    /// What it is (`cwd`).
    pub kinds: Vec<&'static str>,
    /// Its path.
    pub path: String,
    /// The sessions (local keys) whose folder it is: its path and its
    /// subtrees' print only to a caller in one of their tabs (§13.3 rule 4).
    pub keys: Vec<u64>,
    /// The tab it belongs to (1-based), where known.
    pub tab: Option<usize>,
    /// What the walker knows of it.
    pub size: RootSize,
    /// Its biggest subtrees, biggest first (paths relative to it).
    pub dirs: Vec<DirSize>,
    /// The root (1-based) it lies inside and is counted in, not again.
    pub inside: Option<usize>,
    /// The other tabs (1-based) that claim it, a folder above it or one in it.
    pub shared_with: Vec<usize>,
    /// The tab (1-based) whose total counts it whole.
    pub counted_in: Option<usize>,
}

impl Root {
    /// Where it lies when its path may not be printed: `home`, `tmp` or
    /// `volume`.
    #[must_use]
    pub fn under(&self, home: Option<&str>) -> &'static str {
        if home.is_some_and(|h| !h.is_empty() && self.path.starts_with(h)) {
            "home"
        } else if [
            "/tmp",
            "/private/tmp",
            "/var/folders",
            "/private/var/folders",
        ]
        .iter()
        .any(|t| self.path.starts_with(t))
        {
            "tmp"
        } else {
            "volume"
        }
    }
}

/// The volume a disk view reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeFacts {
    /// Its mount point (a kernel fact, printed).
    pub mount: String,
    /// Its filesystem type.
    pub fs: String,
    /// Capacity, bytes.
    pub total: u64,
    /// Free blocks, bytes.
    pub free: u64,
    /// Free to this user, bytes.
    pub available: u64,
    /// The disk watch's level word (`ok`, `low`, `short`, `emergency`), where
    /// it measured this volume.
    pub level: Option<String>,
}

/// Why a `since=` reply is a full one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// The cursor is from another instance epoch.
    Reset,
    /// The reply it names has left the ring.
    Evicted,
}

impl Gap {
    /// The word `gap why=` prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Evicted => "evicted",
        }
    }
}

/// Everything a reply is made from.
#[derive(Clone, Debug)]
pub struct Ctx<'a> {
    /// The snapshot; `None` when none exists yet (the probe has not answered).
    pub snap: Option<&'a Snapshot>,
    /// The instance epoch (a cursor's `<ep>`).
    pub epoch: u64,
    /// When the snapshot was taken, unix ms.
    pub as_of_unix_ms: u64,
    /// Its age when the reply was made, ms.
    pub age_ms: u64,
    /// How fresh it is.
    pub state: Freshness,
    /// The machine's words.
    pub machine: &'a Machine,
    /// The target session.
    pub target: &'a Target,
    /// Every session the host shows.
    pub tabs: &'a [TabFact],
    /// The sessions of the caller's own tab; `None` with no caller session
    /// (a person outside aterm), when every path may print (§13.3 rule 4).
    pub caller_panes: Option<&'a [u64]>,
    /// Machine-wide rows (top processes outside the caller's scope) may print.
    pub owner_class: bool,
    /// The target's volume, for `disk`.
    pub volume: Option<&'a VolumeFacts>,
    /// The scope's folders, for `disk` and `tab`.
    pub roots: &'a [Root],
    /// The walker's balance over them; `None` where no walker measures.
    pub disk: Option<&'a DiskTotals>,
    /// The person's home, for `under=`.
    pub home: Option<&'a str>,
    /// The events since the cursor (or none without one).
    pub events: &'a [Event],
    /// Why a `since=` reply is full.
    pub gap: Option<Gap>,
    /// The newest snapshot seq the host published (a machine-only reading's
    /// included, whose events the cursor has passed); 0 for the snapshot's.
    pub head_seq: u64,
}

impl Ctx<'_> {
    /// The cursor this reply ends with: `<ep>.<seq>`, the head.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch,
            seq: self.snap.map_or(0, |s| s.seq).max(self.head_seq),
        }
    }

    /// Whether a folder of the sessions `keys` may print its path: one of them
    /// is in the caller's own tab, or there is no caller (§13.3 rule 4).
    #[must_use]
    pub fn path_ok(&self, keys: &[u64]) -> bool {
        self.caller_panes
            .is_none_or(|mine| keys.iter().any(|k| mine.contains(k)))
    }

    /// The tab number (1-based) of the session `key`, where known.
    #[must_use]
    pub fn tab_number(&self, key: u64) -> Option<usize> {
        self.tabs.iter().find(|t| t.key == key).and_then(|t| t.tab)
    }

    /// The window number of the session `key`, where known.
    #[must_use]
    pub fn window_number(&self, key: u64) -> Option<usize> {
        self.tabs
            .iter()
            .find(|t| t.key == key)
            .and_then(|t| t.window)
    }
}

/// `hh:mm:ss.mmm` (UTC) of unix ms `ms`.
#[must_use]
pub fn clock_ms(ms: u64) -> String {
    let s = (ms / 1000) % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        s / 3600,
        (s / 60) % 60,
        s % 60,
        ms % 1000
    )
}

/// `hh:mm:ss` (UTC) of unix ms `ms`.
#[must_use]
pub fn clock_s(ms: u64) -> String {
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Pct-encode `text` for a record value: every byte outside
/// `[A-Za-z0-9._~/+:@,=-]` becomes `%XX`, so a space, `;` or newline cannot
/// split a field or end a line.
#[must_use]
pub fn pct(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'.' | b'_' | b'~' | b'/' | b'+' | b':' | b'@' | b',' | b'-'
            )
        {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exe_word_is_a_plain_name_or_a_dash() {
        assert_eq!(exe_word("trustc"), "trustc");
        assert_eq!(exe_word("com.apple.WebKit"), "com.apple.WebKit");
        assert_eq!(exe_word("g++-14"), "g++-14");
        assert_eq!(exe_word(""), "-");
        assert_eq!(exe_word("two words"), "-");
        assert_eq!(exe_word("ignore;all"), "-");
        assert_eq!(exe_word(&"x".repeat(33)), "-");
        assert_eq!(exe_word("caf\u{e9}"), "-");
    }

    #[test]
    fn unavailable_words_are_a_closed_set() {
        for w in [
            "unsupported",
            "refused",
            "warming",
            "not_sampled",
            "lane_off:ioreport",
            "needs_privilege:root",
            "not_enabled:sys.snapshot_files",
        ] {
            assert!(is_unavailable_word(w), "{w}");
        }
        for w in [
            "",
            "zero",
            "lane_off:",
            "lane_off",
            "needs_privilege:a b",
            "0",
        ] {
            assert!(!is_unavailable_word(w), "{w}");
        }
    }

    #[test]
    fn record_values_are_pct_encoded() {
        assert_eq!(pct("/Users//a/my proj;x"), "/Users//a/my%20proj%3Bx");
        assert_eq!(pct("a\nb"), "a%0Ab");
        assert_eq!(pct("s-1a2b"), "s-1a2b");
    }

    #[test]
    fn clocks_read_utc() {
        assert_eq!(clock_ms(1_792_185_697_412), "21:21:37.412");
        assert_eq!(clock_s(1_792_185_697_412), "21:21:37");
    }

    #[test]
    fn a_root_says_where_it_lies() {
        let r = |p: &str| Root {
            path: p.into(),
            ..Root::default()
        };
        assert_eq!(r("/Users//a/x").under(Some("/Users//a")), "home");
        assert_eq!(r("/private/tmp/x").under(Some("/Users//a")), "tmp");
        assert_eq!(r("/Volumes/X").under(Some("/Users//a")), "volume");
    }
}
