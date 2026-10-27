// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Index succession: what a newer signed index may change about where a program comes from,
//! judged against the last index this client trusted.
//!
//! A signature proves that a rostered machine wrote an index, not that the writer kept
//! `atpkg-publish index`'s rules. Two of those rules have no override. A pin is never lowered,
//! in `pin` or in a `pin_by_target` cell: a rollback is a yank or a `min_build`. A
//! `[programs.<p>].repo` that moves names a new build above the head's pin, and above the
//! head's cell on each target that keeps one, because a build number names a release on one
//! repo. A writer that does not keep them, such as the retired shell publisher with a current
//! roster, signs an index that every client would otherwise follow: it can move a program
//! back to an old repo under a pin that repo never released.
//!
//! So [`check`] holds each newer index to those two rules against the [`Sources`] of the
//! last index this client trusted. A newer index that breaks either one is refused whole, and
//! the client stays on the index it trusted. It refuses the whole index, not just the program,
//! because:
//! - a coherence group applies as one tuple, and a channel mixed from two indexes is one that
//!   no publisher signed;
//! - a writer that broke one rule kept none of the others for the index's other rows.
//!
//! The record binds only within its roster generation, as the build floor does
//! ([`crate::sig::BuildFloor::admits`]). A new generation is the paper master's re-issue,
//! which can rescue a store that a bad index drove out of reach. With no record that binds,
//! the oldest candidate of the newest generation starts the chain, so a forgery that follows
//! a sound index in the same release window is still refused on first contact.
//! `<prefix>/index-sources.toml` keeps the record ([`Record`]), with the newer indexes the
//! last selection refused. `aterm pkg doctor`'s `base.pkg.index-succession` row reads it.
//!
//! Two limits follow from judging against the last index this client trusted rather than
//! the head each index was planned on. Both hold a client back rather than move it, and an
//! index that raises the pin past the trusted one, or a new roster generation, releases it:
//! - An index with an absurdly raised pin that a rostered key signs is admitted, and the
//!   publisher's next indexes, which lower nothing against their head, are then refused as
//!   downgrades.
//! - The publisher lets `--retire` (or `--retire-target`) drop a pin and a later index add
//!   it back at any build in any repo. A client that saw the retiring index follows that,
//!   since it judges each step against the one before. A client that never saw it, because
//!   the release window moved past it while the client was away, judges the re-added pin
//!   against the one it trusted, and refuses it if the pin is lower or the repo moved under
//!   the same build.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::manifest::Index;
use crate::store::Layout;

const RECORD_SCHEMA: u32 = 1;
const MAX_RECORD_BYTES: usize = 1 << 20;
/// The refusals a record keeps. One selection weighs at most a release window of candidates
/// plus the cache it falls back to.
const MAX_REFUSALS: usize = 24;

/// One channel's pins as a trusted index set them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    /// `pin`: program -> build.
    #[serde(default)]
    pub pin: BTreeMap<String, u64>,
    /// `pin_by_target`: triple -> (program -> build).
    #[serde(default)]
    pub pin_by_target: BTreeMap<String, BTreeMap<String, u64>>,
}

/// What one trusted index says about where each program comes from, which a newer index is
/// held to ([`check`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    /// The trusted index's `index_build`.
    pub index_build: u64,
    /// The roster generation that authorized it. The record binds only an index of the same
    /// generation.
    pub roster_seq: u64,
    /// `[programs.<p>].repo`, per program.
    #[serde(default)]
    pub repo: BTreeMap<String, String>,
    /// Each `[[channels]]` entry's pins, by channel name.
    #[serde(default)]
    pub channel: BTreeMap<String, Pins>,
}

