// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The behavioural contract a run is judged by.
//!
//! # The law
//!
//! A repair is accepted because the candidate's run over the recorded inputs satisfies a
//! *corrected* contract (plan §4, "Repair"), never because it is equivalent to the faulty
//! code it replaces: its state, observations and schedule may differ on purpose. A contract
//! is a pure judgement of `(inputs, run)`; it names the first cursor where a law fails.
//!
//! The contract is an artifact like any other ([`Contract::id`]), identified by the artifact
//! id it declares. What a [`crate::publish::Boundary`] guarantees about it, exactly:
//!
//! * The boundary owns its contract: it is given once, to `Boundary::new`, and `prepare`
//!   takes only the candidate. After construction, no caller of `prepare` or `commit` can
//!   hand the boundary another judge, and every witness it builds records its own contract.
//! * `prepare` refuses when the boundary's contract declares another artifact than the one the
//!   task identity names (`ContractMismatch`); `commit` refuses a witness that names any other
//!   contract (`WitnessMismatch("contract")`).
//! * The constructor is trusted (first-party trust): a `Contract` impl that declares the
//!   identity's contract id but judges leniently is not detected, by the boundary it is given
//!   to or by another boundary with the same identity, ancestor, log and head that receives a
//!   witness it built. That is the assumption every witness lists
//!   ([`crate::evidence::TESTED_ASSUMPTIONS`]).

use core::fmt;

use crate::ident::ArtifactId;
use crate::replay::Run;
use crate::task::Schema;

/// A behavioural contract over runs of schema `S`.
pub trait Contract<S: Schema> {
    /// The contract's artifact (kind [`crate::ident::ArtifactKind::Contract`]).
    fn id(&self) -> &ArtifactId;
    /// Judges a run of `inputs` (whose observations carry their cursors).
    fn judge(&self, schema: &S, inputs: &[S::Input], run: &Run<S>) -> Verdict;
}

/// A contract's judgement of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every law holds over the run.
    Pass,
    /// The law `law` fails first at `cursor`.
    Fail {
        /// The cursor of the first failing observation.
        cursor: u64,
        /// The law that fails (a stable name).
        law: &'static str,
        /// The offending values.
        detail: String,
    },
}

impl Verdict {
    /// Whether the verdict is [`Verdict::Pass`].
    #[inline]
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass => f.write_str("every law holds"),
            Verdict::Fail {
                cursor,
                law,
                detail,
            } => write!(f, "the law `{law}` fails at cursor {cursor}: {detail}"),
        }
    }
}
