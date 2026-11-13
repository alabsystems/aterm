// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grammar and query parser for the `sys` control verb.

use crate::explain::WhySubject;

/// The usage line a refused `sys` request answers; a stray word after `why` answers [`WHY_USAGE`].
pub const USAGE: &str = "ERR usage: sys [overview|tab|aterm|global|jobs|cores|gpu|mem|disk|net|procs|why|report|left] \
                         [scope=pane|tab|window|request|aterm|global] [instances=all] \
                         [top=<1-64>] [depth=<1-8>] [fresh=<ms>] [since=<ep>.<seq>] [--json]; \
                         scope= on jobs disk procs (cores gpu mem net take it, rows unchanged), \
                         scope=pane|tab|aterm on left, depth= on \
                         disk, instances=all on overview aterm global cores gpu mem net and procs \
                         scope=aterm; or sys name job=<id>|task=<id> [--json], sys policy \
                         [status|collect|cancel] [--json] (aterm help sys)\n";
/// The usage line for a word after `why` that is not its one subject or an option.
pub const WHY_USAGE: &str = "ERR usage: sys why [cpu|mem] [top=<1-64>] [fresh=<ms>] [since=<ep>.<seq>] [--json] \
     (aterm help sys why)\n";

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
    /// Network interfaces, throughput rates and TCP connection state counts.
    Net,
    /// Process rows.
    Procs,
    /// Who uses the CPU or memory, and why.
    Why,
    /// Bounded CPU hardware, policy and preserved anomaly report.
    Report,
    /// What closed tabs and dead verify runs left running, and what macOS's program check holds,
    /// each with its proof and the token `end` takes.
    Left,
}

impl View {
    /// Every view, in the help's order.
    pub const ALL: [View; 14] = [
        Self::Overview,
        Self::Tab,
        Self::Aterm,
        Self::Global,
        Self::Jobs,
        Self::Cores,
        Self::Gpu,
        Self::Mem,
        Self::Disk,
        Self::Net,
        Self::Procs,
        Self::Why,
        Self::Report,
        Self::Left,
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
            Self::Net => "net",
            Self::Procs => "procs",
            Self::Why => "why",
            Self::Report => "report",
            Self::Left => "left",
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.word() == w)
    }

    /// Returns the default scope for `self`, or `None` for scope views and `why`.
    #[must_use]
    pub fn default_scope(self) -> Option<ScopeArg> {
        match self {
            Self::Overview | Self::Tab | Self::Aterm | Self::Global | Self::Why | Self::Report => {
                None
            }
            Self::Jobs | Self::Disk | Self::Procs | Self::Left => Some(ScopeArg::Tab),
            Self::Cores | Self::Gpu | Self::Mem | Self::Net => Some(ScopeArg::Global),
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
    /// Every tab of the request that owns the target's tab (ATERM-KI-1109-3).
    Request,
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
            Self::Request => "request",
            Self::Aterm => "aterm",
            Self::Global => "global",
        }
    }

    fn from_word(w: &str) -> Option<Self> {
        Some(match w {
            "pane" => Self::Pane,
            "tab" => Self::Tab,
            "window" => Self::Window,
            "request" => Self::Request,
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
        if ep.is_empty()
            || ep.len() > 16
            || seq.is_empty()
            || !ep.bytes().all(|b| b.is_ascii_hexdigit())
            || !seq.bytes().all(|b| b.is_ascii_digit())
        {
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
    /// or `scope=`, `depth=` or `instances=` on a view that does not take it, or read options
    /// other than JSON formatting or the advisory caller label on an explicit `name` action;
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
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(USAGE);
                }
                let n: usize = v.parse().map_err(|_| USAGE)?;
                if !(1..=TOP_MAX).contains(&n) {
                    return Err(USAGE);
                }
                q.top = n;
                q.top_set = true;
            } else if let Some(v) = tok.strip_prefix("depth=") {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(USAGE);
                }
                let d: u8 = v.parse().map_err(|_| USAGE)?;
                if !(1..=8).contains(&d) {
                    return Err(USAGE);
                }
                q.depth = Some(d);
            } else if let Some(v) = tok.strip_prefix("fresh=") {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(USAGE);
                }
                let ms: u64 = v.parse().map_err(|_| USAGE)?;
                q.fresh_ms = Some(if ms == 0 { 0 } else { ms.max(FRESH_MIN_MS) });
            } else if let Some(v) = tok.strip_prefix("since=") {
                q.since = Some(Cursor::parse(v).ok_or(USAGE)?);
            } else if let Some(v) = tok.strip_prefix("caller=") {
                q.caller = Some(sid_word(v).ok_or(USAGE)?);
            } else if tok == "--json" || tok == "json" {
                q.json = true;
            } else if naming && let Some(v) = tok.strip_prefix("job=") {
                if q.name.is_some() {
                    return Err(USAGE);
                }
                q.name = Some(NameTarget::Job(id_word(v).ok_or(USAGE)?));
            } else if naming && let Some(v) = tok.strip_prefix("task=") {
                if q.name.is_some() {
                    return Err(USAGE);
                }
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
        if naming
            && (q.scope.is_some()
                || q.top_set
                || q.depth.is_some()
                || q.fresh_ms.is_some()
                || q.since.is_some())
        {
            // Naming is an explicit action in the resolved session, not a scoped/delta read.
            return Err(USAGE);
        }
        if q.scope.is_some() && q.view.default_scope().is_none() {
            return Err(USAGE);
        }
        if q.depth.is_some() && q.view != View::Disk {
            return Err(USAGE);
        }
        if q.view == View::Left
            && matches!(
                q.scope,
                Some(ScopeArg::Window | ScopeArg::Request | ScopeArg::Global)
            )
        {
            return Err(USAGE);
        }
        if q.instances_all {
            let taken = match q.view {
                View::Overview
                | View::Aterm
                | View::Global
                | View::Cores
                | View::Gpu
                | View::Mem
                | View::Net => true,
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
            | View::Net
            | View::Why => true,
            View::Report => true,
            View::Aterm => false,
            View::Jobs | View::Procs | View::Left => {
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
#[path = "query_tests.rs"]
mod tests;