impl Sources {
    /// The sources `index` names, authorized under roster generation `roster_seq`.
    #[must_use]
    pub fn of(index: &Index, roster_seq: u64) -> Self {
        Self {
            index_build: index.index_build,
            roster_seq,
            repo: index
                .programs
                .iter()
                .map(|(p, block)| (p.clone(), block.repo.clone()))
                .collect(),
            channel: index
                .channels
                .iter()
                .map(|c| {
                    (
                        c.name.clone(),
                        Pins {
                            pin: c.pin.clone(),
                            pin_by_target: c.pin_by_target.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// A rule of `atpkg-publish index` that a newer index breaks against the trusted one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Break {
    /// `pin` lowers a program's build.
    Downgrade {
        /// The channel.
        channel: String,
        /// The program.
        program: String,
        /// The trusted index's build.
        from: u64,
        /// The newer index's build.
        to: u64,
    },
    /// A `pin_by_target` cell lowers a program's build on one target.
    TargetDowngrade {
        /// The channel.
        channel: String,
        /// The target triple.
        triple: String,
        /// The program.
        program: String,
        /// The trusted index's build.
        from: u64,
        /// The newer index's build.
        to: u64,
    },
    /// `[programs.<p>].repo` moves, and the pin does not name a new build above the trusted
    /// one.
    RepoSwitch {
        /// The channel whose pin did not rise.
        channel: String,
        /// The program.
        program: String,
        /// The trusted index's repo.
        from_repo: String,
        /// The newer index's repo.
        to_repo: String,
        /// The trusted index's build.
        from: u64,
        /// The newer index's build.
        to: u64,
    },
    /// `[programs.<p>].repo` moves, and a `pin_by_target` cell both indexes hold does not
    /// name a new build above the trusted cell: that target would fetch the old repo's build
    /// number from the new repo.
    TargetRepoSwitch {
        /// The channel.
        channel: String,
        /// The target triple.
        triple: String,
        /// The program.
        program: String,
        /// The trusted index's repo.
        from_repo: String,
        /// The newer index's repo.
        to_repo: String,
        /// The trusted index's build on `triple`.
        from: u64,
        /// The newer index's build on `triple`.
        to: u64,
    },
}

impl fmt::Display for Break {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Downgrade {
                channel,
                program,
                from,
                to,
            } => write!(
                f,
                "channel {channel} lowers the {program} pin from {from} to {to} (a rollback is \
                 a yank or a min_build, never a lowered pin)"
            ),
            Self::TargetDowngrade {
                channel,
                triple,
                program,
                from,
                to,
            } => write!(
                f,
                "channel {channel} lowers the {program} pin on {triple} from {from} to {to} (a \
                 rollback is a yank or a min_build, never a lowered pin)"
            ),
            Self::RepoSwitch {
                channel,
                program,
                from_repo,
                to_repo,
                from,
                to,
            } => write!(
                f,
                "[programs.{program}] moves from repo {from_repo:?} to {to_repo:?}, but channel \
                 {channel} pins {program} at {to}, not a new build above {from} (a repo switch \
                 names a new build)"
            ),
            Self::TargetRepoSwitch {
                channel,
                triple,
                program,
                from_repo,
                to_repo,
                from,
                to,
            } => write!(
                f,
                "[programs.{program}] moves from repo {from_repo:?} to {to_repo:?}, but channel \
                 {channel} pins {program} on {triple} at {to}, not a new build above {from} (a \
                 repo switch names a new build on every target it pins)"
            ),
        }
    }
}

/// The repo `next` names for `program` when it is not the one `prior` trusted.
fn moved_repo<'a>(
    prior: &'a Sources,
    next: &'a Index,
    program: &str,
) -> Option<(&'a str, &'a str)> {
    let from = prior.repo.get(program)?;
    let to = &next.programs.get(program)?.repo;
    (from != to).then_some((from.as_str(), to.as_str()))
}

/// Every rule `next` breaks as a successor of `prior`. Empty when `next` may succeed it.
///
/// Only what both name is compared: a program, channel or target cell that either one lacks
/// was added or retired, which the publisher allows. A repo move is held to the pin and to
/// every target cell both indexes hold for the program, since each names a build to fetch
/// from the new repo.
#[must_use]
pub fn check(prior: &Sources, next: &Index) -> Vec<Break> {
    let mut breaks = Vec::new();
    for c in &next.channels {
        let Some(was) = prior.channel.get(&c.name) else {
            continue;
        };
        for (program, &to) in &c.pin {
            let Some(&from) = was.pin.get(program) else {
                continue;
            };
            if to < from {
                breaks.push(Break::Downgrade {
                    channel: c.name.clone(),
                    program: program.clone(),
                    from,
                    to,
                });
            }
            if let Some((from_repo, to_repo)) = moved_repo(prior, next, program)
                && to <= from
            {
                breaks.push(Break::RepoSwitch {
                    channel: c.name.clone(),
                    program: program.clone(),
                    from_repo: from_repo.to_string(),
                    to_repo: to_repo.to_string(),
                    from,
                    to,
                });
            }
        }
        for (triple, cells) in &c.pin_by_target {
            let Some(old) = was.pin_by_target.get(triple) else {
                continue;
            };
            for (program, &to) in cells {
                let Some(&from) = old.get(program) else {
                    continue;
                };
                if to < from {
                    breaks.push(Break::TargetDowngrade {
                        channel: c.name.clone(),
                        triple: triple.clone(),
                        program: program.clone(),
                        from,
                        to,
                    });
                }
                if let Some((from_repo, to_repo)) = moved_repo(prior, next, program)
                    && to <= from
                {
                    breaks.push(Break::TargetRepoSwitch {
                        channel: c.name.clone(),
                        triple: triple.clone(),
                        program: program.clone(),
                        from_repo: from_repo.to_string(),
                        to_repo: to_repo.to_string(),
                        from,
                        to,
                    });
                }
            }
        }
    }
    breaks
}

/// A newer index that a selection refused against the trusted one, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The release it came from.
    pub label: String,
    /// Its `index_build`.
    pub index_build: u64,
    /// The machine whose key signed it.
    pub machine_id: String,
    /// The index the refusal was judged against.
    pub trusted_build: u64,
    /// The rules it breaks.
    pub breaks: Vec<Break>,
}

impl Refusal {
    /// The refusal as a record row.
    #[must_use]
    pub fn row(&self) -> Refused {
        Refused {
            index_build: self.index_build,
            machine_id: self.machine_id.clone(),
            trusted_build: self.trusted_build,
            why: self
                .breaks
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

/// One refused index as `<prefix>/index-sources.toml` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// Its `index_build`.
    pub index_build: u64,
    /// The machine whose key signed it.
    pub machine_id: String,
    /// The index the refusal was judged against.
    pub trusted_build: u64,
    /// The rules it breaks, as one sentence.
    pub why: String,
}

impl Refused {
    /// The refusal as a sentence for a person.
    #[must_use]
    pub fn sentence(&self) -> String {
        format!(
            "index {} (signed by machine {}) is refused against index {}: {}",
            self.index_build, self.machine_id, self.trusted_build, self.why
        )
    }
}

/// `<prefix>/index-sources.toml`: the [`Sources`] of the last index this client trusted, and
/// the newer indexes that the last selection refused.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Record schema.
    pub schema: u32,
    /// The trusted index's sources.
    pub sources: Sources,
    /// The newer indexes refused against them, oldest first.
    #[serde(default)]
    pub refused: Vec<Refused>,
}

/// `<prefix>/index-sources.toml`.
#[must_use]
pub fn record_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("index-sources.toml")
}

impl Record {
    /// The record on disk, or `None` when it is absent, unreadable or of another schema.
    #[must_use]
    pub fn read(layout: &Layout) -> Option<Self> {
        let text =
            crate::metadata_io::read_bounded_regular_utf8(&record_path(layout), MAX_RECORD_BYTES)
                .ok()?;
        let record: Self = aterm_toml::from_str(&text).ok()?;
        (record.schema == RECORD_SCHEMA && record.refused.len() <= MAX_REFUSALS).then_some(record)
    }

    /// The sources a selection is held to: the record's, or none.
    #[must_use]
    pub fn sources_of(record: Option<&Self>) -> Option<&Sources> {
        record.map(|r| &r.sources)
    }
}

/// What [`trust`] wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trusted {
    /// The refusals that the record did not already hold, for the pass to say once.
    pub new_refusals: Vec<Refused>,
}

/// Record that this client trusts `selected` (when it is not older than the record on disk:
/// of an older roster generation, or of the same one at a lower build) and that the selection
/// refused `refused`.
///
/// Only the refusals newer than the index the record ends on are kept: an index the client
/// has moved past no longer holds it back. The record on disk is read again here, not taken
/// from the selection, so a pass that selected from older candidates while another pass
/// recorded a newer index does not move the record back.
///
/// # Errors
/// The record could not be written.
pub fn trust(
    layout: &Layout,
    selected: Option<&Sources>,
    refused: &[Refusal],
) -> io::Result<Trusted> {
    let on_disk = Record::read(layout);
    let prior = on_disk.as_ref();
    let sources = match (prior, selected) {
        (Some(p), Some(s)) if moves_back(&p.sources, s) => p.sources.clone(),
        (_, Some(s)) => s.clone(),
        (Some(p), None) => p.sources.clone(),
        (None, None) => return Ok(Trusted::default()),
    };
    let mut rows: Vec<Refused> = refused
        .iter()
        .filter(|r| r.index_build > sources.index_build)
        .map(Refusal::row)
        .collect();
    rows.sort_by_key(|r| r.index_build);
    rows.dedup();
    rows.truncate(MAX_REFUSALS);
    let next = Record {
        schema: RECORD_SCHEMA,
        sources,
        refused: rows,
    };
    let new_refusals = next
        .refused
        .iter()
        .filter(|r| prior.is_none_or(|p| !p.refused.contains(r)))
        .cloned()
        .collect();
    if prior != Some(&next) {
        let text = aterm_toml::to_string(&next)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if text.len() > MAX_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the index sources record exceeds its size limit",
            ));
        }
        crate::vendor_direct::durable::write_durable(&record_path(layout), text.as_bytes())?;
    }
    Ok(Trusted { new_refusals })
}

