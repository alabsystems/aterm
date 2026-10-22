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
//!   evicting level-aware ([`evict_class`]), with exact gap counts, pull
//!   cursors and the open/close discipline.
//! - [`Log::carry`] and [`Log::from_carry`] carry a log across a seamless
//!   update as text lines, cut to a budget in the eviction order.
//! - [`journal`] is the disk journal's line codec and the reader's view of
//!   it (crash closes derived at read time); [`Log::from_journal`] the log a
//!   read rebuilds, and [`Notify`] the host writer's wake every append rings.
//! - [`TurnTracker`] turns screen verdict words into turns and steps.
//! - [`Fold`] reduces records to a [`SessionState`]; [`Fold::clause`] is the
//!   tab clause, [`brief()`] the token-budgeted rollup.
//! - [`NodeLog`] is the instance's own ring (node `n`: its start, fabric
//!   link and config facts), RAM only and never on the bus.
//!
//! Producers call typed methods ([`Log::open`], [`Log::close`],
//! [`Log::mark`], [`TurnTracker::observe`]); no payload string is formatted
//! to be parsed back.

#![forbid(unsafe_code)]

pub mod brief;
pub mod bus;
pub mod field;
pub mod fold;
pub mod journal;
pub mod limit;
pub mod line;
pub mod log;
pub mod node_log;
pub mod rec;
pub mod turn;
pub mod vocab;
pub mod words;

pub use brief::{Brief, BriefInput, Briefing, DEFAULT_BUDGET, INSTANCE_BUDGET, brief, tokens};
pub use field::{
    BoundedText, BoundedToken, Cap, Charset, FIELDS, FieldError, Fields, Key, MAX_FIELDS, Tier, Val,
};
pub use fold::{
    BusFacts, BusTotals, Claim, Clause, ClauseRow, ClosedNode, Fold, Hints, MAIL_LINK_WINDOW_MS,
    MAX_MAIL_WAITING, MailLink, MailLinkOf, MailWaiting, Need, OpenNode, Phase, SessionState,
    SnapError, Tone, Totals, Words, amend_refines_out, clause_of, clause_of_proven, clear_lifts,
    confidence_law, lifts_failure, mail_in_window, mail_link_of, mark_may_raise, need_precedence,
    need_raised, need_takes_amend, need_words, shown_label, shown_proven_label, shown_verdicts,
    starts_epoch, turn_out_rebucket,
};
pub use limit::{MARK_ACTORS, MARK_BURST, MARK_OWED_AUTHORS, MARK_REFILL_MS, MarkLimiter, Owed};
pub use line::{
    BusRec, DecodeError, EncodeError, MAX_RECORD_BYTES, decode, decode_bus, encode, encode_bus,
    encode_mirror, encode_with_table,
};
pub use log::{
    Amend, BusCopy, BusCursor, CoverWhy, ENTRY_BYTES, LOG_MIRROR_BYTES, LandRefused, Landed, Prior,
    SNAP_EVERY, Slot, SlotKind, TAKE_MAX, Take, cover_body, is_bus_step,
};
pub use log::{
    CARRY_MAX_BYTES, CARRY_MAX_HOST_BYTES, CARRY_MAX_HOST_LINES, CARRY_MAX_LINE, CARRY_MAX_LINES,
    CARRY_SKELETON_BYTES, CARRY_TOTAL_BYTES, CLOSED_SUMMARY_BYTES, CarryAdopt, CarryBudget,
    CarryError, CarryOut, CarrySnapshot, CarryStats, Close, Cursor, Dur, EvictClass, Gap, GapWhy,
    IdCounters, JOURNAL_IMAGE_MAX_RECORDS, JournalImage, Limits, Log, MAX_BYTES, MAX_HOLES,
    MAX_OPEN, MAX_RECORDS, Mark, NodeView, Notify, Open, OpenEntry, PROGRAM_SHARE_MAX,
    PreparedCarry, Read, Refused, Rollup, SEQ_LIMIT, SharedRead, closes_over_half, evict_class,
    log_owned, low_water, mark_quota, natural_parent, over_limits, program_share,
};
pub use node_log::{NODE_LOG_RECORDS, NODE_MARKS, NodeLog};
pub use rec::{Rec, Stamp};
pub use turn::{
    ATTRIBUTION_MS, ActWord, Admission, AttributionView, Binding, LastClose, SETTLE_MS,
    STEP_ACT_MS, STEP_INPUT_LEAD_MS, ScreenStep, SettleCause, TX_TOOLS_MAX, TrackerView,
    TurnTracker, TxEvent, TxMark, TxPos, TxState, TxView, Verdict, WallJudge, attributes,
    dismisses, rebase, settle_due, settle_out, tx_close_at, within_lead,
};
pub use vocab::{
    Actor, Conf, Kind, Lvl, Node, Op, RESERVED_ACTOR_WORDS, ShortId, Src, is_reserved_actor_word,
};
pub use words::{BoxWord, Out, RunOut, SessionOut, StepOut, TurnOut, WallKind, need_rank};
