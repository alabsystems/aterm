// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Session log engine recording hierarchical session, run, turn, and step events.

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
    PreparedCarry, ROW_SPAN_MAX, Read, Refused, Rollup, Row, RowRule, SEQ_LIMIT, SharedRead,
    closes_over_half, evict_class, log_owned, low_water, mark_quota, natural_parent, over_limits,
    program_share, row_floor,
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