/// Whether recording `next` over `prior` would move the record back: to an older roster
/// generation, or within one to a lower build. A newer generation re-bases it, as it re-bases
/// the build floor.
const fn moves_back(prior: &Sources, next: &Sources) -> bool {
    next.roster_seq < prior.roster_seq
        || (next.roster_seq == prior.roster_seq && next.index_build < prior.index_build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Channel, Program};

    fn program(repo: &str) -> Program {
        Program {
            repo: repo.into(),
            policy: "prebuilt-only".into(),
            coherence_group: None,
            system: None,
            unavailable_hint: None,
            requires: Vec::new(),
        }
    }

    fn index(build: u64, repos: &[(&str, &str)], pins: &[(&str, u64)]) -> Index {
        Index {
            schema: 2,
            index_build: build,
            generated_at: String::new(),
            valid_until: "2099-01-01T00:00:00Z".into(),
            machine_id: None,
            roster_seq: Some(5),
            programs: repos
                .iter()
                .map(|(p, r)| ((*p).to_string(), program(r)))
                .collect(),
            channels: vec![Channel {
                name: "stable".into(),
                channel_build: build,
                min_build: 0,
                min_build_by_program: BTreeMap::new(),
                yanked: Vec::new(),
                pin: pins.iter().map(|(p, b)| ((*p).to_string(), *b)).collect(),
                pin_by_target: BTreeMap::new(),
            }],
        }
    }

    fn sources(i: &Index) -> Sources {
        Sources::of(i, 5)
    }

    #[test]
    fn a_raised_or_carried_pin_under_the_same_repo_succeeds() {
        let prior = index(
            77,
            &[("ay", "ay"), ("codex", "codex-alab")],
            &[("ay", 8256), ("codex", 2_026_102_501)],
        );
        let carried = index(
            78,
            &[("ay", "ay"), ("codex", "codex-alab")],
            &[("ay", 8256), ("codex", 2_026_102_501)],
        );
        let raised = index(
            78,
            &[("ay", "ay"), ("codex", "codex-alab")],
            &[("ay", 8300), ("codex", 2_026_102_601)],
        );
        assert_eq!(check(&sources(&prior), &carried), []);
        assert_eq!(check(&sources(&prior), &raised), []);
    }

    #[test]
    fn a_repo_switch_needs_a_new_build_above_the_trusted_pin() {
        let prior = index(77, &[("codex", "codex-alab")], &[("codex", 2_026_102_501)]);
        let back = index(78, &[("codex", "aterm")], &[("codex", 2_026_102_501)]);
        assert_eq!(
            check(&sources(&prior), &back),
            [Break::RepoSwitch {
                channel: "stable".into(),
                program: "codex".into(),
                from_repo: "codex-alab".into(),
                to_repo: "aterm".into(),
                from: 2_026_102_501,
                to: 2_026_102_501,
            }]
        );
        let lowered = index(78, &[("codex", "aterm")], &[("codex", 2_026_101_501)]);
        let breaks = check(&sources(&prior), &lowered);
        assert!(
            matches!(
                breaks.as_slice(),
                [Break::Downgrade { .. }, Break::RepoSwitch { .. }]
            ),
            "{breaks:?}"
        );
        // The control: the same switch under a new build is what the publisher signs.
        let switched = index(78, &[("codex", "aterm")], &[("codex", 2_026_102_601)]);
        assert_eq!(check(&sources(&prior), &switched), []);
    }

    #[test]
    fn a_repo_switch_needs_a_new_build_on_every_target_cell_it_keeps() {
        const LINUX: &str = "x86_64-unknown-linux-gnu";
        let with_cell = |build, repo, pin, cell: Option<u64>| {
            let mut i = index(build, &[("ay", repo)], &[("ay", pin)]);
            if let Some(cell) = cell {
                i.channels[0]
                    .pin_by_target
                    .insert(LINUX.into(), [("ay".to_string(), cell)].into());
            }
            i
        };
        let prior = sources(&with_cell(10, "ay", 5, Some(9)));
        // The pin rises, but Linux would still fetch the old repo's build 9 from the new one.
        assert_eq!(
            check(&prior, &with_cell(11, "ay-new", 6, Some(9))),
            [Break::TargetRepoSwitch {
                channel: "stable".into(),
                triple: LINUX.into(),
                program: "ay".into(),
                from_repo: "ay".into(),
                to_repo: "ay-new".into(),
                from: 9,
                to: 9,
            }]
        );
        // The controls: the cell names a new build, or the overlay is retired, or the repo
        // stays and the cell is carried.
        assert_eq!(check(&prior, &with_cell(11, "ay-new", 6, Some(10))), []);
        assert_eq!(check(&prior, &with_cell(11, "ay-new", 6, None)), []);
        assert_eq!(check(&prior, &with_cell(11, "ay", 6, Some(9))), []);
    }

    #[test]
    fn a_lowered_pin_or_target_cell_is_refused_and_a_retired_one_is_not() {
        let mut prior = index(10, &[("ay", "ay"), ("nn", "nn")], &[("ay", 5), ("nn", 108)]);
        prior.channels[0].pin_by_target.insert(
            "x86_64-unknown-linux-gnu".into(),
            [("ay".to_string(), 9)].into(),
        );
        let mut next = index(11, &[("ay", "ay"), ("nn", "nn")], &[("ay", 4), ("nn", 108)]);
        next.channels[0].pin_by_target.insert(
            "x86_64-unknown-linux-gnu".into(),
            [("ay".to_string(), 8)].into(),
        );
        let breaks = check(&sources(&prior), &next);
        assert_eq!(breaks.len(), 2, "{breaks:?}");
        assert!(breaks.iter().any(
            |b| matches!(b, Break::Downgrade { program, from: 5, to: 4, .. } if program == "ay")
        ));
        assert!(breaks.iter().any(|b| matches!(b, Break::TargetDowngrade { triple, from: 9, to: 8, .. } if triple == "x86_64-unknown-linux-gnu")));
        // Retiring a program, its overlay or a whole target is the publisher's to do.
        let retired = index(11, &[("nn", "nn")], &[("nn", 108)]);
        assert_eq!(check(&sources(&prior), &retired), []);
        // And a program the trusted index never pinned may arrive at any build in any repo.
        let added = index(
            11,
            &[("ay", "ay"), ("nn", "nn"), ("ty", "ty")],
            &[("ay", 5), ("nn", 108), ("ty", 1)],
        );
        assert_eq!(check(&sources(&prior), &added), []);
    }

    #[test]
    fn the_record_round_trips_and_never_moves_back() {
        let layout = crate::testdir::layout("atpkg-succession-record-");
        let mut t77 = index(77, &[("codex", "codex-alab")], &[("codex", 2_026_102_501)]);
        t77.channels[0].pin_by_target.insert(
            "x86_64-unknown-linux-gnu".into(),
            [("codex".to_string(), 1)].into(),
        );
        let s77 = sources(&t77);
        let refused = Refusal {
            label: "atpkg-index-78".into(),
            index_build: 78,
            machine_id: "m22".into(),
            trusted_build: 77,
            breaks: check(
                &s77,
                &index(78, &[("codex", "aterm")], &[("codex", 2_026_102_501)]),
            ),
        };
        let wrote = trust(&layout, Some(&s77), std::slice::from_ref(&refused)).unwrap();
        assert_eq!(wrote.new_refusals, [refused.row()]);
        let read = Record::read(&layout).expect("the record reads back");
        assert_eq!(read.sources, s77);
        assert_eq!(read.refused, [refused.row()]);
        assert!(
            read.refused[0]
                .sentence()
                .contains("moves from repo \"codex-alab\" to \"aterm\"")
        );

        let again = trust(&layout, Some(&s77), std::slice::from_ref(&refused)).unwrap();
        assert_eq!(
            again.new_refusals,
            [],
            "a refusal already recorded is said once"
        );

        let s76 = sources(&index(
            76,
            &[("codex", "codex-alab")],
            &[("codex", 2_026_102_001)],
        ));
        trust(&layout, Some(&s76), &[]).unwrap();
        let held = Record::read(&layout).unwrap();
        assert_eq!(held.sources, s77, "an older index never rewinds the record");
        assert_eq!(held.refused, [], "the last selection refused nothing");

        let mut s76_next_generation = s76.clone();
        s76_next_generation.roster_seq = 6;
        trust(&layout, Some(&s76_next_generation), &[]).unwrap();
        assert_eq!(
            Record::read(&layout).unwrap().sources,
            s76_next_generation,
            "a new generation re-bases the record, as it re-bases the build floor"
        );
        trust(&layout, Some(&s77), &[]).unwrap();
        assert_eq!(
            Record::read(&layout).unwrap().sources,
            s76_next_generation,
            "an older generation, however high its build, never rewinds the record"
        );
    }
}

