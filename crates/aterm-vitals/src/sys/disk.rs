// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk walker seam for `sys disk` and `sys tab`.

use super::json::Jv;

/// The sizes of one root's last complete walk.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootSizes {
    /// Allocated bytes, hard links shared fairly.
    pub alloc: u64,
    /// Logical bytes.
    pub logical: u64,
    /// Entries (files and folders).
    pub entries: u64,
    /// The largest direct entry count of any folder in it.
    pub direct_entries_max: u32,
    /// The allocation's move between two complete walks, bytes per second (negative: freed).
    pub growth_bps: Option<i64>,
    /// Seconds since that walk ended.
    pub age_s: Option<u64>,
}

/// What the walker knows of one root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RootSize {
    /// No walker measures this host's roots.
    #[default]
    NotSampled,
    /// Claimed and queued, or a walk is running.
    Walking {
        /// Entries the running walk has listed (0 while queued).
        walked_entries: u64,
        /// Bytes the running walk has summed so far.
        partial_alloc: Option<u64>,
        /// The last complete walk, when one exists.
        last: Option<RootSizes>,
    },
    /// The last walk reached its end.
    Complete(RootSizes),
    /// Not measured, and why.
    NotMeasured(&'static str),
}

impl RootSize {
    /// The `state=` word, or `None` without a walker.
    #[must_use]
    pub fn state_word(&self) -> Option<&'static str> {
        match self {
            Self::NotSampled => None,
            Self::Walking { .. } => Some("walking"),
            Self::Complete(_) => Some("complete"),
            Self::NotMeasured(why) => Some(why),
        }
    }

    /// Why no walk measured the root, or `None` when one measured it or is measuring it.
    #[must_use]
    pub fn not_measured_why(&self) -> Option<&'static str> {
        match self {
            Self::NotSampled => Some("not_sampled"),
            Self::NotMeasured(why) => Some(why),
            Self::Walking { .. } | Self::Complete(_) => None,
        }
    }

    /// The sizes that speak for the root.
    #[must_use]
    pub fn sizes(&self) -> Option<&RootSizes> {
        match self {
            Self::Complete(s) => Some(s),
            Self::Walking { last, .. } => last.as_ref(),
            _ => None,
        }
    }

    fn why(&self) -> &'static str {
        match self {
            Self::NotSampled => "not_sampled",
            Self::Walking { .. } => "walking",
            Self::Complete(_) => "not_sampled",
            Self::NotMeasured(why) => why,
        }
    }

    /// `alloc_bytes=` as a record value.
    #[must_use]
    pub fn alloc_text(&self) -> String {
        match self {
            Self::Complete(s) => s.alloc.to_string(),
            Self::Walking {
                partial_alloc: Some(p),
                ..
            } => format!("\u{2265}{p}"),
            other => format!("unavailable({})", other.why()),
        }
    }

    /// `alloc_bytes` in the JSON's four shapes.
    #[must_use]
    pub fn alloc_json(&self) -> Jv {
        match self {
            Self::Complete(s) => Jv::u(s.alloc),
            Self::Walking {
                partial_alloc: Some(p),
                ..
            } => Jv::at_least(*p, "walking"),
            other => Jv::why(other.why()),
        }
    }

    fn figure_text(&self, f: impl Fn(&RootSizes) -> u64) -> String {
        match self {
            Self::Complete(s) => f(s).to_string(),
            other => format!("unavailable({})", other.why()),
        }
    }

    fn figure_json(&self, f: impl Fn(&RootSizes) -> u64) -> Jv {
        match self {
            Self::Complete(s) => Jv::u(f(s)),
            other => Jv::why(other.why()),
        }
    }

    /// `entries=`.
    #[must_use]
    pub fn entries_text(&self) -> String {
        self.figure_text(|s| s.entries)
    }

    /// `growth_bps=`: the complete walk's, else the last one's behind a running walk.
    #[must_use]
    pub fn growth_text(&self) -> String {
        match self.sizes().and_then(|s| s.growth_bps) {
            Some(g) => g.to_string(),
            None => format!("unavailable({})", self.growth_why()),
        }
    }

    fn growth_why(&self) -> &'static str {
        match self {
            Self::Complete(_) => "warming",
            other => other.why(),
        }
    }

    /// The root's JSON fields, after `n`, `kinds` and `path`.
    pub fn json_fields(&self, o: &mut Jv) {
        o.set("alloc_bytes", self.alloc_json());
        o.set("logical_bytes", self.figure_json(|s| s.logical));
        o.set("entries", self.figure_json(|s| s.entries));
        o.set(
            "direct_entries_max",
            self.figure_json(|s| u64::from(s.direct_entries_max)),
        );
        o.set(
            "growth_bytes_per_s",
            match self.sizes().and_then(|s| s.growth_bps) {
                Some(g) => Jv::Int(g),
                None => Jv::why(self.growth_why()),
            },
        );
        match self {
            Self::NotSampled => {
                o.set("complete", Jv::why("not_sampled"));
            }
            Self::Complete(s) => {
                o.set("complete", Jv::Bool(true));
                o.set("walked_age_s", Jv::known_or(s.age_s, "not_sampled"));
            }
            Self::Walking {
                walked_entries,
                last,
                ..
            } => {
                o.set("complete", Jv::Bool(false));
                o.set("walked_entries", Jv::u(*walked_entries));
                o.set(
                    "last_complete_alloc_bytes",
                    Jv::known_or(last.as_ref().map(|l| l.alloc), "walking"),
                );
            }
            Self::NotMeasured(why) => {
                o.set("complete", Jv::why(why));
            }
        }
    }
}

