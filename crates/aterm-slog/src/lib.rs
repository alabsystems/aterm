// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's session log engine (`docs/DESIGN-session-log-2026-10-15.md`): a
//! hierarchical record of what a session does, session › run › turn › step,
//! with point marks on whichever node is open.
//!
//! An ENGINE crate (`aterm-census`'s `engine-platform`): no file system,
//! threads, sockets, process spawning or clock reads. Time enters only as a
//! host-provided [`Stamp`]; the settle wait of the [`TurnTracker`] is a wake
//! the host schedules.
//!
//! - [`Rec`] is one record; its vocabularies are closed ([`vocab`]) and every
//!   field key is declared once, with its tier and cap, in [`FIELDS`].
//! - [`encode`]/[`decode`] are the one-line form (and [`encode_bus`] the
//!   `v=2` bus body). An encoding at [`Tier::Wire`] cannot carry an H or C
//!   key; a record over [`MAX_RECORD_BYTES`] is refused.
//! - [`Log`] is one session's ring, bounded by 2048 records and 256 KiB,
//!   with exact gap counts, pull cursors and the open/close discipline.
//! - [`TurnTracker`] turns screen verdict words into turns and steps.
//! - [`Fold`] reduces records to a [`SessionState`]; [`Fold::clause`] is the
//!   tab clause, [`brief()`] the token-budgeted rollup.
//!
//! Producers call typed methods ([`Log::open`], [`Log::close`],
//! [`Log::mark`], [`TurnTracker::observe`]); no payload string is formatted
//! to be parsed back.

#![forbid(unsafe_code)]

pub mod brief;
pub mod field;
pub mod fold;
pub mod line;
pub mod log;
pub mod rec;
pub mod turn;
pub mod vocab;
pub mod words;

pub use brief::{Brief, DEFAULT_BUDGET, INSTANCE_BUDGET, brief, tokens};
pub use field::{
    BoundedText, BoundedToken, Cap, Charset, FIELDS, FieldError, Fields, Key, MAX_FIELDS, Tier, Val,
};
pub use fold::{
    Claim, Clause, ClosedNode, Fold, Hints, Need, OpenNode, Phase, SessionState, Tone, Totals,
    Words, clause_of, confidence_law, lifts_failure, mark_may_raise, need_precedence, starts_epoch,
};
pub use line::{
    BusRec, DecodeError, EncodeError, MAX_RECORD_BYTES, decode, decode_bus, encode, encode_bus,
    encode_with_table,
};
pub use log::{
    Amend, Close, Cursor, Dur, Gap, GapWhy, IdCounters, Limits, Log, MAX_BYTES, MAX_OPEN,
    MAX_RECORDS, Mark, Open, OpenEntry, Read, Refused, Rollup, log_owned, natural_parent,
    over_limits,
};
pub use rec::{Rec, Stamp};
pub use turn::{
    ATTRIBUTION_MS, LastClose, SETTLE_MS, ScreenStep, SettleCause, TrackerView, TurnTracker,
    Verdict, settle_due, settle_out,
};
pub use vocab::{Actor, Conf, Kind, Lvl, Node, Op, ShortId, Src};
pub use words::{BoxWord, Out, RunOut, SessionOut, StepOut, TurnOut, WallKind, need_rank};