/// The live channel's 74 -> 77 move and a 78 the retired shell publisher would write after it,
/// rebuilt under the test roster (the live files' signatures belong to keys no test holds).
#[cfg(test)]
mod live {
    use super::*;
    use crate::select::{Candidate, Selection, select_index};
    use crate::sig::{Anchor, BuildFloor, testkit};

    const SHELL_HEAD: &str =
        "# Auto-generated by tools/atpkg-index.sh — the MACHINE-signed allow-list.";
    const RUST_HEAD: &str = "# Generated by `atpkg-publish index` — the MACHINE-signed allow-list.";

    /// Index 74's pins (the last the shell publisher wrote).
    const PINS_74: [(&str, u64); 12] = [
        ("ay", 8256),
        ("clean", 8606),
        ("nn", 108),
        ("ny", 4707),
        ("ty", 3007),
        ("trust-mc", 20065),
        ("trust-ir", 1123),
        ("trust-cg", 3908),
        ("trust-vc", 1216),
        ("trust", 9192),
        ("codex", 2_026_101_501),
        ("claude", 2_026_102_001),
    ];

    /// Index 77's pins: the rustc group and codex raised, the rest carried.
    const PINS_77: [(&str, u64); 12] = [
        ("ay", 8256),
        ("clean", 8606),
        ("nn", 108),
        ("ny", 4707),
        ("ty", 3007),
        ("trust-mc", 20065),
        ("trust-ir", 2414),
        ("trust-cg", 9751),
        ("trust-vc", 2082),
        ("trust", 15698),
        ("codex", 2_026_102_501),
        ("claude", 2_026_102_001),
    ];