/// One subtree inside a root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirSize {
    /// Its path relative to the root.
    pub rel: String,
    /// Allocated bytes.
    pub alloc: u64,
    /// Entries in its subtree.
    pub entries: u64,
}

impl DirSize {
    /// How deep it lies under its root.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.rel.split('/').filter(|c| !c.is_empty()).count()
    }
}

/// A folder past the entry-count line (§10.8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HugeDirFact {
    /// Its path.
    pub path: String,
    /// Entries directly in it.
    pub entries: u64,
    /// The root it lies in (1-based), where it lies in one.
    pub root: Option<usize>,
}

/// A scope's disk balance from the walker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskTotals {
    /// Bytes of the outermost roots.
    pub total: u64,
    /// …that no other tab claims.
    pub exclusive: u64,
    /// …that two or more tabs claim.
    pub shared: u64,
    /// A walk is queued or running for one of them.
    pub walking: bool,
    /// The why of each outermost root no walk measured ([`RootSize::not_measured_why`]), in
    /// root order: the figures leave those roots out, so they are lower bounds.
    pub not_measured: Vec<&'static str>,
    /// Σ growth of the complete outermost roots, bytes per second.
    pub growth_bps: Option<i64>,
    /// The §10.8 folders across its roots, most entries first.
    pub warnings: Vec<HugeDirFact>,
    /// `sys disk`'s scope lines, every scope read from one board (flow record §7.12).
    pub scopes: Vec<ScopeLine>,
    /// `sys disk`'s pane lines: each pane of the asked scope, where it is and what it adds.
    pub panes: Vec<PaneLine>,
}

/// One `scope` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeLine {
    /// This Mac: the used and total of home's pool.
    Global {
        /// Home's pool (`container:disk3`).
        pool: Option<String>,
        /// The volume's size less its free space (`aterm_sysprobe::VolumeReading::used`).
        used: Option<u64>,
        /// The volume's size.
        total: Option<u64>,
    },
    /// aterm's or a tab's bytes.
    Bytes(ScopeBytes),
}

/// aterm's or one tab's bytes, as the bar draws them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeBytes {
    /// `aterm` or `tab`.
    pub name: &'static str,
    /// The tab's number on its window's strip.
    pub tab: Option<usize>,
    /// The tab's name where it has no number (a parked worker tab: `Workers ▸ <label>`).
    pub tab_name: Option<String>,
    /// The tab's panes (none for aterm).
    pub keys: Vec<u64>,
    /// Home's pool.
    pub pool: Option<String>,
    /// What the bar draws: the scope's bytes after the clamp.
    pub bytes: u64,
    /// Whether it draws: some root counts.
    pub drawn: bool,
    /// Why it draws nothing (`walking`, `none_counted`).
    pub why: Option<&'static str>,
    /// Every walkable root on home's pool counts.
    pub complete: bool,
    /// Its roots.
    pub roots: usize,
    /// The roots that count.
    pub counted: usize,
    /// Seconds since the oldest walk its bytes sum ended.
    pub oldest_walk_s: Option<u64>,
    /// The clamp cut it.
    pub clamped: bool,
}

