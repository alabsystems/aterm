// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Proposers, which supply Aos M4 candidates as bytes and hold no authority over how they are judged (design §10.2).
//!
//! A proposer is shown [`TrainFeedback`] rows, whose private fields only the evaluator builds, carrying training-split facts alone (the plan digest, admitted, the first
//! failing law, the training sum of `T`). There is no held-out field to read:
//!
//! ```compile_fail,E0599
//! use aterm_effects::aos_momentum::search::TrainFeedback;
//! fn peek(f: &TrainFeedback) -> Option<u64> {
//!     f.held_out_ticks()
//! }
//! ```
//!
//! and a row's fields are not public:
//!
//! ```compile_fail,E0616
//! use aterm_effects::aos_momentum::search::TrainFeedback;
//! fn peek(f: &TrainFeedback) -> Option<u64> {
//!     f.ticks
//! }
//! ```

use aterm_aos::Digest;

use super::plan::{PLAN_DOMAIN, SchedulePlan};
use super::policy::GRID_V1;

/// The domain under which a proposer's configuration is digested ([`ProposerId::new`]).
pub const PROPOSER_DOMAIN: &[u8] = b"aterm-aos-m4-proposer-v1\0";

/// [`GridProposer`]'s name.
pub const GRID_PROPOSER: &str = "grid";

/// [`FileProposer`]'s name.
pub const FILE_PROPOSER: &str = "file";

/// Proposer name recorded by on-demand re-evaluation (`m4::Evaluator::admit_one`).
pub const ON_DEMAND: &str = "on-demand";

/// Canonical candidate digest `Digest::of(PLAN_DOMAIN, [bytes])` ([`SchedulePlan::digest`]).
#[inline]
#[must_use]
pub fn candidate_id(bytes: &[u8]) -> Digest {
    Digest::of(PLAN_DOMAIN, &[bytes])
}

/// Declared proposer identity recorded in the bundle (`name`, `version`, config digest).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProposerId {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: u32,
    /// `Digest::of(PROPOSER_DOMAIN, [the configuration's bytes])`.
    pub config: Digest,
}

impl ProposerId {
    /// The proposer `name` v`version` configured by `config`.
    #[must_use]
    pub fn new(name: &str, version: u32, config: &[u8]) -> ProposerId {
        ProposerId {
            name: name.to_owned(),
            version,
            config: Digest::of(PROPOSER_DOMAIN, &[config]),
        }
    }

    /// The identity the on-demand re-evaluation records: [`ON_DEMAND`] v1, configured by nothing.
    #[must_use]
    pub fn on_demand() -> ProposerId {
        ProposerId::new(ON_DEMAND, 1, &[])
    }
}

/// What supplies candidates: bytes, and nothing else.
pub trait Proposer {
    /// Its identity, as the bundle records it.
    fn id(&self) -> ProposerId;

    /// Candidates for `round` (from 0), given feedback from earlier rounds in proposal order.
    fn propose(&mut self, round: u8, feedback: &[TrainFeedback]) -> Vec<Vec<u8>>;
}

/// Training-split feedback for one judged candidate, built only by the evaluator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrainFeedback {
    plan: Digest,
    admitted: bool,
    law: Option<&'static str>,
    ticks: Option<u64>,
}

impl TrainFeedback {
    /// A candidate admitted on every training item, with its training sum of `T`.
    #[inline]
    #[must_use]
    pub(super) const fn admitted(plan: Digest, ticks: u64) -> TrainFeedback {
        TrainFeedback {
            plan,
            admitted: true,
            law: None,
            ticks: Some(ticks),
        }
    }

    /// A candidate refused, named by the first law it failed on the training split or by its refusal (`PlanRefusal`, `LaneNotLogged`, `LaneNotReproduced`); a proposal over the byte budget is never digested, so it gets no row.
    #[inline]
    #[must_use]
    pub(super) const fn refused(plan: Digest, law: &'static str) -> TrainFeedback {
        TrainFeedback {
            plan,
            admitted: false,
            law: Some(law),
            ticks: None,
        }
    }

    /// The candidate's id ([`candidate_id`]).
    #[inline]
    #[must_use]
    pub const fn plan(&self) -> Digest {
        self.plan
    }

    /// Whether it was admitted on every training item.
    #[inline]
    #[must_use]
    pub const fn is_admitted(&self) -> bool {
        self.admitted
    }

    /// The first law it failed on the training split, if it was refused.
    #[inline]
    #[must_use]
    pub const fn law(&self) -> Option<&'static str> {
        self.law
    }

    /// Its training sum of `T`, if it was admitted.
    #[inline]
    #[must_use]
    pub const fn ticks(&self) -> Option<u64> {
        self.ticks
    }
}