    /// One index shaped like the live ones: `head` line, every program block (codex's repo
    /// given), one `stable` channel, signed by the test roster's machine.
    fn text(head: &str, build: u64, codex_repo: &str, pins: &[(&str, u64)]) -> String {
        let mut out = format!(
            "{head}\n# Authorized by the master-signed roster published beside it; the \
             signature covers\n# the exact bytes, so DO NOT edit by hand.\nschema = 2\n\
             index_build = {build}\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
             machine_id = \"{}\"\nroster_seq = {}\n",
            testkit::MACHINE_ID,
            testkit::SEQ
        );
        for (program, _) in pins {
            let repo = if *program == "codex" {
                codex_repo
            } else if *program == "claude" {
                "aterm"
            } else {
                program
            };
            out.push_str(&format!(
                "\n[programs.{program}]\nrepo = \"{repo}\"\npolicy = \"prebuilt-only\"\n"
            ));
            if program.starts_with("trust") && *program != "trust-mc" {
                out.push_str("coherence_group = \"rustc\"\n");
            }
        }
        let pin = pins
            .iter()
            .map(|(p, b)| format!("{p} = {b}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\n[[channels]]\nname = \"stable\"\nchannel_build = {build}\nmin_build = 0\n\
             pin = {{ {pin} }}\n"
        ));
        out
    }