/// One `pane` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneLine {
    /// Its session key.
    pub key: u64,
    /// Its tab's number on its window's strip.
    pub tab: Option<usize>,
    /// Its tab's name where the tab has no number (a parked worker tab).
    pub tab_name: Option<String>,
    /// Its place in its tab, 1-based, in layout order (where the focus is moves nothing).
    pub pane: usize,
    /// Where its live directory came from (`fg`, `shell`, `osc7`).
    pub from: Option<&'static str>,
    /// The program whose directory it is.
    pub exe: Option<String>,
    /// The other machine a report named.
    pub host: Option<String>,
    /// Its live directory.
    pub live: Option<String>,
    /// Its project.
    pub project: Option<String>,
    /// Its project is a member of its tab's scope.
    pub member: bool,
    /// Why it adds nothing.
    pub why: Option<&'static str>,
}

impl DiskTotals {
    /// Whether the figures are lower bounds: a walk is owed, or a root they fold was not measured.
    #[must_use]
    pub fn at_least(&self) -> bool {
        self.walking || !self.not_measured.is_empty()
    }

    /// Why the figures stand for no size at all: a root was not measured, no walk is owed and
    /// nothing measured adds a byte, so `≥0` would say nothing. The first such root's why.
    #[must_use]
    pub fn unavailable_why(&self) -> Option<&'static str> {
        if self.walking || self.total > 0 {
            return None;
        }
        self.not_measured.first().copied()
    }

    /// The not-measured whys, each once, in root order.
    fn whys(&self) -> String {
        let mut seen: Vec<&str> = Vec::new();
        for w in &self.not_measured {
            if !seen.contains(w) {
                seen.push(w);
            }
        }
        seen.join(",")
    }

    /// One balance figure in the JSON's shapes: bare when exact; a lower bound whose cause is
    /// `walking`, else the first not-measured root's why; or that why alone when
    /// [`Self::unavailable_why`] names one.
    #[must_use]
    pub fn figure_json(&self, v: u64) -> Jv {
        if let Some(why) = self.unavailable_why() {
            return Jv::why(why);
        }
        match (self.walking, self.not_measured.first()) {
            (true, _) => Jv::at_least(v, "walking"),
            (false, Some(why)) => Jv::at_least(v, why),
            (false, None) => Jv::u(v),
        }
    }
}

/// Bytes as people read them, in decimal units.
#[must_use]
pub fn bytes_word(b: u64) -> String {
    let f = b as f64;
    if f >= 1e12 {
        format!("{:.2} TB", f / 1e12)
    } else if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.0} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{b} B")
    }
}

