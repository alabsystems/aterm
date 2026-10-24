// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grammar and query parser for the `sys` control verb.

use crate::explain::WhySubject;

/// The usage line a refused `sys` request answers; a stray word after `why` answers [`WHY_USAGE`].
pub const USAGE: &str = "ERR usage: sys [overview|tab|aterm|global|jobs|cores|gpu|mem|disk|procs|why] \
                         [scope=pane|tab|window|aterm|global] [instances=all] [top=<1-64>] \
                         [depth=<1-8>] [fresh=<ms>] [since=<ep>.<seq>] [--json]; scope= on \
                         jobs disk procs (cores gpu mem take it, rows unchanged), depth= on \
                         disk, instances=all on overview aterm global cores gpu mem and procs \
                         scope=aterm\n";
/// The usage line for a word after `why` that is not its one subject or an option.
pub const WHY_USAGE: &str =
    "ERR usage: sys why [cpu|mem] [top=<1-64>] [fresh=<ms>] [since=<ep>.<seq>] [--json]\n";

/// The rows a view lists by default.
pub const TOP_DEFAULT: usize = 8;
/// Maximum rows a view lists.
pub const TOP_MAX: usize = 64;
/// Minimum nonzero `fresh=` interval in milliseconds.
pub const FRESH_MIN_MS: u64 = 250;

/// A view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    /// The three scope sentences and the split.
    Overview,
    /// The target's tab across every resource.
    Tab,
    /// This aterm instance across every resource, with its parts.
    Aterm,
    /// The machine across every resource.
    Global,
    /// The tab's jobs and their verdicts.
    Jobs,
    /// Per-core use by cluster.
    Cores,
    /// The GPU.
    Gpu,
    /// Memory, compressor, swap and pressure, and who holds it.
    Mem,
    /// The scope's folders, writers and volume.
    Disk,
    /// Process rows.
    Procs,
    /// Who uses the CPU or memory, and why.
    Why,
}

impl View {
    /// Every view, in the help's order.
    pub const ALL: [View; 11] = [
        Self::Overview,
        Self::Tab,
        Self::Aterm,
        Self::Global,
        Self::Jobs,
        Self::Cores,
        Self::Gpu,
        Self::Mem,
        Self::Disk,
        Self::Procs,
        Self::Why,
    ];

    /// Its word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Tab => "tab",
            Self::Aterm => "aterm",
            Self::Global => "global",
            Self::Jobs => "jobs",
            Self::Cores => "cores",
            Self::Gpu => "gpu",
            Self::Mem => "mem",
            Self::Disk => "disk",
            Self::Procs => "procs",
            Self::Why => "why",
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.word() == w)
    }

    /// Returns the default scope for `self`, or `None` for scope views and `why`.
    #[must_use]
    pub fn default_scope(self) -> Option<ScopeArg> {
        match self {
            Self::Overview | Self::Tab | Self::Aterm | Self::Global | Self::Why => None,
            Self::Jobs | Self::Disk | Self::Procs => Some(ScopeArg::Tab),
            Self::Cores | Self::Gpu | Self::Mem => Some(ScopeArg::Global),
        }
    }
}

/// A `scope=` word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeArg {
    /// The target session alone.
    Pane,
    /// The target's tab (its split panes).
    Tab,
    /// The target's window.
    Window,
    /// This aterm instance.
    Aterm,
    /// The machine.
    Global,
}

impl ScopeArg {
    /// Its word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Pane => "pane",
            Self::Tab => "tab",
            Self::Window => "window",
            Self::Aterm => "aterm",
            Self::Global => "global",
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        Some(match w {
            "pane" => Self::Pane,
            "tab" => Self::Tab,
            "window" => Self::Window,
            "aterm" => Self::Aterm,
            "global" => Self::Global,
            _ => return None,
        })
    }
}

/// Snapshot cursor `<ep>.<seq>` with hex epoch and decimal sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// The instance epoch.
    pub epoch: u64,
    /// The snapshot sequence.
    pub seq: u64,
}

impl Cursor {
    /// Parse `<hex>.<decimal>`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (ep, seq) = text.split_once('.')?;
        if ep.is_empty() || ep.len() > 16 || seq.is_empty() {
            return None;
        }
        Some(Self {
            epoch: u64::from_str_radix(ep, 16).ok()?,
            seq: seq.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}.{}", self.epoch, self.seq)
    }
}