    fn candidate(build: u64, body: &str) -> Candidate {
        let (roster_bytes, roster_sig) = testkit::published_roster();
        Candidate {
            label: format!("atpkg-index-{build}"),
            index_bytes: body.as_bytes().to_vec(),
            sig: testkit::sign(&testkit::MACHINE_SEED, body.as_bytes()),
            roster_bytes,
            roster_sig,
        }
    }

    fn i74() -> Candidate {
        candidate(74, &text(SHELL_HEAD, 74, "aterm", &PINS_74))
    }

    fn i77() -> Candidate {
        candidate(77, &text(RUST_HEAD, 77, "codex-alab", &PINS_77))
    }

    /// What the shell writes when handed head 77: every repo from its old spec (codex's is
    /// `aterm`), and each pin at least the baseline's, because it refuses a downgrade.
    fn shell_78() -> Candidate {
        candidate(78, &text(SHELL_HEAD, 78, "aterm", &PINS_77))
    }

    /// The control: what `atpkg-publish index` writes after 77 (trust raised, codex's row kept).
    fn rust_78() -> Candidate {
        let mut pins = PINS_77;
        pins[9].1 = 15_700;
        candidate(78, &text(RUST_HEAD, 78, "codex-alab", &pins))
    }

    fn trusted(c: &Candidate) -> Sources {
        let index = testkit::trusted_index_at(testkit::SEQ, c.index_bytes.clone());
        Sources::of(&index, testkit::SEQ)
    }

