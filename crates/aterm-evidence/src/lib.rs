// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Evidence that gates anything, as dated and witnessed observations of one subject, and the
//! hold vocabulary that bounds every wait on it.
//!
//! - [`Obs`] is one reading: a value, the time of the evidence itself ([`Obs::at`]), the
//!   [`WitnessId`] it was read from, its [`Source`] and the [`Subject`] it is about. A
//!   credential is named by its [`CredentialPrint`], a SHA-256 over a namespace, a row pk and a
//!   binding generation, never over credential bytes.
//! - [`Ledger`] folds readings: for a subject, the newest first reading decides
//!   ([`Ledger::verdict`]). Arrival order never decides, a witness read again is the same
//!   observation and renews nothing, and a reading of one print says nothing of another.
//! - [`Fresh`] is a reading young enough to act on now, made only by [`Obs::fresh`].
//! - [`HoldBound`] says how a hold ends: within a time, at an event this program makes (with a
//!   backstop for when that event never comes), or only while a person's state or progressing
//!   work is true now. [`EpisodeClock`] counts one hold episode ([`Episode`]) across changes of
//!   word and holding process and across clear gaps shorter than [`CLEAR_GAP_S`].
//!
//! The crate does no I/O and knows no vendor: its callers read transcripts, screens, taps and
//! peers, and hand it what they read.
//!
//! # Examples
//!
//! A strike read at 100 and the same login serving at 160: the newer evidence decides, and a
//! re-read of the old strike, stamped later, renews nothing.
//!
//! ```
//! use aterm_evidence::{CredentialPrint, Ledger, Obs, Observed, Source, Subject, WitnessId};
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
//! enum Login {
//!     Serves,
//!     Struck,
//! }
//!
//! let print = CredentialPrint::new("claude", "row-1", 3);
//! let obs = |value, at, witness: &str| Obs {
//!     value,
//!     at,
//!     witness: WitnessId::Reading(witness.to_string()),
//!     source: Source::Reading,
//!     subject: Subject::Credential(print),
//! };
//! let mut ledger = Ledger::new();
//! assert_eq!(ledger.observe(obs(Login::Struck, 100, "wall")), Observed::Decides);
//! assert_eq!(ledger.observe(obs(Login::Serves, 160, "turn")), Observed::Decides);
//! assert_eq!(ledger.observe(obs(Login::Struck, 220, "wall")), Observed::Reread);
//! assert_eq!(ledger.verdict(&print).map(|o| o.value), Some(Login::Serves));
//! ```

mod episode;
mod hold;
mod ledger;
mod obs;

pub use episode::{CLEAR_GAP_S, Episode, EpisodeClock};
pub use hold::{HoldBound, PersonHold, WorkHold};
pub use ledger::{Ledger, Observed, login_evidence_environment};
pub use obs::{CredentialPrint, Fresh, Obs, Source, Subject, WitnessId};