/// The scope's sentence clause for its roots' sizes.
#[must_use]
pub fn totals_clause(totals: Option<&DiskTotals>, bps_word: impl Fn(f64) -> String) -> String {
    let Some(t) = totals else {
        return "sizes unavailable(not_sampled)".to_owned();
    };
    let mut out = if t.unavailable_why().is_some() {
        format!("sizes unavailable({})", t.whys())
    } else {
        let ge = if t.at_least() { "\u{2265}" } else { "" };
        let mut out = format!("{ge}{} exclusive", bytes_word(t.exclusive));
        if t.shared > 0 {
            out.push_str(&format!(", {ge}{} shared", bytes_word(t.shared)));
        }
        match (t.walking, t.not_measured.len()) {
            (false, 0) => {}
            (true, 0) => out.push_str(" (walking)"),
            (false, n) => out.push_str(&format!(" ({n} not measured: {})", t.whys())),
            (true, n) => out.push_str(&format!(" (walking; {n} not measured: {})", t.whys())),
        }
        out
    };
    match t.growth_bps {
        Some(g) if g > 0 => out.push_str(&format!(" · growing {}", bps_word(g as f64))),
        Some(g) if g < 0 => out.push_str(&format!(
            " · shrinking {}",
            bps_word(g.unsigned_abs() as f64)
        )),
        Some(_) => out.push_str(" · not growing"),
        None => {}
    }
    out
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn tab_word(tab: Option<usize>, name: Option<&str>, pct: &dyn Fn(&str) -> String) -> String {
    tab.map_or_else(|| pct(name.unwrap_or("-")), |t| t.to_string())
}

impl ScopeLine {
    /// The `scope` line's text (`pct` escapes a free-text value, `unavailable` names a missing one).
    #[must_use]
    pub fn text(
        &self,
        pct: &dyn Fn(&str) -> String,
        unavailable: &dyn Fn(&str) -> String,
    ) -> String {
        let known =
            |v: Option<u64>| v.map_or_else(|| unavailable("not_sampled"), |v| v.to_string());
        let pool = |p: Option<&String>| p.map_or_else(|| unavailable("not_sampled"), |p| pct(p));
        match self {
            Self::Global {
                pool: p,
                used,
                total,
            } => format!(
                "scope name=global pool={} used_bytes={} total_bytes={}",
                pool(p.as_ref()),
                known(*used),
                known(*total)
            ),
            Self::Bytes(b) => {
                let mut out = format!("scope name={}", b.name);
                if b.name == "tab" {
                    out.push_str(&format!(
                        " tab={}",
                        tab_word(b.tab, b.tab_name.as_deref(), pct)
                    ));
                }
                out.push_str(&format!(
                    " pool={} bytes={} drawn={}",
                    pool(b.pool.as_ref()),
                    b.bytes,
                    yes(b.drawn)
                ));
                if let Some(why) = b.why {
                    out.push_str(&format!(" why={why}"));
                }
                out.push_str(&format!(
                    " complete={} roots={} counted={} oldest_walk_s={} clamped={}",
                    yes(b.complete),
                    b.roots,
                    b.counted,
                    known(b.oldest_walk_s),
                    yes(b.clamped)
                ));
                out
            }
        }
    }

    /// The `scopes` array's object: the line's keys.
    #[must_use]
    pub fn json(&self) -> Jv {
        let known = |v: Option<u64>| Jv::known_or(v, "not_sampled");
        let pool =
            |p: Option<&String>| p.map_or_else(|| Jv::why("not_sampled"), |p| Jv::s(p.clone()));
        match self {
            Self::Global {
                pool: p,
                used,
                total,
            } => Jv::obj([
                ("name", Jv::s("global")),
                ("pool", pool(p.as_ref())),
                ("used_bytes", known(*used)),
                ("total_bytes", known(*total)),
            ]),
            Self::Bytes(b) => {
                let mut o = Jv::obj([("name", Jv::s(b.name))]);
                if b.name == "tab" {
                    match (b.tab, b.tab_name.as_ref()) {
                        (Some(t), _) => o.set("tab", Jv::u(t as u64)),
                        (None, Some(n)) => o.set("tab_name", Jv::s(n.clone())),
                        (None, None) => o.set("tab", Jv::why("not_sampled")),
                    }
                }
                o.set("pool", pool(b.pool.as_ref()));
                o.set("bytes", Jv::u(b.bytes));
                o.set("drawn", Jv::Bool(b.drawn));
                if let Some(why) = b.why {
                    o.set("why_not", Jv::s(why));
                }
                o.set("complete", Jv::Bool(b.complete));
                o.set("roots", Jv::u(b.roots as u64));
                o.set("counted", Jv::u(b.counted as u64));
                o.set("oldest_walk_s", known(b.oldest_walk_s));
                o.set("clamped", Jv::Bool(b.clamped));
                o
            }
        }
    }
}

impl PaneLine {
    /// The `pane` line's text; `path_ok` false prints where its folders lie (`under=`), not them.
    #[must_use]
    pub fn text(
        &self,
        path_ok: bool,
        home: Option<&str>,
        pct: &dyn Fn(&str) -> String,
        exe_word: &dyn Fn(&str) -> String,
    ) -> String {
        let mut out = format!(
            "pane tab={} pane={}",
            tab_word(self.tab, self.tab_name.as_deref(), pct),
            self.pane
        );
        out.push_str(&format!(" from={}", self.from.unwrap_or("-")));
        if let Some(exe) = self.exe.as_deref() {
            out.push_str(&format!(" exe={}", exe_word(exe)));
        }
        if let Some(host) = self.host.as_deref() {
            out.push_str(&format!(" host={}", pct(host)));
        }
        let path = |p: Option<&str>| p.map_or_else(|| "-".to_owned(), pct);
        match (path_ok, self.live.as_deref()) {
            (false, Some(live)) => {
                out.push_str(&format!(" under={}", super::under(live, home)));
            }
            _ => out.push_str(&format!(
                " live={} project={}",
                path(self.live.as_deref()),
                path(self.project.as_deref())
            )),
        }
        out.push_str(&format!(" member={}", yes(self.member)));
        if let Some(why) = self.why {
            out.push_str(&format!(" why={why}"));
        }
        out
    }

    /// The `panes` array's object: the line's keys.
    #[must_use]
    pub fn json(&self, path_ok: bool, home: Option<&str>, exe_word: &dyn Fn(&str) -> String) -> Jv {
        let mut o = Jv::Obj(Vec::new());
        match (self.tab, self.tab_name.as_ref()) {
            (Some(t), _) => o.set("tab", Jv::u(t as u64)),
            (None, Some(n)) => o.set("tab_name", Jv::s(n.clone())),
            (None, None) => o.set("tab", Jv::why("not_sampled")),
        }
        o.set("pane", Jv::u(self.pane as u64));
        o.set(
            "from",
            self.from.map_or_else(|| Jv::why("not_sampled"), Jv::s),
        );
        if let Some(exe) = self.exe.as_deref() {
            o.set("exe", Jv::s(exe_word(exe)));
        }
        if let Some(host) = self.host.as_deref() {
            o.set("host", Jv::s(host));
        }
        match (path_ok, self.live.as_deref()) {
            (false, Some(live)) => o.set("under", Jv::s(super::under(live, home))),
            _ => {
                let path = |p: Option<&String>| {
                    p.map_or_else(|| Jv::why("not_sampled"), |p| Jv::s(p.clone()))
                };
                o.set("live", path(self.live.as_ref()));
                o.set("project", path(self.project.as_ref()));
            }
        }
        o.set("member", Jv::Bool(self.member));
        if let Some(why) = self.why {
            o.set("why_not", Jv::s(why));
        }
        o
    }
}

/// The scope's JSON balance fields.
pub fn totals_json(totals: Option<&DiskTotals>, o: &mut Jv) {
    match totals {
        None => {
            o.set("exclusive_bytes", Jv::why("not_sampled"));
            o.set("shared_bytes", Jv::why("not_sampled"));
            o.set("total_bytes", Jv::why("not_sampled"));
            o.set("growth_bytes_per_s", Jv::why("not_sampled"));
            o.set("not_measured", Jv::why("not_sampled"));
            o.set("warnings", Jv::why("not_sampled"));
        }
        Some(t) => {
            o.set("exclusive_bytes", t.figure_json(t.exclusive));
            o.set("shared_bytes", t.figure_json(t.shared));
            o.set("total_bytes", t.figure_json(t.total));
            o.set(
                "growth_bytes_per_s",
                t.growth_bps.map_or_else(
                    || Jv::why(t.unavailable_why().unwrap_or("warming")),
                    Jv::Int,
                ),
            );
            o.set(
                "not_measured",
                Jv::Arr(t.not_measured.iter().map(|w| Jv::s(*w)).collect()),
            );
            o.set(
                "warnings",
                Jv::Arr(
                    t.warnings
                        .iter()
                        .map(|w| {
                            let mut h = Jv::obj([
                                ("kind", Jv::s("huge_dir")),
                                ("entries", Jv::u(w.entries)),
                                ("line", Jv::u(100_000)),
                            ]);
                            if let Some(r) = w.root {
                                h.set("root", Jv::u(r as u64));
                            }
                            h
                        })
                        .collect(),
                ),
            );
        }
    }
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
