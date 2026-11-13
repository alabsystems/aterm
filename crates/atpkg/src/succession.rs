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
#[path = "succession_tests.rs"]
mod tests;

/// The live channel's 74 -> 77 move and a 78 the retired shell publisher would write after it,
/// rebuilt under the test roster (the live files' signatures belong to keys no test holds).
#[cfg(test)]
#[path = "succession_live_tests.rs"]
mod live;
