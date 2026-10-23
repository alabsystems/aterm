// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALKER SEAM of `sys disk` and `sys tab` (design §10.4, §10.5, §13.2, §13.6): what the disk
//! walker knows of a scope's folders, as plain values the views print.

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
        /// Bytes the running walk has summed so far: a lower bound.
        partial_alloc: Option<u64>,
        /// The last complete walk, when one exists.
        last: Option<RootSizes>,
    },
    /// The last walk reached its end.
    Complete(RootSizes),
    /// Not measured, and why: one of the disk unavailable words (`protected`, `remote`,
    /// `removable`, `dataless`, `other_user`, `too_broad`).
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

    /// The sizes that speak for the root: the complete walk's, else the last complete one behind a
    /// running walk.
    #[must_use]
    pub fn sizes(&self) -> Option<&RootSizes> {
        match self {
            Self::Complete(s) => Some(s),
            Self::Walking { last, .. } => last.as_ref(),
            _ => None,
        }
    }

    /// The word an unmeasured figure of this root carries.
    fn why(&self) -> &'static str {
        match self {
            Self::NotSampled => "not_sampled",
            Self::Walking { .. } => "walking",
            Self::Complete(_) => "not_sampled",
            Self::NotMeasured(why) => why,
        }
    }

    /// `alloc_bytes=` as a record value: bare when complete, `≥n` while a walk runs, else
    /// `unavailable(<why>)`.
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

    /// A complete figure of the root (`entries`, `logical_bytes`) as a record
    /// value.
    fn figure_text(&self, f: impl Fn(&RootSizes) -> u64) -> String {
        match self {
            Self::Complete(s) => f(s).to_string(),
            other => format!("unavailable({})", other.why()),
        }
    }

    /// …and in JSON.
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

/// One subtree inside a root: where the space is.
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
    /// How deep it lies under its root: its path's component count.
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

/// A scope's disk balance from the walker: over the outermost roots, `total = exclusive + shared`,
/// each folder once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskTotals {
    /// Bytes of the outermost roots.
    pub total: u64,
    /// …that no other tab claims.
    pub exclusive: u64,
    /// …that two or more tabs claim.
    pub shared: u64,
    /// A walk is queued or running for one of them: the figures are lower bounds.
    pub walking: bool,
    /// Σ growth of the complete outermost roots, bytes per second.
    pub growth_bps: Option<i64>,
    /// The §10.8 folders across its roots, most entries first.
    pub warnings: Vec<HugeDirFact>,
}

/// Bytes as people read them, in decimal units: `15.3 GB`, `820 MB`.
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