    fn select(trusted: Option<&Sources>, candidates: Vec<Candidate>) -> Selection {
        select_index(
            &Anchor::of(vec![testkit::pubkey_b64(&testkit::MASTER_SEED)], 0),
            candidates,
            BuildFloor::none(),
            trusted,
            testkit::NOW,
        )
    }

    fn chose(s: &Selection) -> Option<u64> {
        s.selected.as_ref().map(|s| s.index.index_build)
    }

    #[test]
    fn the_live_74_to_77_move_is_admitted() {
        let after_74 = select(Some(&trusted(&i74())), vec![i77()]);
        assert_eq!(chose(&after_74), Some(77));
        assert_eq!(after_74.refused, [], "codex's repo moved under a new build");
        let first_contact = select(None, vec![i74(), i77()]);
        assert_eq!(chose(&first_contact), Some(77));
        assert_eq!(first_contact.refused, []);
    }

    #[test]
    fn a_shell_written_78_after_77_is_refused_and_the_client_stays_on_77() {
        let after_74 = select(Some(&trusted(&i74())), vec![i74(), i77(), shell_78()]);
        assert_eq!(chose(&after_74), Some(77), "{:?}", after_74.refused);
        let [refusal] = after_74.refused.as_slice() else {
            panic!("one refusal: {:?}", after_74.refused)
        };
        assert_eq!(
            (
                refusal.index_build,
                refusal.trusted_build,
                refusal.machine_id.as_str()
            ),
            (78, 77, testkit::MACHINE_ID)
        );
        assert_eq!(
            refusal.breaks,
            [Break::RepoSwitch {
                channel: "stable".into(),
                program: "codex".into(),
                from_repo: "codex-alab".into(),
                to_repo: "aterm".into(),
                from: 2_026_102_501,
                to: 2_026_102_501,
            }]
        );

        let after_77 = select(Some(&trusted(&i77())), vec![shell_78()]);
        assert_eq!(chose(&after_77), None, "the window held only the forgery");
        assert_eq!(after_77.refused.len(), 1);

        let first_contact = select(None, vec![i77(), shell_78()]);
        assert_eq!(
            chose(&first_contact),
            Some(77),
            "the window's own 77 starts the chain"
        );

        // 74's rows renumbered 78 break the pin rule too.
        let renumbered = candidate(78, &text(SHELL_HEAD, 78, "aterm", &PINS_74));
        let lowered = select(Some(&trusted(&i77())), vec![renumbered]);
        assert_eq!(chose(&lowered), None);
        let downgrades = lowered.refused[0]
            .breaks
            .iter()
            .filter(|b| matches!(b, Break::Downgrade { .. }))
            .count();
        assert_eq!(downgrades, 5, "{:?}", lowered.refused[0].breaks);
    }

    #[test]
    fn the_controls_the_publishers_78_and_no_trusted_predecessor_are_admitted() {
        let rust = select(Some(&trusted(&i77())), vec![i77(), rust_78()]);
        assert_eq!(chose(&rust), Some(78));
        assert_eq!(rust.refused, []);
        // With nothing trusted to judge it against, the shell's 78 is what the client took
        // before this rule: the chain needs a trusted predecessor.
        assert_eq!(chose(&select(None, vec![shell_78()])), Some(78));
        // A record from an older generation does not bind, as an older build floor does not.
        let mut older = trusted(&i77());
        older.roster_seq = testkit::SEQ - 1;
        assert_eq!(chose(&select(Some(&older), vec![shell_78()])), Some(78));
    }