/// Target of a `sys name` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameTarget {
    /// A job id.
    Job(String),
    /// An agent's task id.
    Task(String),
}

/// A parsed `sys` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysQuery {
    /// The view (overview by default).
    pub view: View,
    /// `scope=`, where given.
    pub scope: Option<ScopeArg>,
    /// `instances=all`.
    pub instances_all: bool,
    /// `top=`.
    pub top: usize,
    /// Whether `top=` was given (it lifts `why`'s byte cut).
    pub top_set: bool,
    /// `depth=` (disk).
    pub depth: Option<u8>,
    /// `fresh=`, ms (0 forces a sample).
    pub fresh_ms: Option<u64>,
    /// `since=`.
    pub since: Option<Cursor>,
    /// `--json`.
    pub json: bool,
    /// `caller=`.
    pub caller: Option<String>,
    /// `sys name job=|task=`.
    pub name: Option<NameTarget>,
    /// `why`'s subject; `None` for a bare `why` (the reply chooses) and every
    /// other view.
    pub subject: Option<WhySubject>,
}

/// An id `job=`/`task=` takes: `[A-Za-z0-9_-]{1,32}`.
fn id_word(w: &str) -> Option<String> {
    (!w.is_empty()
        && w.len() <= 32
        && w.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| w.to_owned())
}

/// A session id `caller=` takes: `[A-Za-z0-9_-]{1,64}`.
fn sid_word(w: &str) -> Option<String> {
    (!w.is_empty()
        && w.len() <= 64
        && w.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| w.to_owned())
}