/// How a result was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Proposer rounds with training-split selection and a single held-out evaluation.
    Search,
    /// On-demand re-evaluation of one candidate (`m4::Evaluator::admit_one`).
    OnDemand,
    /// A search whose training judgments ran in workers and were checked here only as far as
    /// the winner needs, so its admitted count rests on their claims (`m4::Distributed`, design
    /// §11.3).
    Distributed,
}

impl Mode {
    /// Its tag in a bundle.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            Mode::Search => 0,
            Mode::OnDemand => 1,
            Mode::Distributed => 2,
        }
    }

    /// The mode a bundle's tag names.
    #[inline]
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Mode> {
        match tag {
            0 => Some(Mode::Search),
            1 => Some(Mode::OnDemand),
            2 => Some(Mode::Distributed),
            _ => None,
        }
    }
}

/// What a search counted, where `proposed = oversized + duplicates + over_budget + distinct`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SearchStats {
    /// Rounds the proposer was asked for.
    pub rounds: u8,
    /// Proposals read.
    pub proposed: u64,
    /// Proposals over the byte budget, refused before they were digested or decoded.
    pub oversized: u64,
    /// Proposals whose id was already judged, skipped.
    pub duplicates: u64,
    /// New ids after the distinct budget was spent, skipped unjudged.
    pub over_budget: u64,
    /// Distinct candidates judged.
    pub distinct: u64,
    /// Candidates admitted on every training item.
    pub admitted: u64,
}

impl SearchStats {
    /// The counts of the on-demand re-evaluation: one round, one proposal, judged and admitted.
    pub const ONE: SearchStats = SearchStats {
        rounds: 1,
        proposed: 1,
        oversized: 0,
        duplicates: 0,
        over_budget: 0,
        distinct: 1,
        admitted: 1,
    };
}

/// Proposes [`GRID_V1`]'s 54 plans in order in round 0 and nothing in later rounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridProposer;

impl GridProposer {
    /// The grid proposer.
    #[inline]
    #[must_use]
    pub const fn new() -> GridProposer {
        GridProposer
    }

    /// Encoded plan bytes in [`GRID_V1`] order (54 plans).
    #[must_use]
    pub fn plans() -> Vec<Vec<u8>> {
        GRID_V1
            .iter()
            .filter_map(|&(split, wake)| SchedulePlan::new(split, wake).ok())
            .map(|p| p.encode().to_vec())
            .collect()
    }
}

impl Proposer for GridProposer {
    /// [`GRID_PROPOSER`] v1, configured by the grid's bytes in order.
    fn id(&self) -> ProposerId {
        ProposerId::new(GRID_PROPOSER, 1, &Self::plans().concat())
    }

    fn propose(&mut self, round: u8, _feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        if round == 0 {
            Self::plans()
        } else {
            Vec::new()
        }
    }
}

/// Round-0 proposer reading hex-encoded or raw candidate lines (with `#` comments stripped).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileProposer {
    candidates: Vec<Vec<u8>>,
    text: Digest,
}

/// The value of one hex digit.
#[inline]
const fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let (pairs, rest) = s.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[hi, lo]| Some((hex_value(hi)? << 4) | hex_value(lo)?))
        .collect()
}

impl FileProposer {
    /// The candidates `text` holds (the type's docs).
    #[must_use]
    pub fn from_text(text: &str) -> FileProposer {
        let candidates = text
            .lines()
            .filter_map(|line| {
                let line = line.split('#').next().unwrap_or("").trim();
                (!line.is_empty())
                    .then(|| hex_bytes(line).unwrap_or_else(|| line.as_bytes().to_vec()))
            })
            .collect();
        FileProposer {
            candidates,
            text: Digest::of(PROPOSER_DOMAIN, &[text.as_bytes()]),
        }
    }

    /// The candidates, in line order.
    #[inline]
    #[must_use]
    pub fn candidates(&self) -> &[Vec<u8>] {
        &self.candidates
    }
}

impl Proposer for FileProposer {
    /// [`FILE_PROPOSER`] v1, configured by the text.
    fn id(&self) -> ProposerId {
        ProposerId {
            name: FILE_PROPOSER.to_owned(),
            version: 1,
            config: self.text,
        }
    }

    fn propose(&mut self, round: u8, _feedback: &[TrainFeedback]) -> Vec<Vec<u8>> {
        if round == 0 {
            self.candidates.clone()
        } else {
            Vec::new()
        }
    }
}