/// The scope's sentence clause for its roots' sizes: `15.3 GB exclusive · growing 7.4 MB/s`, `≥2.1
/// GB exclusive (walking)`, or `sizes unavailable(not_sampled)` without a walker.
#[must_use]
pub fn totals_clause(totals: Option<&DiskTotals>, bps_word: impl Fn(f64) -> String) -> String {
    let Some(t) = totals else {
        return "sizes unavailable(not_sampled)".to_owned();
    };
    let ge = if t.walking { "\u{2265}" } else { "" };
    let mut out = format!("{ge}{} exclusive", bytes_word(t.exclusive));
    if t.shared > 0 {
        out.push_str(&format!(", {ge}{} shared", bytes_word(t.shared)));
    }
    if t.walking {
        out.push_str(" (walking)");
    }
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

/// The scope's JSON balance fields.
pub fn totals_json(totals: Option<&DiskTotals>, o: &mut Jv) {
    match totals {
        None => {
            o.set("exclusive_bytes", Jv::why("not_sampled"));
            o.set("shared_bytes", Jv::why("not_sampled"));
            o.set("total_bytes", Jv::why("not_sampled"));
            o.set("growth_bytes_per_s", Jv::why("not_sampled"));
            o.set("warnings", Jv::why("not_sampled"));
        }
        Some(t) => {
            let fig = |v: u64| {
                if t.walking {
                    Jv::at_least(v, "walking")
                } else {
                    Jv::u(v)
                }
            };
            o.set("exclusive_bytes", fig(t.exclusive));
            o.set("shared_bytes", fig(t.shared));
            o.set("total_bytes", fig(t.total));
            o.set(
                "growth_bytes_per_s",
                t.growth_bps.map_or_else(|| Jv::why("warming"), Jv::Int),
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
mod tests {
    use super::*;
    use crate::sys::json::contract_violations;

    fn sizes(alloc: u64) -> RootSizes {
        RootSizes {
            alloc,
            logical: alloc / 2,
            entries: 63_039,
            direct_entries_max: 900,
            growth_bps: Some(7_400_000),
            age_s: Some(3),
        }
    }

    #[test]
    fn a_root_without_a_walker_says_not_sampled_never_zero() {
        let r = RootSize::NotSampled;
        assert_eq!(r.state_word(), None);
        assert_eq!(r.alloc_text(), "unavailable(not_sampled)");
        assert_eq!(r.entries_text(), "unavailable(not_sampled)");
        assert_eq!(r.growth_text(), "unavailable(not_sampled)");
        let mut o = Jv::obj::<&str>([]);
        r.json_fields(&mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
        assert_eq!(o.get("alloc_bytes"), Some(&Jv::why("not_sampled")));
    }

    #[test]
    fn a_walking_root_is_a_lower_bound_with_its_progress() {
        let r = RootSize::Walking {
            walked_entries: 12_000,
            partial_alloc: Some(2_100_000_000),
            last: Some(sizes(7_050_000_000)),
        };
        assert_eq!(r.state_word(), Some("walking"));
        assert_eq!(r.alloc_text(), "\u{2265}2100000000");
        assert_eq!(r.entries_text(), "unavailable(walking)");
        assert_eq!(r.growth_text(), "7400000");
        let mut o = Jv::obj::<&str>([]);
        r.json_fields(&mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
        assert_eq!(
            o.get("alloc_bytes"),
            Some(&Jv::at_least(2_100_000_000, "walking"))
        );
        assert_eq!(o.get("walked_entries"), Some(&Jv::u(12_000)));
        assert_eq!(
            o.get("last_complete_alloc_bytes"),
            Some(&Jv::u(7_050_000_000))
        );
        let q = RootSize::Walking {
            walked_entries: 0,
            partial_alloc: None,
            last: None,
        };
        assert_eq!(q.alloc_text(), "unavailable(walking)");
    }

    #[test]
    fn a_complete_root_prints_bare_figures_and_a_refused_one_its_word() {
        let r = RootSize::Complete(sizes(7_050_000_000));
        assert_eq!(r.alloc_text(), "7050000000");
        assert_eq!(r.entries_text(), "63039");
        let mut o = Jv::obj::<&str>([]);
        r.json_fields(&mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
        assert_eq!(o.get("complete"), Some(&Jv::Bool(true)));
        let p = RootSize::NotMeasured("protected");
        assert_eq!(p.alloc_text(), "unavailable(protected)");
        let mut o = Jv::obj::<&str>([]);
        p.json_fields(&mut o);
        assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    }

    #[test]
    fn the_totals_clause_and_json_keep_the_four_shapes() {
        let bps = |b: f64| format!("{:.1} MB/s", b / 1e6);
        assert_eq!(totals_clause(None, bps), "sizes unavailable(not_sampled)");
        let t = DiskTotals {
            total: 15_300_000_000,
            exclusive: 15_300_000_000,
            shared: 0,
            walking: false,
            growth_bps: Some(7_400_000),
            warnings: vec![HugeDirFact {
                path: "/x/target/debug/deps".into(),
                entries: 120_000,
                root: Some(1),
            }],
        };
        assert_eq!(
            totals_clause(Some(&t), bps),
            "15.3 GB exclusive · growing 7.4 MB/s"
        );
        let w = DiskTotals {
            walking: true,
            shared: 2_000_000_000,
            growth_bps: None,
            ..t.clone()
        };
        assert_eq!(
            totals_clause(Some(&w), bps),
            "\u{2265}15.3 GB exclusive, \u{2265}2.0 GB shared (walking)"
        );
        for totals in [None, Some(&t), Some(&w)] {
            let mut o = Jv::obj::<&str>([]);
            totals_json(totals, &mut o);
            assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
        }
    }

    #[test]
    fn a_subtree_knows_its_depth() {
        let d = |p: &str| DirSize {
            rel: p.into(),
            ..DirSize::default()
        };
        assert_eq!(d("target").depth(), 1);
        assert_eq!(d("scratchpad/target-probe/debug").depth(), 3);
        assert_eq!(bytes_word(820_000_000), "820 MB");
        assert_eq!(bytes_word(1_850_000_000_000), "1.85 TB");
    }
}