    #[test]
    fn a_pin_retired_and_re_added_lower_is_followed_by_a_client_that_saw_the_retirement() {
        let without_ay: Vec<(&str, u64)> = PINS_77
            .iter()
            .copied()
            .filter(|(p, _)| *p != "ay")
            .collect();
        let retired = candidate(79, &text(RUST_HEAD, 79, "codex-alab", &without_ay));
        let mut pins = PINS_77;
        pins[0].1 = 8000;
        let re_added = candidate(80, &text(RUST_HEAD, 80, "codex-alab", &pins));
        // The publisher allows it (the head it plans 80 on pins no ay), and so does a client
        // that saw 79, in the same window or on an earlier pass.
        let saw = select(
            Some(&trusted(&i77())),
            vec![retired.clone(), re_added.clone()],
        );
        assert_eq!((chose(&saw), saw.refused.len()), (Some(80), 0));
        let held_79 = select(Some(&trusted(&retired)), vec![re_added.clone()]);
        assert_eq!((chose(&held_79), held_79.refused.len()), (Some(80), 0));
        // The limit the module names: a client still on 77, whose window moved past 79 while
        // it was away, judges 80 against 77.
        let away = select(Some(&trusted(&i77())), vec![re_added]);
        assert_eq!(chose(&away), None);
        assert!(
            matches!(
                away.refused[0].breaks.as_slice(),
                [Break::Downgrade {
                    from: 8256,
                    to: 8000,
                    ..
                }]
            ),
            "{:?}",
            away.refused
        );
    }

    /// The fetcher of one pass: the candidates the channel serves, and nothing else.
    struct Served(std::cell::RefCell<Vec<Candidate>>);

    impl crate::flow::Fetcher for Served {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            Ok(self.0.borrow().clone())
        }
        fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            Err("this test serves no manifest".into())
        }
        fn download(&self, _: &str, _: &str, _: &std::path::Path) -> Result<(), String> {
            Err("this test serves no download".into())
        }
    }

    #[test]
    fn a_client_keeps_77_across_passes_and_falls_back_to_its_cache_past_the_forgery() {
        let layout = crate::testdir::layout("atpkg-succession-passes-");
        let anchor = Anchor::of(vec![testkit::pubkey_b64(&testkit::MASTER_SEED)], 0);
        let served = Served(std::cell::RefCell::new(vec![i74(), i77()]));
        let pass = |serve: Vec<Candidate>| {
            *served.0.borrow_mut() = serve;
            crate::flow::resolve_verified_index(
                &served,
                &layout,
                &anchor,
                BuildFloor::none(),
                testkit::NOW,
            )
            .map(|i| i.index_build)
            .map_err(|e| e.to_string())
        };
        assert_eq!(pass(vec![i74(), i77()]), Ok(77));
        let record = Record::read(&layout).expect("the pass recorded what it trusted");
        assert_eq!(record.sources.index_build, 77);
        assert_eq!(
            record.sources.repo.get("codex").map(String::as_str),
            Some("codex-alab")
        );
        assert_eq!(record.refused, []);

        assert_eq!(pass(vec![i77(), shell_78()]), Ok(77));
        let record = Record::read(&layout).unwrap();
        assert_eq!(record.sources.index_build, 77);
        assert_eq!(
            record
                .refused
                .iter()
                .map(|r| r.index_build)
                .collect::<Vec<_>>(),
            [78]
        );

        // The window moved past 77: only the forgery is served, and the cache still holds 77.
        assert_eq!(pass(vec![shell_78()]), Ok(77));
        assert_eq!(Record::read(&layout).unwrap().refused.len(), 1);

        // The publisher's fix (codex re-pinned to a new build in its ALab repo) lands.
        let mut pins = PINS_77;
        pins[10].1 = 2_026_102_601;
        let fixed = candidate(79, &text(RUST_HEAD, 79, "codex-alab", &pins));
        assert_eq!(pass(vec![shell_78(), fixed]), Ok(79));
        let record = Record::read(&layout).unwrap();
        assert_eq!(record.sources.index_build, 79);
        assert_eq!(
            record.refused,
            [],
            "a client past the refused index is no longer held"
        );
    }
}