impl SysQuery {
    /// Parses the arguments after `sys`.
    ///
    /// # Errors
    /// [`USAGE`] for an unknown or second view, an unknown key, an invalid value, a bare `name`,
    /// or `scope=`, `depth=` or `instances=` on a view that does not take it;
    /// [`WHY_USAGE`] for any other word after `why`.
    pub fn parse(rest: &str) -> Result<Self, &'static str> {
        let mut q = Self {
            view: View::Overview,
            scope: None,
            instances_all: false,
            top: TOP_DEFAULT,
            top_set: false,
            depth: None,
            fresh_ms: None,
            since: None,
            json: false,
            caller: None,
            name: None,
            subject: None,
        };
        let mut view_seen = false;
        let mut naming = false;
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("scope=") {
                q.scope = Some(ScopeArg::from_word(v).ok_or(USAGE)?);
            } else if let Some(v) = tok.strip_prefix("instances=") {
                if v != "all" {
                    return Err(USAGE);
                }
                q.instances_all = true;
            } else if let Some(v) = tok.strip_prefix("top=") {
                let n: usize = v.parse().map_err(|_| USAGE)?;
                if !(1..=TOP_MAX).contains(&n) {
                    return Err(USAGE);
                }
                q.top = n;
                q.top_set = true;
            } else if let Some(v) = tok.strip_prefix("depth=") {
                let d: u8 = v.parse().map_err(|_| USAGE)?;
                if !(1..=8).contains(&d) {
                    return Err(USAGE);
                }
                q.depth = Some(d);
            } else if let Some(v) = tok.strip_prefix("fresh=") {
                let ms: u64 = v.parse().map_err(|_| USAGE)?;
                q.fresh_ms = Some(if ms == 0 { 0 } else { ms.max(FRESH_MIN_MS) });
            } else if let Some(v) = tok.strip_prefix("since=") {
                q.since = Some(Cursor::parse(v).ok_or(USAGE)?);
            } else if let Some(v) = tok.strip_prefix("caller=") {
                q.caller = Some(sid_word(v).ok_or(USAGE)?);
            } else if tok == "--json" || tok == "json" {
                q.json = true;
            } else if naming && let Some(v) = tok.strip_prefix("job=") {
                q.name = Some(NameTarget::Job(id_word(v).ok_or(USAGE)?));
            } else if naming && let Some(v) = tok.strip_prefix("task=") {
                q.name = Some(NameTarget::Task(id_word(v).ok_or(USAGE)?));
            } else if q.view == View::Why {
                q.subject = match tok {
                    "cpu" | "global.cpu" if q.subject.is_none() => Some(WhySubject::Cpu),
                    "mem" | "global.mem" if q.subject.is_none() => Some(WhySubject::Mem),
                    _ => return Err(WHY_USAGE),
                };
            } else if !view_seen && tok == "name" {
                view_seen = true;
                naming = true;
                q.view = View::Jobs;
            } else if !view_seen && let Some(v) = View::from_word(tok) {
                view_seen = true;
                q.view = v;
            } else {
                return Err(USAGE);
            }
        }
        if naming && q.name.is_none() {
            return Err(USAGE);
        }
        if q.scope.is_some() && q.view.default_scope().is_none() {
            return Err(USAGE);
        }
        if q.depth.is_some() && q.view != View::Disk {
            return Err(USAGE);
        }
        if q.instances_all {
            let taken = match q.view {
                View::Overview
                | View::Aterm
                | View::Global
                | View::Cores
                | View::Gpu
                | View::Mem => true,
                View::Procs => q.scope == Some(ScopeArg::Aterm),
                _ => false,
            };
            if !taken || naming {
                return Err(USAGE);
            }
        }
        Ok(q)
    }

    /// The scope the request reads (`scope=`, else the view's default).
    #[must_use]
    pub fn effective_scope(&self) -> Option<ScopeArg> {
        self.scope.or(self.view.default_scope())
    }

    /// Returns whether the request reads only aggregates or the target tab's own rows (`disk` up
    /// to its window, other tabs' writers withheld and their folders unnamed).
    #[must_use]
    pub fn target_rows_only(&self) -> bool {
        if self.instances_all || self.name.is_some() {
            return false;
        }
        match self.view {
            View::Overview
            | View::Tab
            | View::Global
            | View::Cores
            | View::Gpu
            | View::Mem
            | View::Why => true,
            View::Aterm => false,
            View::Jobs | View::Procs => {
                matches!(self.effective_scope(), Some(ScopeArg::Tab | ScopeArg::Pane))
            }
            View::Disk => matches!(
                self.effective_scope(),
                Some(ScopeArg::Tab | ScopeArg::Pane | ScopeArg::Window)
            ),
        }
    }

    /// Returns the default snapshot freshness threshold in milliseconds for `self.view`.
    #[must_use]
    pub fn freshness_default_ms(&self) -> u64 {
        match self.view {
            View::Cores => 1_000,
            View::Why => 5_000,
            View::Disk => 10_000,
            _ => 2_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bare_verb_is_the_overview() {
        let q = SysQuery::parse("").unwrap();
        assert_eq!(q.view, View::Overview);
        assert_eq!(q.top, TOP_DEFAULT);
        assert!(!q.json && q.since.is_none() && q.scope.is_none());
        assert!(q.target_rows_only());
    }

    #[test]
    fn every_view_word_parses_with_its_options() {
        for v in View::ALL {
            let q = SysQuery::parse(v.word()).unwrap();
            assert_eq!(q.view, v);
        }
        let q =
            SysQuery::parse("procs top=12 fresh=100 --json since=7f3a.4812 caller=s-ab12").unwrap();
        assert_eq!(q.view, View::Procs);
        assert_eq!(q.top, 12);
        assert_eq!(q.fresh_ms, Some(FRESH_MIN_MS), "below 250 reads as 250");
        assert!(q.json);
        assert_eq!(
            q.since,
            Some(Cursor {
                epoch: 0x7f3a,
                seq: 4812
            })
        );
        assert_eq!(q.caller.as_deref(), Some("s-ab12"));
        assert_eq!(SysQuery::parse("fresh=0").unwrap().fresh_ms, Some(0));
        assert_eq!(SysQuery::parse("disk depth=3").unwrap().depth, Some(3));
        assert_eq!(SysQuery::parse("cores json").map(|q| q.json), Ok(true));
    }

    #[test]
    fn refusals_answer_the_usage_line() {
        for bad in [
            "bogus",
            "tab scope=global",
            "overview scope=tab",
            "global scope=aterm",
            "procs top=0",
            "procs top=65",
            "procs top=x",
            "procs depth=2",
            "disk depth=9",
            "since=7f3a",
            "since=zz.1",
            "procs instances=all",
            "jobs instances=all",
            "disk instances=all",
            "instances=some",
            "cores cores",
            "scope=moon procs",
            "name",
            "name job=a;b",
            "procs caller=a%20b",
            "why scope=tab",
            "why scope=global",
            "why instances=all",
            "why depth=2",
            "why top=65",
            "procs why",
            "cpu why",
        ] {
            assert_eq!(SysQuery::parse(bad), Err(USAGE), "{bad}");
        }
    }

    /// `why` takes at most one subject, either spelling; with none the reply
    /// chooses. Any other word after it, a second subject included, answers
    /// `why`'s own usage line, never the built one.
    #[test]
    fn why_takes_one_subject_and_answers_its_own_usage() {
        let subject = |s: &str| SysQuery::parse(s).map(|q| (q.view, q.subject));
        assert_eq!(subject("why"), Ok((View::Why, None)));
        for (text, want) in [
            ("why cpu", WhySubject::Cpu),
            ("why global.cpu", WhySubject::Cpu),
            ("why mem", WhySubject::Mem),
            ("why global.mem --json top=3", WhySubject::Mem),
        ] {
            assert_eq!(subject(text), Ok((View::Why, Some(want))), "{text}");
        }
        let q = SysQuery::parse("why top=3 fresh=0 since=7f3a.2 caller=s-1").unwrap();
        assert!(q.top_set && q.top == 3 && q.fresh_ms == Some(0) && q.since.is_some());
        assert!(!SysQuery::parse("why").unwrap().top_set);
        assert_eq!(SysQuery::parse("mem").unwrap().subject, None);
        for bad in [
            "why pid=1",
            "why cpu mem",
            "why mem mem",
            "why gpu",
            "why disk",
            "why tab.cpu",
            "why name",
            "why why",
            "why job=a1",
        ] {
            assert_eq!(SysQuery::parse(bad), Err(WHY_USAGE), "{bad}");
        }
        assert_eq!(
            SysQuery::parse("why").unwrap().freshness_default_ms(),
            5_000
        );
        assert_eq!(SysQuery::parse("why").unwrap().effective_scope(), None);
    }

    #[test]
    fn the_scope_and_instance_forms_are_classified_for_access() {
        let rows_only = |s: &str| SysQuery::parse(s).unwrap().target_rows_only();
        for ok in [
            "",
            "overview",
            "tab",
            "global",
            "cores",
            "gpu",
            "mem",
            "jobs",
            "disk",
            "procs",
            "procs scope=tab",
            "procs scope=pane",
            "jobs scope=pane",
            "disk scope=window",
            "mem top=20",
            "why",
            "why cpu",
            "why global.mem --json",
        ] {
            assert!(rows_only(ok), "{ok}");
        }
        for owner in [
            "aterm",
            "procs scope=window",
            "procs scope=aterm",
            "procs scope=global",
            "jobs scope=aterm",
            "disk scope=aterm",
            "disk scope=global",
            "overview instances=all",
            "global instances=all",
            "procs scope=aterm instances=all",
            "name job=a40977",
            "name task=b4u3h4kc3",
        ] {
            assert!(!rows_only(owner), "{owner}");
        }
    }

    #[test]
    fn a_cursor_round_trips() {
        let c = Cursor {
            epoch: 0x7f3a,
            seq: 4812,
        };
        assert_eq!(c.to_string(), "7f3a.4812");
        assert_eq!(Cursor::parse("7f3a.4812"), Some(c));
        assert_eq!(Cursor::parse(".1"), None);
        assert_eq!(Cursor::parse("1."), None);
        assert_eq!(Cursor::parse("12345678901234567.1"), None);
    }

    #[test]
    fn scopes_default_by_view() {
        assert_eq!(
            SysQuery::parse("procs").unwrap().effective_scope(),
            Some(ScopeArg::Tab)
        );
        assert_eq!(
            SysQuery::parse("mem").unwrap().effective_scope(),
            Some(ScopeArg::Global)
        );
        assert_eq!(SysQuery::parse("tab").unwrap().effective_scope(), None);
    }
}
