// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic fabric simulation world and fault/settle scheduler.
//!
//! Drives [`FLEET`], [`NODE`], and [`PERSON`] sessions from [`Opts::seed`] across bus mirrors
//! ([`Opts::mirror`], `turns` or `auto`: [`Opts::auto`]) over a retained broker or an
//! unretained one ([`Opts::retained`]), broker restarts ([`bus::Stop`]), held acks
//! ([`Opts::hold_acks`], [`bus::Broker::answer_held`]), disk write errors
//! ([`Opts::write_errors`], [`Device::fill`]), whole-machine [`Opts::power_cuts`]
//! ([`Opts::torn_device`]), and isolated [`Opts::bus_losses`].
//!
//! RETENTION (sim item B05): a retained broker ticks at every open that sets its rules and
//! every minute of the world's clock, and in three seeds in four the fault phase draws, from
//! a stream of its own ([`Opts::cuts`]), more ticks, cuts standing for a byte bound the
//! fleet's traffic reaches, days passing at once (so the 7-day age rule bites), opens that
//! set no rules, and opens that set rules no aterm build sets ([`changed_rules`]: a change
//! made outside aterm, as `asb serve --retain-file` makes one, the open after it aterm's
//! again). Half those seeds run aterm's rules at bytes a few records reach
//! ([`Opts::scaled`]) and a mirror window to match ([`Opts::window`]); a settle whose mirror
//! waits on that window lets a week pass. The laws learn what retention may do: a mail it
//! gave up unread is no loss once a `link gap` aterm was told for its session holds it (and is one
//! otherwise: `given up untold`); a post on the bus twice is one copy where every older copy
//! went with retention unread, and a receipt's answer twice one where every older copy went
//! before its asker was handed it; an `auto` mirror writes nothing onto a broker that retains
//! nothing; the raw at-most-once of a mirror's records excuses a copy republished at the
//! key a cut dropped; and the model's tap follows a give-up ([`conform`]).
//!
//! DEADLINES AND TOPICS (item F4): in a world of [`Opts::deadlines`] the sessions' asks carry
//! `dl=` and their targets decide them, some in time and some after, across bridge kills,
//! handoffs ([`Opts::handoffs`]), power cuts and broker restarts; in one of [`Opts::topics`]
//! the peer says on broadcast topics and the sessions follow them from the head, the base or
//! below it, across retention's cuts. The deadline sweep and a topic replay read past the
//! floors retention raised, and the laws hold them: a deadline fires once for every ask
//! nobody answered and never over an answer the broker holds
//! ([`World::judge_expiries`], [`World::expired_once`], [`World::deadlines_fired`]); a
//! follower is handed every broadcast it is owed that the broker holds, in offset order and
//! once, and a `link gap … topic=` covers each one retention gave up ([`World::topic_rows`],
//! [`World::topics_followed`]).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use super::bus::{self, ArmMutant, Escalation, Stop, Tier};
use super::conform;
use super::device::Device;
use super::endpoint::{Endpoint, EpRules, EpState, FetchRead, Row};
use super::{lock, Core, Process, Rng, SimKill};
use crate::body::Body;
use crate::bridge::{Bridge, Config};
use crate::ctl::Ctl;
use crate::fx::Fx;
use crate::mailbox::Mailbox;
use crate::transport::Backoff;
use aterm_types::fabric_grammar::ANSWER_KINDS;

/// The fleet.
pub(crate) const FLEET: &str = "sim";
/// This node.
pub(crate) const NODE: &str = "n-51a0000000000001";
/// The peer node, a producer of the world's own.
pub(crate) const PEER: &str = "n-9ee7000000000002";
/// The person.
pub(crate) const PERSON: &str = "h-alice";

/// One rule of the bridge switched off, for a negative control ([`crate::fx::Fx::mutant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mutant {
    /// The attach reads no lineage.
    IgnoreLineage,
    /// The attach takes its incarnation without the newest one its node's presence names on the
    /// bus: a state dir that lost its incarnation, its node kept by the node key (§11 C3),
    /// numbers its records from the start again.
    IgnoreBusIncarnation,
    /// A post left queued by a kill takes a fresh sequence on its retry instead of the one pinned.
    FreshSeqOnRetry,
    /// A receipt left owed by a kill takes a fresh sequence on its retry.
    FreshReceiptSeq,
    /// A sender's next post goes out before its last one is answered against a broker that does
    /// not gap-fence ([`Opts::gap_fences`]), as it does against one that does.
    PipelineSender,
    /// The inbox commit the bridge owed kept across a move that took what it names.
    KeepOwedCommitAcrossMove,
    /// The incarnation lease trusted across boots.
    LeaseAcrossBoots,
    /// The bus mirror's pinned batch kept across a move of the log.
    KeepMirrorPinAcrossMove,
    /// A move drops only the pinned batch a hello judged into memory: one still on disk alone
    /// (an `auto` mirror before its hello) is recovered onto the log the move left.
    ForgetOnlyAJudgedPin,
    /// The hello's judge tells aterm the mirror is on and keeps the wait on the window budget
    /// an earlier run announced: the window's check, which speaks only on a change, never
    /// says the wait again (`Bridge::mirror_judge_pin`).
    JudgeKeepsAnAnnouncedWait,
    /// No ghost sweep: a session row whose instance died unannounced stays `state=live`.
    NoGhostSweep,
    /// `session-exited` publishes no `state=exited` row.
    SkipExitedRow,
    /// A relaunched bridge does not recover the mirror's pinned batch.
    FreshMirrorTake,
    /// The attach records no witnessed will: a row under another state dir's
    /// mark whose will it read — a dead sibling's ([`Opts::sibling`]) — is not
    /// its to retire, and that incarnation's ghost stands.
    IgnoreWitnessedWill,
    /// A will's incarnation is trusted without matching its writer's mark.
    WitnessByIncarnationAlone,
    /// The sweep reads no row's state-dir mark, before the fix decision.
    ReadNoMark,
    /// The sweep takes every incarnation below its own for an earlier bridge of its state dir.
    RetireEveryEarlierIncarnation,
    /// The inbox group is committed through a record before it is delivered.
    CommitBeforeDelivery,
    /// A local refusal's notice goes out ahead of the `deliver` lines its inbox run queued
    /// before it (`Bridge::refuse_locally`), which are sent after it.
    RefusalAheadOfQueuedRows,
    /// The inbox commit a round held rides the next publish unclamped: past the first
    /// record still owed to a session or left unaccounted (`Bridge::take_held_commit`).
    HeldCommitPastTheFloor,
    /// The wind-down drops a held inbox commit: neither landed nor left for the next bridge.
    DropHeldCommitAtWindDown,
    /// The bus mirror sends its slots without the inbox commit a round holds, as before
    /// critic item C13 (`Bridge::mirror_send`): a snapshot a delivery owed lands in a batch
    /// of its own, and the commit in another once the grace is out.
    MirrorSendsItsSlotsBare,
    /// The bus mirror's burst carries only a commit a round already holds
    /// (`Bridge::take_commit_for_the_mirror`): a pass right after a delivery, before the
    /// round's settle holds the commit it owes, sends the snapshot bare.
    MirrorRidesOnlyAHeldCommit,
    /// Every park that takes nothing sends a held inbox commit, one the bus mirror's pass
    /// cut short of the grace included (`Bridge::park_and_take`), so a commit that pass
    /// would carry goes alone.
    ShortParkSendsTheHeldCommit,
    /// The bus mirror's take goes the round it falls due though nothing would carry it, as
    /// before critic item E04 (`Bridge::mirror_rides_nothing`): a sender's snapshot of its own
    /// post's mail count lands in a writer batch of its own, ahead of the delivery's commit.
    MirrorSendsAnUnowedSlotAtOnce,
    /// The take after a landing whose take left a backlog is held for a commit to carry like any
    /// other, as E04 first landed (`Bridge::mirror_rides_nothing`, `MirrorRun::drains`): a drain
    /// waits up to the grace before each batch after its first.
    MirrorHoldsADrainingTake,
    /// The round's settle dates the inbox commit's hold from the settle, after the mirror's
    /// pass, as E04 first landed (`Bridge::mirror_pass_then_settle`): a take that comes back
    /// empty puts its ctl round trip ahead of the commit's grace.
    CommitGraceFromTheSettle,
    /// The bus mirror's take inside the endpoint's rate hold goes the round it falls due with
    /// an inbox commit owed, as E04's review left it (`Bridge::mirror_behind_the_commit`): its
    /// ctl round trip, which comes back empty, runs ahead of the round's settle and the
    /// commit's write.
    RateHeldTakeAheadOfTheCommit,
    /// The bus mirror's take inside the endpoint's rate hold waits for the commit's write
    /// whichever session woke it, as E04c first landed (`Bridge::mirror_behind_the_commit`,
    /// `MirrorRun::unheld`): the hold is per session, so a take another session's snapshot
    /// owes goes behind the commit, and the commit and the snapshot go in two writes.
    RateHoldCoversEverySession,
    /// The bus mirror's first pass after an attach runs ahead of what the loop holds queued
    /// while the open has yet to make the pin's fence, as before critic item D06
    /// (`Bridge::mirror_yields`): the first delivery an attach or a handoff finds waiting
    /// waits behind the fence (the bridge's own two full flushes, or where the broker settles
    /// the pin its barriers and the settle the take waits for) and the pass.
    MirrorAheadOfTheFirstDelivery,
    /// The first-pass hold forgets the posts a drain published (`Bridge::mirror_drained_post`),
    /// as D06's first form did: a pass due before their records come back goes ahead of them.
    FirstPassForgetsTheDrainedPosts,
    /// A held first pass counts no inbox record it took off the ones it awaits
    /// (`Bridge::mirror_took_inbox`), so it waits out the whole hold after the record came.
    FirstPassAwaitsPastItsRecords,
    /// The first-pass hold counts every post a drain published, one to another node's session
    /// too, as D06 counted every post a drain listed (`Bridge::mirror_drained_post`, critic item
    /// G04): that post's record never comes back on this node's inbox face, so the pass waits
    /// out the whole hold for it. Its control is a bridge unit test
    /// (`a_post_to_another_nodes_session_does_not_hold_the_first_pass`).
    FirstPassCountsAnotherNodesPost,
    /// Every subject a drain or a mirror pass opens reads the node's subject budget
    /// (`Bridge::budget_allows`), as every build before critic item G04 did: an attach's first
    /// drain over K fresh lanes makes K `subject_budget` requests, each behind every owed ack.
    /// Its control is a bridge unit test
    /// (`a_drain_reads_the_subject_budget_once_for_every_fresh_lane_it_opens`).
    BudgetReadPerSubject,
    /// Once a drain or a mirror pass has read the node's subject budget, every later subject it
    /// meets may open (`Bridge::budget_allows`, critic item G04): the post that would take the
    /// budget into the reserve its kind keeps whole opens a subject there. Its control is a
    /// bridge unit test (`the_count_since_the_reading_refuses_where_the_per_subject_read_would`).
    BudgetCountTakenAsUnlimited,
    /// Where the count since a scope's reading says the node's subject budget is short, the
    /// subject is refused on the count alone, never read again (`Bridge::budget_allows`, critic
    /// item G04): a subject a retirement freed room for since the reading, or one the count
    /// over-counted for, is refused where the per-subject read lets it open. Its control is a
    /// bridge unit test (`a_budget_freed_inside_a_drain_is_read_where_the_count_says_short`).
    BudgetShortOnTheCount,
    /// A drain's reading of the node's subject budget is kept for the drains after it
    /// (`Bridge::budget_scoped`, critic item G04): what another connection under this producer
    /// spends between them is never read, so a post opens a subject inside the reserve its kind
    /// keeps whole. Its control is a bridge unit test
    /// (`each_drain_reads_the_budget_afresh_what_another_writer_spent_or_a_retirement_freed`).
    BudgetReadOutlivesItsScope,
    /// A pass that makes the pin's fence leaves the inbox commit its deliveries owe for its
    /// burst behind the fence (`Bridge::commit_ahead_of_the_fence`).
    CommitBehindTheFence,
    /// The bus mirror takes and sends without first making the pin's fence durable
    /// (`Bridge::mirror_key_on_the_media`): the control of the fence's law (critic item D06),
    /// under which an open's first take can be answered with no pin of this build on the media.
    TakeAheadOfTheFence,
    /// The pin's fence is held once its settle is SENT, not answered
    /// (`Bridge::make_the_pin_fence`, critic item F07): a take goes while the broker's writer
    /// may still be flushing, the pin's barriers not yet drained.
    FenceBeforeTheSettle,
    /// The settle goes ahead of the pin's barriers (`Bridge::make_the_pin_fence`, critic item
    /// F07): the broker's flush may drain the device before the pin is written, and its answer
    /// then vouches for a pin no flush covered.
    SettleAheadOfTheBarrier,
    /// The bridge trusts a broker's full flush whatever device its log is on
    /// (`Bridge::broker_settles_the_pin`, critic item F07): a flush of another device settles
    /// nothing of the state dir's.
    TrustAnotherDevicesFlush,
    /// The pin's settle's answer is read only where F07 first read it, at an idle moment or a
    /// park that took nothing (`Bridge::read_the_pins_settle`, critic item F07's review): under
    /// a stream of items the answer sits unread, and the first take waits for the queue to
    /// drain past D06's hold.
    SettleReadOnlyWhenIdle,
    /// The take the settle's answer lets go is held for a commit to carry as any later take is
    /// (`MirrorRun::fence_settled`, critic item F07's review): the open's first take waits up to
    /// a grace more behind its fence.
    SettledFenceTakeWaitsForACommit,
    /// A settle the broker refused is forgotten (`Bridge::pin_settle_refused`, critic item F07):
    /// every pass asks that broker again and is refused again, and the pin is never fenced.
    SettleRefusalForgotten,
    /// A settle the broker refused is taken as its answer (`Bridge::pin_settle_refused`, critic
    /// item F07): the pin is held as the fence with no flush behind its barriers.
    RefusedSettleMakesTheFence,
    /// A drain peeks the outbox with a post still in flight, its answer not read first, and
    /// sends what it lists.
    PeekPastOwedPost,
    /// A pipelining drain skips a listed post by what is owed when it reaches the post, not by
    /// what was owed at the peek (C12 step 0').
    LiveOwedSkip,
    /// The bridge never lifts the gap fences its refused writes set (`Bridge::lift_gap_fences`).
    KeepGapFences,
    /// The attach's replay of the in-flight journal does not hold the gap fences
    /// (`Bridge::fences_held` stays off in `Bridge::settle_in_flight`): a refused entry's fence
    /// is lifted at the next entry's publish, so a later post of its sender lands ahead of its
    /// retry. Its control is a bridge unit test on a real broker
    /// (`the_attachs_replay_holds_the_fences_a_refused_entry_set`).
    LiftFencesDuringReplay,
    /// A post's first pin is made with an earlier post of its sender in the same words still
    /// owed (`Bridge::settle_same_words` skipped): the pin's head lies below that post's record,
    /// and its retry's read back takes that record for its own. Its control is a bridge unit
    /// test on a real broker (`a_senders_second_post_in_the_same_words_lands_after_the_first`).
    PinAheadOfSameWords,
    /// The bridge pipelines a sender's posts on the `Hello`'s offer of the fence alone and
    /// never asks for it (`Bridge::ask_gap_fences`), so the broker does not fence its link.
    NeverAskForTheFence,
    /// A post goes out, or is journaled, though an answer read since its drain reached it, or
    /// a group append the disk refused since, held its sender (`Bridge::held_behind`).
    SendPastAHeldSender,
    /// A post the broker's rate limit refused is retired at the door, as one refused for good
    /// (`Bridge::post_refused`, critic item E06): it is lost.
    RateLimitedIsForGood,
    /// A rate-limited refusal holds nothing (`Bridge::rate`, critic item E06): the refused post
    /// and every other publish of the producer go again at the next drain, into the same empty
    /// bucket.
    RetryRateLimitedAtOnce,
    /// An `undeliverable` notice the rate limit held back is dropped, as one refused for good
    /// (`Bridge::notices_held`, critic item E06's review): the inbox commit passes its record
    /// and the remote sender is never told.
    NoticeHeldIsDropped,
    /// A post the rate limit refused holds only the producer, not its sender for the whole wait
    /// the refusal named (`RateHold::hold_sender`, critic item E06's review): a byte-limited
    /// post whose wait is long is sent again after every producer hold, into the same refusal.
    SenderWaitsOnlyTheProducerHold,
    /// A quit's withdrawal publishes each `state=exited` row once, a hold behind the rate limit
    /// waited out by none (`Bridge::withdraw`, critic item E06's review): the rows stay `live`.
    WithdrawalWaitsNothing,
    /// Every read of an owed ack waits for it (`Bridge::try_settle_one`).
    WaitForEveryAck,
    /// At an idle moment the loop reads an owed post's answer only once it has arrived.
    IdlePostLeftToBackstop,
    /// A file a power cut tore is refused where it is read, at every attach of every later boot.
    RefuseTornAfterPowerCut,
    /// The first bridge of a boot after an unclean stop leaves every whole
    /// in-flight entry where it is, and the attach publishes each again at its
    /// sequence.
    RepublishInFlightOfAnEarlierBoot,
    /// The first bridge of a boot after an unclean stop leaves a torn mirror
    /// pin where it is, as before its durable copies (`PinRules::set_aside_torn`
    /// off): the mirror stops on it, for good.
    KeepTornPin,
    /// The alias key has no durable copy, as before `logkey`.
    KeyInThePinAlone,
    /// The total is read off the pin alone, as before `logfloor`.
    TotalInThePinAlone,
    /// A bridge tells aterm what its start moved into the quarantine in place of what the quarantine holds (`Bridge::tell_quarantine`).
    TellMovedNotHeld,
    /// A recovery step stopped by a write error still writes `run` after it (`Bridge::recover`).
    BeginRunPastAFailedRecovery,
    /// The mirror judges its pin at the hello before recovery completes (`Bridge::mirror_hello_answered`).
    JudgePinBeforeRecovery,
    /// An own record whose `acked` write or in-flight entry's unlink failed is delivered with its entry standing (`Bridge::settled_before_delivery`).
    DeliverPastAStandingEntry,
    /// A fetch (`inbox get @<off>`) hands a session an own record whose in-flight entry stands (`Bridge::fetch_record`).
    FetchPastAStandingEntry,
    /// A fetch hands a session a record of its lane past one a delivery would hold back: held in
    /// hand behind an own record, an own post whose ack is owed, or an own record whose in-flight
    /// entry stands (`Bridge::holds_back_below`).
    FetchPastAStandingEntryBelow,
    /// A fetch hands an answer, or this node's `expired`, leaving the deadline it settles standing (`Bridge::fetch_record`).
    FetchLeavesTheDeadline,
    /// An own record whose in-flight entry stands for aterm's drain drops the broker link, as
    /// every build before ATERM-KI-1111-3 did, where the bridge holds it alone
    /// (`Bridge::hold_record`).
    DropTheLinkAtAHeldOwnRecord,
    /// The inbox commit passes a held own record (`Bridge::inbox_commit_floor`).
    CommitPastAHeldOwnRecord,
    /// A held own record a session's refill read ahead of the inbox group's live tail is marked
    /// as the tail's (`Bridge::hold_record`) and so accounted against the inbox commit once
    /// delivered, as every build before ATERM-KI-1116-4504 accounted it: the commit passes the
    /// node's other mail the tail has yet to take.
    HeldRefillAccounted,
    /// The mirror's floor is trusted as it reads.
    TrustTheFloorThatReads,
    /// An attach that could not write the state dir's witness of the log goes on and delivers on it.
    AttachPastAnUnwrittenWitness,
    /// A bridge whose recovery after an unclean stop is owed stops its run cleanly all the same.
    CleanStopOverAnOwedRecovery,
    /// Each try of the recovery's moves publishes a copy of every file again.
    CopyOnEveryTry,
    /// A recovery remembers one quarantine copy per source, whatever its prefix
    /// (`StateDir::quarantine_try`): the retired copy of the record the journal holds at a torn
    /// file's sequence ends the memory of that file's copy, and the next try makes a second.
    OneQuarantineMemoryPerSource,
    /// The sweep gives no departed session's subject back (`Bridge::reclaim_subjects`).
    NoSubjectRetire,
    /// The sweep gives a subject back with the unconditional `/a/retire`, whatever landed on it since it read it.
    RetireUnconditionally,
    /// The first start of a boot sweeps the files an earlier boot's sessions left and retires
    /// none of their post reservations, as before item C14
    /// (`StateDir::sweep_sessions_of_an_earlier_boot`): a post whose `outbox sent` a power cut
    /// stopped holds its session, and the subjects it names, back from the budget for good.
    KeepEarlierBootReservations,
    /// Every lease the bridge writes names only a boot whose page cache vouches for the state
    /// dir ([`crate::state::StateDir::trusted_boot`]), as before item RZ2: off the boot volume
    /// ([`Opts::off_boot_volume`]) none, so the next boot finds no earlier one and sweeps
    /// nothing.
    LeaseNamesNoBootOffTheBootVolume,
    /// An in-flight entry is published again at its pinned sequence without reading its
    /// record back first (`Bridge::landed_since`), trusting astream's dedup past a cut.
    ReplayWithoutReadingBack,
    /// A post or receipt retried at a pin an earlier attempt took is published again without
    /// reading its record back first (`Bridge::landed_at_its_pin`).
    RetryWithoutReadingBack,
    /// A retry at a pin, and the attach's replay, take no landing the bridge holds unhanded for
    /// its standing entry, held in hand or queued in the mailbox (`Bridge::unhanded_landing`
    /// answers `None`): past a cut that gave that copy up on the bus they read back, find none
    /// and publish again, and the reader is handed both, as every build before
    /// ATERM-KI-1115-149 did. Its witness is a real broker's
    /// (`bridge::tests::a_retry_settles_at_the_landing_the_bridge_holds_unhanded`): no seed of the
    /// asking, asking-with-write-errors, asking-with-fetches or keyed lossy worlds reaches it on
    /// this build.
    RetryPastAnUnhandedLanding,
    /// A retry at a pin takes no copy of its standing entry's landing that the inbox's reader
    /// has yet to read (`Bridge::unhanded_landing` answers no `Unhanded::Unread`): a record
    /// astream streamed to that reader before a cut, not yet pushed into the mailbox, is in
    /// neither place the retry looks, so it reads back, finds none past the cut and publishes
    /// again, and the reader is handed both, as every build before ATERM-KI-1115-1149 did. Its
    /// witness is a real broker's, a frame held between it and the reader
    /// (`bridge::tests::a_retry_waits_for_the_inbox_reader_to_pass_its_landing`): a world's
    /// tails send no mark, so its inbox reader is unmarked (`Fx::tail_marks`) and no world's
    /// retry waits.
    RetryPastAnUnreadInboxRecord,
    /// The waits for the inbox's reader take it as marked only once a mark came to it
    /// (`Mailbox::inbox_marked`, read by `Bridge::unhanded_landing` and `Bridge::answer_queued`),
    /// not from what its subscription negotiated (`Bridge::tail_marks`), as the first cut of
    /// ATERM-KI-1115-1149's sweep did: every attach resets the face, and a resubscribed group
    /// delivers its backlog before its catch-up mark, so a retry or the sweep in that backlog
    /// publishes past a record still on its way in. Its witnesses are a real broker's, a frame
    /// held between it and the reader
    /// (`bridge::tests::a_retry_waits_for_the_inbox_reader_to_pass_its_landing`), and the
    /// reader played at the mailbox's own doors
    /// (`bridge::tests::the_sweep_waits_for_the_inbox_reader_to_pass_what_a_cut_gave_up`'s
    /// backlog arm); no world's reader is marked.
    UnreadOnlyOnceMarked,
    /// The inbox's reader is taken as marked whatever its broker answered idle marks
    /// (`Bridge::tail_marks`), as the first cut of ATERM-KI-1115-1149's retry took every reader:
    /// on a broker that declines them, a retry waits for a reader that, its node quiet, never
    /// passes the offset, and the sender is held for good. Its witness is a real broker's
    /// behind one that refuses `IdleMarks`
    /// (`bridge::tests::a_retry_waits_for_the_inbox_reader_to_pass_its_landing`'s unmarked arm).
    UnmarkedTailTakenAsMarked,
    /// A gap notice reads the inbox face to the end of its range (`Mailbox::push_gap`,
    /// `Mailbox::gaps_pass_their_range`), as the first cut of ATERM-KI-1115-1149 did: astream
    /// delivers a record retention still holds inside the range after the notice, so a retry
    /// waiting for it takes the notice for its passing, publishes again, and the reader is
    /// handed both. Its witness is a real broker's, a frame held between it and the reader
    /// (`bridge::tests::a_retry_waits_for_the_inbox_reader_to_pass_its_landing`'s gap arms);
    /// no world's reader is marked.
    GapPassesItsRange,
    /// A keyed re-post's own attempt's unread copy (`PinLanding::Unread`) answers before its
    /// key's winner is read (`Bridge::keyed_repost_landed`), as the first cut of
    /// ATERM-KI-1115-1149 did: a winner the attach's replay landed at the entry's offset, which
    /// the inbox's reader has not read past, holds the re-post in a wait where the winner's
    /// read settles it at once. Its witness is the reader played at the mailbox's own doors
    /// (`bridge::tests::a_keyed_reposts_unread_landing_settles_at_a_winner_read_there`);
    /// no world's reader is marked.
    KeyedUnreadCutsOffItsWinner,
    /// A read back before a republish goes on at the floor (`transport::fetch_kept`), never
    /// told: a record retention still holds below its subject's oldest is missed.
    ReadBackWithoutTheHeld,
    /// A read of one offset that its filter's oldest lies above is taken for no record there,
    /// never read again told (`Bridge::read_at`, and `Bridge::held_record`'s read of an old
    /// node's row; the `read_back_rows` of before finding F3): a record retention still holds
    /// there reads as gone, so a session's fetch of it is answered `retained`, a dedup's read
    /// back finds nothing to verify, and a receipt for an old node's row names it by its
    /// offset here. Its control for an old node's row is a bridge unit test
    /// (`an_old_nodes_row_held_below_its_lanes_oldest_is_read_there`).
    ReadAtWithoutTheHeld,
    /// A relayed (`via=`) record is classified by a local rule that skips astream's relayed
    /// demotion (`Bridge::classify_delivery`): it keeps the kind it claims unless another of
    /// [`astream_wire::WakeClass::classify_for_sender`]'s rules demotes it. Its control is a
    /// bridge unit test (`classify_delivery_is_astreams_sender_demotion_in_every_cell`).
    RelayedKeepsItsKind,
    /// A read of a session's lane that met its filter's oldest goes on at the floor on the
    /// link, never read again told (`Bridge::lane_page`, the reading before the F3 review): a
    /// record retention still holds below the oldest is skipped, and the session is told it
    /// was given up. The refill's and the deadline sweep's, so a held answer is judged absent
    /// and its ask `expired`. Its controls are bridge unit tests on a real broker
    /// (`a_refill_offers_the_mail_retention_holds_below_its_lanes_oldest`,
    /// `the_deadline_sweep_finds_an_answer_held_below_the_askers_oldest`).
    LaneReadWithoutTheHeld,
    /// An in-flight entry journaled on a log a move continued is read back from its own head
    /// on that log, not from where the new log continues it (`Bridge::read_back_from`): a copy
    /// a replay published since, below that head, is missed.
    ReadBackFromTheJournaledHead,
    /// A session's refill plain-fetches its lane: a floor retention raised refuses it, and
    /// it stops there instead of reading on from the floor (`transport::fetch_kept`).
    RefillWithoutTheFloor,
    /// The attach's refill of several sessions together reads the node's inbox from the
    /// highest session's start, not the lowest (`Bridge::refill_together`): the others' records
    /// below it are not offered again.
    RefillTogetherFromTheHighest,
    /// A session whose start lies below the node inbox's oldest joins the attach's shared refill
    /// read there without reading its own lane below it (`Bridge::refill_together`): its records
    /// retention still holds below another lane's floor are not offered again.
    RefillJoinBelowTheOldest,
    /// The inbox's gap notice is taken as the whole range gone: the group is committed past
    /// it, the held records in it with it.
    CommitPastTheGap,
    /// The inbox's gap notice is not told to aterm (`link gap`).
    TellNoGap,
    /// The mirror's window budget never takes the broker's `oldest=`: what retention gave up
    /// is counted in the window still.
    MirrorIgnoresOldest,
    /// A drop of a record in flight that the disk refuses leaves the record in the open, as the
    /// journal did before B19's review (`StateDir::append_post_frame`): the next attach of
    /// the same process publishes it again.
    KeepARefusedDrop,
    /// THE DEADLINE SWEEP'S PLAIN FETCH (item F4): the sweep reads the asker's lane with a
    /// plain `Fetch` and takes a `retained-from=` refusal for a failed read
    /// (`Bridge::settled_on_bus`), never reading on from the floor: past a floor retention
    /// raised on that lane, the sweep stops there every time, and the ask never expires.
    SweepWithAPlainFetch,
    /// THE TOPIC REPLAY'S PLAIN FETCH (item F4): a broadcast topic's catch-up reads its
    /// filter with a plain `Fetch` and takes a `retained-from=` refusal for a failed read
    /// (`Bridge::catch_up_topic_page`): a cursor below the floor never moves, and the
    /// follower is handed nothing of the topic the broker holds past it.
    TopicReplayWithAPlainFetch,
    /// A topic's catch-up reads on past what retention gave up below it and tells aterm
    /// nothing (`Bridge::catch_up_topic_page`'s `link gap … topic=`): a silent skip.
    TopicReplayTellsNoGap,
    /// A topic's catch-up drops its told page's uncounted gap notice, as a lane's read does
    /// (F3's rule, `Bridge::lane_page`), where no face tells the topic otherwise: what went
    /// below the first broadcast the page delivers is skipped untold.
    TopicReplayDropsAnUncountedNotice,
    /// The deadline sweep publishes `expired` for every ask whose deadline passed, reading
    /// nothing of the asker's lane (`Bridge::settled_on_bus`): an answered ask expires.
    SweepIgnoresAnswers,
    /// The deadline sweep trusts the read of the asker's lane it made before it published
    /// `expired` (`Bridge::answered_below` reads nothing below it), as every build before
    /// ATERM-KI-1115-7889: an answer another node's bridge landed between the read and the
    /// `expired`, below it, leaves the `expired` the ask's verdict, said `expired re=<ask>` on
    /// the asker's `ev` and journaled `expired=<e>`. Delivery in lane order still makes it moot
    /// behind the answer (`Bridge::late_answer`); with `LateByTheExpiryAlone` too, as every
    /// build before, the answer is handed `late=1`, then the `expired`. Its witness is a real
    /// broker's, the answer landed by a test's seam between the two
    /// (`bridge::tests::an_answer_landing_between_the_sweeps_read_and_its_expired_is_the_verdict`):
    /// the simulator runs one node, and a bridge step is atomic there.
    SweepTrustsItsPreRead,
    /// The fence takes an answer this bridge alone holds below an `expired`, queued in the
    /// mailbox or held in hand, for one the bus holds (`Bridge::answered_below`), as the first
    /// cut of ATERM-KI-1115-7889 took it: the `expired` is journaled moot and its deadline
    /// dropped, and once a reconnect's reset of the mailbox loses that answer (a cut gave it up
    /// on the bus), the asker is handed neither it nor the `expired`. Its witness is a real
    /// broker's
    /// (`bridge::tests::an_expired_behind_an_answer_only_in_hand_is_handed_once_that_answer_is_lost`).
    FenceSettlesOnAnAnswerInHand,
    /// The fence behind this node's `expired` reads no answer held in hand behind an own record
    /// of the asker's lane (`Bridge::answered_below` reads no `Bridge::held_own`): an `expired`
    /// the attach's replay lands above an answer held across the link drop, a cut having taken
    /// it off the bus, is said `expired re=<ask>` and journaled `expired=<e>`, the ask's
    /// verdict, before that answer is delivered (whose delivery then makes it moot). At the
    /// sweep the read finds nothing the sweep's own did not (`SweepPastAHeldAnswer`'s). Its
    /// witness is a real broker's
    /// (`bridge::tests::an_expired_the_replay_lands_above_an_answer_held_in_hand_is_left_unjudged`).
    FenceBlindToAHeldAnswer,
    /// This node's `expired` that no bridge remembers judged is handed unread
    /// (`Bridge::expiry_stands`), as every build before ATERM-KI-1115-7889 handed it: a bridge
    /// after the one that published it (killed before its asks journal named the verdict, or
    /// whose fence read failed) hands an `expired` its ask's answer lies below. Its witness is
    /// a real broker's
    /// (`bridge::tests::a_successor_hands_no_moot_expired_with_or_without_the_journals_verdict`).
    DeliverAnUnjudgedExpiry,
    /// A later copy of an `expired` whose ask's kept verdict is moot is judged by its own fence
    /// alone (`Bridge::remember_verdict` keeps no earlier copy's moot verdict for it), as the
    /// first cut of ATERM-KI-1115-7889 judged it: past a cut that gave the answer and the first
    /// copy up, a copy the attach's replay lands again from an entry that names no head (an
    /// older build's, published at its pin unread) finds no answer below it, is said and kept
    /// the ask's verdict, and is handed, and fetched, after the answer the asker holds. Its
    /// witness is a real broker's
    /// (`bridge::tests::a_copy_of_an_expired_behind_a_kept_moot_verdict_is_never_handed`'s
    /// headless arm).
    CopyJudgedPastAKeptMoot,
    /// A copy of an `expired` above its ask's kept moot verdict reads its fence at each delivery
    /// and fetch (`Bridge::expiry_stands` passes `Bridge::kept_moot_below` by), the kept verdict
    /// then turning it moot (`Bridge::remember_verdict`): past the cut that gave the answer up
    /// the read is a told one, so while the told reader is refused at the broker's connection
    /// cap the copy is held unjudged, its session's later mail behind it, and fetched
    /// `err=unsettled`, and each read that goes says `expired-moot` again. Its witness is a real
    /// broker's
    /// (`bridge::tests::a_copy_of_an_expired_behind_a_kept_moot_verdict_is_never_handed`'s
    /// headless-read arm).
    CopyReadPastAKeptMoot,
    /// The attach's replay reads no kept moot verdict of the ask before it publishes again an
    /// `expired` it read back nowhere (`Bridge::expiry_moot`): past a cut that gave the answer
    /// and the first copy up, nothing on the lane says the ask settled, so the copy is published
    /// again, a record on the bus that raw readers of the lane see. The kept verdict still keeps
    /// it from being handed (`Bridge::remember_verdict`), and with `CopyJudgedPastAKeptMoot` too
    /// it is handed. Its witness is a real broker's
    /// (`bridge::tests::a_copy_of_an_expired_behind_a_kept_moot_verdict_is_never_handed`'s
    /// headed arm).
    ReplayBlindToAKeptMoot,
    /// The attach's replay lands an `expired` again unfenced (`Bridge::replay_in_flight`): no
    /// read of the lane below where it lands judges it, so nothing says it moot on the asker's
    /// `ev` or in the asks journal, and the ask's deadline stands, until its delivery judges it
    /// (`Bridge::expiry_stands`). Its witness is a real broker's, the answer landed by a test's
    /// seam between the replay's read and its publish
    /// (`bridge::tests::an_expired_the_replay_lands_again_is_fenced_as_the_sweeps_is`).
    ReplayLandsUnfenced,
    /// A delivery whose read of the asker's lane, judging an answer or this node's `expired`,
    /// fails with the link standing (the told reader's dial, refused at the broker's connection
    /// cap) drops the link (`Bridge::deliver_record`), as the first cut of ATERM-KI-1115-7889
    /// did: the attach offers the record again to the same failure, and every session's mail on
    /// the node waits while it lasts. Its witness is a real broker's
    /// (`bridge::tests::a_fence_whose_told_read_cannot_be_dialled_holds_its_record_for_its_session_alone`).
    DropTheLinkAtAnUnreadFence,
    /// A record held unjudged (`Bridge::judge_waits`) is tried again at every release of held
    /// records (`Bridge::release_held`: the loop's, `Bridge::deliver_held`, and the attach's
    /// that makes room in hand, `Bridge::make_room_in_hand`), not at the held back-off's next
    /// try alone, and the release stops at it (with no wait to pass it by, it would be picked
    /// again): while the told reader is refused at the broker's connection cap, every pass of
    /// the loop dials it again, and inside the attach the lanes behind it are not released. Its
    /// witness, a real broker's with the dials counted, drives the loop's release
    /// (`bridge::tests::a_fence_whose_told_read_cannot_be_dialled_holds_its_record_for_its_session_alone`).
    JudgeAtEveryPass,
    /// A release of held records (`Bridge::release_held`: the loop's, `Bridge::deliver_held`,
    /// and the attach's that makes room in hand, `Bridge::make_room_in_hand`) stops at a record
    /// it held again unjudged, as at any record held again: another session's record the same
    /// try freed (its entry ended at that try's drain) waits for a later pass, the next mailbox
    /// record or the roster's refresh, and every try repeats it while the judging read fails;
    /// inside the attach, no lane past it is released to make room. Its witness, a real
    /// broker's, drives the loop's release
    /// (`bridge::tests::a_release_held_unjudged_again_lets_another_sessions_held_mail_go`).
    StopAtAnUnjudgedRelease,
    /// A record held unjudged keeps its wait once what it waited on is decided
    /// (`Bridge::lift_judge_waits_on` lifts nothing): an `expired` that waited behind an answer
    /// in hand, made moot by that answer's delivery, stays held until the held back-off's next
    /// try, its session's later mail behind it; one that waited behind an answer's pipelined
    /// line aterm refused stays held the same. Its witnesses are a real broker's
    /// (`bridge::tests::an_expired_behind_an_answer_in_hand_waits_and_is_moot_once_the_answer_is_taken`,
    /// `bridge::tests::an_expired_in_its_answers_run_waits_for_aterm_to_take_the_answer`'s
    /// refused arm).
    KeepTheWaitPastItsVerdict,
    /// This node's `expired` whose fence finds only an answer this bridge holds in hand below it
    /// (`Fenced::InHand`) is handed, and fetched, at once (`Bridge::expiry_stands`), ahead of
    /// that answer, which then makes moot an `expired` the asker holds. Its witness is a real
    /// broker's
    /// (`bridge::tests::an_expired_behind_an_answer_in_hand_waits_and_is_moot_once_the_answer_is_taken`).
    HandPastAnAnswerInHand,
    /// An answer's delivery makes the `expired` above it moot at once, remembered, said and
    /// journaled before aterm has taken the answer (`Bridge::late_answer`), as the first cut of
    /// the ATERM-KI-1115-7889 review made it: where a cut took the answer off the bus and its
    /// hand-off then fails (a refused write of the records told aterm, which drops the link; a
    /// lost lane under which aterm took nothing, the next bridge restoring the moot from the
    /// journal), the answer is gone, the
    /// `expired` is moot, and the asker is handed neither. Its witness is a real broker's
    /// (`bridge::tests::an_answer_aterm_does_not_take_leaves_its_expired_to_be_handed`).
    MootBeforeTheAnswerIsTaken,
    /// This node's `expired` is judged by its fence while an answer below it waits on aterm's
    /// reply to its pipelined line (`Bridge::moots_in_flight`, `Bridge::expiry_stands`): the run
    /// took the answer out of the mailbox and a cut off the bus, so the fence finds nothing,
    /// the `expired` is judged the ask's verdict and handed after the answer aterm then takes.
    /// Its witness is a real broker's
    /// (`bridge::tests::an_expired_in_its_answers_run_waits_for_aterm_to_take_the_answer`).
    JudgePastAMootInFlight,
    /// A pipelined row aterm did not take keeps the moot its answer made of the `expired` above
    /// it (`Bridge::send_rows` ends no hand-off it did not see taken): where the disk refused
    /// the records told aterm (the batch never goes, the link drops) or aterm refused the
    /// answer's line, the moot stays in flight (`Bridge::moots_in_flight`, which no reconnect
    /// clears), so the `expired` waits in hand at every judgment and is never handed, its
    /// session's later mail behind it. Its witness is a real broker's
    /// (`bridge::tests::an_expired_in_its_answers_run_waits_for_aterm_to_take_the_answer`'s told
    /// and refused arms).
    KeepTheMootOfAnUntakenRow,
    /// An answer is delivered `late=1` wherever an `expired` of its ask stands remembered, below
    /// it on the lane or above it, and makes no `expired` above it moot (`Bridge::late_answer`),
    /// as every build before ATERM-KI-1115-7889 read it: an answer the inbox's reader brings in
    /// below an `expired` the fence could not see it under (retention gave it up on the bus,
    /// unread) is handed late, and the `expired` after it. Its witnesses are a real broker's
    /// (`bridge::tests::an_answer_queued_below_the_expired_and_cut_off_the_bus_is_the_verdict`,
    /// `bridge::tests::the_sweep_leaves_an_answer_held_behind_an_own_record_to_its_delivery`).
    LateByTheExpiryAlone,
    /// The deadline sweep reads the asker's lane on the bus alone, not the inbox records the
    /// mailbox holds read and not yet handled (`Bridge::answer_queued`), as every build before
    /// ATERM-KI-1114-2241: for an answer pumped before a cut gave it up on the bus, the sweep
    /// publishes an `expired` above it and drops the deadline, which breaks the sim's deadline
    /// law (seed 2241). The fence behind the `expired` then finds the answer in hand and leaves
    /// it unjudged (`Bridge::answered_below`), and the answer's delivery makes it moot; before
    /// ATERM-KI-1115-7889 the asker was handed the `expired`, then the answer `late=1`.
    SweepIgnoresQueuedAnswers,
    /// The deadline sweep takes no answer the inbox's reader may still bring in
    /// (`Bridge::answer_queued` with the range its read was told retention gave up): an answer
    /// astream streamed to that reader before a cut, not yet pushed, is neither queued nor on
    /// the bus, so the sweep publishes `expired` and the asker is handed the answer after it,
    /// as every build before ATERM-KI-1115-1149. Its witness plays the inbox's reader at the
    /// mailbox's own doors, on a real broker's lane, no frame held
    /// (`bridge::tests::the_sweep_waits_for_the_inbox_reader_to_pass_what_a_cut_gave_up`:
    /// `Mailbox::reader_starts`, `push_mark` and `push` called as the reader calls them): the
    /// simulator's tails send no mark, so its inbox reader is unmarked (`Fx::tail_marks`) and
    /// no world's sweep waits.
    SweepPastAnUnreadAnswer,
    /// The deadline sweep's wait for the inbox's reader takes no range its read of the lane was
    /// told only that retention may have given up (`Bridge::settled_on_bus`, an uncounted gap
    /// notice: astream's ledger does not name every record of it, as after a rules change, a
    /// compaction no rules planned, or for a class kept by last value), as the first cut of
    /// ATERM-KI-1115-1149 did: an answer retention gave up there, streamed to the reader and
    /// not yet pushed, is expired over. Its witness plays the reader at the mailbox's own doors
    /// on a real broker whose rules keep the lane's answers by last value
    /// (`bridge::tests::the_sweep_waits_for_the_inbox_reader_to_pass_what_a_cut_gave_up`'s
    /// uncounted arm); no world's reader is marked.
    SweepPastAnUncountedGap,
    /// The deadline sweep reads the asker's lane again at every sweep while it waits for the
    /// inbox's reader (`Bridge::sweep_waits`, `Bridge::expire_deadlines`), as the first cut of
    /// ATERM-KI-1115-1149 did: a read, a told reader's dial and a retention line every
    /// `DEADLINE_RETRY` until the reader passes the range. Its witness is a real broker's, an
    /// answer landed on the lane while the sweep waits, which a read settles the deadline at
    /// (`bridge::tests::the_sweep_waits_for_the_inbox_reader_to_pass_what_a_cut_gave_up`'s
    /// record arm); no world's reader is marked.
    SweepRereadsWhileItWaits,
    /// The deadline sweep takes no answer held in hand behind an own record of the asker's
    /// lane (`Bridge::answer_queued` reads no `Bridge::held_own`), as every build before the
    /// ATERM-KI-1115-1149 review: past a cut that gave the answer up on the bus, the sweep
    /// publishes `expired` while the bridge holds the answer and drops the deadline. The fence
    /// behind the `expired` finds the answer in hand and leaves it unjudged, and the answer's
    /// delivery in lane order makes it moot (`Bridge::late_answer`); with
    /// `FenceBlindToAHeldAnswer` too the `expired` is said and journaled the ask's verdict at the
    /// sweep, and with `LateByTheExpiryAlone` as well the asker is handed the answer `late=1`,
    /// then the `expired`. Its witness is a real broker's
    /// (`bridge::tests::the_sweep_leaves_an_answer_held_behind_an_own_record_to_its_delivery`).
    SweepPastAHeldAnswer,
    /// No subscription's read deadline is armed before its first mark
    /// (`Bridge::start_reader`), as astream's `recv_live` rule alone arms it: a marked inbox
    /// tail that stalls before its catch-up mark holds every wait for the inbox's reader, the
    /// retries waiting on their own lanes and their senders, and the sweep's deadlines, without
    /// bound. Its witness is a stand-in broker that takes the opt-ins and sends nothing
    /// (`bridge::tests::a_marked_tail_that_sends_nothing_ends_its_reader_at_the_live_deadline`);
    /// no world's reader is marked or reads on a live socket.
    ReaderUnboundUntilItsFirstMark,
    /// A refused post's later entries leave the in-flight journal lowest sequence first
    /// (`Bridge::drop_behind_a_failed_post`), as every build before ATERM-KI-1114-56: a stop
    /// after the first drop leaves later entries the replay publishes ahead of the dropped
    /// post's send from the drain, out of its sender's order.
    DropBehindLowestFirst,
    /// The replay publishes an `expired` verdict it read back nowhere at its pin whatever the
    /// asker's lane holds since (`Bridge::expiry_moot`), as every build before ATERM-KI-1114-3752:
    /// past a cut, the copy lands behind the ask's answer.
    ReplayRepostsAMootExpiry,
    /// A hand-off whose heads the disk refused unlinks the journal all the same, as before item
    /// F4 (`StateDir::hand_posts_to_files`): the next bridge publishes each record in flight
    /// again at its pin without reading it back, and past a cut that dropped its key a post or
    /// a receipt lands twice.
    HandOffWithoutTheHeads,
    /// A clean stop vouches for a hand-off's files and leaves the heads it kept beside them
    /// unflushed (`StateDir::a_clean_stop_flushes`): a power cut after the stop keeps the files
    /// and takes the heads, and the next bridge publishes each record in flight again at its pin
    /// without reading it back, a second copy past a cut that dropped its key.
    StopLeavesTheHeadsUnflushed,
    /// A post with an in-flight entry is sent behind its sender's post to another lane, which has
    /// none, with that one's answer unread (`Bridge::settle_unjournaled_ahead`): a kill leaves
    /// the entry, the attach's replay lands it, and that post's retry lands behind it.
    EntryAheadOfAPostWithNone,
    /// The told samples a lineage move took to a later generation forget the log they were read
    /// on (`ToldSamples::moved_from`, none recorded at the move and none read by the proof): a
    /// remint met under the witness an attach refused after the move left on that log proves
    /// nothing from them, and aterm forgets what it holds; the same move met again drops them.
    MovedSamplesUnpinned,
    /// A record held in hand keeps the mark of the inbox group's tail that took it across the
    /// attach that closes that tail's subscription (`Bridge::forget_the_tails_marks`): released
    /// inside the attach, where its refill makes room, or after it, it is accounted against the
    /// inbox commit, which passes a record that tail took and did not hand before the new tail
    /// offers it again. Witnesses: the asking world with fetches' seed 7179 (after the attach),
    /// and bridge's
    /// `a_release_inside_the_attach_owes_no_commit_past_what_the_new_tail_has_yet_to_take`.
    HeldTailKeptAcrossAResubscribe,
    /// A held record released unmarked (`Bridge::deliver_held`) and not taken by aterm (owed, or
    /// its control lost) does not floor the inbox commit, which a commit the earlier tail owed
    /// past it then passes.
    UnmarkedReleaseUnfloored,
    /// A drain pass's group of fresh own-lane posts is sent ahead of the one append that
    /// reserves them and journals their entries (ATERM-KI-1111-5, `Bridge::flush_post_group`):
    /// a kill between leaves a publish on the bus that no reservation names, and the next drain
    /// takes the post afresh at a new sequence and lands it again.
    GroupSentAheadOfItsAppend,
    /// The checks that read what is owed leave a drain pass's staged group out
    /// (ATERM-KI-1111-5, `Bridge::owes_or_stages`): a post is pinned and grouped behind a staged
    /// post of its sender's in the same words, or, against a broker that does not gap-fence,
    /// behind a staged post of its sender's at all.
    GroupedPostsUnowed,
    /// A refusal read before a group's later burst drops no entry of its sender's posts the
    /// group journaled and no burst has written yet (ATERM-KI-1111-5's review,
    /// `Bridge::unwritten_behind`): a kill before the loop reaches them leaves their
    /// entries, and the attach's replay lands them ahead of the refused post's retry.
    UnwrittenGroupKeepsItsEntries,
    /// An attach takes its incarnation before it takes a stop's hand-off's files into the
    /// journal, as before item F4 (`StateDir::name_attach_in_posts`): one that stops between
    /// the two leaves the hand-off's heads stale for the next attach, whose replay publishes each
    /// record in flight again at its pin unread.
    AdoptAfterTheAttach,
    /// An adoption of a stop's hand-off's files names no attach, as before item F4
    /// (`StateDir::adopt_legacy_posts`): a bridge that took them and handed them on before its
    /// own attach came up kept their heads under no attach, which the next refuses.
    AdoptWithoutTheAttach,
    /// An answer, or this node's `expired`, is delivered while the deadline table that its
    /// delivery settles is not written, as before item F4 (`Bridge::settle_deadline_for`): a
    /// bridge killed then leaves the next one the deadline, which expires again an ask whose
    /// answer or `expired` the asker holds once retention gave that record up.
    DeliverOverAnUnwrittenSettle,
    /// A post whose deadline the disk refused to write is retired all the same, as before item
    /// F4 (`Bridge::landing_line`): a bridge killed before the table is written again leaves
    /// the next one no deadline to sweep, and the ask never expires.
    RetireOverAnUnwrittenDeadline,
    /// An ask the attach lands again as its own attempt, its entry standing past aterm's
    /// `outbox sent`, has nothing noted where it lands (`Bridge::note_relanded_ask`), as every
    /// build before ATERM-KI-1116-7314's fix: on a log the attach forgot its offset on (a power
    /// loss's next generation, its fork below it), neither its recipient nor its deadline
    /// stands, and the ask never expires. Witness: bridge's
    /// `an_ask_the_replay_lands_again_on_a_moved_generation_expires_at_its_own_deadline`.
    ReplayLeavesTheDeadlineUnnoted,
    /// An ask the attach lands again has its deadline noted `dl` from now, as the drain's retry
    /// measures one (`Bridge::note_relanded_ask`), not from the record's own `t=`: late by
    /// however long the outage between its first landing and the attach lasted. Witness:
    /// bridge's `an_ask_the_replay_lands_again_on_a_moved_generation_expires_at_its_own_deadline`.
    ReplayDeadlineFromNow,
    /// An entry held for the drain whose ask was landed again is ended while its deadline is
    /// not on the disk (`Bridge::relanded_ask_noted`), as `RetireOverAnUnwrittenDeadline`
    /// retires a post: a bridge killed then leaves the next one no entry and no deadline, and
    /// the ask never expires. Witness: bridge's
    /// `an_undecided_asks_record_taken_for_its_drain_is_released_once_its_deadline_is_on_disk`.
    ReleaseOverAnUnwrittenDeadline,
    /// Every record the attach's replay lands an ask's entry at is the entry's own attempt
    /// (`Bridge::relanded_own_ask`), its key's winner's too: a keyed re-post deduped onto its
    /// winner's record notes a deadline there, where a re-post deduped onto an existing record
    /// creates none. Witness: bridge's
    /// `an_ask_the_replay_lands_at_its_keys_winner_or_its_own_attempt_is_noted_only_at_its_own`.
    ReplayTakesTheWinnerForItsOwn,
    /// An ask the attach lands again at another offset than its first landing has its deadline
    /// noted at the copy whatever the asks journal still holds of that landing
    /// (`Bridge::note_relanded_ask`, as its fix first noted it, once an offset): past a cut that
    /// gave the first record up on a log the attach forgot nothing of, or below a moved
    /// generation's fork, the one ask holds two deadlines, and unanswered it expires twice.
    /// Witness: bridge's
    /// `an_ask_the_replay_lands_again_where_its_first_landing_stands_is_noted_once`.
    RelandedAskNotedPerOffset,
    /// The attach's replay decides whether a keyed re-post's landing is its own attempt by its
    /// pin's winner's offset whatever log that offset was taken on (`replayed_own_attempt`), as
    /// a landing decides by its post's pin: a copy landed past a move at the offset the pin kept
    /// is taken for its winner's, noted nowhere, and a dedup onto its winner's record at any
    /// other is taken for its own, a deadline noted on another post's record. Witness: bridge's
    /// `a_keyed_ask_the_replay_lands_is_its_own_by_where_it_lies_against_its_pins_head`.
    ReplayWinnerOffsetFromAnyLog,
    /// A reply to an ask the attach landed again, delivered before the release notes its
    /// recipient (`Bridge::reply_from`), is judged by `asked` and the deadlines alone, which a log
    /// the attach forgot the offset on left empty there: another principal's reply settles the
    /// ask, and the release arms no deadline. Witness: bridge's
    /// `a_reply_to_an_ask_the_replay_lands_again_before_its_release_is_judged_by_its_recipient`.
    ReplyFromIgnoresTheRelandedAsk,
    /// An ask the attach lands again whose answer was delivered ahead of the release is armed
    /// all the same (`Bridge::note_relanded_ask`), due at its own `t + dl`, mostly past: where
    /// retention gave the answer up, the sweep publishes an `expired` over an answer the asker
    /// holds. Witness: bridge's
    /// `a_reply_to_an_ask_the_replay_lands_again_before_its_release_is_judged_by_its_recipient`.
    ReplayArmsOverAnUnarmedSettle,
    /// A landed post's in-flight entry ends ahead of its offset in the self-lane log
    /// (`Bridge::land_group`, critic item E07; a landed receipt's too, since F15 settles it
    /// there): a bridge killed between leaves the next one an own record neither in flight nor
    /// known as its own, which it refuses as forged.
    LandedBeforeAcked,
    /// Every landed post's offset goes into the self-lane log, one onto another node's lane or a
    /// person's too, as every build before critic item F17 wrote it (`Bridge::land_group`,
    /// `Landing::remembers`): each takes a slot of the forged-self window, so a full window
    /// drops an own offset for it and its floor rises past a forged own record, unaccused.
    WideAckedSet,
    /// The forged-self window's floor is read off its oldest offset, as every build before
    /// critic item F17's review read it (`Bridge::window_accuses`, `self_window`): a fresh
    /// window's first own record raises it past a forged own record below, unaccused.
    FloorOffTheOldest,
    /// A window whose `acked` file was missing, unreadable or empty is floored at 0, not at the
    /// head its attach reads, as every build before critic item F20 loaded it
    /// (`Bridge::floor_at_the_head`): every own record an earlier run landed is accused as
    /// forged.
    UnreadWindowAtZero,
    /// A landed post's reservation is forgotten ahead of aterm's `outbox sent`
    /// (`Bridge::land_group`, critic item E07; a landed receipt's ahead of its `deliver
    /// receipt=`, since F15): a bridge killed between leaves the next one a post aterm still
    /// lists and no pin holds, which goes out again at a fresh sequence.
    RetireBeforeOutboxSent,
    /// A landed post's in-flight entry ends at its ack, ahead of aterm's `outbox sent`, as every
    /// build before critic item E07 ended it (`Bridge::land_group`; a landed receipt's too, as
    /// every build before F15 ended it): a bridge killed between leaves the next one its own
    /// record to deliver while the post stands queued, and the drain's retry at the pin, past a
    /// cut that gave the record up, appends a second copy.
    LandedBeforeOutboxSent,
    /// A landed post's in-flight entry ends at any answer to its `outbox sent` and at a hold,
    /// as critic item E07's first cut ended it (`Bridge::land_group`; a landed receipt's at any
    /// answer, since F15): an answer that leaves the post queued (aterm's `ERR busy
    /// update-committing`), or a key or a deadline the disk refused, lets the record be
    /// delivered while the post stands queued, and the drain's retry at the pin, past a cut that
    /// gave the record up, appends a second copy.
    LandedWhileThePostStaysQueued,
    /// A landed receipt's in-flight entry ends at its ack, ahead of aterm's `deliver <sid>
    /// receipt=<rid>`, as every build before critic item F15 ended it (`Bridge::send_receipt`,
    /// the old order: `Bridge::publish_journaled` settles the entry as the record lands): a
    /// bridge killed between leaves the next one the receipt's own record to deliver while aterm
    /// still owes the receipt, and the drain's retry at the pin, past a cut that gave the record
    /// up, lands the answer a second time.
    ReceiptRetiredAtAck,
    /// A post's deadline is armed again over the settle the asks journal remembers, as before
    /// item C04 (`Bridge::note_deadline`): a held ask's retry, by the bridge that held it or by
    /// the next after a kill, arms the deadline its answer or `expired` settled while it was
    /// held, and a sweep expires it again once retention gave that record up.
    ArmOverASettle,
    /// The broadcast face's gap notice is only logged, as before item F4 (`Bridge::on_gap`):
    /// the face reads on past it, and the cursors at its frontier go past what retention gave
    /// up with its next record, telling no follower.
    SayFaceGapUntold,
    /// The deadline sweep's read takes no `expired` of this node's for a verdict
    /// (`Bridge::settled_on_bus`): a deadline left in the table by a bridge killed after it
    /// published the ask's `expired` is expired again by the next.
    ExpireWithoutReadingBack,
    /// NOT A BRIDGE RULE: THE TOPIC LAW'S OWN, as the asking world's first wide search read it
    /// (item F4): a follow's `since=` is taken on the broker's log at the follow, not on the
    /// log its session was last told ([`World::follow`]). A session a move had yet to be told
    /// to then has the bridge's move of its cursor to the fork judged a delivery below its
    /// `since`, a violation of no rule of the bridge's. A found seed pins the law's reading.
    FollowOnTheBrokersLog,
    /// NOT A BRIDGE RULE: THE RECEIPT AND DEADLINE LAWS' OWN, as every world read them until
    /// ATERM-KI-1114-1487, first met in the asking world with logs set aside: an older copy of an
    /// answer ([`World::check_settled`]) or of an `expired` ([`World::expired_once`]) counts as
    /// handed when any session holds a row at its offset, of any lane and of any log. A replay
    /// that lands again a copy retention gave up unread is then judged a second answer wherever
    /// a session was handed another record at that offset, on another lane or on a log since
    /// retired. A found seed pins the receipt law's reading.
    AnswerHandedByOffset,
    /// NOT A BRIDGE RULE: THE DEADLINE LAW'S OWN, as it first read fetches (ATERM-KI-1115-9210):
    /// an answer a session read by a fetch counts as handed below an `expired`
    /// ([`World::judge_expiries`]) whatever log replaced the one it was read on, one that gave up
    /// the ask it names included. A broker power loss ([`World::broker_stop`], `Stop::PowerLoss`)
    /// that gave up the answer and the ask it named, and another ask landing at that offset, then
    /// has the new ask's `expired` judged past an answer to the old one. A found seed pins the
    /// law's reading.
    FetchReadOnEveryLog,
    /// Every presence row reads its session's screen (`@<sid> text --json`) as it is published,
    /// as before critic item D03 (`Bridge::publish_session_presence`). Its control is a bridge
    /// unit test on the scripted verb lane
    /// (`an_attach_of_ready_sessions_reads_one_status_batch_and_no_meta_or_screen`).
    ReadTheScreenEveryRow,
    /// A session's meaning fields are read by its own `@<sid> meta`, as before critic item E05:
    /// a `sessions status` row's `role= attention= user_title=` and a watch ack's are left
    /// unread (`Bridge::status_into_presence`, `Bridge::refresh_meta`). Its control is a bridge
    /// unit test on the scripted verb lane
    /// (`an_attach_of_ready_sessions_reads_one_status_batch_and_no_meta_or_screen`).
    ReadMetaPerSession,
    /// A local event ends the reconnect back-off's wait (`wait_for_retry_or_aterm_close`), as a
    /// wait that dials again on any input would: a busy aterm turns the back-off into a rapid
    /// dial loop. Its control is a bridge unit test on a clock the test holds (critic item E04e,
    /// `busy_local_events_cannot_accelerate_a_failed_broker_retry`).
    EventEndsTheBackoff,
    /// A reservation's answer its read back cannot vouch for drops the broker link
    /// (`reason=read`), as every build before critic item F21 dropped it
    /// (`Bridge::unverified_publication`): every drain ends at that reservation, and every
    /// other sender's posts wait with it until retention cuts its record. Its controls are the
    /// stray world's seed 115 ([`Opts::strays`], a sender's later post on the bus ahead of the
    /// held one: `each_rule_switched_off_is_caught`) and a bridge unit test on a real broker
    /// (`an_unverifiable_reservation_holds_only_its_own_sender`).
    UnverifiableDropsTheLink,
    /// A reservation's answer its read back cannot vouch for holds nothing
    /// (`Bridge::post_refused`, critic item F21): the drain goes on to its sender's later
    /// posts, which land ahead of its retry. Its controls are the stray world's seed 6 and the
    /// same bridge unit test.
    UnverifiableHoldsNothing,
    /// The attach's replay publishes a sender's later entries past one whose answer its read
    /// back cannot vouch for (`Bridge::replay_in_flight`'s point 5, critic item F21): a later
    /// post lands ahead of the held one's retry. Its controls are the stray world's seed 115
    /// and a bridge unit test (`the_attachs_replay_holds_an_unverifiable_entrys_sender`).
    ReplayPastAnUnverifiableEntry,
    /// A held lane's post, keyed or not, or owed receipt whose record the attach's replay read
    /// back on the bus waits for the hold (`Bridge::landed_behind_an_unverifiable_hold`, critic
    /// item F21): its record is not delivered, and each delivery that meets it drops the link,
    /// for as long as the hold lasts. No sim world meets it (3,000 seeds of the stray world:
    /// `each_rule_switched_off_is_caught` says why), so its controls are the replay's bridge
    /// unit test and
    /// `a_held_lanes_receipt_or_keyed_post_the_replay_read_back_is_retired_where_it_landed`.
    HeldRecordWaitsForTheHold,
    /// A retry whose words no longer match its pin (`Bridge::unverified_publication`'s case 1)
    /// reads nothing back and publishes its current words at its pin
    /// (`Bridge::reworded_landing`, ATERM-KI-1111-7): past a cut that dropped the dedup key of
    /// a first attempt that landed, retention still holding its record, that attempt is
    /// published a second time. No sim world cuts there, so its control is the bridge unit test
    /// `a_reworded_retry_past_a_cut_reads_its_landed_attempt_back`.
    RewordedWithoutReadingBack,
    /// A retry whose words no longer match its pin reads nothing back and holds its lane, as
    /// every build before ATERM-KI-1111-7 did (`Bridge::reworded_landing`): its post is never
    /// settled while aterm lists it. Its control is every arm of the reworded world
    /// (`a_retry_whose_words_left_its_pin_is_read_back_and_never_held_for_good`).
    HoldAReworded,
    /// An owed receipt's pin keeps no head beside it (`StateDir::receipt_pin`, as before
    /// ATERM-KI-1111-7): its retry reads back only from its record in flight while one stands.
    /// Used with `ReceiptRetiredAtAck`, whose hole a head narrows: a retry at a pin with a head
    /// reads the record back until a cut gives it up
    /// (`a_landed_receipts_entry_stands_until_aterm_retired_it`).
    ReceiptPinKeepsNoHead,
    /// The attach's replay holds a later entry by its record's `from=`, whatever aterm's outbox
    /// lists (`Bridge::replay_in_flight`'s point 5 as F21 first cut it): an entry whose post
    /// already left the outbox, its record given up by retention, is dropped and never
    /// published again. No sim world meets it (as `HeldRecordWaitsForTheHold`), so its control
    /// is a bridge unit test
    /// (`an_entry_whose_post_left_the_outbox_is_published_behind_an_unverifiable_one`).
    HoldPastTheListing,
    /// A keyed re-post is published at its pin with no read back of its key's winner, as every
    /// build before the F23 sim's keyed world (`Bridge::keyed_winner_at`): past a cut that
    /// carried the winner's record below the base, unread, and dropped its dedup key, the
    /// re-post lands a second copy of its key beside the one its reader is still handed.
    KeyedRepostWithoutReadingBack,
    /// A keyed re-post's retry reads back no attempt of its own by its words, as every build
    /// before ATERM-KI-1111-4's lossy keyed seeds (`Bridge::keyed_repost_own_attempt`): an
    /// attempt no replay entry and no claim names (an answer lost on a lane nothing journals, a
    /// verified landing held when its key could not be moved), its record carried past a cut
    /// that dropped its dedup key, is published again as a second copy of its key.
    KeyedRepostReadsNoOwnAttempt,
    /// A key's winner whose record at its key pin's offset retention gave up is not read on past
    /// that cut (`Bridge::keyed_winner_past_a_cut`), as every build before ATERM-KI-1114-858: the
    /// owner's own words published again at its sequence above the cut are not found, and a
    /// re-post under the key is published as a further copy of it.
    KeyedWinnerNotReadPastACut,
    /// Replay by dedup sequence instead of journal publication order: a keyed re-post can
    /// pass an earlier post when both surviving entries are published onto another log. Bridge's
    /// `replay_holds_an_older_sequence_behind_a_refused_publication` holds it on one log too.
    ReplayBySequence,
    /// An in-flight entry journaled on a log before ANOTHER LOG is read back from nowhere on it
    /// (`Bridge::read_back_from`'s `None`), so a copy an earlier replay on this log published,
    /// its settle refused by the disk, is published again once a cut dropped its key
    /// (ATERM-KI-1111-4).
    NoReadBackOnAnotherLog,
    /// `StateDir::forget_log_offsets` takes no pin's head out on another log: a pin's `at` stays
    /// the old log's head (`StateDir::publication_offset_resets` skipped).
    ForgetPinResetsOnAnotherLog,
    /// `Bridge::forget_log_offsets` keeps the old log's offsets below its high water on another
    /// log, as if it continued them.
    KeepFloorOnAnotherLog,
    /// `Bridge::judge_log` reads another log as a move that continues the old one to the lower
    /// of its high water and the new head.
    AnotherLogAsAMove,
    /// The attach's replay lands a keyed post's record anew and leaves its key naming no
    /// winner's offset (`Bridge::land_key_winner` skipped): on another log, or past a move's
    /// fork, the key's next re-post reads no winner back and, past a cut that carried the
    /// record and dropped its dedup key, lands a second copy of its key (ATERM-KI-1111-4).
    ReplayLeavesTheKeysWinnerUntold,
    /// Another log takes every pin's head out (`StateDir::publication_offset_resets`), as every
    /// build before ATERM-KI-1111-4 did: a retry reads nothing back, so an attempt the attach's
    /// replay published there, its answer unknown, is published again once a cut dropped its
    /// dedup key.
    PinHeadGoneOnAnotherLog,
    /// astream's "outcome unknown" refusal (the op whose own full flush poisoned the log) is
    /// judged the broker's answer, the link kept, as every build before ATERM-KI-1111-4's seed
    /// 590 (`Bridge::refusal_answers`): the publish's in-flight entry is dropped as a refused
    /// one's, and once the log is reopened, its record shown and carried below a cut that dropped
    /// its dedup key, the next attach publishes it again with nothing that says to read it back.
    OutcomeUnknownIsAnAnswer,
    /// Another log's drop of the mirror's pin is written under the pin's fence alone, not on the
    /// media (`Bridge::write_dropped_pin`), as every build before ATERM-KI-1111-4's seed 355: a
    /// power cut brings the old log's window ledger back under the new log's witness, and the
    /// window budget holds the mirror on offsets the new log's oldest stays below. Witness:
    /// bridge's `another_logs_drop_of_a_pin_holding_a_window_ledger_is_written_on_the_media` (no
    /// seed of `lossy_logs_light_rebase` to 60,000 since ATERM-KI-1116-7314's told rows).
    DroppedPinUnderTheFence,
    /// The log the self-lane window's `acked` file names is unread, as every build before the
    /// lossy-log world's seed 3483: a load whose bus-log witness a power cut tore knows no log
    /// (`Bridge::new`'s `last_log`), and an attach that judges no log replaced keeps the window
    /// whatever log it was kept on (`Bridge::drop_a_window_of_another_log` skipped). On another
    /// log an own-lane record whose ack is unread sits below the old log's floor, unaccused, and
    /// is delivered while its in-flight entry stands; a kill leaves the entry, which the next
    /// attach publishes again on another log, and the session holds the record twice.
    UnboundWindow,
    /// A key's own post whose pin is rebuilt from its key's (its own pin's write refused after
    /// the key's took) keeps the key pin's missing head, as every build before it did: no retry
    /// of it reads its attempt back, so past a cut that carried that attempt unread and dropped
    /// its dedup key, the retry at the pin lands a second copy of its key.
    OwnersPinFromItsKeyHeadless,
    /// A read back on a log that continues none (`state::Head::Anywhere`) takes a row of the
    /// post's words wherever it stands, without its origin (`Bridge::found_anywhere`), as
    /// ATERM-KI-1111-4's rule read every such log from 0: on a log astream gave another id
    /// keeping its records (a fresh one where its identity sidecar does not read, one derived
    /// from its records where the sidecar is gone), the read back meets the sender's earlier
    /// post of the same words, settles the post on it and never publishes it (critic review
    /// F1, 2026-11-13). Every sim world's posts carry words of their own, so no world meets it,
    /// and its control is a bridge unit test on a real broker whose identity sidecar rots or
    /// goes
    /// (`a_log_reminted_keeping_its_records_never_settles_a_post_on_an_earlier_one_of_the_same_words`).
    ReadBackAnywhereByWords,
    /// A read back on a log that continues none starts at the head the attach read when it met
    /// that log, as every build from critic review F1 to ATERM-KI-1114-1414 started it, not
    /// anywhere: a pin's head goes there (`StateDir::publication_offset_resets`), and every
    /// entry in flight but a reply's is re-witnessed onto that log there, as a keyed re-post's
    /// is (`StateDir::rewitness_in_flight`, `StateDir::heads_at_the_attach`). An attempt
    /// whose record a carried cut kept below that head, its dedup key dropped, on a log astream
    /// reminted keeping its records, goes unread, and the retry or the replay lands a second
    /// copy. Its control is a bridge unit test on a real broker, an entry in flight and a pin
    /// alone (`a_log_reminted_keeping_a_carried_attempt_settles_it_where_it_landed`).
    ReadBackFromTheAttachHead,
    /// A read back on a log that continues none that cannot say whether a row of the post's
    /// words is its own (no origin the broker stamps, no told read below a floor, a page that
    /// does not advance) answers that none landed, and the post is published again at its pin,
    /// not held (`Bridge::found_anywhere`'s case 6 of `Bridge::unverified_publication`).
    /// Witness: the reminted stampless world's seed 334 (`dup`), and bridge's
    /// `an_anywhere_read_whose_origin_the_broker_cannot_say_settles_its_post_at_the_record_it_takes`.
    AnywhereUntoldPublishes,
    /// A read back from anywhere passes a row of the post's words at its sequence under a
    /// producer of no node it knows while the mesh's hand-off, which names the nodes this Mac
    /// retired, does not read (`Bridge::own_producers`): an attempt an older node key of this
    /// Mac's published is taken for another writer's, and the post is published again beside
    /// it.
    AnywherePassesAnUnplacedProducer,
    /// A reply in flight re-witnessed onto a log that continues its own below a fork keeps the
    /// head it had on the log before, as every build before ATERM-KI-1116-2615 kept it, not
    /// brought down to the fork (`StateDir::forget_log_offsets`): a copy the replay published
    /// between the fork and that head goes unread, and the next replay publishes a second one.
    RewitnessKeepsTheOldHead,
    /// A re-base's witness is written as any other witness is, not on the media
    /// (`Bridge::save_log_witness`, `StateDir::rebase_log_witness`), as every build before critic
    /// review F1: a power cut may bring back the witness of the log before it, and the next attach
    /// judges the log replaced again and puts a keyed re-post's pin at a later head, above the
    /// copy the replay published there. No seeded world breaks a law by this alone; its control
    /// is a bridge unit test whose state dir is on a simulated device
    /// (`a_rebase_onto_another_log_keeps_a_keyed_pin_below_the_replays_copy_through_a_power_cut`).
    /// The re-base on the media, written after the attach drops the mirror's pin, puts that drop on
    /// the media too, so [`Mutant::DroppedPinUnderTheFence`] was pinned in a lossy-log world with
    /// this on (the sim's `lossy_logs_light_rebase`) until ATERM-KI-1116-7314's told rows moved
    /// every trajectory; a bridge unit test is its control since.
    RebaseWitnessUnflushed,
    /// A log astream gave another id keeping its records is never proved to continue the one
    /// before (`Bridge::prove_remint`), as every build before ATERM-KI-1116-7314: the attach
    /// forgets every offset, aterm is told a bare `link log`, and the mail it was shown below
    /// the head is offered again as new.
    RemintUnproved,
    /// The told samples are written once aterm answered the line that offers them, not before it
    /// goes (`Bridge::flush_told`): a bridge killed between aterm's answer and the write leaves
    /// samples below a record aterm was shown, a later proof's fork falls below it, and it is
    /// shown again. A fetch's sample the same, written once its lines went (`Bridge::answer_fetch`;
    /// bridge's `a_record_a_fetch_hands_aterm_is_sampled_as_told_before_its_lines_go`).
    ToldAfterTheHand,
    /// The proof takes a record of a sample's subject at its offset without comparing its bytes
    /// (`Bridge::prove_remint`): a log set aside whose new records reach a sample's offset proves
    /// a fork, and aterm answers new mail below it from what it held of the old log.
    ProofByOffset,
    /// The proof takes a sample whose read the broker could not answer (`ReadAt::Unasked`) as no
    /// evidence, not a refusal of the attach (`Bridge::prove_remint`): the newest record told
    /// goes unread, a lower sample proves a fork below it, and what aterm was shown between is
    /// shown again.
    UndecidedSampleSkipped,
    /// A note keeps each subject's newest record told alone (`Bridge::note_told`,
    /// `ToldSamples::note_newest_of_each_subject_only`), as every build before: a power loss cuts
    /// the log below a subject's newest record, astream reminting it, and the proof skips that
    /// row above the new head, so it proves a fork below the older record of the subject aterm
    /// holds, which aterm forgets and is offered again. Witness: the reminted world's seed 184
    /// (`dup delivery`), and bridge's
    /// `a_subjects_older_record_told_proves_a_remint_whose_head_fell_below_its_newest`.
    ToldNewestOfEachSubjectOnly,
    /// A lineage move drops every row of a subject whose newest row lies at or above its fork
    /// (`Bridge::forget_log_offsets`, `ToldSamples::drop_subjects_moved_past`), as every build
    /// before's move dropped that subject: the older record of it aterm holds below the fork is
    /// no row, a later remint proves a fork below it, and aterm is offered it again. No seed of
    /// 1..=1,000 of the reminted, stampless, fetching or keyed reminted worlds breaks a law with
    /// it on that holds every law with every rule on (2026-11-18; each that breaks a law with
    /// every rule on breaks one with it on too); its witness is bridge's
    /// `a_lineage_move_keeps_a_subjects_rows_below_its_fork_where_its_newest_lies_above_it`.
    MovedDropsTheSubject,
    /// The cap on the told rows evicts the lowest row, a subject's newest among them
    /// (`ToldSamples::note_evicting_anchors`): a busy subject's rows take a quiet subject's
    /// only one, which `told-samples` (the file every build before reads) then names no more. No
    /// seeded world tells enough records; its witness is state's
    /// `the_told_rows_keep_each_subjects_newest_past_the_row_cap`.
    ToldRowsEvictASubjectsNewest,
    /// The proof refuses the step at a row that reads another record or none
    /// (`Bridge::prove_remint`), as every build before: a told record a power loss took off the
    /// log's tail, another record (a will) landing at its offset before the attach, leaves aterm
    /// to forget every offset it holds, below the cut too. Witness: the reminted world's seed
    /// 122 (`dup delivery`), and the fetching one's 112.
    ProofRefusedAtAMismatch,
    /// The proof counts a match above a row that reads another record (`Bridge::prove_remint`):
    /// a record published since in a told record's own words at that record's offset keeps the
    /// offsets below it, the replaced one among them, whose new record aterm then never takes.
    /// No seed of 1..=1,000 of the reminted or stampless worlds breaks a law with it on that
    /// holds every law with every rule on (2026-11-18); its witness is bridge's
    /// `a_told_records_copy_above_a_mismatch_proves_nothing_past_it`.
    ProofPastAMismatch,
    /// The proof reads every told row on the link (`Bridge::prove_remint`), as every build
    /// before, not through the grant that reaches it (`Bridge::proof_read`): a row of the lane of
    /// a node this Mac held before a key rotation, or of the fleet a dir served before the mesh,
    /// is one the link's ring does not read, so a guarded broker refuses the read, the link drops
    /// and the attach is refused, at every attach for good. No seed reaches it: the sim's ring is
    /// empty and its node never renamed, so every row is in reach. Its witness is bridge's
    /// `a_told_row_of_a_node_before_a_key_rotation_is_read_through_the_grant_that_reaches_it`.
    ProofReadsEveryRowOnTheLink,
    /// A told row no grant this bridge holds reads stops the proof (`Bridge::prove_remint`), not
    /// passed over as a cut one is: after a key rotation the old node's rows are the lowest, so a
    /// row of the new node's above them proves nothing and aterm forgets what it holds. No seed
    /// reaches it: the sim's ring is empty and its node never renamed, so every row is in reach.
    /// Its witness is bridge's
    /// `a_told_row_of_a_node_before_a_key_rotation_is_read_through_the_grant_that_reaches_it`.
    UnreachableRowStopsTheProof,
    /// A told row read under the node service's reader's ring that holds no record of its
    /// subject at its offset is no evidence (`Bridge::proof_read`), as `Bridge::held_record`
    /// answers it `None`, not a mismatch that stops the walk: a log set aside whose new records
    /// hold another subject at an old node's row proves a fork past it from a row above it. No
    /// seed reaches it: the sim's ring is empty and its node never renamed, so every row is read
    /// on the link. Its witness is bridge's
    /// `a_told_row_of_a_node_before_a_key_rotation_is_read_through_the_grant_that_reaches_it`.
    HeldMismatchIsNoEvidence,
    /// A told row's read under the node service's reader's ring that the broker refuses for good
    /// (`ERR_UNAUTHORIZED`, `ERR_BAD_FILTER`: `bridge::read_refused_for_good` answers false) is
    /// an error that refuses the attach (`Bridge::proof_read`), not no evidence: a reader's ring
    /// minted under a secret the root no longer holds is refused at its attach, and every attach
    /// meets the row again, so the attach is refused for good. No seed reaches it: the sim's ring
    /// is empty and its node never renamed, so every row is read on the link. Its witness is
    /// bridge's
    /// `a_told_row_of_a_node_before_a_key_rotation_is_read_through_the_grant_that_reaches_it`.
    ReaderRefusalRefusesTheAttach,
    /// A bridge starts from `told-samples` alone (`Bridge::told_at_start`, which `Bridge::new`
    /// reads its rows through, `StateDir::told_samples_alone`), as every build before read it: a
    /// row the anchors stopped naming in an earlier run (its subject told again since) proves
    /// nothing, and a remint whose head fell below the newer record proves a fork below it.
    /// Witness: the reminted world's seed 184 (`dup delivery`), and bridge's
    /// `a_subjects_older_record_told_proves_a_remint_whose_head_fell_below_its_newest`.
    ToldHistoryUnread,
    /// The told rows' two files are written the anchors first (`StateDir::set_told_samples`):
    /// a refused or killed second write of a move's flush leaves `told-samples` of the move beside
    /// `told-history` of the log before it, one move behind them, which no read accepts, so the
    /// rows below the fork that are no anchor are on no file read. No seed of 1..=1,000 of the
    /// reminted or stampless worlds breaks a law with it on that holds every law with every rule
    /// on (2026-11-18); its witness is state's
    /// `a_refused_second_told_write_keeps_every_row_on_the_disk`.
    ToldHistoryWrittenLast,
    /// `told-history` is read beside `told-samples` whatever log and move it names
    /// (`StateDir::told_samples`): a history a reset or a build before left of another log
    /// passes its rows for records of this one. Its witness is state's
    /// `a_told_history_of_another_log_is_never_read_beside_the_samples`.
    ToldHistoryOfAnotherHeader,
    /// `told-history` is read beside `told-samples` only under the same log and move
    /// (`StateDir::told_samples`, `ToldSamples::union_of_the_same_header`), as the first cut of
    /// ATERM-KI-1116-7314's told rows read it: a move's flush whose samples' write is refused or
    /// killed after the history's leaves the history one move ahead of the samples, its rows
    /// below the fork on the disk under a header no read accepts, and the move met again keeps
    /// the anchors alone. No seed of 1..=1,000 of the reminted or stampless worlds breaks a law
    /// with it on that holds every law with every rule on (2026-11-18); its witness is state's
    /// `a_refused_second_told_write_keeps_every_row_on_the_disk`.
    ToldHistoryOfTheMoveAheadUnread,
    /// `told-history` holds the rows that are no anchor alone (`StateDir::set_told_samples`,
    /// `ToldSamples::render_history_without_the_anchors`), as the first cut of
    /// ATERM-KI-1116-7314's told rows wrote it: a move makes a row below its fork an anchor that
    /// the samples on the disk do not name, so a refused or killed samples' write after the
    /// history's leaves it on neither file. No seed of 1..=1,000 of the reminted or stampless
    /// worlds breaks a law with it on that holds every law with every rule on (2026-11-18); its
    /// witness is state's `a_refused_second_told_write_keeps_every_row_on_the_disk`.
    ToldHistoryWithoutTheAnchors,
    /// A proved step is not read where astream's lineage would be (`Bridge::read_back_from`,
    /// `StateDir::forget_log_offsets_proved`): a reply in flight kept on the log before is
    /// dropped as decided there, and a head kept there reads back from anywhere.
    ProvedStepUnread,
    /// The proof runs for an endpoint below `LINK_PROVED_LANE` (`Bridge::prove_remint`): it is
    /// told `proved=1`, answers usage, and every attach is refused.
    ProvedToAnOldEndpoint,
    /// A head above a proved fork comes down to it, as on a move astream's lineage names
    /// (`Bridge::read_back_from`, `StateDir::forget_log_offsets_proved`): a remint kept the
    /// records past the fork, so a read from there by words alone takes a sender's earlier post
    /// of the same words for this one's attempt, and the post is never published.
    ProvedHeadToTheFork,
    /// A keyed re-post's head above a proved fork goes where a head on a log that continues none
    /// goes, as every build before ATERM-KI-1114-1414's own head took it, not to its own head
    /// (`state::Head::OwnFrom`): a pin's to the head the attach read
    /// (`StateDir::publication_offset_resets`), an entry's in flight re-witnessed onto the proved
    /// log there (`StateDir::rewitness_in_flight`), a keyed reply's in flight anywhere.
    /// Its own attempt, which a carried cut kept below that head, its dedup key dropped, goes
    /// unread: the re-post is retired `dup=1` at its key's winner, or published again, the key's
    /// second record. Its controls are bridge unit tests on a proved remint
    /// (`a_keyed_reposts_own_attempt_above_its_own_head_settles_there_across_a_proved_step`) and
    /// a state test of the seam
    /// (`a_head_above_a_proved_fork_reads_as_on_a_log_that_continues_none`).
    ProvedOwnHeadAtTheAttach,
    /// A keyed re-post's head above a proved fork that is not its own (a plain `From`) reads from
    /// the fork by its words alone (`Bridge::read_back_from`), as every build before
    /// ATERM-KI-1114-1414's step S6 read it from the node's meeting with the log's line, there
    /// the fork, not from itself by its words and its origin (`state::Head::Since`): the remint
    /// kept the records past the fork at their offsets and the proved log's claims start empty,
    /// so a sender's earlier post of the same words at another sequence, between the fork and
    /// the head, is taken for the entry's attempt, the post settled there and never published
    /// (C2 of the step's map). Its controls are bridge unit tests on a proved remint
    /// (`a_keyed_plain_head_above_a_proved_fork_never_settles_on_another_sequences_record`,
    /// `a_keyed_plain_head_above_a_proved_fork_settles_at_its_own_attempt_above_another_sequences_record`).
    KeyedFromHeadToTheFork,
    /// A keyed re-post's entry in flight on a log the attach's log continues none of stays on
    /// that log (`StateDir::rewitness_in_flight`), as every build before
    /// ATERM-KI-1114-1414's step S6 kept it, not re-witnessed onto the attach's log at the head
    /// the attach read (`StateDir::rewitnessed_entry`): with no meeting to start from, its read
    /// back reads nothing, and a copy the replay published there, its answer lost and its dedup
    /// key cut, is published again, the key's second record. Its control is a bridge unit test
    /// (`a_keyed_entry_kept_on_another_log_is_read_back_where_the_attach_met_this_one`).
    KeyedEntryKeptOnItsLog,
    /// The meeting an older build's witness names (`state::LogWitness::older_meeting`) is not
    /// read at the first attach (`Bridge::rewitness_at_an_older_meeting`): a keyed re-post's
    /// entry that build kept on another log stays there, the attach's pass puts it at the head
    /// the attach read (`Bridge::rewitness_in_flight`), above a copy that build's replay
    /// published above the meeting, and with that copy's dedup key cut it is published again,
    /// the key's second record. Its control is a bridge unit test on an older build's witness,
    /// through the attach (`an_older_builds_meeting_re_witnesses_its_keyed_entry_once`).
    OlderMeetingUnread,
    /// A keyed re-post's entry kept on another log is re-witnessed onto the log an attach read
    /// where the attach forgets the log before (`Bridge::forget_log_offsets`), before its
    /// witness moves, as the first build of ATERM-KI-1114-1414's step S6 made it, not once the
    /// witness names that log (`Bridge::rewitness_in_flight`): an attach refused after it leaves
    /// the entry on the media on a log the witness does not name, kept there only by its own log
    /// (`state::InFlight::own_log`), which the next attach, back on that log, puts it back on;
    /// with `OwnLogForgotten` too (`Fx::os_without_each`) that attach reads it from that log's
    /// head, above the attempt a carried cut kept, and publishes the key's second record. Its
    /// control is a bridge unit test of a refused attach, which reads the journal that attach
    /// leaves and, with both rules off, the face
    /// (`a_refused_attach_leaves_a_keyed_entry_on_its_own_log`).
    RewitnessBeforeTheWitness,
    /// An attach that judged no log replaced re-witnesses no keyed re-post's entry kept on another
    /// log (`Bridge::rewitness_in_flight`), as the forget that re-witnesses a reply's runs only at
    /// a re-base (`StateDir::forget_log_offsets_proved`), not at every attach: an entry kept
    /// there where no witness named a log, or one a build from before critic review F1 kept, its
    /// witness naming this log and no meeting (where every build from F1 met the log at its first
    /// attach), stays, its read back reads nothing, and a copy the replay published, its answer
    /// lost and its dedup key cut, is published again, the key's second record. Its control is a
    /// bridge unit test of both
    /// (`an_attach_re_witnesses_a_keyed_entry_kept_on_another_log_at_its_head`).
    RewitnessOnlyWhereReplaced,
    /// A proved step stays on the generation it was proved on (`Bridge::carry_proved_steps`),
    /// not carried onto each later generation an attach reads (`state::Proved::carried`): once
    /// astream's lineage window passes that generation no step reaches the log, and a keyed
    /// re-post's entry kept on the log before the step is re-witnessed at the attach's head,
    /// above a copy its replay published past the step. Its control is a bridge unit test of
    /// lineage moves past the window
    /// (`a_proved_step_follows_its_log_past_the_lineage_window`, and through the attach,
    /// `an_attach_carries_a_proved_step_onto_a_moved_generation_on_the_media`).
    ProvedStepLeftOnItsGeneration,
    /// A proved step carried onto a later generation keeps its fork (`state::Proved::carried`),
    /// as the second build of ATERM-KI-1114-1414's step S6 carried it, its head brought down
    /// below the fork where the generation continues the step's log below it, which the media
    /// holds as the fork (`state::Proved::render_head`): after a restart, or the next proof
    /// re-reading the steps, the step reaches its log down to the fork again, and a keyed
    /// re-post's own head between the move's fork and the step's stands above records published
    /// since the move. Its controls are a state unit test that reopens the state dir
    /// (`a_proved_step_is_carried_onto_each_later_generation_its_logs_lineage_continues`) and a
    /// bridge unit test through the attach
    /// (`an_attach_carries_a_proved_step_onto_a_moved_generation_on_the_media`).
    CarriedStepKeepsItsFork,
    /// A keyed re-post's entry an attach moves onto a log that continues none of its own keeps
    /// no own log (`state::InFlight::own_log`, `StateDir::rewitnessed_entry`, the seam
    /// `StateDir::own_logs_forgotten`), as the second build of ATERM-KI-1114-1414's step S6
    /// moved it: an attach refused or killed after the move, or whose replay's answer was lost,
    /// then an attach back on the entry's own log puts it at that log's head, above the attempt
    /// a carried cut kept there, its dedup key dropped, and it is published again, the key's
    /// second record; and an entry an older build's meeting moved
    /// (`StateDir::rewitness_at_an_older_meeting`), its own head above a stored proved step's
    /// fork, stays at the meeting's head, below that fork, where its read back by its words alone
    /// takes an earlier post's record of its words at another sequence for its attempt, the post
    /// settled there and never published. Its controls are a bridge unit test of each schedule
    /// (`a_keyed_entry_moved_off_its_own_log_goes_back_to_its_own_head_there`), one on an older
    /// build's witness across a proved remint
    /// (`an_older_builds_meeting_leaves_a_keyed_entry_a_proved_step_reaches`), and a state unit
    /// test (`an_entry_moved_off_its_own_log_goes_back_where_a_log_reaches_it`).
    OwnLogForgotten,
    /// A keyed re-post's entry an attach puts back on its own log (`StateDir::rewitnessed_entry`)
    /// goes back alone: its post's reservation keeps the head the attach's forget put it at
    /// (`StateDir::publication_offset_resets`), as the third build of ATERM-KI-1114-1414's step
    /// S6 left it, not the head its entry's pin stands at there (`StateDir::own_pins_back`). After
    /// an attach killed once its pass moved the entry onto a log that continues none of its own,
    /// the attach back to its own log, judged replaced, puts the reservation at that log's head,
    /// above the attempt a carried cut kept there, its dedup key dropped: the replay settles the
    /// entry at the attempt from its own head, and the drain's retry of the post aterm still lists
    /// reads from the reservation's head, finds none, and publishes the key's second record. Its
    /// control is a bridge unit test's `killed` schedule alone, aterm listing the post
    /// (`a_keyed_entry_moved_off_its_own_log_goes_back_to_its_own_head_there`, its `pin-left`
    /// arm): on its `lost` and `restart` schedules the retry settles with the rule off too.
    OwnLogPinLeft,
    /// An older build's meeting moves only the keyed entries the log the attach reads continues
    /// by neither astream's lineage nor a stored proved step
    /// (`StateDir::rewitness_at_an_older_meeting`), as the second build of ATERM-KI-1114-1414's
    /// step S6 judged them, against a log the attach may yet be refused before its witness names,
    /// not by the witness alone: the meeting is used up all the same, and an entry the first
    /// attach left on the log it read, refused before its witness moved, is put at the head the
    /// next attach read on the witnessed log, above a copy that build's replay published above the
    /// meeting, its dedup key cut, and published again, the key's second record. Its control is a
    /// bridge unit test of a refused first attach
    /// (`an_older_builds_meeting_is_judged_by_its_witness_not_by_a_refused_attachs_log`).
    OlderMeetingJudgedOnTheAttachsLog,
    /// A keyed re-post on a proved log whose own head is known (`state::Head::OwnFrom`) tells its
    /// own attempt from its key's earlier records by order, as where no own head is known
    /// (`Bridge::keyed_winner_on_a_proved_log`), not by where each stands
    /// (`Bridge::keyed_records_split_at`): in its winner's own words, a newer record of them
    /// than the winner's, an earlier re-post's below its own head while its own attempt never
    /// landed, is taken for its own attempt, and an ask's deadline is armed on it.
    KeyedRecordsSplitByOrder,
    /// The drain makes a keyed re-post's pin, and journals its entry, at a plain `From` head, as
    /// a build before the own head's mark wrote it (`Bridge::drain_post`,
    /// `Bridge::publish_journaled_entry`), not at its own head (`state::Head::OwnFrom`): on a
    /// proved log its key's records are told by order (`Bridge::keyed_winner_on_a_proved_log`),
    /// so a re-post pinned on the proved log since the step, its first drain cut short, takes an
    /// earlier re-post's record of its winner's words below its head for its own attempt, and an
    /// ask's deadline is armed on it.
    KeyedPinHeadUnmarked,
    /// A keyed re-post's plain `From` head reads as its own (`state::Head::own`), as the own
    /// head's second cut read every keyed `From` (`StateDir::publication_offset_resets`,
    /// `Bridge::read_back_from`, `Bridge::keyed_winner_on_a_proved_log`): a head an older build
    /// wrote at the head an attach read, where the log continued none, becomes
    /// `state::Head::OwnFrom` across a later proved step whose fork lies below it, though it
    /// bounds none of the post's attempts, and the split there takes its own attempt below it, a
    /// carried cut's, for its key's winner's side: the post is retired `dup=1`.
    PlainFromHeadReadAsOwn,
    /// A keyed re-post's own head across a proved step is not brought down to where astream's
    /// lineage moved the step's log since (`Bridge::read_back_from`): past that fork the
    /// offsets name records published since the move, and an attempt the replay published there,
    /// below the head, its answer lost and its dedup key cut, goes unread: the entry is
    /// published again, the key's second record.
    OwnHeadPastALineageFork,
    /// A keyed re-post's own head (`state::Head::OwnFrom`) is read from anywhere on a broker
    /// that does not tell fetches (`Bridge::found_since`), as `state::Head::Anywhere` is: a read
    /// below a face's oldest that retention cut is refused there whatever it still holds, so the
    /// read answers case 6 of `Bridge::unverified_publication` before the key's winner is read,
    /// which skips such a broker, and the keyed sender is held for good.
    OwnHeadReadAnywhereUntold,
    /// A keyed re-post's own head (`state::Head::OwnFrom`) is read from anywhere on a broker
    /// that tells fetches but takes no ask for origins (`Bridge::found_since`,
    /// `Bridge::read_anywhere`'s `AnywhereRead::Undecided` with `no_origins`), as
    /// `state::Head::Anywhere` is: no row of its words names an origin there, so the read
    /// answers case 6 of
    /// `Bridge::unverified_publication` at every retry, and the keyed sender is held for good.
    OwnHeadReadAnywhereUnstamped,
    /// A keyed re-post's own head (`state::Head::OwnFrom`) across a proved step is not brought
    /// down to the head the step was proved at (`state::Proved::head`;
    /// `StateDir::publication_offset_resets`, `Bridge::read_back_from`): on a log a power loss
    /// cut short before astream reminted it, that head lies below the one the node knew on the
    /// log before, and records published since stand between, the re-post's own attempt among
    /// them, which its read from the own head misses: it is retired `dup=1` at its key's winner,
    /// or published again.
    OwnHeadAboveAShorterLog,
    /// A key's winner whose offset a proved step took out of the key pin is not read on the log
    /// the step reaches (`Bridge::keyed_winner_on_a_proved_log`): a cut that carried it dropped
    /// its dedup key, so astream's dedup does not answer the re-post with it, and the re-post
    /// lands the key's second record.
    KeyedWinnerUnreadOnAProvedLog,
    /// A key's winner on a proved log is read from anywhere on a broker that does not tell
    /// fetches (`Bridge::keyed_winner_on_a_proved_log`), where a read from 0 of a face retention
    /// cut never decides and the re-post's sender is held for good.
    KeyedWinnerReadUntold,
    /// The read of a key's records on a proved log answers the first record of the winner's
    /// words (`Bridge::keyed_winner_on_a_proved_log`), though the re-post's own later attempt
    /// is the newest record astream's dedup answers.
    KeyedWinnerFirstRecordAnswered,
    /// A key's winner read with no offset of it lands with none (`Bridge::drain_post`): an ask
    /// or task in its winner's words is left queued, its sender held at every retry.
    WinnerLandedWithoutItsOffset,
    /// The attach's replay gives the attach up where no read decides which record a keyed
    /// entry's key's dedup answers (`Bridge::replay_in_flight`), at every try while it stays so.
    ReplayRefusedAtAnUndecidedWinner,
    /// The attach's replay publishes a keyed entry at its pin though the read of its key's
    /// records on a proved log found its own attempt there (`Bridge::replay_in_flight`): past a
    /// cut that dropped that attempt's dedup key, the key's next record lands beside it.
    ReplayPublishesPastItsReadAttempt,
    /// A keyed re-post in its winner's own words that attempted is answered the winner at the
    /// newest record of those words on a proved log (`Bridge::keyed_winner_on_a_proved_log`),
    /// though the oldest is the winner's and the newer its own attempt: an ask is retired
    /// `dup=1` there, its recipient and deadline never noted.
    SameWordsAttemptTakenForTheWinner,
    /// A stored step is not taken for the step (`Bridge::prove_remint`): an attach killed after
    /// its tell, its witness still on the log before, proves again from samples that lag what
    /// the first read and tells a fork below the one aterm keeps.
    ProvedStepRecomputed,
    /// A connection asks for origins only with the mesh on (`Bridge::connect`), as every build
    /// before ATERM-KI-1114-1414's delivery gate: a delivery names no origin, so where no read
    /// from anywhere ran whole the gate takes a sender's earlier post of the same words for the
    /// attempt (`Bridge::take_undecided`). No sim seed (none of the reminted world to 1,000 that
    /// holds every law with every rule on); its witness is bridge's
    /// `a_link_with_the_mesh_off_asks_for_origins`.
    LinkAsksNoOrigins,
    /// A read from anywhere that cannot decide names none of the candidates it read
    /// (`Bridge::read_anywhere`'s `seen`), as every build before the delivery gate named none: the
    /// delivery takes the first record of the words for the attempt, a sender's earlier post of the
    /// same words among them. No sim seed of the reminted stampless world to 1,000 breaks a law
    /// with it off that holds with every rule on (2026-11-18: 424, 654 and 914, which break
    /// `dup post` with every rule on, break a law with it off too); its witness is bridge's
    /// `an_anywhere_read_whose_origin_the_broker_cannot_say_settles_its_post_at_the_record_it_takes`.
    UndecidedSeenUncollected,
    /// No delivery takes a record of an attempt no read from anywhere decided
    /// (`Bridge::take_undecided`), as every build before the delivery gate: the record is handed
    /// while its post stands at its pin, and once a cut or a log set aside gave it up the retry
    /// reads nothing back and publishes it again, which the reader is handed too. Witness: the
    /// reminted stampless world's seed 6 (`dup`), and bridge's
    /// `a_case_six_hold_hands_its_post_once_across_the_cut_that_gives_its_records_up` beside
    /// `Mutant::UndecidedInHandUnseen`, whose take of the live tail's queued copy masks it there.
    UndecidedHandedUnheld,
    /// The delivery gate reads no origin (`undecided_takes`): where no read ran whole, a sender's
    /// earlier post of the same words, of another sequence, is taken for the attempt. No sim
    /// seed (none of the reminted world to 1,000 that holds every law with every rule on); its
    /// witnesses are bridge's
    /// `the_delivery_gate_takes_an_undecided_attempts_record_by_its_origin_else_by_the_newest_seen`
    /// and `the_delivery_gates_call_site_holds_each_of_its_rules`.
    UndecidedTakesPastItsOrigin,
    /// The delivery gate takes the first record of the attempt's words (`undecided_takes`), not
    /// the newest the read saw: a sender's earlier post of the same words is taken for the
    /// attempt. No sim seed (none of the reminted stampless world to 1,000 that holds every law
    /// with every rule on); its witnesses are bridge's
    /// `the_delivery_gate_takes_an_undecided_attempts_record_by_its_origin_else_by_the_newest_seen`
    /// and `the_delivery_gates_call_site_holds_each_of_its_rules`.
    UndecidedTakesTheFirst,
    /// A keyed re-post's read from its own head, or from a keyed head not its own above a proved
    /// fork, that cannot decide arms nothing (`Bridge::arm_undecided_keyed`), as every build before
    /// ATERM-KI-1114-1414's keyed hold: its record is handed while the post is held, and once a
    /// cut gave it up the retry reads nothing back and publishes the key's second record, which
    /// the reader is handed too. No sim seed (the sim's bridges run with the mesh off, so every
    /// producer is placed, and every row a stamping broker delivers names its origin; with it off
    /// the keyed reminted world to 1,000 breaks the laws it breaks with every rule on, at the same
    /// seeds and offsets, 2026-11-18); its witness is bridge's
    /// `a_keyed_case_six_hold_hands_its_attempt_once_across_the_cut_that_gives_it_up`.
    KeyedHoldUnarmed,
    /// The delivery gate reads no keyed arm's floor (`undecided_takes`): an earlier post of the
    /// key in the same words below the re-post's own head, the key's own post's record claimed at
    /// the key's sequence among them, is taken for the re-post's attempt. No sim seed (as
    /// `Mutant::KeyedHoldUnarmed`; the keyed reminted world to 128 with it off, 2026-11-18); its
    /// witnesses are bridge's
    /// `the_delivery_gate_takes_an_undecided_attempts_record_by_its_origin_else_by_the_newest_seen`
    /// and `the_delivery_gates_call_site_holds_each_of_its_rules`.
    UndecidedTakesBelowItsFloor,
    /// A record a keyed arm took for a re-post in its winner's own words, whose pin names no
    /// winner's offset, is not known as its own (`Bridge::keyed_own_copy`): the retry reads it
    /// again, undecided, and holds its sender and its recipient's lane for as long as the record
    /// stands. No sim seed (as `Mutant::KeyedHoldUnarmed`; the keyed reminted world to 128 with it
    /// off, 2026-11-18); its witness is bridge's
    /// `a_keyed_record_taken_in_its_winners_words_settles_its_retry_there`.
    KeyedTakenUnowned,
    /// A keyed arm armed again by a read of its words keeps the lower of the two floors
    /// (`Bridge::arm_undecided_keyed`), not the higher: a record of those words between the two,
    /// an earlier post's claimed at the key's sequence among them, is taken for the later read's
    /// attempt. No sim seed (as `Mutant::KeyedHoldUnarmed`; the keyed reminted world to 128 with
    /// it off, 2026-11-18); its witness is bridge's
    /// `one_arm_a_sequence_stands_between_two_posts_and_a_keyed_re_arm_keeps_its_higher_floor`.
    KeyedArmKeepsTheLowerFloor,
    /// One arm a sequence goes unkept between the two posts that can hold one sequence at once, a
    /// key's own post and a re-post of its key (`Bridge::one_arm_a_sequence_unkept`): a read that
    /// decides takes out the other post's arm (`Bridge::disarm`), a plain read writes its
    /// candidates over a keyed arm (`Bridge::arm_undecided`), a keyed read writes over a plain arm
    /// and merges into a keyed arm with no want in common (`Bridge::arm_undecided_keyed`), a
    /// keyed arm takes a record while its sequence's record awaits the drain, moving that hold
    /// (`Bridge::undecided_candidate`), and a post that left aterm's outbox takes any arm at its
    /// sequence, whatever its kind and words (`Bridge::land_group`): a re-post's landing takes
    /// its key's own post's plain arm, and a landing an arm of other words. No sim seed (as
    /// `Mutant::KeyedHoldUnarmed`; the keyed reminted world to 128 with it off, 2026-11-18,
    /// before the landing's rule joined it: that rule acts only on a standing arm, and that
    /// world's broker stamps every open, its producers placed, so no read there arms one, read,
    /// not run); its witness is bridge's
    /// `one_arm_a_sequence_stands_between_two_posts_and_a_keyed_re_arm_keeps_its_higher_floor`,
    /// its landing rows for the landing's rule.
    OneArmASequenceUnkept,
    /// A post that left aterm's outbox while an arm of its words held its sequence leaves that
    /// arm, and the entry the read that armed it read, standing (`Bridge::land_group`): its retry
    /// settled by another read (a keyed re-post its key made its own, read by its words), the
    /// refill's delivery of its record is taken for a post that is gone, held in hand and not
    /// handed, and a fetch of it answers `unsettled`, until a later drain's listing names the post
    /// nowhere (`Bridge::release_replays`). No sim seed (as `Mutant::KeyedHoldUnarmed`; the keyed
    /// reminted world to 128 with it off, 2026-11-18); its witness is bridge's
    /// `a_keyed_arm_goes_with_its_post_whatever_read_settled_it`.
    ArmOutlivesItsPost,
    /// The attach reads no pin whose head is anywhere before its refill
    /// (`Bridge::arm_undecided_pins`): only the drain's retry arms it, after the refill handed
    /// its record, and a cut that then gave it up leaves the retry nothing to read back, so it
    /// publishes again. No sim seed (none of the reminted stampless world to 3,000 that holds
    /// every law with every rule on); its witness is bridge's
    /// `a_case_six_hold_hands_its_post_once_across_the_cut_that_gives_its_records_up`, the arm
    /// whose post has no in-flight entry, beside `Mutant::UndecidedInHandUnseen`, which masks it
    /// there.
    UndecidedArmedByTheDrain,
    /// A fetch hands a record a delivery would take for an undecided attempt's drain
    /// (`Bridge::fetch_record`): the session reads it while its post stands at its pin, and past
    /// a cut the retry publishes it again. No sim seed (none of the reminted stampless world with
    /// fetches to 3,000 that holds every law with every rule on); its witness is bridge's
    /// `a_fetch_holds_back_a_record_a_delivery_would_take_for_an_undecided_attempt`.
    UndecidedFetched,
    /// A read from anywhere no origin can decide on a broker that takes no ask for origins, of a
    /// lane no delivery of this bridge's reaches (a person's, a peer node's), holds the post as
    /// on a broker that stamps (`Bridge::found_anywhere`): no read ever decides, and it waits
    /// for good. Witness: the reminted stampless world's seed 53 (`liveness`: a post to
    /// `h-alice` retried at every back-off and never retired).
    UnstampedUnreachableHolds,
    /// A fetch notes no told sample of the record it hands aterm (`Bridge::fetch_record`), as
    /// every build before it: a later proof's fork falls below it, and it is shown again.
    /// Witness: the reminted stampless world with fetches, seed 2476 (`dup delivery`), and bridge's
    /// `a_record_a_fetch_hands_aterm_is_sampled_as_told_before_its_lines_go`.
    FetchUntold,
    /// One owed receipt's reservation that does not read refuses the attach
    /// (`Bridge::arm_undecided_pins`), as the first cut of ATERM-KI-1114-1414's case-6 gate did: no
    /// session of the node is delivered anything until a person moves the file. No sim seed of the
    /// reminted worlds (stamping, stampless, stampless with fetches) to 128 breaks a law with it
    /// off that holds with every rule on (2026-11-17: the world with fetches breaks one at seed 21
    /// with every rule on, the same with it off); its witness is bridge's
    /// `an_owed_receipts_reservation_that_does_not_read_arms_nothing_and_the_attach_goes_on`.
    UnreadReceiptPinRefusesTheAttach,
    /// A record of an undecided attempt held in hand behind another own record of its lane is not
    /// taken for its drain as it is held (`Bridge::deliver_record`). On one connection the retry
    /// takes it all the same (`Bridge::take_undecided_in_hand`, which masks this rule there);
    /// across a link drop the attach empties every arm, its read after a cut that gave the record
    /// up finds none and arms nothing, and the retry publishes again, the record handed beside the
    /// new copy at its release. No sim seed of the reminted worlds (stamping, stampless, stampless
    /// with fetches) to 128 breaks a law with it off that holds with every rule on (2026-11-17: the
    /// world with fetches breaks one at seed 21 with every rule on, the same with it off); its
    /// witness is bridge's
    /// `an_undecided_attempts_record_held_behind_another_is_taken_as_it_is_held`.
    UndecidedHeldBehindUntaken,
    /// The retry of an undecided attempt looks for no record of it this bridge holds untaken, in
    /// hand or queued in the mailbox (`Bridge::take_undecided_in_hand`): past a cut that gave that
    /// record up its read finds none, and it is published again beside it. No sim seed of the
    /// reminted worlds (stamping, stampless, stampless with fetches) to 128 breaks a law with it
    /// off that holds with every rule on (2026-11-17: the world with fetches breaks one at seed 21
    /// with every rule on, the same with it off); its witness is bridge's
    /// `an_undecided_attempts_record_this_bridge_holds_untaken_is_taken_before_its_retry_reads`.
    UndecidedInHandUnseen,
    /// The attach's refill holds a lane's mail behind a record held in hand to the cap with no room
    /// made (`Bridge::make_room_in_hand`): the link drops at the cap, nothing releases the lane
    /// inside the next attach, whose refill drops it at the same record, and the node never
    /// attaches. No sim seed of the reminted worlds (stamping, stampless, stampless with fetches)
    /// to 128 breaks a law with it off that holds with every rule on (2026-11-17: the world with
    /// fetches breaks one at seed 21 with every rule on, the same with it off); its witness is
    /// bridge's
    /// `a_refill_that_takes_a_record_with_more_mail_behind_it_than_the_hand_holds_attaches`, at the
    /// shared read's call site (`Bridge::refill_node`) and a lane's own (`Bridge::refill_lane`).
    RefillHoldsPastTheHand,
    /// A fetch's lines go whatever the write of the told samples answered (`Bridge::answer_fetch`):
    /// a record is shown that no sample on the disk names, so a later proof's fork falls below it
    /// and it is shown again. No sim seed of the reminted worlds (stamping, stampless, stampless
    /// with fetches) to 128 breaks a law with it off that holds with every rule on (2026-11-17: the
    /// world with fetches breaks one at seed 21 with every rule on, the same with it off); its
    /// witness is bridge's `a_fetch_whose_told_sample_the_disk_refuses_answers_unsettled`.
    FetchPastARefusedToldWrite,
    /// A read from anywhere that meets a candidate no origin decides and then a hit answers
    /// undecided (`Bridge::read_anywhere`): the post is held, or settled through the delivery gate
    /// at that candidate, not at its own record. No sim seed of the reminted worlds (stamping,
    /// stampless, stampless with fetches) to 128 breaks a law with it off that holds with every
    /// rule on (2026-11-17: the world with fetches breaks one at seed 21 with every rule on, the
    /// same with it off); its witness is bridge's
    /// `a_read_from_anywhere_settles_at_a_hit_past_a_candidate_it_cannot_decide`.
    UndecidedOverALaterHit,
    /// A record held in hand for its own drain is refused at the cap as mail held behind one is
    /// (`Bridge::hold_record`): with the hand full of other lanes' mail, the link drops at the
    /// record whose drain would free it, at every attach, and the node never attaches. No sim seed
    /// of the reminted worlds (stamping, stampless, stampless with fetches) to 128 breaks a law
    /// with it off that holds with every rule on (2026-11-17: the world with fetches breaks one at
    /// seed 21 with every rule on, the same with it off); its witness is bridge's
    /// `a_record_held_for_its_drain_first_on_its_lane_is_held_past_a_full_hand`, through the shared
    /// refill, a lane's own and the live tail.
    HeldForItsDrainRefusedAtTheCap,
    /// An owed receipt of a session one of whose earlier receipts stalled the drain waits, though
    /// its record is held in hand for its retry (`Bridge::held_receipt_landed`): behind an earlier
    /// receipt whose reservation stays unread, its record, taken for its drain, holds the asker's
    /// lane in hand for good. No sim seed of the reminted worlds (stamping, stampless, stampless
    /// with fetches) to 128 breaks a law with it off that holds with every rule on (2026-11-17: the
    /// world with fetches breaks one at seed 21 with every rule on, the same with it off); its
    /// witness is bridge's
    /// `a_receipt_held_in_hand_behind_its_sessions_stalled_receipt_settles_there`.
    StalledReceiptWaitsInHand,
    /// No own receipt record handed while a reservation of its session's is unread is noted
    /// (`Bridge::note_handed_unread`): once the file reads, the retry, its attempt's record handed
    /// and then cut, reads nothing back and publishes it again. No sim seed of the reminted worlds
    /// (stamping, stampless, stampless with fetches) to 128 breaks a law with it off that holds
    /// with every rule on (2026-11-17: the world with fetches breaks one at seed 21 with every rule
    /// on, the same with it off); its witness is bridge's
    /// `an_owed_receipt_unread_at_the_attach_settles_at_the_record_handed_meanwhile_once_it_reads`.
    UnreadReceiptHandedUnnoted,
    /// An own receipt record a fetch reads while a reservation of its session's is unread is noted
    /// as handed before the fetch's refusals (`Bridge::fetch_record`), as the merge of
    /// ATERM-KI-1114-1414's case 6 with ATERM-KI-1115-7889 left it, not once aterm handed every
    /// line of the answer to the read that waits for it (`Bridge::answer_fetch`): a fetch refused
    /// (`via`, or the told write the disk refused), or one no read waits for (`OK unrequested=1`:
    /// aterm keeps nothing of it, or caches a whole answer for a later read), leaves the
    /// reservation's first read to settle the retry at a record aterm was never handed, and past a
    /// cut that gave it up the receipt is lost. No sim seed of the reminted stampless world, with
    /// fetches or without, to 128 breaks a law with it off that holds with every rule on
    /// (2026-11-17: with fetches seed 21 breaks one with every rule on, the same with it off;
    /// without, none breaks one); its witness is bridge's
    /// `a_receipt_record_a_fetch_did_not_hand_settles_no_retry_once_its_reservation_reads`, its
    /// `via`, `told` and `unrequested` arms.
    UnreadReceiptNotedAtARefusedFetch,
    /// An own receipt record a delivery offers while a reservation of its session's is unread is
    /// noted as handed before the delivery's refusals and aterm's answer
    /// (`Bridge::deliver_record`), as the merge of ATERM-KI-1114-1414's case 6 with
    /// ATERM-KI-1115-7889 left it, not once aterm took its row (`Bridge::deliver_answered`): a
    /// record refused before its line (`via`, a carried person, a judging read that took the link,
    /// an unjudged hold the cap refused, the deadline it settles left standing), a row aterm
    /// refused, or a lane lost under it, leaves the reservation's first read to settle the retry at
    /// a record aterm was never handed, and past a cut that gave it up the receipt is lost. No sim
    /// seed of the reminted stampless world, with fetches or without, to 128 breaks a law with it
    /// off that holds with every rule on (2026-11-17: with fetches seed 21 breaks one with every
    /// rule on, the same with it off; without, none breaks one); its witness is bridge's
    /// `a_receipt_record_a_delivery_did_not_hand_settles_no_retry_once_its_reservation_reads`, its
    /// `via` and `refused` arms, alone and queued.
    UnreadReceiptNotedBeforeItsRowIsTaken,
    /// The first read of a reservation an attach could not read arms nothing where no record handed
    /// meanwhile settles it (`Bridge::arm_unread_receipt`): its attempt's record, held in hand
    /// behind another own record of its lane or queued while the file did not read, is handed at
    /// its release, and the retry, the record cut, reads nothing back and publishes it again; at a
    /// later attach that read takes no copy held over the link drop, so its read of the pin past
    /// the cut finds none, and the same. No sim seed of the reminted worlds (stamping, stampless,
    /// stampless with fetches) to 128 breaks a law with it off that holds with every rule on
    /// (2026-11-17: the world with fetches breaks one at seed 21 with every rule on, the same with
    /// it off); its witness is bridge's
    /// `an_owed_receipt_unread_at_the_attach_takes_its_record_held_behind_another_once_it_reads`,
    /// its `drain` and `attach` arms.
    UnreadReceiptReadArmsNothing,
    /// No undecided attempt's record this bridge holds untaken, in hand or queued, is taken for a
    /// post or receipt behind its lane's unverifiable hold or its session's stalled receipt
    /// (`Bridge::taken_for_its_retry`): it waits there with its copy in hand, and a link drop
    /// meanwhile, past a cut that gave the record up, leaves its retry nothing to read back. No sim
    /// seed of the reminted worlds (stamping, stampless, stampless with fetches) to 128 breaks a
    /// law with it off that holds with every rule on (2026-11-17: the world with fetches breaks one
    /// at seed 21 with every rule on, the same with it off); its witness is bridge's
    /// `a_receipt_held_in_hand_behind_its_sessions_stalled_receipt_settles_there`, its `untaken`
    /// arm (the stall's branch) and its `unverified` arm (the hold's), which show the drain's wait
    /// with it off (nothing retired, nothing behind it delivered), not the second copy: their
    /// record is staged where the drain's own release takes it next.
    UndecidedUntakenPastAHold,
}

/// What one run is.
#[derive(Clone, Debug)]
pub(crate) struct Opts {
    pub(crate) seed: u64,
    /// Steps of the fault phase.
    pub(crate) steps: usize,
    /// The tier, or `None` for the seed's choice.
    pub(crate) tier: Option<Tier>,
    pub(crate) ep: EpRules,
    /// The escalation table the broker holds (the oracle always holds aterm's).
    pub(crate) table: Escalation,
    pub(crate) arm: ArmMutant,
    pub(crate) mutants: BTreeSet<Mutant>,
    /// The discovery target, independent of other intentionally disabled protections.
    pub(crate) controlled_mutant: Option<Mutant>,
    pub(crate) power_cuts: bool,
    /// The broker's log may lose power alone.
    pub(crate) bus_losses: bool,
    /// How often a power cut is drawn, against a pass's 40 (default 1).
    pub(crate) cut_weight: u64,
    /// The state dir's device keeps every write (a control).
    pub(crate) lossless_device: bool,
    /// The state dir's device may tear an unflushed file (POSIX's worst case, [`Device::tears`]).
    pub(crate) torn_device: bool,
    /// The bus mirror: `[fabric] log = "turns"`, the sessions' logs mirrored onto their `ev` faces.
    pub(crate) mirror: bool,
    /// The broker is retained by aterm's rules ([`bus::State::retain`]); unretained, a
    /// mirror takes its capped path with no window ledger.
    pub(crate) retained: bool,
    /// The rules are aterm's at bytes a few records reach ([`SCALED`],
    /// `broker_rules::rules_scaled`): the ticks cut by the bytes as well as the age.
    pub(crate) scaled: bool,
    /// RETENTION'S DRAWS (sim item B05), on a retained broker: ticks, cuts standing for a
    /// byte bound the fleet's traffic reaches, days passing, and opens that set no rules.
    /// Without them a retained broker still ticks every minute of the world's clock and at
    /// every open, as astream's does.
    pub(crate) cuts: bool,
    /// The mirror is `[fabric] log = "auto"`, not `"turns"`: it runs only over a broker that
    /// retains this node's `ev` faces.
    pub(crate) auto: bool,
    /// The mirror's window budget at this many body bytes, not `mirror::NODE_WINDOW_BYTES`
    /// (`Bridge::simulate_window`), so a scaled world's mirror waits on it.
    pub(crate) window: Option<u64>,
    /// Held acks: the broker answers some acks late.
    pub(crate) hold_acks: bool,
    /// The broker rolls back some of the node's batches ([`bus::State::refuse_next`]).
    pub(crate) refused_batches: bool,
    /// The broker GAP-FENCES the publishes of a connection that asks ([`bus::State::gap_fences`]),
    /// as `aterm link broker` opens it, and the bridge asks: three seeds in four, by blocks of 128
    /// (`(seed / 128) % 4 != 3`), so every seed of the committed sweep (1..=128) fences. The
    /// fourth is a broker an older build opened, against which a sender keeps one post in
    /// flight; the committed seeds run there too, with this turned off
    /// (`every_seed_holds_every_law_against_a_broker_that_does_not_gap_fence`).
    pub(crate) gap_fences: bool,
    /// A sibling under this node's id.
    pub(crate) sibling: bool,
    /// A sibling that races an attach ([`bus::Race`]).
    pub(crate) race: bool,
    /// The sibling takes the current attach's incarnation.
    pub(crate) alias: bool,
    /// Write errors ([`Device::arm_write_errors`]).
    pub(crate) write_errors: bool,
    /// Aterm quits and comes back, in the same boot ([`World::quit`]).
    pub(crate) quits: bool,
    /// SESSIONS FETCH RECORDS OF THEIR LANES BY OFFSET (`inbox get @<off>`, finding F3), from a
    /// stream of their own, so a world without them draws as it did: a record the broker
    /// carries below its base most of the time, which a rules change can leave below its
    /// lane's oldest ([`changed_rules`]), and each answer is held to what the broker holds
    /// there ([`World::fetch_answers`]).
    pub(crate) fetches: bool,
    /// ASKS WITH A DEADLINE (item F4), from a stream of their own: three in four of the
    /// sessions' asks carry `dl=` ([`DEADLINES_MS`]), some short enough to pass before
    /// their target decides them and some long enough to be answered in time, and the laws
    /// of the deadline sweep hold them ([`World::judge_expiries`],
    /// [`World::deadlines_fired`]).
    pub(crate) deadlines: bool,
    /// BROADCAST TOPICS (item F4), from a stream of their own: the peer says on
    /// [`TOPICS`], the sessions follow them from an offset of their own choosing (the head,
    /// the base or below it, a record in between), and the laws of a follower hold them
    /// ([`World::topic_rows`], [`World::topics_followed`]).
    pub(crate) topics: bool,
    /// aterm hands its sessions off to a successor, in the same boot ([`World::handoff`]):
    /// the bridge says `bye handoff`, ends with its run record open, and the next bridge
    /// serves the same sessions.
    pub(crate) handoffs: bool,
    /// The door's journal ([`bus::Event`]).
    pub(crate) journal: bool,
    /// The state dir stands off the boot volume (item RZ2): its page cache vouches for no lease
    /// ([`crate::state::StateDir::trusted_boot`]), so every attach flushes `inc`.
    pub(crate) off_boot_volume: bool,
    /// THE RATE LIMIT every open of the broker sets (critic item E06): aterm's
    /// (`broker_rules::rate_limit`), which the worlds' traffic never reaches, or one low enough
    /// to bind ([`BINDING_RATE`]); `None`: none.
    pub(crate) rate_limit: Option<astream_broker::RateLimit>,
    /// THE LOG IS ON THE STATE DIR'S DEVICE (critic item F07): each full flush of the broker's
    /// drains it ([`bus::OnDevice`]), and its `Hello` names it, so the bridge fences its pin by
    /// barriers and the broker's settle. Every committed world's log is on another device,
    /// where the bridge flushes the pin itself.
    pub(crate) same_device: bool,
    /// KEYED RE-POSTS AND A CUT BETWEEN A DEDUP ANSWER AND ITS READ BACK (critic item F23), from
    /// streams of their own: half the sessions' notes carry an idempotency key (`post key=`), a
    /// session posts again under the key of one of its earlier keyed notes (an agent retrying a
    /// post it may have sent, [`World::repost`]), and one dedup answer of the node's in two is
    /// followed by a cut just past the answered record before the next request is served
    /// ([`bus::State::cut_after_dedup`]): a byte bound the traffic reaches landing between the
    /// answer and the read back that vouches for it. A keyed re-post's words are its own, so no
    /// read back of them precedes its publish at its pin: these are the worlds that could meet
    /// `Bridge::unverified_publication`'s case 4. Its key's winner, read back first
    /// (`Bridge::keyed_winner_at`), leaves the dedup only to a winner still in aterm's outbox,
    /// unread, which aterm's rules give up only past the mail rule's bytes or a week: so case 4
    /// is rare here, not impossible, and likeliest in the scaled seeds ([`Opts::scaled`]),
    /// whose mail bytes hold a few records and whose cut after the answer can give the winner
    /// up. The key's law is [`World::key_copies`].
    pub(crate) keys: bool,
    /// ANOTHER WRITER UNDER THE NODE'S PRODUCER ID AND A CUT AFTER A DEDUP ANSWER (critic item
    /// F23, from streams of their own): one refused publish of the node's in two is followed
    /// at once by that writer's record at the sequence the refusal named, on a face of its own
    /// ([`bus::State::stray`]: a sibling bridge under this node's id at this incarnation, as
    /// [`Opts::alias`]'s, one record of its), and one dedup answer in two by a cut just past
    /// the answered record ([`bus::State::cut_after_dedup`]). The node's retry at its pin meets
    /// a dedup answer with none of the pin's faces' records (`Bridge::unverified_publication`'s
    /// case 5), or that answer cut before its read back (case 4): the worlds in which F21's
    /// hold of an unverifiable reservation's own sender is judged by every law, its order
    /// and liveness included. The settle lets a week pass while a queued post or owed receipt
    /// waits on a stray record whose dedup key stands ([`World::waits_on_a_stray`]), as aterm's
    /// rules give it up then, and the hold's next retry appends anew. Its broker opens with
    /// rules every time ([`World::broker_start`]): on one that set none (`aterm link broker`'s
    /// fallback when setting them fails) nothing drops a stray's key, and the sender it holds
    /// waits for good, which the world's first census met in seeds 75 and 98 (a limit of
    /// `Bridge::unverified_publication`'s case 5 the F23 report records, not a law to judge).
    pub(crate) strays: bool,
    /// THE LOG SET ASIDE (ATERM-KI-1111-4, a stream of its own): one open of the broker in six
    /// follows `aterm link broker --retire-log` ([`bus::Broker::retire_log`]), so the bridge
    /// attaches to ANOTHER LOG, a fresh id that continues nothing, with every record the old
    /// one held gone.
    pub(crate) retire_logs: bool,
    /// A POISONED BROKER (ATERM-KI-1111-4, the same stream): on a weak tier the log's next full
    /// flush fails now and then ([`bus::Broker::fail_next_full_flush`]), so the escalated post,
    /// commit or `Settle` that asks for it is answered "outcome unknown", the tier latches to
    /// Strict and every write is refused until a reopen that moves the generation; a poisoned
    /// broker is stopped and opened again by its supervisor, as `aterm link broker` exits 75 on
    /// a poison ([`crate::broker_health`]) and is started again.
    pub(crate) poisons: bool,
    /// THE LOG REMINTED KEEPING ITS RECORDS (ATERM-KI-1116-7314, a stream of its own): one open
    /// of the broker in four that sets no log aside reads its identity sidecar as unreadable
    /// ([`bus::Broker::lose_identity`]), so astream mints another id, generation 0 with no
    /// lineage, that keeps every record at its offset (`logid::settle`'s `Prior::Unreadable`).
    pub(crate) remints: bool,
    /// A REMINTED OPEN TAKES NO ASK FOR ORIGINS (ATERM-KI-1114-1414, case 6): each open of
    /// [`Opts::remints`] that reminted the log refuses `Request::OriginStamps`
    /// ([`bus::State::stampless`]), so a read from anywhere on it names no origin and
    /// `Bridge::found_anywhere` cannot decide whose a record of a post's words is. A field of its
    /// own, not an [`ArmMutant`]: a control's production world (`with_every_rule`) resets `arm`,
    /// and would judge another world than the one the control ran.
    pub(crate) stampless_starts: bool,
}

/// The session the other writer under the node's producer id posts from ([`Opts::strays`]).
pub(crate) const STRAY_SID: &str = "tstray00000";

/// The deadlines an ask draws ([`Opts::deadlines`]), in ms: from one that passes before its
/// target can decide it to one a decision meets in time.
pub(crate) const DEADLINES_MS: [u64; 5] = [1_000, 5_000, 20_000, 60_000, 180_000];

/// The broadcast topics of a world of [`Opts::topics`].
pub(crate) const TOPICS: [&str; 2] = ["alpha", "beta"];

/// The peer's session that says on [`TOPICS`].
pub(crate) const SAYER: &str = "tzzzzzzzzzy";

/// How long past an ask's deadline (judged from where its post retired) the sweep may take
/// to say so, the settle's clocks being the bridge's: its roster refresh re-arms it, and a
/// sweep the link was down for runs again once it is back.
const DEADLINE_SLACK_MS: u64 = 30_000;

/// The sibling's one session ([`Opts::sibling`]).
pub(crate) const SIBLING_SID: &str = "tzzzzzzzzzz";

/// A fresh hosted session's deterministic ID, leaving the top two IDs for the peer and sibling.
fn hosted_sid(local: u64) -> String {
    let limit = (1u64 << aterm_types::tab_id::BITS) - 2;
    assert!(
        (1..limit).contains(&local),
        "a unique hosted session ID below the two reserved IDs"
    );
    aterm_types::tab_id::TabId::from_bits(local)
        .as_str()
        .to_owned()
}

/// A scaled world's rules' bytes ([`Opts::scaled`]): the bulk stream's, the mail lanes' and
/// every other subject's, a few dozen of the world's records each.
pub(crate) const SCALED: (u64, u64, u64) = (2 * 1024, 3 * 1024, 12 * 1024);

/// A scaled world's mirror window, in body bytes ([`Opts::window`]): its records' bytes are
/// past what the bulk stream's rule keeps ([`SCALED`]), so the mirror reaches it only while
/// a tick has yet to cut, waits, and goes on once one has (one under the rule's bytes would
/// wait on the age alone, a week).
pub(crate) const SCALED_WINDOW: u64 = 1024;

/// A RATE LIMIT LOW ENOUGH TO BIND (critic item E06, [`Opts::rate_limit`]): ten records a second
/// and a burst of four, so a drain's posts, a round's presence and `ev` rows, or the mirror's
/// slots run past it in every committed seed, gap-fenced too, while a settle's half hour still
/// has room for every world's log.
///
/// E06 bound a burst of three, because one of four reached, at seed 78, a hole the limit only
/// widens: a bridge killed between a post's ack (its in-flight entry ended with its `acked`
/// write) and the `outbox sent` that retires it left the next bridge an own record it delivered
/// while the post stood queued; once the commit passed it and retention gave it up, the drain's
/// retry at the pin found no copy and appended a second (`dup delivery`), the limit's hold
/// delaying that drain past a kill. Critic item E07 closed it (the entry stands until the post
/// left aterm's outbox, `Bridge::land_group`), and seed 78 is among the committed
/// seeds here (`Mutant::LandedBeforeOutboxSent` brings the hole back). Critic item F15 closed
/// the same hole for receipts, which settle through the same steps: `Mutant::ReceiptRetiredAtAck`
/// brings it back at seed 515, which answers one decision twice.
pub(crate) fn binding_rate() -> astream_broker::RateLimit {
    astream_broker::RateLimit::records(10, 4)
        .unwrap_or_else(|| unreachable!("astream takes ten records a second, a burst of four"))
}

/// A CHANGE OF RULES NO ATERM BUILD MAKES (sim item B05): aterm's at the world's bytes, but each
/// lane's receipts keep their last value only, a class no aterm rules ever had (every retaining
/// build since 2ff425db16 sets `broker_rules::default_rules`, whose four rules hold none such).
/// So rules set outside aterm (`asb serve --retain-file`), or a later change of aterm's own
/// classes or bytes, and never a downgrade: the worlds reach a record held below its subject's
/// oldest only through such a change. The open that sets them gives the receipts held below
/// the base up (its rules hold none of them), and the open after it, setting aterm's again,
/// takes the mail class whole from the base alone (astream's `retention::carried_since`: the
/// receipts' class carried none of its records below it), so the mail held below the base
/// lies below every lane's oldest: a tail and a told read still reach it, a read on at the
/// floor does not, and the inbox's gap notice covers it.
pub(crate) fn changed_rules(scaled: bool) -> astream_broker::Retention {
    use crate::broker_rules::{
        DEFAULT_BYTES, EV_BYTES, EV_FILTER, MAIL_BYTES, MAIL_FILTER, MAIL_READER, MAX_AGE,
        PRESENCE_FILTER,
    };
    use astream_broker::{Retention, Rule};
    let (ev, mail, default) = if scaled {
        SCALED
    } else {
        (EV_BYTES, MAIL_BYTES, DEFAULT_BYTES)
    };
    let rules = || -> std::io::Result<Retention> {
        Retention::new(
            vec![
                Rule::prefix(EV_FILTER)?.max_age(MAX_AGE).max_bytes(ev),
                Rule::hold(PRESENCE_FILTER)?.last_only(),
                Rule::hold("/f/*/in/*/*/*/ack")?.last_only(),
                Rule::hold(MAIL_FILTER)?
                    .until_read(MAIL_READER)?
                    .max_age(MAX_AGE)
                    .max_bytes(mail),
            ],
            Rule::default_keeping_all()
                .max_age(MAX_AGE)
                .max_bytes(default),
        )
    };
    rules().unwrap_or_else(|e| unreachable!("the changed rules are refused: {e}"))
}

/// The retention tick's interval on the world's clock (astream's `RETENTION_INTERVAL`).
const TICK_EVERY_MS: u64 = 60_000;

impl Opts {
    /// The committed configuration for `seed`.
    pub(crate) fn seed(seed: u64) -> Self {
        Self {
            seed,
            steps: 400,
            tier: None,
            ep: EpRules::default(),
            table: Escalation::aterm(),
            arm: ArmMutant::None,
            mutants: BTreeSet::new(),
            controlled_mutant: None,
            power_cuts: seed % 2 == 1,
            bus_losses: seed.is_multiple_of(2),
            cut_weight: 1,
            lossless_device: false,
            torn_device: true,
            mirror: (seed / 2) % 2 == 1,
            // Three in four retained, as every broker aterm ships; the found seeds that met
            // their bug over an unretained broker (122) keep it.
            retained: (seed / 16) % 4 != 3,
            scaled: (seed / 32) % 2 == 1,
            cuts: true,
            auto: (seed / 64) % 2 == 1,
            window: ((seed / 32) % 2 == 1).then_some(SCALED_WINDOW),
            hold_acks: (seed / 4) % 2 == 1,
            refused_batches: true,
            gap_fences: (seed / 128) % 4 != 3,
            sibling: false,
            race: false,
            alias: false,
            write_errors: (seed / 8) % 2 == 1,
            quits: false,
            journal: false,
            fetches: false,
            deadlines: false,
            topics: false,
            handoffs: false,
            off_boot_volume: false,
            rate_limit: Some(crate::broker_rules::rate_limit()),
            same_device: false,
            keys: false,
            strays: false,
            retire_logs: false,
            poisons: false,
            remints: false,
            stampless_starts: false,
        }
    }

    /// THE LOSSY-LOG SWEEP's configuration for `seed` (ATERM-KI-1111-4): the committed one,
    /// with logs set aside and poisoned brokers among the faults ([`Opts::retire_logs`],
    /// [`Opts::poisons`]), drawn from a stream of their own so every other draw is the
    /// committed seed's.
    pub(crate) fn lossy_logs(seed: u64) -> Self {
        Self {
            retire_logs: true,
            poisons: true,
            ..Self::seed(seed)
        }
    }

    /// THE REMINTED SWEEP's configuration for `seed` (ATERM-KI-1116-7314): the lossy-log
    /// world's, with logs astream reminted keeping their records among its faults
    /// ([`Opts::remints`]), drawn from a stream of their own.
    pub(crate) fn lossy_remints(seed: u64) -> Self {
        Self {
            remints: true,
            ..Self::lossy_logs(seed)
        }
    }

    /// THE REMINTED STAMPLESS WORLD for `seed` (ATERM-KI-1114-1414, case 6): the reminted
    /// sweep's, each open that reminted the log taking no ask for origins
    /// ([`Opts::stampless_starts`]), so the bridge's read from anywhere there holds its post
    /// undecided.
    pub(crate) fn lossy_remints_stampless(seed: u64) -> Self {
        Self {
            stampless_starts: true,
            ..Self::lossy_remints(seed)
        }
    }

    /// [`Opts::lossy_remints_stampless`] with the sessions' fetches by offset
    /// ([`Opts::fetches`]): a fetch reads a record a case-6 hold keeps undecided. Not yet a
    /// sweep (read 2026-11-17, seeds 1..=128): 127 hold every law, and one breaks one. Seed 21
    /// breaks `dup post`, the cost of `Bridge::found_anywhere`'s publish at the pin: a post to a
    /// person, whose lane no delivery of the bridge's reaches, read undecided past a remint on a
    /// broker that takes no ask for origins, is published at its pin, beside its carried
    /// attempt (ATERM-KI-1114-1414's residual). It holds every law with that rule off
    /// (`Mutant::UnstampedUnreachableHolds`, whose cost is a hold for good: seed 53's
    /// `liveness` in the stampless world), with every case-6 rule off, and with stamps on. Seed
    /// 112, where a power loss took a told record off the log's tail and a will landed at its
    /// offset before the attach, holds since the proof stops at that row and keeps the matches
    /// below it (ATERM-KI-1116-7314's told rows, `Mutant::ProofRefusedAtAMismatch`).
    pub(crate) fn lossy_remints_stampless_fetching(seed: u64) -> Self {
        Self {
            fetches: true,
            ..Self::lossy_remints_stampless(seed)
        }
    }

    /// The reclaim scenario's world on `tier` ([`run_reclaim`]): one device that keeps
    /// every write, a broker that answers at once and rolls nothing back, no faults.
    pub(crate) fn reclaiming(seed: u64, tier: Tier) -> Self {
        Self {
            tier: Some(tier),
            steps: 0,
            power_cuts: false,
            bus_losses: false,
            hold_acks: false,
            refused_batches: false,
            write_errors: false,
            mirror: false,
            ..Self::seed(seed)
        }
    }

    /// The rules every open of this world's broker sets (`None`: unretained).
    pub(crate) fn rules(&self) -> Option<astream_broker::Retention> {
        self.retained.then(|| {
            if self.scaled {
                let (ev, mail, default) = SCALED;
                crate::broker_rules::rules_scaled(ev, mail, default)
            } else {
                crate::broker_rules::default_rules()
            }
        })
    }

    /// `self` with one bridge rule switched off.
    pub(crate) fn with(mut self, m: Mutant) -> Self {
        self.mutants.insert(m);
        self
    }

    /// Disable the rule whose necessity this discovery world tests. Secondary
    /// switches added with `with` do not change which rule its control restores.
    pub(crate) fn testing(mut self, target: Mutant) -> Self {
        self.mutants.insert(target);
        self.controlled_mutant = Some(target);
        self
    }

    /// The committed configuration for `seed`.
    pub(crate) fn with_a_sibling(seed: u64) -> Self {
        Self {
            sibling: true,
            power_cuts: false,
            bus_losses: true,
            ..Self::seed(seed)
        }
    }

    /// The committed configuration for `seed`.
    pub(crate) fn with_a_racing_sibling(seed: u64) -> Self {
        Self {
            sibling: false,
            race: true,
            ..Self::with_a_sibling(seed)
        }
    }

    pub(crate) fn with_an_aliasing_sibling(seed: u64) -> Self {
        Self {
            alias: true,
            steps: 0,
            tier: Some(Tier::Strict),
            hold_acks: false,
            mirror: false,
            ..Self::with_a_racing_sibling(seed)
        }
    }
}

/// What one run found.
#[derive(Clone, Debug)]
pub(crate) struct Outcome {
    pub(crate) seed: u64,
    pub(crate) tier: Tier,
    pub(crate) violations: Vec<String>,
    pub(crate) trace: Vec<String>,
    pub(crate) stats: Stats,
    /// What the model's tap followed.
    pub(crate) model: (u64, u64, BTreeSet<&'static str>),
    /// The broker's journal ([`Opts::journal`]).
    pub(crate) journal: Vec<bus::Event>,
}

/// How much one run exercised, so a run that did nothing cannot pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stats {
    pub(crate) passes: u64,
    pub(crate) pumps: u64,
    pub(crate) kills: u64,
    pub(crate) relaunches: u64,
    pub(crate) broker_restarts: u64,
    pub(crate) moves: u64,
    pub(crate) lost_records: u64,
    pub(crate) power_cuts: u64,
    pub(crate) mail: u64,
    pub(crate) posts: u64,
    pub(crate) receipts: u64,
    pub(crate) halts: u64,
    pub(crate) delivered: u64,
    /// The `deliver` lines a bridge sent in one write with another (`Bridge::send_rows`).
    pub(crate) deliveries_pipelined: u64,
    /// The `outbox sent` lines a bridge sent in one write with another
    /// (`Bridge::retire_on_the_lane`, critic item E07).
    pub(crate) retires_pipelined: u64,
    pub(crate) exits: u64,
    pub(crate) log_records: u64,
    /// Ghost rows the bridge retired (`presence-retired … reason=unhosted`).
    pub(crate) ghosts_retired: u64,
    /// Ghost rows no running bridge may retire, left to the operator.
    pub(crate) ghosts_left: u64,
    /// The sibling ([`Opts::sibling`]), once settled, 1 or 0.
    pub(crate) sibling_kept: u64,
    pub(crate) sibling_retired: u64,
    /// The racing sibling ([`Opts::race`]), 1 or 0.
    pub(crate) sibling_raced: u64,
    /// The alias injected a live sibling at the current incarnation and the
    /// real bridge's first node publish actually collided with its row.
    pub(crate) sibling_alias_collided: u64,
    /// The specific same-dir session observed live before the first bridge died.
    pub(crate) alias_ghost_retired: u64,
    /// `ms` of the live sessions' logs the bus held once settled.
    pub(crate) mirrored: u64,
    /// Passes of a mirroring bridge whose last attach found the broker retaining its `ev`
    /// faces ([`Opts::retained`]): the mirror's window path.
    pub(crate) mirror_retained_passes: u64,
    /// Passes of a mirroring bridge whose last attach found the broker unretained: the
    /// mirror's capped path, with no window ledger.
    pub(crate) mirror_unretained_passes: u64,
    /// Torn state files a bridge moved into the state dir's quarantine after a power cut.
    pub(crate) set_aside: u64,
    /// Of those, how many the bridges told aterm of (`link set-aside`).
    pub(crate) set_aside_told: u64,
    /// Power cuts that tore `logpin`, the mirror's pin.
    pub(crate) pins_torn: u64,
    /// Of the torn files set aside.
    pub(crate) pins_set_aside: u64,
    /// Whole in-flight entries the first bridge of a boot retired into the quarantine.
    pub(crate) retired: u64,
    /// Held acks ([`Opts::hold_acks`], [`bus::Broker::held_counts`]).
    pub(crate) acks_held: u64,
    pub(crate) tries_not_yet: u64,
    pub(crate) seen_before_ack: u64,
    /// Inbox commits that rode a publish in one write (critic item B08), and ones written
    /// alone ([`bus::Broker::commit_writes`]).
    pub(crate) commits_corked: u64,
    pub(crate) commits_alone: u64,
    /// Publishes the broker refused behind a gap fence, and lifts that took one down
    /// ([`bus::Broker::fence_counts`]).
    pub(crate) gap_fenced: u64,
    pub(crate) fences_lifted: u64,
    /// Publishes the broker's rate limit refused in every open ([`bus::Broker::rate_limited_total`]).
    pub(crate) rate_limited: u64,
    /// The `Settle`s the broker answered ([`bus::Broker::settles`], critic item F07): the pin's
    /// fences that went by the broker's flush.
    pub(crate) settles: u64,
    /// The durable replaces of `logpin` the device made ([`Device::pin_durable_replaces`]).
    pub(crate) pin_durable_replaces: u64,
    /// WRITE errors the device injected ([`Opts::write_errors`], [`super::device::WriteErrors`]).
    pub(crate) writes_failed: u64,
    pub(crate) unlinks_failed: u64,
    pub(crate) flushes_failed: u64,
    pub(crate) renamed_unflushed: u64,
    pub(crate) appends_cut: u64,
    pub(crate) disk_full_refused: u64,
    pub(crate) disk_full_spells: u64,
    /// Times the bridge said its link down for a state-dir write the disk refused.
    pub(crate) held_for_the_disk: u64,
    /// Quits ([`Opts::quits`]).
    pub(crate) quits: u64,
    /// Quits whose closes the bus held before their bye (fix unit Q1, [`World::quit`]).
    pub(crate) quit_closes_landed: u64,
    pub(crate) clean_stops: u64,
    /// Kills that met the bridge holding an inbox commit for its next write (critic item B08).
    pub(crate) kills_holding: u64,
    /// Kills between a drain group's one append and its first write, and between a write of a
    /// group's posts and their acks (ATERM-KI-1111-5, `Bridge::killed_inside_a_group`).
    pub(crate) kills_group_appended: u64,
    pub(crate) kills_group_unacked: u64,
    /// RETENTION (sim item B05): ticks the broker took, cuts that moved its base, records
    /// given up, fetches refused `retained-from=`, gap notices a tail was told and `link
    /// gap` lines aterm was, days passed at once, opens that set no rules, opens that set
    /// changed rules ([`changed_rules`]), and times the mirror waited on its window budget.
    pub(crate) retention_ticks: u64,
    pub(crate) retention_cuts: u64,
    pub(crate) given_up: u64,
    pub(crate) retained_from: u64,
    pub(crate) gaps_noticed: u64,
    pub(crate) gaps_told: u64,
    pub(crate) days_passed: u64,
    pub(crate) unretained_starts: u64,
    pub(crate) changed_starts: u64,
    pub(crate) mirror_waits: u64,
    /// Refills from a watermark the refill law judged, and the records they had to offer
    /// (`sim::refill`).
    pub(crate) refills_judged: u64,
    pub(crate) refill_rows: u64,
    /// Reads of the node's whole inbox for several sessions together, and their sessions.
    pub(crate) refills_together: u64,
    pub(crate) refill_together_sessions: u64,
    /// Sessions that read their own lanes below the node inbox's oldest before the shared read.
    pub(crate) refills_below_oldest: u64,
    /// RESERVATIONS PUBLISHED AT THEIR PINS that asked for a verdict (critic item F23,
    /// [`crate::fx::ReservationTap`]): a dedup answer the read back verified, one another
    /// writer's record answered, and the answers of each of `Bridge::unverified_publication`'s
    /// six cases (index 0 is case 1).
    pub(crate) reservations_verified: u64,
    pub(crate) reservation_collisions: u64,
    pub(crate) unverifiable: [u64; 6],
    /// KEYED RE-POSTS ([`Opts::keys`]): posts under an earlier note's key; of them, the ones
    /// aterm was told landed as their key's earlier record (`dup=1`) and the ones that landed a
    /// copy of their own (their key's earlier records given up); and the cuts that landed
    /// between a dedup answer and the next request ([`bus::State::cut_after_dedup`]).
    pub(crate) reposts: u64,
    pub(crate) reposts_deduped: u64,
    pub(crate) reposts_appended: u64,
    pub(crate) cuts_after_dedup: u64,
    /// Keyed re-posts' entries the attach's replay settled at their key's winner, read back
    /// (`Bridge::keyed_winner_at`).
    pub(crate) replayed_at_the_winner: u64,
    /// Records a delivery took for an attempt no read from anywhere decided, held for its drain
    /// (`Bridge::take_undecided`, ATERM-KI-1114-1414).
    pub(crate) undecided_taken: u64,
    /// Records the other writer under the node's producer id wrote at a refused publish's
    /// sequence ([`Opts::strays`]), and the weeks the settle let pass for one of them.
    pub(crate) strays: u64,
    pub(crate) stray_weeks: u64,
    /// FETCHES BY OFFSET ([`Opts::fetches`], finding F3): asked; answered with the record,
    /// and of those the ones below their lane's oldest (read again told); answered
    /// `retained`; answered `no-record`.
    pub(crate) fetches: u64,
    pub(crate) fetched: u64,
    pub(crate) fetched_below_oldest: u64,
    pub(crate) fetched_retained: u64,
    pub(crate) fetched_none: u64,
    /// DEADLINES (item F4): asks that landed with one; of them, settled with an answer and
    /// no `expired`, with an `expired` and no answer, and with both (the answer late); the
    /// `expired` records a bridge published that another bridge noted the deadline of (across
    /// an incarnation); and the asks no sweep owes a verdict once settled: their asker gone,
    /// or their record taken by a broker stop.
    pub(crate) asks_with_deadline: u64,
    pub(crate) answered_in_time: u64,
    pub(crate) expired: u64,
    pub(crate) answered_late: u64,
    pub(crate) expired_across_incarnations: u64,
    pub(crate) asks_unjudged: u64,
    /// Asks whose only verdict a broker stop took past what it kept ([`World::broker_stop`]).
    pub(crate) verdicts_lost: u64,
    /// TOPICS (item F4): broadcasts the peer said, follows made (and of them from an offset
    /// below the broker's base, where retention gave up what the follower is owed), broadcast
    /// rows the followers were handed, and the broadcasts owed to a follower that retention
    /// gave up with a `link gap … topic=` told over them.
    pub(crate) says: u64,
    pub(crate) follows: u64,
    pub(crate) follows_below_the_base: u64,
    pub(crate) broadcasts_handed: u64,
    pub(crate) broadcasts_given_up_told: u64,
    /// Handoffs ([`Opts::handoffs`]).
    pub(crate) handoffs: u64,
    /// ANOTHER LOG (ATERM-KI-1111-4): opens that settled one (an empty log's, a loss that kept
    /// none, a retired log's), logs set aside ([`Opts::retire_logs`]), full flushes that
    /// failed and poisoned the broker ([`Opts::poisons`]), the writes refused while it was
    /// poisoned, and the restarts its supervisor made of a poisoned broker.
    pub(crate) another_logs: u64,
    pub(crate) retired_logs: u64,
    /// The opens that reminted the log keeping its records ([`Opts::remints`]).
    pub(crate) remints: u64,
    /// Of them, the opens that took no ask for origins ([`Opts::stampless_starts`]).
    pub(crate) stampless_starts: u64,
    /// The steps to another id the endpoint was told a bridge proved (`link log … proved=1`).
    pub(crate) remints_proved: u64,
    /// The remints the endpoint forgot across, unproved ([`super::conform::View::forgot_remints`]).
    pub(crate) remints_forgotten: u64,
    pub(crate) poisoned: u64,
    pub(crate) poisoned_refusals: u64,
    pub(crate) poison_restarts: u64,
    /// Steps of the model tap's slots opened on a log after one was set aside: a slot opened
    /// before a retire is no longer followed past it ([`super::conform::View::retired_logs`]),
    /// so these are the tap's only refinement checks of a world that retires a log.
    pub(crate) tap_past_a_retire: u64,
}

/// One ask with a deadline a session posted ([`Opts::deadlines`]).
#[derive(Clone, Debug)]
struct Ask {
    asker: String,
    target: String,
    id: u64,
    dl: u64,
    /// Where it landed, the wall clock when its post retired, and the bridge process that
    /// retired it: the deadline the bridge notes is due no later than that clock plus `dl`.
    landed: Option<(u64, u64, u64)>,
    /// Its post retired with no record of its own landed (`dup=1`, or `off=-`): no deadline is
    /// noted (the deadline-expiry row: a re-post deduped onto an existing record creates none).
    unnoted: bool,
}

/// One broadcast topic a session follows ([`Opts::topics`]).
#[derive(Clone, Debug)]
struct Follow {
    sid: String,
    topic: String,
    /// Owed every record of the topic from this offset on (`since=@<n>`).
    since: u64,
    /// On this log (`<id>.<generation>`), the one the session was last told: a move of the
    /// log since leaves what it is owed the bridge's to decide (`Bridge::forget_log_offsets` moves
    /// its cursor to the fork).
    log: String,
}

/// The producer keys of a record's copies on the bus, oldest first.
type Keys = Vec<Option<(u64, u64)>>;

/// One message the world made, by its marker (the text it carries).
#[derive(Clone, Debug)]
struct Sent {
    /// The session it is for (`None`: the person's lane).
    to: Option<String>,
    /// A post's sender and id.
    post: Option<(String, u64)>,
}

/// One session's `ev` face as a reader sees it ([`World::mirror_faces`]).
#[derive(Clone, Debug, Default)]
struct Face {
    /// Per `ms`, the distinct `(ep, seq)` records that hold it.
    recs: BTreeMap<u64, BTreeSet<(u32, u64)>>,
    /// Every cover: `(first ms, how many)`.
    covers: Vec<(u64, u64)>,
    /// How many bus records carry each `(ep, seq)`, before the reader's dedup.
    raw: BTreeMap<(u32, u64), usize>,
}

impl Face {
    fn holds(&self, ms: u64) -> bool {
        self.recs.contains_key(&ms)
            || self
                .covers
                .iter()
                .any(|&(lo, n)| ms >= lo && ms < lo.saturating_add(n))
    }
}

/// ONE RECORD OF THE MODEL BROKER'S LOG AS THE ORACLE READS IT ([`World::log`]): its bytes, and
/// each decoding of them made at most once while the log keeps the record. The laws read the
/// whole log at every step; copying it and decoding each record again at each of them was most
/// of what those reads cost.
struct Seen {
    subject: String,
    body: Vec<u8>,
    key: Option<(u64, u64)>,
    /// Not a record a reader is handed ([`bus::Broker::records`] leaves it out).
    hidden: bool,
    fabric: std::cell::OnceCell<Body>,
    slog: std::cell::OnceCell<Option<aterm_slog::BusRec>>,
}

impl Seen {
    fn of(rec: &bus::Rec) -> Self {
        Self {
            subject: rec.subject.clone(),
            body: rec.body.clone(),
            key: rec.key,
            hidden: rec.hidden(),
            fabric: std::cell::OnceCell::new(),
            slog: std::cell::OnceCell::new(),
        }
    }

    /// Whether the log holds this record still at its offset: one a power loss took may be
    /// another record there now.
    fn is(&self, rec: &bus::Rec) -> bool {
        self.key == rec.key
            && self.hidden == rec.hidden()
            && self.subject == rec.subject
            && self.body == rec.body
    }

    /// The body as a fabric reader decodes it ([`Body::decode`]).
    fn fabric(&self) -> &Body {
        self.fabric.get_or_init(|| Body::decode(&self.body).0)
    }

    /// The body as a session-log record, where it decodes as one ([`aterm_slog::decode_bus`]).
    fn slog(&self) -> Option<&aterm_slog::BusRec> {
        self.slog
            .get_or_init(|| aterm_slog::decode_bus(&String::from_utf8_lossy(&self.body)).ok())
            .as_ref()
    }
}

/// A session's presence row as the laws read it ([`World::presence_rows`]): `(state=, inc=,
/// statedir=)`.
type PresenceRow = (String, Option<u64>, Option<String>);

/// A record as the model's tap reads it ([`conform::View::records`]): `(sid, text)` of one on a
/// lane of this node's inbox.
type InboxRecord = Option<(String, String)>;

/// A node presence row as the laws read it ([`World::node_rows`]): `(state=, inc=, statedir=)`,
/// the mark read as hex.
type NodeRow = (String, Option<u64>, Option<u64>);

/// ONE FOLD OF THE LOG the laws read ([`LogReads`]), over its records in offset order and kept
/// up with it: the records appended since it last folded are folded in, and a record changed
/// below what it folded (a power loss's cut, then other records there) folds it again from the
/// first. Its value is a function of the records it folded alone.
struct Fold<T> {
    /// The lowest offset of [`LogReads::seen`] changed since this fold last caught up.
    dirty: std::cell::Cell<Option<usize>>,
    /// How many records it folded, and what it made of them.
    value: std::cell::RefCell<(usize, T)>,
}

impl<T: Default> Default for Fold<T> {
    fn default() -> Self {
        Self {
            dirty: std::cell::Cell::new(None),
            value: std::cell::RefCell::new((0, T::default())),
        }
    }
}

impl<T: Default> Fold<T> {
    /// A change of `seen` at offset `at`.
    fn lower(&self, at: usize) {
        let low = self.dirty.get().map_or(at, |low| low.min(at));
        self.dirty.set(Some(low));
    }

    /// The fold of `seen`, `step` taking each record not yet folded with its offset.
    fn of<'a>(
        &'a self,
        seen: &[Seen],
        step: impl Fn(&mut T, usize, &Seen),
    ) -> std::cell::Ref<'a, T> {
        if let Some(low) = self.dirty.take() {
            let mut value = self.value.borrow_mut();
            if low < value.0 {
                *value = (0, T::default());
            }
            let (folded, acc) = &mut *value;
            for (off, rec) in seen.iter().enumerate().skip(*folded) {
                step(acc, off, rec);
            }
            *folded = seen.len();
        }
        std::cell::Ref::map(self.value.borrow(), |(_, value)| value)
    }
}

/// THE MODEL BROKER'S LOG AS THE LAWS READ IT ([`World::log`]), and the folds of it they read at
/// every step.
#[derive(Default)]
struct LogReads {
    /// The log as it stood when last read, by offset, hidden records included.
    seen: std::cell::RefCell<Vec<Seen>>,
    /// The log's counts when `seen` was last brought up to it ([`bus::Log::read`]).
    read: std::cell::Cell<Option<bus::LogRead>>,
    inbox_records: Fold<Vec<InboxRecord>>,
    inbox_texts: Fold<conform::Texts>,
    bus_markers: Fold<BTreeMap<String, Vec<u64>>>,
    presence_rows: Fold<BTreeMap<String, PresenceRow>>,
    node_rows: Fold<Vec<NodeRow>>,
    mirror_faces: Fold<BTreeMap<String, Face>>,
}

impl LogReads {
    /// A change of `seen` at offset `at`, owed to every fold.
    fn lower(&self, at: usize) {
        self.inbox_records.lower(at);
        self.inbox_texts.lower(at);
        self.bus_markers.lower(at);
        self.presence_rows.lower(at);
        self.node_rows.lower(at);
        self.mirror_faces.lower(at);
    }
}

/// THE MODEL BROKER'S LOG AS IT STANDS, by offset, hidden records included, through `reads`
/// (the world's [`LogReads`] of it): a record is read again only where the log no longer holds
/// the one read last ([`Seen::is`]), so each is decoded once while the log keeps it; where the
/// log was only appended to since it was last read ([`bus::Log::changes`]), only the records
/// appended are read, and where it did not change, none. A caller drops what this returns
/// before it reads the log again.
fn log_of<'a>(broker: &bus::Broker, reads: &'a LogReads) -> std::cell::Ref<'a, Vec<Seen>> {
    {
        let s = lock(&broker.state);
        let changes = s.log.changes(reads.read.get());
        if changes != bus::LogChanges::None {
            let mut seen = reads.seen.borrow_mut();
            let mut low = None;
            if seen.len() > s.log.len() {
                seen.truncate(s.log.len());
                low = Some(s.log.len());
            }
            let from = if changes == bus::LogChanges::Appended {
                seen.len()
            } else {
                0
            };
            for (i, rec) in s.log.iter().enumerate().skip(from) {
                match seen.get_mut(i) {
                    Some(was) if was.is(rec) => continue,
                    Some(was) => *was = Seen::of(rec),
                    None => seen.push(Seen::of(rec)),
                }
                low = Some(low.map_or(i, |low: usize| low.min(i)));
            }
            if let Some(low) = low {
                reads.lower(low);
            }
            reads.read.set(Some(s.log.read()));
        }
    }
    reads.seen.borrow()
}

/// One bridge process, running.
struct Live {
    bridge: Bridge,
    process: Arc<Process>,
    backoff: Backoff,
    mailbox: Arc<Mailbox>,
}

/// THE WORLD.
struct World {
    opts: Opts,
    rng: Rng,
    tier: Tier,
    _dir: aterm_tempfile::TempDir,
    cfg: Config,
    core: Arc<Core>,
    ep: Arc<Endpoint>,
    live: Option<Live>,
    next_pid: u64,
    /// Which boot this is (a power cut starts the next).
    boot: u8,
    /// The next local id and sid generation aterm mints.
    next_local: u64,
    marker: u64,
    sent: BTreeMap<String, Sent>,
    /// Every halt row the person published, in order, a press astream answered with an unknown
    /// outcome among them.
    halt_rows: Vec<(String, bool)>,
    peer_seq: u64,
    person_seq: u64,
    violations: Vec<String>,
    stats: Stats,
    /// The lowest offset a broker restart cut back to: an offset at or above
    /// it may name another record now than when it was handed out.
    lost_floor: Option<u64>,
    /// What a sibling's rows carry past the fields every row.
    sibling_mark: String,
    /// This directory's mark under the injected current boot, for the oracle.
    mark: Option<String>,
    /// The sibling ([`Opts::sibling`]).
    sibling_inc: Option<u64>,
    sibling_attaches_at: Option<usize>,
    sibling_dies_at: Option<usize>,
    sibling_dead: bool,
    /// The finite alias fixture's actual departed B1 session.
    alias_ghost: Option<String>,
    /// Tier-1 of `FabricExactlyOnce`.
    tap: std::cell::RefCell<conform::Tap>,
    /// The quarantine's names when the running bridge's process was launched.
    quarantine_at_launch: BTreeSet<String>,
    /// Retention's draws (sim item B05), a stream of their own.
    ret_rng: Rng,
    /// Which opens set changed rules ([`changed_rules`]), a stream of its own.
    rules_rng: Rng,
    /// The fetches by offset ([`Opts::fetches`]), a stream of their own.
    fetch_rng: Rng,
    /// The world's wall clock at the last retention tick.
    last_tick_ms: u64,
    /// The asks' deadlines ([`Opts::deadlines`]), a stream of their own.
    ask_rng: Rng,
    /// Every ask with a deadline, by its marker.
    asks: BTreeMap<String, Ask>,
    /// The offsets of the `expired` records judged ([`World::judge_expiries`]), of the log the
    /// broker holds: a restart that gave up the records at and past an offset
    /// ([`World::book_restart`]) drops those, for the records landing there next are others.
    expiries_judged: BTreeSet<u64>,
    /// Every whole record a session read by a fetch, with the own records of its lane below it
    /// then ([`World::fetch_read_past`]); a restart lowers each one's head to what it kept.
    reads_past: Vec<(String, super::endpoint::ReadPast)>,
    /// `expired` records that followed an answer retention gave up before the sweep read it:
    /// `(asker, the answer's offset, the expiry's)`, honest once aterm was told a range of the
    /// asker's lane holding the answer ([`World::check_settled`]).
    expired_over_given_up: Vec<(String, u64, u64)>,
    /// `(asker, ask)` of every verdict on an asker's lane (an answer, an `expired`) a broker
    /// stop took with the acked records past what it kept ([`World::broker_stop`]).
    verdicts_lost: BTreeSet<(String, u64)>,
    /// The topics ([`Opts::topics`]), a stream of their own.
    say_rng: Rng,
    /// Every broadcast the peer said, by its marker: its topic.
    says: BTreeMap<String, String>,
    /// Every follow a session made.
    follows: Vec<Follow>,
    /// The model broker's log as the laws last read it, and their folds of it ([`World::log`]).
    reads: LogReads,
    /// The keyed re-posts ([`Opts::keys`]), a stream of their own.
    key_rng: Rng,
    /// Every key a note was posted under, by its name: who posted it, to whom, and the markers
    /// of its posts, first to last ([`World::repost`], [`World::key_copies`]).
    keyed: BTreeMap<String, Keyed>,
    /// Logs set aside and poisoned brokers ([`Opts::retire_logs`], [`Opts::poisons`]), a
    /// stream of their own, drawn from only in a world of either.
    log_rng: Rng,
    /// The draws of [`Opts::remints`].
    remint_rng: Rng,
    /// The ids the remints minted, as `link log` spells them before the `.`.
    reminted_ids: BTreeSet<String>,
}

/// The posts of one idempotency key ([`Opts::keys`]).
#[derive(Debug)]
struct Keyed {
    /// The posting session's local id and sid.
    local: u64,
    from: String,
    /// The note's address and its recipient session, if one.
    to: String,
    target: Option<String>,
    /// Its posts' markers, in the order they were posted.
    markers: Vec<String>,
}

/// The prefix of every subject of session `sid`'s mail on this node: the filter a fetch of
/// its lane reads (`Bridge::fetch_record`), but its `>`.
fn lane_of(sid: &str) -> String {
    format!("/f/{FLEET}/in/{NODE}/{sid}/")
}

/// The kinds an ask's deadline is noted for (`bridge::WAITING_KINDS`).
const WAITING: [&str; 2] = ["ask", "task"];

/// Who sent a record of a lane whose subject names `src`: the session its body's `from=` names
/// where a node sent it on that session's behalf (`fabric::traffic_of`), else `src`.
fn sender(src: &str, body: &Body) -> String {
    match body.from.as_deref() {
        Some(from) if src.starts_with("n-") && crate::subject::is_session_id(from) => {
            from.to_string()
        }
        _ => src.to_string(),
    }
}

/// WHETHER `asker` HOLDS THE COPY AT `off` OF AN ANSWER: a row of one of its sessions at that
/// offset that is this answer, of `kind`, naming the ask at `re` and, where `from` names one,
/// from that sender, or the same answer read by a fetch there
/// ([`super::endpoint::Session::read_by_fetch`]). A row at the offset of another kind, another
/// ask's or another sender's is another record. It counts a row or a read off the log the
/// endpoint was last told too, so the same answer to an ask at the same offset of a log since
/// retired still counts: the law errs toward a violation there, never past one.
fn holds_answer(
    s: &EpState,
    asker: &str,
    off: u64,
    kind: &str,
    re: u64,
    from: Option<&str>,
) -> bool {
    let sender = |by: &str| from.is_none_or(|from| by == from);
    s.sessions
        .values()
        .chain(s.gone.iter())
        .filter(|session| session.sid == asker)
        .any(|session| {
            session.rows.iter().any(|row| {
                row.off == off
                    && row.kind == kind
                    && row.re == Some(re)
                    && sender(row.from.split('@').next().unwrap_or(""))
            }) || session.read_by_fetch.iter().any(|read| {
                read.off == off && read.kind == kind && read.re == re && sender(&read.from)
            })
        })
}

/// A subject of mail on this node's lanes, `/f/<F>/in/<node>/<sid>/<src>/<kind>`: `(sid, src,
/// kind)`.
fn in_lane(subject: &str) -> Option<(&str, &str, &str)> {
    let rest = subject
        .strip_prefix("/f/")?
        .strip_prefix(FLEET)?
        .strip_prefix("/in/")?
        .strip_prefix(NODE)?
        .strip_prefix('/')?;
    let mut segs = rest.split('/');
    match (segs.next(), segs.next(), segs.next(), segs.next()) {
        (Some(sid), Some(src), Some(kind), None) => Some((sid, src, kind)),
        _ => None,
    }
}

/// The producer id of `principal` (astream's derivation).
fn producer(principal: &str) -> u64 {
    astream_cap::producer_id_of(principal)
}

/// A real directory's mark under the WORLD'S kernel boot, never the test host's.
fn boot_mark(path: &std::path::Path, boot: u8) -> Option<String> {
    let state = crate::state::StateDir::open(path).expect("the world's state dir");
    state.set_inc_seam(crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(boot))),
        ..crate::state::IncSeam::default()
    });
    state.mark(NODE)
}

impl World {
    fn new(opts: Opts) -> Self {
        let seed = opts.seed;
        let mut rng = Rng::new(opts.seed);
        let tier = opts.tier.unwrap_or(match rng.below(3) {
            0 => Tier::Strict,
            1 => Tier::Ordered,
            _ => Tier::Relaxed,
        });
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-sim")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("the state dir");
        let copy = dir.path().join("copy");
        std::fs::create_dir_all(&copy).expect("the copy's dir");
        let sibling_mark = boot_mark(&copy, if opts.alias { 2 } else { 1 })
            .map(|mark| format!(" {}={mark}", crate::state::PRESENCE_MARK))
            .unwrap_or_default();
        let mark = boot_mark(&state_dir, 1);
        let mut device = Device::new(&state_dir);
        device.lossless = opts.lossless_device;
        device.tears = opts.torn_device;
        device.seed(&state_dir.join("node"), format!("{NODE}\n").as_bytes());
        let table = if tier == Tier::Strict {
            Escalation::none()
        } else {
            opts.table.clone()
        };
        let broker = bus::Broker::new(tier, table);
        lock(&broker.state).mutant = opts.arm;
        lock(&broker.state).gap_fences = opts.gap_fences;
        lock(&broker.state).stampless_remints = opts.stampless_starts;
        broker.limit_every_open(opts.rate_limit);
        broker.retain(opts.rules());
        if opts.journal {
            broker.keep_journal();
        }
        if opts.race {
            lock(&broker.state).race = Some(bus::Race {
                refuse_will: if opts.alias { 2 } else { 3 },
                node_face: crate::subject::node_face(FLEET, NODE, "presence"),
                session_face: crate::subject::session_face(FLEET, NODE, SIBLING_SID, "presence"),
                extra: sibling_mark.clone(),
                alias: opts.alias,
                ..bus::Race::default()
            });
        }
        if opts.hold_acks {
            broker.hold_acks(Rng::new(opts.seed ^ 0x0a0c_4e1d_ac45));
        }
        if opts.keys || opts.strays {
            lock(&broker.state).cut_after_dedup = Some(Rng::new(opts.seed ^ 0x0c07_afde_d0f2_3000));
        }
        if opts.strays {
            lock(&broker.state).stray = Some(bus::Stray {
                draw: Rng::new(opts.seed ^ 0x057a_4e77_e1f2_3005),
                subject: format!("/f/{FLEET}/in/{PEER}/s-p/{NODE}/note"),
                body: format!("v=1 t=1 from={STRAY_SID} text=stray").into_bytes(),
            });
        }
        for p in [NODE, PEER, PERSON] {
            broker.name_producer(producer(p), p);
        }
        // WHERE THE LOG IS (critic item F07): on the state dir's device, each full flush of the
        // broker's draining it, or on another (a device number one past the state dir's).
        let state_dev = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(&state_dir).expect("the state dir").dev()
        };
        broker.on_device(if opts.same_device {
            bus::OnDevice {
                dev: state_dev,
                drain: Some(device.drain()),
            }
        } else {
            bus::OnDevice {
                dev: state_dev.wrapping_add(1),
                drain: None,
            }
        });
        let core = Core::new(opts.seed, broker, device);
        *lock(&core.mutants) = opts.mutants.clone();
        if opts.write_errors {
            let mut errors = Rng::new(opts.seed ^ 0x0e10_e5bc_0057);
            let per = [20, 50, 120][usize::try_from(errors.below(3)).unwrap_or(0)];
            core.device.arm_write_errors(errors, per);
        }
        let ep = Endpoint::new(opts.ep, opts.mirror);
        if opts.fetches {
            ep.watch(Arc::clone(&core));
        }
        if opts.mirror {
            ep.fence_takes(Arc::clone(&core));
        }
        let cfg = Config {
            fleet: FLEET.to_string(),
            broker: "sim".to_string(),
            cap_files: Vec::new(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            accept_from: Vec::new(),
            sock: None,
            token: None,
            presence: crate::presence::Mode::default(),
            receipts: true,
            log: if opts.mirror && opts.auto {
                crate::mirror::LogMode::Auto
            } else if opts.mirror {
                crate::mirror::LogMode::Turns
            } else {
                crate::mirror::LogMode::Off
            },
            mesh: None,
        };
        let mut w = World {
            opts,
            rng,
            tier,
            _dir: dir,
            cfg,
            core,
            ep,
            live: None,
            next_pid: 1,
            boot: 1,
            next_local: 1,
            marker: 0,
            sent: BTreeMap::new(),
            halt_rows: Vec::new(),
            peer_seq: 0,
            person_seq: 0,
            violations: Vec::new(),
            lost_floor: None,
            sibling_mark,
            mark,
            sibling_inc: None,
            sibling_attaches_at: None,
            sibling_dies_at: None,
            sibling_dead: false,
            alias_ghost: None,
            tap: std::cell::RefCell::new(conform::Tap::new(
                aterm_models_session::fabric_exactly_once_model(),
            )),
            quarantine_at_launch: BTreeSet::new(),
            stats: Stats::default(),
            ret_rng: Rng::new(seed ^ 0x7e7a_14ed_c075_0b05),
            rules_rng: Rng::new(seed ^ 0x01de_b011_d5ee_d0b5),
            fetch_rng: Rng::new(seed ^ 0x0f3f_e7c4_ed0f_f5e7),
            last_tick_ms: 0,
            ask_rng: Rng::new(seed ^ 0x0dea_d11e_0a5c_00f4),
            asks: BTreeMap::new(),
            expiries_judged: BTreeSet::new(),
            reads_past: Vec::new(),
            expired_over_given_up: Vec::new(),
            verdicts_lost: BTreeSet::new(),
            say_rng: Rng::new(seed ^ 0x05a7_7091_c5f4_0b0b),
            says: BTreeMap::new(),
            follows: Vec::new(),
            reads: LogReads::default(),
            key_rng: Rng::new(seed ^ 0x0ce7_5eed_4e75_0f23),
            keyed: BTreeMap::new(),
            log_rng: Rng::new(seed ^ 0x0109_c0de_1111_0004),
            remint_rng: Rng::new(seed ^ 0x0109_c0de_1116_7314),
            reminted_ids: BTreeSet::new(),
        };
        // The first open's tick, asked for at once when its rules are set.
        w.tick_now();
        if w.opts.sibling {
            let mut fate = Rng::new(w.opts.seed ^ 0x5b1b_11e6_dead_0001);
            let steps = w.opts.steps.max(16) as u64;
            let at = steps - steps / 8 + fate.below(steps / 16);
            w.sibling_attaches_at = usize::try_from(at).ok();
            if !fate.chance(1, 4) {
                w.sibling_dies_at = usize::try_from(at + 1 + fate.below(steps / 16)).ok();
            }
        }
        w.note(format!("world seed={} tier={tier:?}", w.opts.seed));
        w.open_sessions();
        w
    }

    /// Attaches the sibling ([`Opts::sibling`]) at the next incarnation above the node's highest published incarnation.
    fn sibling_attaches(&mut self) {
        if self.sibling_inc.is_some() {
            return;
        }
        if !lock(&self.core.broker.state).up {
            self.sibling_attaches_at = self.sibling_attaches_at.map(|at| at + 1);
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        }
        let inc = self.core.broker.max_incarnation(producer(NODE)) + 1;
        let Some(base) = astream_broker::incarnation_base(inc) else {
            return;
        };
        let node_face = crate::subject::node_face(FLEET, NODE, "presence");
        let session_face = crate::subject::session_face(FLEET, NODE, SIBLING_SID, "presence");
        let rows = [
            (
                node_face,
                format!(
                    "v=1 t=1 state=live inc={inc} fabric=connected{}",
                    self.sibling_mark
                ),
            ),
            (
                session_face,
                format!(
                    "v=1 t=1 inc={inc} epoch=- gen=- state=live hold=0 attention=-{}",
                    self.sibling_mark
                ),
            ),
        ];
        for (n, (subject, body)) in rows.iter().enumerate() {
            let seq = base + 1 + n as u64;
            if self
                .core
                .broker
                .publish_as(producer(NODE), seq, subject, body.as_bytes())
                .is_none()
            {
                return;
            }
        }
        let mut s = lock(&self.core.broker.state);
        s.durable = s.log.len() as u64;
        drop(s);
        self.sibling_inc = Some(inc);
        self.note(format!("sibling attaches inc={inc} {SIBLING_SID}"));
    }

    /// Ends the sibling's connection ([`Opts::sibling`]) and fires its will unless fenced by a higher incarnation.
    fn sibling_dies(&mut self) {
        if self.sibling_dead {
            return;
        }
        let Some(inc) = self.sibling_inc else {
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        };
        if !lock(&self.core.broker.state).up {
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        }
        self.sibling_dead = true;
        let Some(seq) = astream_broker::will_seq(inc) else {
            return;
        };
        if self.core.broker.fences(producer(NODE), seq) {
            self.note(format!("sibling dies inc={inc}, its will fenced"));
            return;
        }
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        let body = format!(
            "v=1 t=1 state={} inc={inc} fabric=disconnected{}",
            crate::glance::AWAY,
            self.sibling_mark
        );
        if self
            .core
            .broker
            .publish_as(producer(NODE), seq, &face, body.as_bytes())
            .is_some()
        {
            let mut s = lock(&self.core.broker.state);
            s.durable = s.log.len() as u64;
        }
        self.note(format!("sibling dies inc={inc}, its will fired"));
    }

    fn note(&self, line: impl Into<String>) {
        self.core.note(line);
    }

    /// aterm's sessions of this boot: two, with distinct IDs from their local allocations.
    fn open_sessions(&mut self) {
        for _ in 0..2 {
            let local = self.next_local;
            self.next_local += 1;
            let sid = hosted_sid(local);
            self.ep.create(local, &sid, self.stamp());
            self.note(format!("session {local} {sid}"));
        }
    }

    /// The world's clock as a session log stamps a record.
    fn stamp(&self) -> aterm_slog::Stamp {
        let mono = u64::try_from(self.core.elapsed().as_millis()).unwrap_or(u64::MAX);
        aterm_slog::Stamp::new(self.core.wall_ms(), mono)
    }

    /// A tab opens: a session joins, and aterm says so on the push lane.
    fn open_one(&mut self) {
        if self.ep.locals().len() >= 4 {
            return;
        }
        self.open_session();
    }

    /// A session joins, whatever aterm already hosts: its local id.
    fn open_session(&mut self) -> u64 {
        let local = self.next_local;
        self.next_local += 1;
        let sid = hosted_sid(local);
        self.ep.create(local, &sid, self.stamp());
        self.note(format!("session {local} {sid} opens"));
        self.push_event(&format!("EVENT * session-created {sid}"));
        local
    }

    /// Session `local` leaves, and aterm says so on the push lane.
    fn exit_local(&mut self, local: u64) {
        if let Some(sid) = self.ep.exit(local) {
            self.stats.exits += 1;
            self.note(format!("session {local} {sid} exits"));
            self.push_event(&format!("EVENT {local} session-exited {sid}"));
        }
    }

    /// Session `sid`, gone, is served again under a new local id ([`Endpoint::revive`]).
    fn revive(&mut self, sid: &str) -> bool {
        let local = self.next_local;
        if !self.ep.revive(sid, local) {
            return false;
        }
        self.next_local += 1;
        self.note(format!("session {local} {sid} revived"));
        self.push_event(&format!("EVENT * session-created {sid}"));
        true
    }

    /// A tab closes: a session leaves, and aterm says so on the push lane.
    fn exit_one(&mut self) {
        let locals = self.ep.locals();
        if locals.len() <= 2 {
            return;
        }
        let Some(local) = self.rng.pick(&locals).copied() else {
            return;
        };
        let Some(sid) = self.ep.exit(local) else {
            return;
        };
        self.stats.exits += 1;
        self.note(format!("session {local} {sid} exits"));
        self.push_event(&format!("EVENT {local} session-exited {sid}"));
    }

    /// A session's log grows: a bus-class record (three in four) or a local
    /// one, and the push lane's line that accompanies a timeline record.
    fn log_record(&mut self) {
        let locals = self.ep.locals();
        let Some(local) = self.rng.pick(&locals).copied() else {
            return;
        };
        let bus = self.rng.below(4) != 0;
        if self.ep.append_log(local, bus, self.stamp()) {
            self.stats.log_records += 1;
            self.push_event(&format!("EVENT {local} human"));
        }
    }

    fn next_marker(&mut self) -> String {
        self.marker += 1;
        format!("m{}", self.marker)
    }

    /// Move the broker's and the endpoint's notes into the trace.
    fn collect_notes(&mut self) {
        for n in self.core.broker.take_notes() {
            self.core.note(n);
        }
        let reqs = std::mem::take(&mut lock(&self.ep.state).requests);
        for r in reqs {
            // The link said down for a state-dir write the disk refused
            // (sim item S7): an attach held back, a record held back.
            if r.starts_with("link down reason=error") {
                self.stats.held_for_the_disk += 1;
            }
            self.core.note(format!("ctl {r}"));
        }
    }

    /// Launch a bridge, as aterm's supervisor does.
    fn copies_since_launch(&mut self) {
        let dir = std::path::Path::new(&self.cfg.state_dir).join(crate::state::QUARANTINE);
        let now: BTreeSet<String> = std::fs::read_dir(&dir)
            .map(|dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let made: Vec<String> = now
            .difference(&self.quarantine_at_launch)
            .cloned()
            .collect();
        for v in quarantine_copies(&dir, &made) {
            if !self.violations.contains(&v) {
                self.violations.push(v);
            }
        }
        self.quarantine_at_launch = now;
    }

    fn launch(&mut self) {
        self.copies_since_launch();
        let pid = self.next_pid;
        self.next_pid += 1;
        let process = Process::new(Arc::clone(&self.core), pid);
        let ctl = Ctl::simulated(self.ep.lane());
        self.note(format!("launch pid={pid} boot={}", self.boot));
        let fx = Fx::sim(Arc::clone(&process));
        let built = std::panic::catch_unwind(AssertUnwindSafe(|| {
            Bridge::simulated(self.cfg.clone(), fx, ctl)
        }));
        let mut bridge = match built {
            Ok(Ok(bridge)) => bridge,
            Ok(Err(e)) => {
                self.violations
                    .push(format!("the bridge could not be built: {e}"));
                return;
            }
            Err(payload) => {
                self.absorb(payload, &process);
                return;
            }
        };
        self.stats.relaunches += 1;
        let lease = u8::from(self.opts.mutants.contains(&Mutant::LeaseAcrossBoots));
        bridge.simulate_boot(crate::durability::test_boot(self.boot), lease);
        if self.opts.off_boot_volume {
            bridge.simulate_off_the_boot_volume(
                self.opts
                    .mutants
                    .contains(&Mutant::LeaseNamesNoBootOffTheBootVolume),
            );
        }
        if let Some(window) = self.opts.window {
            bridge.simulate_window(window);
        }
        if self.opts.mutants.contains(&Mutant::IgnoreLineage) {
            bridge.simulate_ignore_lineage();
        }
        let mailbox = bridge.mailbox_handle();
        let started = std::panic::catch_unwind(AssertUnwindSafe(|| bridge.start()));
        match started {
            Ok(None) => {
                self.live = Some(Live {
                    bridge,
                    process,
                    backoff: Backoff::default(),
                    mailbox,
                });
            }
            Ok(Some(ended)) => {
                self.violations
                    .push(format!("the bridge ended before its loop: {ended:?}"));
            }
            Err(payload) => {
                drop(bridge);
                self.absorb(payload, &process);
            }
        }
    }

    /// A step unwound: a simulated kill (the process is dead and its bridge
    /// is dropped by the caller), or a real panic, which is a violation.
    fn absorb(&mut self, payload: Box<dyn std::any::Any + Send>, process: &Arc<Process>) {
        if payload.is::<SimKill>() {
            self.stats.kills += 1;
            return;
        }
        process.kill();
        let msg = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "a non-string panic".to_string());
        self.violations.push(format!("the bridge panicked: {msg}"));
    }

    /// One pass of the bridge's loop.
    fn pass(&mut self) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        self.stats.passes += 1;
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| live.bridge.pass(&mut live.backoff)));
        match r {
            Ok(None) => {
                if self.opts.mirror {
                    match live.bridge.broker_retention_read() {
                        Some(true) => self.stats.mirror_retained_passes += 1,
                        Some(false) => self.stats.mirror_unretained_passes += 1,
                        None => {}
                    }
                }
                self.live = Some(live);
            }
            Ok(Some(ended)) => {
                self.note(format!("bridge ended: {ended:?}"));
                live.process.kill();
            }
            Err(payload) => {
                if payload.is::<SimKill>() {
                    if live.bridge.holds_inbox_commit() {
                        self.stats.kills_holding += 1;
                    }
                    self.count_group_kill(&live.bridge);
                }
                let process = Arc::clone(&live.process);
                drop(live);
                self.absorb(payload, &process);
            }
        }
    }

    /// A kill that met `bridge` inside a drain group ([`Stats::kills_group_appended`]).
    fn count_group_kill(&mut self, bridge: &Bridge) {
        let (appended, unacked) = bridge.killed_inside_a_group();
        self.stats.kills_group_appended += u64::from(appended);
        self.stats.kills_group_unacked += u64::from(unacked);
    }

    /// Kill the bridge now (a `kill -9` between two of its steps).
    fn kill(&mut self) {
        if let Some(live) = self.live.take() {
            self.note(format!("kill pid={} now", live.process.pid));
            self.stats.kills_holding += u64::from(live.bridge.holds_inbox_commit());
            self.count_group_kill(&live.bridge);
            live.process.kill();
            drop(live);
            self.stats.kills += 1;
        }
    }

    /// Queue one of aterm's push-lane lines for the running bridge.
    fn push_event(&self, line: &str) {
        if let Some(live) = &self.live {
            self.note(format!("event {line}"));
            live.mailbox.push_event(line.to_string());
        }
    }

    fn sessions(&self) -> Vec<(u64, String)> {
        self.ep
            .locals()
            .into_iter()
            .filter_map(|l| self.ep.sid(l).map(|s| (l, s)))
            .collect()
    }

    /// The peer posts a note to one of this node's sessions.
    fn peer_mail(&mut self) {
        let sessions = self.sessions();
        let Some((_, sid)) = self.rng.pick(&sessions).cloned() else {
            return;
        };
        self.peer_mail_to(sid);
    }

    /// The peer posts a note to session `sid`.
    fn peer_mail_to(&mut self, sid: String) {
        let marker = self.next_marker();
        self.peer_seq += 1;
        let subject = crate::subject::post_lane(FLEET, NODE, &sid, PEER, "note");
        let body = format!("v=1 t={} text={marker}", self.core.wall_ms());
        let landed =
            self.core
                .broker
                .publish_told(producer(PEER), self.peer_seq, &subject, body.as_bytes());
        self.note(format!("peer mail {marker} to {sid}: {landed:?}"));
        // An outcome astream calls unknown (a poisoned log, ATERM-KI-1111-4) is a mail sent:
        // its record is held out of sight, and a reopen may keep it.
        if landed.is_ok() {
            self.stats.mail += 1;
        }
        if landed != Err(false) {
            self.sent.insert(
                marker,
                Sent {
                    to: Some(sid),
                    post: None,
                },
            );
        }
    }

    /// A session posts: to the other session (a note or an ask), or to the
    /// person.
    fn post(&mut self) {
        let sessions = self.sessions();
        if sessions.len() < 2 {
            return;
        }
        let n = sessions.len() as u64;
        let i = usize::try_from(self.rng.below(n)).unwrap_or(0);
        let j = (i + 1 + usize::try_from(self.rng.below(n - 1)).unwrap_or(0)) % sessions.len();
        let (local, from) = sessions[i].clone();
        let to_sid = sessions[j].1.clone();
        let (to, kind, target) = match self.rng.below(6) {
            0 => (PERSON.to_string(), "note", None),
            1 | 2 => (format!("@{to_sid}"), "ask", Some(to_sid)),
            _ => (format!("@{to_sid}"), "note", Some(to_sid)),
        };
        self.post_as(local, &from, &to, kind, target);
    }

    /// Session `local` (`from`) posts `kind` to `to` (session `target`, if one). In a world of
    /// deadlines an ask to a session carries one three times in four ([`DEADLINES_MS`]); in one
    /// of keys a note carries a key of its own one time in two ([`Opts::keys`]).
    fn post_as(&mut self, local: u64, from: &str, to: &str, kind: &str, target: Option<String>) {
        let key = (self.opts.keys && kind == "note" && self.key_rng.chance(1, 2))
            .then(|| format!("k{}", self.keyed.len() + 1));
        if let Some(key) = &key {
            self.keyed.insert(
                key.clone(),
                Keyed {
                    local,
                    from: from.to_string(),
                    to: to.to_string(),
                    target: target.clone(),
                    markers: Vec::new(),
                },
            );
        }
        self.post_keyed(local, from, to, kind, target, key);
    }

    /// A KEYED RE-POST ([`Opts::keys`]): a session posts its note again under the key of one of
    /// its earlier keyed notes, to the same address, as an agent retries a post it may have sent
    /// already. Its words are its own (a marker of its own), so the bus says which of the key's
    /// posts a copy is; the key's law is [`World::key_copies`].
    fn repost(&mut self) {
        let live: BTreeSet<u64> = self.sessions().into_iter().map(|(l, _)| l).collect();
        let keys: Vec<String> = self
            .keyed
            .iter()
            .filter(|(_, k)| live.contains(&k.local))
            .map(|(key, _)| key.clone())
            .collect();
        // The newest key one time in two: a retry soon after the post, often in its drain.
        let key = if self.key_rng.chance(1, 2) {
            keys.iter()
                .max_by_key(|key| key.trim_start_matches('k').parse::<u64>().unwrap_or(0))
                .cloned()
        } else {
            self.key_rng.pick(&keys).cloned()
        };
        let Some(key) = key else {
            return;
        };
        let (local, from, to, target) = {
            let k = &self.keyed[&key];
            (k.local, k.from.clone(), k.to.clone(), k.target.clone())
        };
        self.stats.reposts += 1;
        self.post_keyed(local, &from, &to, "note", target, Some(key));
    }

    /// [`World::post_as`], under `key` where one is given.
    fn post_keyed(
        &mut self,
        local: u64,
        from: &str,
        to: &str,
        kind: &str,
        target: Option<String>,
        key: Option<String>,
    ) {
        let marker = self.next_marker();
        let dl = if self.opts.deadlines
            && kind == "ask"
            && target.is_some()
            && self.ask_rng.chance(3, 4)
        {
            self.ask_rng.pick(&DEADLINES_MS).copied()
        } else {
            None
        };
        let Some(id) = self.ep.post(local, to, kind, &marker, dl, key.as_deref()) else {
            return;
        };
        self.stats.posts += 1;
        if let Some(k) = key.as_ref().and_then(|key| self.keyed.get_mut(key)) {
            k.markers.push(marker.clone());
        }
        if let (Some(dl), Some(target)) = (dl, target.as_ref()) {
            self.asks.insert(
                marker.clone(),
                Ask {
                    asker: from.to_string(),
                    target: target.clone(),
                    id,
                    dl,
                    landed: None,
                    unnoted: false,
                },
            );
        }
        let dl_word = dl.map_or_else(String::new, |dl| format!(" dl={dl}"));
        let key_word = key.map_or_else(String::new, |key| format!(" key={key}"));
        self.note(format!(
            "post {from} #{id} {marker} to={to} kind={kind}{dl_word}{key_word}"
        ));
        self.sent.insert(
            marker,
            Sent {
                to: target,
                post: Some((from.to_string(), id)),
            },
        );
        self.push_event(&format!("EVENT {local} post {id}"));
    }

    /// A SESSION FETCHES ONE RECORD OF ITS LANE BY OFFSET ([`Opts::fetches`]): one the broker
    /// carries below its base, where there is one, three times in four; else one of the lane's
    /// records, or now and then an offset at or past the head.
    fn fetch(&mut self) {
        let sessions = self.sessions();
        let Some((local, sid)) = self.fetch_rng.pick(&sessions).cloned() else {
            return;
        };
        let (all, held, head) = self.core.broker.offsets_of(&lane_of(&sid));
        let off = if !held.is_empty() && self.fetch_rng.chance(3, 4) {
            self.fetch_rng.pick(&held).copied()
        } else if self.fetch_rng.chance(7, 8) {
            self.fetch_rng.pick(&all).copied()
        } else {
            None
        }
        .unwrap_or_else(|| head + self.fetch_rng.below(2));
        if self.ep.fetch(local, off) {
            self.stats.fetches += 1;
            self.note(format!("fetch {sid} @{off}"));
            self.push_event(&format!("EVENT {local} fetch {off}"));
        }
    }

    /// NO FETCH READS PAST WHAT A DELIVERY WOULD HOLD BACK (ATERM-KI-1115-41): once a session
    /// read a record of its lane by a fetch, no copy of an own record of the lane below it lands
    /// after the read (at or past the log's head then, as each restart since lowered it). The
    /// replay lands such a copy only of a record whose in-flight entry stood as the session read
    /// past it, a cut or a power loss having given its first copy up: the session then holds the
    /// later record before the earlier one, an `expired` after the answer it read. A copy is
    /// known by its subject and body, as the replay publishes the journaled entry again. The
    /// reads end with their sessions ([`World::sessions_end`]: a power cut of the machine, a
    /// quit), and a copy that lands after (a clear a power cut undid on the media) reaches none.
    fn fetch_read_past(&mut self) -> Vec<String> {
        self.reads_past.extend(self.ep.take_read_past());
        if self.reads_past.is_empty() {
            return Vec::new();
        }
        let mut bad = Vec::new();
        for (sid, read) in &self.reads_past {
            self.core
                .broker
                .each_visible_from(read.head, |q, subject, body| {
                    if read.below.iter().any(|(s, b)| s == subject && b == body) {
                        bad.push(format!(
                            "fetch past: {sid} read @{} past an own record whose copy landed @{q}",
                            read.off
                        ));
                    }
                });
        }
        bad
    }

    /// EVERY FETCH IS ANSWERED AS THE BROKER HOLDS ITS RECORD (finding F3), judged against
    /// what the broker held of the session's lane at that offset the moment the bridge
    /// answered (the endpoint reads it then, [`super::endpoint::Fetched`]): a record is
    /// answered with the lane's record at that offset; `retained` or `no-record` never while
    /// the broker holds a record of the lane there, below the lane's oldest included; and
    /// `no-record` never for a record of the lane retention gave up (a cut record is not one
    /// never written). Every other answer (the link down, a forged own record, an observer)
    /// says nothing of the record and is not judged.
    fn fetch_answers(&mut self) -> Vec<String> {
        let mut bad = Vec::new();
        for (sid, (off, answer, at)) in self.ep.take_fetched() {
            let word = |key: &str| {
                answer
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            };
            if answer.starts_with("from=") {
                if word("more").is_some() || word("at").is_some() {
                    continue; // one chunk of a body cut in lines: the world's are a marker each
                }
                self.stats.fetched += 1;
                let text = word("text").map(crate::pct::decode);
                match &at {
                    Some(held) if Some(Body::decode(&held.body).0.text) == text => {
                        if held.below_oldest {
                            self.stats.fetched_below_oldest += 1;
                        }
                    }
                    _ => bad.push(format!(
                        "fetch: {sid} @{off} answered a record its lane does not hold there: \
                         {answer}"
                    )),
                }
                continue;
            }
            let err = word("err");
            match err {
                Some("retained") => self.stats.fetched_retained += 1,
                Some("no-record") => self.stats.fetched_none += 1,
                _ => continue,
            }
            match &at {
                Some(held) if held.holds => bad.push(format!(
                    "fetch: {sid} @{off} answered `{answer}` while the broker holds its record \
                     (below the lane's oldest: {})",
                    held.below_oldest
                )),
                Some(_) if err == Some("no-record") => bad.push(format!(
                    "fetch: {sid} @{off} answered `{answer}` for a record retention gave up"
                )),
                _ => {}
            }
        }
        bad
    }

    /// THE DEADLINE LAW, AT EVERY `expired` THE BUS TAKES (item F4), judged the step it lands,
    /// before a tick can take what it is judged by. The record names an ask with a deadline
    /// its asker posted, and no answer of the ask's target to it lies below it on the asker's
    /// lane that the broker still holds (the sweep reads that lane for one, told below its
    /// oldest: `Bridge::settled_on_bus`), nor one the asker was handed. An answer retention
    /// gave up before the sweep read it is honest once aterm was told a range of the asker's
    /// lane holding it, judged once settled ([`World::check_settled`]). It also notes where
    /// each ask landed as its post retires, the clock then, and the bridge process.
    fn judge_expiries(&mut self) {
        if !self.opts.deadlines {
            return;
        }
        let wall = self.core.wall_ms();
        // The newest process, the one that ran since the last judge: one runs at a time, and
        // a launch is a step of its own.
        let pid = self.next_pid.saturating_sub(1);
        for ask in self
            .asks
            .values_mut()
            .filter(|a| a.landed.is_none() && !a.unnoted)
        {
            let Some(r) = self.ep.retired(&ask.asker, ask.id) else {
                continue;
            };
            let off = r
                .split_whitespace()
                .find_map(|t| t.strip_prefix("off="))
                .and_then(crate::parse_uint::<u64>);
            match off {
                Some(off) if !r.contains("dup=1") => ask.landed = Some((off, wall, pid)),
                _ => ask.unnoted = true,
            }
        }
        let log = log_of(&self.core.broker, &self.reads);
        let records = || {
            log.iter()
                .enumerate()
                .filter(|(_, rec)| !rec.hidden)
                .map(|(off, rec)| (off as u64, rec))
        };
        let mut bad = Vec::new();
        for (e, rec) in records() {
            if self.expiries_judged.contains(&e) {
                continue;
            }
            let Some((asker, src, "expired")) = in_lane(&rec.subject) else {
                continue;
            };
            self.expiries_judged.insert(e);
            if src != NODE {
                continue;
            }
            let Some(re) = rec.fabric().re else {
                bad.push(format!(
                    "deadline: @{e} on {asker}'s lane expired no record"
                ));
                continue;
            };
            let ask = records().find(|r| r.0 == re).and_then(|(_, r)| {
                let (target, src, kind) = in_lane(&r.subject)?;
                let b = r.fabric();
                (sender(src, b) == asker && WAITING.contains(&kind) && b.dl.is_some())
                    .then(|| (target.to_string(), b.text.clone()))
            });
            let Some((target, marker)) = ask else {
                bad.push(format!(
                    "deadline: @{e} expired @{re} on {asker}'s lane, which is no ask of \
                     {asker}'s with a deadline"
                ));
                continue;
            };
            if self
                .asks
                .get(&marker)
                .and_then(|a| a.landed)
                .is_some_and(|(_, _, by)| by != pid)
            {
                self.stats.expired_across_incarnations += 1;
            }
            for (a, r) in records().filter(|r| r.0 < e) {
                let Some((sid, src, kind)) = in_lane(&r.subject) else {
                    continue;
                };
                let b = r.fabric();
                if sid != asker
                    || sender(src, b) != target
                    || !ANSWER_KINDS.contains(&kind)
                    || b.re != Some(re)
                {
                    continue;
                }
                if self.core.broker.holds(a) {
                    bad.push(format!(
                        "deadline: @{e} expired @{re} ({marker}), answered by {target} at @{a}, \
                         which the broker holds"
                    ));
                } else {
                    self.expired_over_given_up.push((asker.to_string(), a, e));
                }
            }
            let every_log = self.opts.mutants.contains(&Mutant::FetchReadOnEveryLog);
            let s = lock(&self.ep.state);
            let mine = || {
                s.sessions
                    .values()
                    .chain(s.gone.iter())
                    .filter(|x| x.sid == asker)
            };
            // A row, or an answer the asker read by a fetch, which leaves none
            // (ATERM-KI-1115-9210), while the ask it names stands on the log the endpoint was
            // last told (an offset of a log since replaced names another record): below the
            // `expired`, or handed before a fork took its own offset (ATERM-KI-1115-2409).
            let row_stands = |row: &Row| row.re_on_log && (!row.on_log || row.off < e);
            let read_stands = |read: &FetchRead| {
                (read.ask_on_log || every_log) && (read.off_replaced || read.off < e)
            };
            let answers = |kind: &str| ANSWER_KINDS.contains(&kind);
            let handed = mine().find_map(|x| {
                x.rows
                    .iter()
                    .find(|row| row.re == Some(re) && answers(&row.kind) && row_stands(row))
                    .map(|row| row.off)
                    .or_else(|| {
                        x.read_by_fetch
                            .iter()
                            .find(|read| read.re == re && answers(&read.kind) && read_stands(read))
                            .map(|read| read.off)
                    })
            });
            // NOR A SECOND `expired` OF THE ASK (ATERM-KI-1115-2409): one the asker was handed
            // whose record a log change gave up, the ask kept. [`World::expired_once`] counts
            // the copies the broker still holds.
            let again = mine().find_map(|x| {
                x.rows
                    .iter()
                    .find(|row| {
                        row.kind == "expired" && row.re == Some(re) && row.re_on_log && !row.on_log
                    })
                    .map(|row| row.off)
                    .or_else(|| {
                        x.read_by_fetch
                            .iter()
                            .find(|read| {
                                read.kind == "expired"
                                    && read.re == re
                                    && read.ask_on_log
                                    && read.off_replaced
                            })
                            .map(|read| read.off)
                    })
            });
            drop(s);
            if let Some(a) = handed {
                bad.push(format!(
                    "deadline: @{e} expired @{re} ({marker}), whose answer @{a} {asker} was handed"
                ));
            }
            if let Some(a) = again {
                bad.push(format!(
                    "deadline: @{e} expired @{re} ({marker}) again, {asker} handed its `expired` \
                     @{a} on a log since replaced"
                ));
            }
        }
        for b in bad {
            if !self.violations.contains(&b) {
                self.violations.push(b);
            }
        }
    }

    /// THE DEADLINE LAW'S ONCE, AT EVERY STEP (item F4): an ask expires on its asker's lane once,
    /// across every bridge that held its deadline. A second `expired` of one ask is excused
    /// only where every older copy went with retention before its asker was handed it
    /// ([`holds_answer`]): the sweep that reads the lane for one then finds none (as a
    /// receipt's answer is excused).
    fn expired_once(&self) -> Vec<String> {
        let gone = self.core.broker.gone();
        let mut copies: BTreeMap<(String, u64), Vec<u64>> = BTreeMap::new();
        for (off, rec) in self.log().iter().enumerate().filter(|(_, rec)| !rec.hidden) {
            if let Some((asker, src, "expired")) = in_lane(&rec.subject) {
                if let Some(re) = rec.fabric().re.filter(|_| src == NODE) {
                    copies
                        .entry((asker.to_string(), re))
                        .or_default()
                        .push(off as u64);
                }
            }
        }
        let s = lock(&self.ep.state);
        let by_offset = self.opts.mutants.contains(&Mutant::AnswerHandedByOffset);
        let handed = |asker: &str, off: u64, re: u64| {
            if by_offset {
                return s
                    .sessions
                    .values()
                    .chain(s.gone.iter())
                    .any(|x| x.rows.iter().any(|r| r.on_log && r.off == off));
            }
            holds_answer(&s, asker, off, "expired", re, None)
        };
        copies
            .into_iter()
            .filter(|((asker, re), offs)| {
                offs.len() > 1
                    && !offs.split_last().is_some_and(|(_, older)| {
                        older
                            .iter()
                            .all(|o| gone.contains(o) && !handed(asker, *o, *re))
                    })
            })
            .map(|((asker, re), offs)| {
                format!(
                    "deadline: {asker}'s ask @{re} expired {} times {offs:?}",
                    offs.len()
                )
            })
            .collect()
    }

    /// The asks with a deadline that landed and whose asker aterm still hosts, each with its
    /// record still on the bus where it landed: `(marker, ask, answered, expired)`, where an
    /// answer is its target's on the asker's lane and an `expired` this node's there, each
    /// counted wherever it lies, held or given up.
    fn asks_standing(&self) -> Vec<(String, Ask, bool, bool)> {
        let log = self.log();
        let records = || {
            log.iter()
                .enumerate()
                .filter(|(_, rec)| !rec.hidden)
                .map(|(off, rec)| (off as u64, rec))
        };
        let hosted: BTreeSet<String> = lock(&self.ep.state)
            .sessions
            .values()
            .map(|x| x.sid.clone())
            .collect();
        self.asks
            .iter()
            .filter(|(_, a)| hosted.contains(&a.asker))
            .filter_map(|(marker, a)| {
                let (off, _, _) = a.landed?;
                let on_bus = records().any(|(o, r)| {
                    let b = r.fabric();
                    o == off
                        && in_lane(&r.subject).is_some_and(|(to, src, kind)| {
                            to == a.target && sender(src, b) == a.asker && kind == "ask"
                        })
                        && b.text == *marker
                });
                if !on_bus {
                    return None;
                }
                let (mut answered, mut expired) = (false, false);
                for (_, r) in records() {
                    let Some((sid, src, kind)) = in_lane(&r.subject) else {
                        continue;
                    };
                    let b = r.fabric();
                    if sid != a.asker || b.re != Some(off) {
                        continue;
                    }
                    answered |= sender(src, b) == a.target && ANSWER_KINDS.contains(&kind);
                    expired |= src == NODE && kind == "expired";
                }
                Some((marker.clone(), a.clone(), answered, expired))
            })
            .collect()
    }

    /// Whether a verdict on the ask at `off` of `asker` went with a broker stop
    /// ([`World::verdicts_lost`]).
    fn verdict_lost(&self, asker: &str, off: u64) -> bool {
        self.verdicts_lost.contains(&(asker.to_string(), off))
    }

    /// Whether an ask's deadline is still to fire: one standing ([`World::asks_standing`])
    /// neither answered nor expired, nor one whose verdict a broker stop took.
    fn deadlines_pending(&self) -> bool {
        self.opts.deadlines
            && self
                .asks_standing()
                .iter()
                .any(|(_, a, answered, expired)| {
                    !answered
                        && !expired
                        && !a
                            .landed
                            .is_some_and(|(off, _, _)| self.verdict_lost(&a.asker, off))
                })
    }

    /// THE DEADLINE LAW, ONCE SETTLED (item F4): every ask standing ([`World::asks_standing`])
    /// whose deadline passed, judged from the clock when its post retired plus its `dl=` and
    /// [`DEADLINE_SLACK_MS`], was answered by its target or expired: a deadline fires for
    /// every ask nobody answered, whichever bridge held its table when it came due. And an
    /// `expired` that followed an answer retention gave up before the sweep read it was told:
    /// aterm holds a `link gap` over the answer for the asker.
    fn deadlines_fired(&mut self) -> Vec<String> {
        let now = self.core.wall_ms();
        let mut bad = Vec::new();
        self.stats.asks_with_deadline =
            self.asks.values().filter(|a| a.landed.is_some()).count() as u64;
        let hosted = self.asks_standing();
        self.stats.asks_unjudged = self
            .asks
            .values()
            .filter(|a| a.landed.is_some())
            .count()
            .saturating_sub(hosted.len()) as u64;
        for (marker, a, answered, expired) in hosted {
            let Some((off, at, _)) = a.landed else {
                continue;
            };
            match (answered, expired) {
                (true, false) => self.stats.answered_in_time += 1,
                (false, true) => self.stats.expired += 1,
                (true, true) => self.stats.answered_late += 1,
                (false, false) if self.verdict_lost(&a.asker, off) => {
                    self.stats.verdicts_lost += 1;
                }
                (false, false) => {
                    let due = at.saturating_add(a.dl);
                    if now >= due.saturating_add(DEADLINE_SLACK_MS) {
                        bad.push(format!(
                            "deadline: {marker} (ask @{off} of {} to {}, dl={}) was due {} ms \
                             ago and neither answered nor expired",
                            a.asker,
                            a.target,
                            a.dl,
                            now - due
                        ));
                    }
                }
            }
        }
        let s = lock(&self.ep.state);
        for (asker, a, e) in &self.expired_over_given_up {
            if !s.gaps.iter().any(|g| g.tells_mail_of(asker, *a)) {
                bad.push(format!(
                    "deadline: @{e} expired an ask of {asker}'s whose answer @{a} retention gave \
                     up, and no `link gap` aterm was told holds it: {:?}",
                    s.gaps
                ));
            }
        }
        bad
    }

    /// A FOLLOWER'S ROWS, AT EVERY STEP (item F4): each broadcast row a session was handed is
    /// one the peer said on that row's topic, on a topic the session follows, at or past the
    /// offset it follows it from on the log it followed it on, and never twice; and the rows
    /// of each topic on the log the session was last told come in offset order.
    fn topic_rows(&self) -> Vec<String> {
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        for session in s.sessions.values().chain(s.gone.iter()) {
            let mut last: BTreeMap<&str, u64> = BTreeMap::new();
            let mut texts: BTreeSet<&str> = BTreeSet::new();
            for row in &session.rows {
                let Some(topic) = row.topic.as_deref() else {
                    continue;
                };
                let sid = &session.sid;
                if self.says.get(&row.text).map(String::as_str) != Some(topic) {
                    bad.push(format!(
                        "topic: {sid} was handed {} @{} on {topic}, which the peer did not say \
                         there",
                        row.text, row.off
                    ));
                }
                if !session.topics.contains_key(topic) {
                    bad.push(format!(
                        "topic: {sid} was handed {} @{} on {topic}, which it does not follow",
                        row.text, row.off
                    ));
                }
                if !texts.insert(row.text.as_str()) {
                    bad.push(format!(
                        "topic: {sid} holds the broadcast {} twice",
                        row.text
                    ));
                }
                if !row.on_log {
                    continue;
                }
                if let Some(prev) = last.insert(topic, row.off).filter(|prev| *prev >= row.off) {
                    bad.push(format!(
                        "topic: {sid}'s broadcasts on {topic} out of order: @{prev}, then @{}",
                        row.off
                    ));
                }
                if let Some(f) = self.follows.iter().find(|f| {
                    f.sid == *sid
                        && f.topic == topic
                        && session.bus_log.as_deref() == Some(f.log.as_str())
                }) {
                    if row.off < f.since {
                        bad.push(format!(
                            "topic: {sid} was handed {} @{} on {topic}, below the @{} it \
                             follows it from",
                            row.text, row.off, f.since
                        ));
                    }
                }
            }
        }
        bad
    }

    /// THE FOLLOWER'S LAW, ONCE SETTLED (item F4): a session aterm hosts that follows a topic,
    /// on the log it followed it on, was handed every broadcast of the topic from its `since=`
    /// on that the broker holds; and each one retention gave up that it was not handed lies in
    /// a `link gap … sid=<it> topic=<t>` aterm was told. Every retained record, or an honest
    /// gap notice: never a silent skip.
    fn topics_followed(&mut self) -> Vec<String> {
        let log = self.core.broker.log_identity();
        let records = self.core.broker.records();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut told = 0;
        for f in self.follows.iter().filter(|f| f.log == log) {
            let Some(session) = s.sessions.values().find(|x| x.sid == f.sid) else {
                continue;
            };
            let suffix = format!("/say/{}", f.topic);
            for (off, subject, body, _) in &records {
                if *off < f.since
                    || !subject.starts_with(&format!("/f/{FLEET}/pub/"))
                    || !subject.ends_with(&suffix)
                {
                    continue;
                }
                let text = Body::decode(body).0.text;
                if session.rows.iter().any(|r| r.off == *off && r.text == text) {
                    continue;
                }
                if self.core.broker.holds(*off) {
                    bad.push(format!(
                        "topic: {} follows {} since @{} and was never handed {text} @{off}, \
                         which the broker holds",
                        f.sid, f.topic, f.since
                    ));
                } else if s.gaps.iter().any(|g| {
                    g.sid.as_deref() == Some(f.sid.as_str())
                        && g.topic.as_deref() == Some(f.topic.as_str())
                        && (g.from..g.to).contains(off)
                }) {
                    told += 1;
                } else {
                    bad.push(format!(
                        "topic: {} follows {} since @{}: {text} @{off} was given up untold, a \
                         silent skip (told {:?})",
                        f.sid, f.topic, f.since, s.gaps
                    ));
                }
            }
        }
        self.stats.broadcasts_given_up_told = told;
        self.stats.broadcasts_handed = s
            .sessions
            .values()
            .chain(s.gone.iter())
            .map(|x| x.rows.iter().filter(|r| r.topic.is_some()).count() as u64)
            .sum();
        bad
    }

    /// THE PEER SAYS ON A TOPIC ([`Opts::topics`]): one broadcast of its session [`SAYER`]
    /// on one of [`TOPICS`], on the subject every node's bridge reads (`/f/<F>/pub/<node>/
    /// <sid>/say/<topic>`).
    fn say(&mut self) {
        let Some(topic) = self.say_rng.pick(&TOPICS).copied() else {
            return;
        };
        self.marker += 1;
        let marker = format!("b{}", self.marker);
        self.peer_seq += 1;
        let subject = format!("/f/{FLEET}/pub/{PEER}/{SAYER}/say/{topic}");
        let body = format!("v=1 t={} text={marker}", self.core.wall_ms());
        let landed =
            self.core
                .broker
                .publish_told(producer(PEER), self.peer_seq, &subject, body.as_bytes());
        self.note(format!("say {marker} on {topic}: {landed:?}"));
        // An outcome astream calls unknown is a broadcast said: a reopen may keep its record.
        if landed.is_ok() {
            self.stats.says += 1;
        }
        if landed != Err(false) {
            self.says.insert(marker, topic.to_string());
        }
    }

    /// A SESSION FOLLOWS A TOPIC ([`Opts::topics`]) it does not follow yet, from an offset of
    /// its own (`topic add <t> since=@<n>`): the head half the time, else the broker's base,
    /// the log's start, or an offset in between, so a follower is owed broadcasts retention
    /// gave up below the base as often as ones it holds. aterm says so on the push lane, and
    /// `topic ls` names it from now on (a bridge that missed the line samples it).
    fn follow(&mut self) {
        let sessions = self.sessions();
        let Some((local, sid)) = self.say_rng.pick(&sessions).cloned() else {
            return;
        };
        let Some(topic) = self.say_rng.pick(&TOPICS).copied() else {
            return;
        };
        let head = lock(&self.core.broker.state).log.len() as u64;
        let base = self.core.broker.base();
        let since = match self.say_rng.below(8) {
            0..=3 => head,
            4 => base,
            5 => 0,
            _ => self.say_rng.below(head + 1),
        };
        if !self.ep.follow(local, topic, &format!("@{since}")) {
            return;
        }
        self.stats.follows += 1;
        if since < base {
            self.stats.follows_below_the_base += 1;
        }
        // Its offset is on the log the session was last told (`link log`), as aterm's sessions
        // read offsets: a move the bridge has yet to tell is the bridge's to apply to it; a
        // session told none yet names the broker's.
        let told = lock(&self.ep.state)
            .sessions
            .get(&local)
            .and_then(|x| x.bus_log.clone())
            .filter(|_| !self.opts.mutants.contains(&Mutant::FollowOnTheBrokersLog));
        self.follows.push(Follow {
            sid: sid.clone(),
            topic: topic.to_string(),
            since,
            log: told.unwrap_or_else(|| self.core.broker.log_identity()),
        });
        self.note(format!(
            "follow {sid} {topic} since=@{since} (base {base}, head {head})"
        ));
        self.push_event(&format!("EVENT {local} topic add {topic} since=@{since}"));
    }

    /// ATERM HANDS ITS SESSIONS OFF to a successor in the same boot ([`Opts::handoffs`]): the
    /// bridge is told `bye handoff`, then aterm's lane closes and the bridge ends with its run
    /// record open (`Bridge::end_run`); its process goes, and the next bridge serves the same
    /// sessions, their rows and their state dir as the first left them.
    fn handoff(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        self.stats.handoffs += 1;
        self.note("aterm hands off");
        live.mailbox
            .note_bye(aterm_types::fabric_grammar::Bye::Handoff);
        self.push_event("EVENT * bye handoff");
        let acked = lock(&self.ep.state).byes_acked;
        for _ in 0..8 {
            if self.live.is_none() || lock(&self.ep.state).byes_acked > acked {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        if let Some(live) = &self.live {
            live.mailbox.push_aterm_closed();
        }
        for _ in 0..4 {
            if self.live.is_none() {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        self.kill();
    }

    /// A session marks its rows seen, deciding its newest ask now and then.
    fn seen(&mut self) {
        let sessions = self.sessions();
        let Some((local, sid)) = self.rng.pick(&sessions).cloned() else {
            return;
        };
        let verdict = match self.rng.below(3) {
            0 => None,
            1 => Some("handled"),
            _ => Some("refused"),
        };
        self.mark_seen(local, &sid, verdict);
    }

    /// A SESSION DECIDES AN ASK IT HOLDS ([`Opts::deadlines`]), from the asks' stream: one
    /// that holds an undecided ask marks its rows seen with a verdict, so some asks are
    /// answered before their deadline and some after.
    fn decide(&mut self) {
        let deciding: Vec<(u64, String)> = {
            let s = lock(&self.ep.state);
            s.sessions
                .iter()
                .filter(|(_, x)| {
                    let decided: BTreeSet<u64> = x.receipts.iter().map(|r| r.off).collect();
                    x.rows
                        .iter()
                        .any(|r| r.kind == "ask" && r.on_log && !decided.contains(&r.off))
                })
                .map(|(local, x)| (*local, x.sid.clone()))
                .collect()
        };
        let Some((local, sid)) = self.ask_rng.pick(&deciding).cloned() else {
            return;
        };
        let verdict = if self.ask_rng.chance(1, 2) {
            "handled"
        } else {
            "refused"
        };
        self.mark_seen(local, &sid, Some(verdict));
    }

    /// Session `local` (`sid`) marks its rows seen, deciding its newest undecided ask with
    /// `verdict` where one is given, and aterm says so on the push lane.
    fn mark_seen(&mut self, local: u64, sid: &str, verdict: Option<&str>) {
        let Some(line) = self.ep.seen(local, verdict) else {
            return;
        };
        if line.contains("verdict=") {
            self.stats.receipts += 1;
        }
        self.note(format!("seen {sid}: {line}"));
        self.push_event(&line);
    }

    /// The person halts the fleet, or lifts the halt.
    fn halt(&mut self) {
        let on = !self.halt_rows.last().is_some_and(|(_, on)| *on);
        let marker = self.next_marker();
        self.person_seq += 1;
        let subject = crate::subject::person_halt(FLEET, PERSON);
        let body = format!(
            "v=1 t={} state={} reason={marker}",
            self.core.wall_ms(),
            if on { "on" } else { "off" }
        );
        let landed = self.core.broker.publish_told(
            producer(PERSON),
            self.person_seq,
            &subject,
            body.as_bytes(),
        );
        self.note(format!(
            "halt {} {marker}: {landed:?}",
            if on { "on" } else { "off" }
        ));
        // An outcome astream calls unknown is a press made: a reopen may keep its record.
        if landed.is_ok() {
            self.stats.halts += 1;
        }
        if landed != Err(false) {
            self.halt_rows.push((marker, on));
        }
    }

    /// The broker stops, as `stop` says, and the law is checked on what it kept.
    fn broker_stop(&mut self, stop: Stop) {
        if !lock(&self.core.broker.state).up {
            return;
        }
        let mut rng = self.rng.fork(0xb0);
        let r = self.core.broker.stop(stop, &mut rng);
        self.stats.broker_restarts += 1;
        self.book_restart(&r);
    }

    /// What a restart took, booked for the laws, and [`bus::law_check`] on it: a stop's
    /// ([`World::broker_stop`]) or a retired log's ([`World::broker_start`]).
    fn book_restart(&mut self, r: &bus::Restart) {
        if r.another_log {
            self.stats.another_logs += 1;
        } else if r.generation_after != r.generation_before {
            self.stats.moves += 1;
        }
        self.stats.lost_records += r.before.len() as u64 - r.kept;
        if r.kept < r.before.len() as u64 {
            self.lost_floor = Some(self.lost_floor.map_or(r.kept, |f| f.min(r.kept)));
        }
        // AN OFFSET A RESTART GAVE UP NAMES ANOTHER RECORD NEXT (ATERM-KI-1115-2409): the
        // `expired` that lands there is judged as its own.
        self.expiries_judged.retain(|&off| off < r.kept);
        // The reads the endpoint took this step, before the restart, are lowered with the rest.
        self.reads_past.extend(self.ep.take_read_past());
        for (_, read) in &mut self.reads_past {
            read.head = read.head.min(r.kept);
        }
        // THE VERDICTS THE STOP TOOK (item F4): an answer or an `expired` acked past what the
        // stop kept is gone with it (what a weak tier lets a power loss take, P3), and nothing
        // publishes it again, as nothing does a retired post past `lost_floor`.
        for rec in r
            .before
            .iter()
            .skip(usize::try_from(r.kept).unwrap_or(usize::MAX))
            .filter(|rec| matches!(rec.kind, bus::Kind::Plain))
        {
            let Some((asker, _, kind)) = in_lane(&rec.subject) else {
                continue;
            };
            if kind == "expired" || ANSWER_KINDS.contains(&kind) {
                if let Some(re) = Body::decode(&rec.body).0.re {
                    self.verdicts_lost.insert((asker.to_string(), re));
                }
            }
        }
        let oracle = lock(&self.core.broker.state).oracle.clone();
        for v in bus::law_check(r, &oracle) {
            self.violations.push(format!("broker law: {v}"));
        }
    }

    /// The broker opens again. In a world of retention's draws one open in four sets no
    /// rules, as `aterm link broker` serves when setting them fails, and of the rest one in
    /// four sets changed rules ([`changed_rules`], as `asb serve --retain-file` would, and the
    /// open after it aterm's again); one that sets them ticks at once, as astream's first tick
    /// is asked for when its rules are set.
    /// The remints the endpoint forgot across, unproved: the ids they minted that a session
    /// forgot what it held across ([`super::endpoint::EpState::forgot_across`]).
    fn remints_forgotten(&self) -> u64 {
        let ep = lock(&self.ep.state);
        self.reminted_ids.intersection(&ep.forgot_across).count() as u64
    }

    fn broker_start(&mut self) {
        // THE LOG SET ASIDE while the broker is down ([`Opts::retire_logs`]): every record it
        // held is gone, so every offset at or above 0 may name another record now.
        let mut retired = false;
        if self.opts.retire_logs && self.log_rng.chance(1, 6) {
            if let Some(r) = self.core.broker.retire_log() {
                retired = true;
                self.stats.retired_logs += 1;
                self.note("broker log retired (--retire-log)");
                self.book_restart(&r);
            }
        }
        // THE LOG REMINTED KEEPING ITS RECORDS ([`Opts::remints`]): another id, every record kept.
        let mut reminted = false;
        if !retired && self.opts.remints && self.remint_rng.chance(1, 4) {
            if let Some(r) = self.core.broker.lose_identity() {
                reminted = true;
                self.stats.remints += 1;
                self.note("broker identity unreadable: another log, every record kept");
                self.book_restart(&r);
            }
        }
        let drawn = self.opts.retained && self.opts.cuts;
        // A world of strays opens with rules every time: on a broker that set none nothing
        // drops a stray record's dedup key, so nothing ends the hold it puts on a sender
        // ([`Opts::strays`]).
        if drawn && !self.opts.strays && self.ret_rng.chance(1, 4) {
            self.stats.unretained_starts += 1;
            self.core.broker.start_unretained();
        } else if drawn && self.rules_rng.chance(1, 4) {
            self.stats.changed_starts += 1;
            self.core.broker.start_with(changed_rules(self.opts.scaled));
        } else {
            self.core.broker.start();
        }
        if reminted {
            let id = lock(&self.core.broker.state).id;
            self.reminted_ids.insert(
                astream_broker::LogIdentity { id, generation: 0 }
                    .to_string()
                    .split_once('.')
                    .map_or_else(String::new, |(id, _)| id.to_string()),
            );
        }
        self.tick_now();
    }

    /// A RETENTION TICK at the world's wall clock ([`bus::Broker::tick`]).
    fn tick_now(&mut self) {
        let now = self.core.wall_ms();
        self.last_tick_ms = now;
        self.core.broker.tick(now);
    }

    /// RETENTION (sim item B05), once a step: in the fault phase of a world of its draws a
    /// tick, a cut, or days passing, from a stream of its own; and in every world, the
    /// tick every minute of the world's clock runs.
    fn retention_step(&mut self, faults: bool) {
        if !self.opts.retained {
            return;
        }
        if faults && self.opts.cuts {
            match self.ret_rng.below(80) {
                0..=5 => self.tick_now(),
                6 | 7 => {
                    let (base, head) = (
                        self.core.broker.base(),
                        lock(&self.core.broker.state).log.len() as u64,
                    );
                    if head > base {
                        let at = base + 1 + self.ret_rng.below(head - base);
                        self.note(format!("cut at {at}"));
                        self.core.broker.cut_to(at);
                    }
                }
                8 => {
                    let d = Duration::from_secs(
                        86_400 * (1 + self.ret_rng.below(3)) + 3_600 * self.ret_rng.below(24),
                    );
                    self.stats.days_passed += 1;
                    self.note(format!("time +{}h (days pass)", d.as_secs() / 3_600));
                    self.core.advance(d);
                }
                _ => {}
            }
        }
        if self.core.wall_ms().saturating_sub(self.last_tick_ms) >= TICK_EVERY_MS {
            self.tick_now();
        }
    }

    /// Every session of this aterm ends ([`Endpoint::power_cut`]), and with it every read past
    /// ([`World::fetch_read_past`]): no reader of the order is left, and a copy a power cut lands
    /// (a clear it undid on the media) reaches none of them.
    fn sessions_end(&mut self) {
        drop(self.ep.take_read_past());
        self.reads_past.clear();
        self.ep.power_cut();
    }

    /// A POWER CUT of the whole machine: the bridge dies, the broker loses
    /// what the law lets it, the state dir keeps what the device lets it,
    /// aterm and its sessions are gone, and the next boot begins.
    fn power_cut(&mut self) {
        self.stats.power_cuts += 1;
        self.note("power cut");
        self.kill();
        if lock(&self.core.broker.state).up {
            self.broker_stop(Stop::PowerLoss);
        }
        let mut rng = self.rng.fork(0xdc);
        for n in self.core.device.power_cut(&mut rng) {
            if n.starts_with("power-cut logpin ") && n.ends_with(" torn") {
                self.stats.pins_torn += 1;
            }
            self.note(n);
        }
        self.sessions_end();
        self.boot = self.boot.wrapping_add(1).max(1);
        self.mark = boot_mark(std::path::Path::new(&self.cfg.state_dir), self.boot);
        self.core.broker.start();
        self.tick_now();
        self.open_sessions();
    }

    /// Quits and restarts aterm in the same boot ([`Opts::quits`]), in aterm's order (fix unit
    /// Q1, `fabric_launch::quit_in_order`): every live session's log closes `out=app-quit`, the
    /// `quit-closes` line wakes the bridge's mirror, and, while the bridge can land them (its
    /// link as aterm's `mirror_lands_now` reads it, [`Endpoint::mirror_lands_now`]), the quit
    /// waits a bounded count of passes for the bus to hold every live log through its close,
    /// then says `bye quit`. The law ([`World::judge_quit_order`]) holds each close the quit
    /// made before its withdrawal.
    fn quit(&mut self) {
        if self.live.is_none() {
            return;
        }
        self.stats.quits += 1;
        self.note("aterm quits");
        let start = self.log().len() as u64;
        let closed = self.ep.close_logs_at_quit(self.stamp());
        let mut landed = closed.is_empty();
        if !closed.is_empty() {
            self.push_event("EVENT * quit-closes");
            for _ in 0..8 {
                landed = self.live.is_some() && self.ep.quit_closes_landed();
                if landed || self.live.is_none() || !self.ep.mirror_lands_now() {
                    break;
                }
                self.pass();
                self.collect_notes();
            }
            landed = self.live.is_some() && self.ep.quit_closes_landed();
            if landed {
                self.stats.quit_closes_landed += 1;
            }
        }
        let bye_at = self.log().len() as u64;
        if let Some(live) = &self.live {
            live.mailbox
                .note_bye(aterm_types::fabric_grammar::Bye::Quit);
        }
        self.push_event("EVENT * bye quit");
        let acked = lock(&self.ep.state).byes_acked;
        for _ in 0..8 {
            if self.live.is_none() || lock(&self.ep.state).byes_acked > acked {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        if let Some(live) = &self.live {
            live.mailbox.push_aterm_closed();
        }
        for _ in 0..4 {
            if self.live.is_none() {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        let bad = self.judge_quit_order(&closed, start, landed.then_some(bye_at));
        self.violations.extend(bad);
        self.kill();
        let run = crate::state::StateDir::peek(&self.cfg.state_dir).last_run();
        if matches!(run, Ok(Some(run)) if run.clean) {
            self.stats.clean_stops += 1;
        }
        self.sessions_end();
        self.open_sessions();
    }

    /// ONE STEP OF THE FAULT PHASE.
    fn fault_step(&mut self) {
        let up = lock(&self.core.broker.state).up;
        let readable = self.core.readable();
        let alive = self.live.is_some();
        let mut menu: Vec<(u64, u8)> =
            vec![(6, 2), (6, 3), (4, 4), (2, 5), (2, 13), (1, 16), (1, 17)];
        if self.opts.mirror {
            menu.push((6, 15));
        }
        if alive {
            menu.push((40, 0));
            menu.push((2, 6));
            menu.push((2, 7));
        } else {
            menu.push((12, 8));
        }
        if !readable.is_empty() {
            menu.push((25, 1));
        }
        if up {
            menu.push((1, 9));
            menu.push((1, 14));
            if self.tier == Tier::Ordered {
                menu.push((2, 12));
            }
        } else {
            menu.push((8, 10));
        }
        if self.opts.power_cuts {
            menu.push((self.opts.cut_weight, 11));
        }
        if self.opts.hold_acks {
            menu.push((4, 18));
        }
        if alive && self.opts.quits {
            menu.push((1, 20));
        }
        if self.opts.write_errors {
            menu.push((if self.core.device.is_full() { 4 } else { 1 }, 19));
        }
        if self.opts.topics {
            menu.push((4, 21));
            menu.push((1, 22));
        }
        if alive && self.opts.handoffs {
            menu.push((1, 23));
        }
        if self.opts.keys {
            menu.push((3, 24));
        }
        let total: u64 = menu.iter().map(|(w, _)| w).sum();
        let mut draw = self.rng.below(total);
        let mut action = 0;
        for (w, a) in &menu {
            if draw < *w {
                action = *a;
                break;
            }
            draw -= w;
        }
        match action {
            0 => self.pass(),
            1 => {
                let pick = self.rng.pick(&readable).map(|(c, _)| *c);
                if let Some(conn) = pick {
                    self.stats.pumps += 1;
                    self.core.pump(conn);
                }
            }
            2 => self.peer_mail(),
            3 => self.post(),
            4 => self.seen(),
            5 => self.halt(),
            6 => self.kill(),
            7 => {
                if let Some(live) = &self.live {
                    let k = 1 + self.rng.below(12);
                    self.note(format!("arm kill pid={} in {k}", live.process.pid));
                    self.core.arm_kill(live.process.pid, k);
                }
            }
            8 => self.launch(),
            9 => {
                let kinds = if self.opts.bus_losses { 3 } else { 2 };
                let stop = match self.rng.below(kinds) {
                    0 => Stop::Clean,
                    1 => Stop::Crash,
                    _ => Stop::PowerLoss,
                };
                self.broker_stop(stop);
            }
            10 => self.broker_start(),
            11 => self.power_cut(),
            12 => self.core.broker.horizon(),
            14 => {
                let n = 1 + u32::try_from(self.rng.below(2)).unwrap_or(0);
                if self.opts.refused_batches {
                    self.core.broker.refuse_next(n);
                    self.note(format!(
                        "broker refuses the next {n} of the node's publishes and commits"
                    ));
                }
            }
            15 => self.log_record(),
            16 => self.open_one(),
            17 => self.exit_one(),
            18 => {
                self.core.broker.answer_held(false);
            }
            19 => {
                let full = !self.core.device.is_full();
                self.core.device.fill(full);
                if full {
                    self.stats.disk_full_spells += 1;
                }
                self.note(if full { "disk full" } else { "disk has room" });
            }
            20 => self.quit(),
            21 => self.say(),
            22 => self.follow(),
            23 => self.handoff(),
            24 => self.repost(),
            _ => {
                let d = Duration::from_millis(100 + self.rng.below(20_000));
                self.note(format!("time +{}ms", d.as_millis()));
                self.core.advance(d);
            }
        }
        if self.opts.fetches && self.fetch_rng.chance(1, 5) {
            self.fetch();
        }
        if self.opts.deadlines && self.ask_rng.chance(1, 6) {
            self.decide();
        }
        if self.opts.poisons {
            self.poison_step();
        }
        // Before a tick can take what an `expired` is judged against.
        self.judge_expiries();
        self.retention_step(true);
        self.collect_notes();
        self.check_always();
    }

    /// A POISONED BROKER'S DRAWS ([`Opts::poisons`]), from [`World::log_rng`]: on a weak tier
    /// that is up and healthy, now and then the next full flush fails; and a poisoned broker is
    /// stopped and opened again, as its supervisor restarts an `aterm link broker` that exited 75
    /// on its poison. The stop is a crash: a power cut drawn while it is poisoned stays one.
    fn poison_step(&mut self) {
        let (up, healthy, owed) = {
            let s = lock(&self.core.broker.state);
            (s.up, s.poison.is_none(), s.fail_full_flush)
        };
        if up && healthy && !owed && self.tier != Tier::Strict && self.log_rng.chance(1, 40) {
            self.note("broker: the next full flush fails");
            self.core.broker.fail_next_full_flush();
        }
        if up && !healthy && self.log_rng.chance(1, 3) {
            self.restart_poisoned();
        }
    }

    /// A POISONED BROKER RESTARTED by its supervisor: a crash and an open.
    fn restart_poisoned(&mut self) {
        self.stats.poison_restarts += 1;
        self.note("broker poisoned: its supervisor restarts it");
        self.broker_stop(Stop::Crash);
        self.broker_start();
    }

    /// A fingerprint of everything that can still move.
    fn fingerprint(&self) -> (usize, usize, usize, usize, usize) {
        let head = lock(&self.core.broker.state).log.len();
        let s = lock(&self.ep.state);
        let rows: usize = s.sessions.values().map(|x| x.rows.len()).sum();
        let open_posts: usize = s
            .sessions
            .values()
            .map(|x| x.posts.iter().filter(|p| p.retired.is_none()).count())
            .sum();
        let open_receipts: usize = s
            .sessions
            .values()
            .map(|x| x.receipts.iter().filter(|r| r.retired.is_none()).count())
            .sum();
        let holds = s
            .sessions
            .values()
            .filter(|x| x.fleet_hold.is_some())
            .count();
        (head, rows, open_posts, open_receipts, holds)
    }

    /// THE SETTLE PHASE: no more faults; run until nothing moves.
    fn settle(&mut self) {
        self.core.disarm_kill();
        self.note("settle");
        self.core.device.clean_disk();
        // A fault armed and not met is disarmed, and a poisoned broker its supervisor restarts.
        lock(&self.core.broker.state).fail_full_flush = false;
        if self.core.broker.poisoned() {
            self.restart_poisoned();
        }
        if !lock(&self.core.broker.state).up {
            self.core.broker.start();
            self.tick_now();
        }
        self.core.broker.answer_held(true);
        let mut started = self.core.elapsed();
        let mut last = self.fingerprint();
        let mut still = 0;
        let mut weeks = 0;
        let budget = Duration::from_secs(1800);
        for _ in 0..20_000 {
            if self.core.elapsed().saturating_sub(started) > budget {
                break;
            }
            if self.live.is_none() {
                self.launch();
                continue;
            }
            let readable = self.core.readable();
            if let Some(&(conn, _)) = readable.first() {
                self.stats.pumps += 1;
                self.core.pump(conn);
            } else {
                self.pass();
            }
            self.judge_expiries();
            self.retention_step(false);
            self.collect_notes();
            self.check_always();
            let now = self.fingerprint();
            let waited = self.core.elapsed().saturating_sub(started);
            if now == last && readable.is_empty() {
                still += 1;
                // A MIRROR WAITING ON ITS WINDOW BUDGET in a quiet world waits for the age
                // rule: a week passes (the settle's budget does not count it), and the tick
                // after it gives the oldest up (sim item B05).
                if still >= 60 && weeks < 2 && self.mirror_waits_on_its_budget() {
                    weeks += 1;
                    let d = crate::broker_rules::MAX_AGE + Duration::from_secs(86_400);
                    self.note(format!(
                        "time +{}h (a week passes: the mirror waits on its window budget)",
                        d.as_secs() / 3_600
                    ));
                    self.core.advance(d);
                    started += d;
                    self.tick_now();
                    still = 0;
                    continue;
                }
                // A SENDER HELD BEHIND A STRAY RECORD'S DEDUP KEY in a quiet world waits for
                // the cut that drops that key (`Bridge::unverified_publication`'s case 5): a
                // week passes, as for the mirror, and the hold's next retry appends anew.
                if still >= 60 && weeks < 2 && self.waits_on_a_stray(now) {
                    weeks += 1;
                    self.stats.stray_weeks += 1;
                    let d = crate::broker_rules::MAX_AGE + Duration::from_secs(86_400);
                    self.note(format!(
                        "time +{}h (a week passes: a sender waits on a stray record's key)",
                        d.as_secs() / 3_600
                    ));
                    self.core.advance(d);
                    started += d;
                    self.tick_now();
                    still = 0;
                    continue;
                }
                if still >= 60
                    && waited >= Duration::from_secs(90)
                    && now.2 == 0
                    && now.3 == 0
                    && (waited >= Duration::from_secs(600) || self.converged())
                {
                    return;
                }
            } else {
                still = 0;
                last = now;
            }
        }
        let (head, rows, posts, receipts, holds) = self.fingerprint();
        self.violations.push(format!(
            "liveness: the world did not settle (head {head}, rows {rows}, posts queued {posts}, \
             receipts owed {receipts}, holds {holds})"
        ));
    }

    /// The model broker's log as it stands ([`log_of`]).
    fn log(&self) -> std::cell::Ref<'_, Vec<Seen>> {
        log_of(&self.core.broker, &self.reads)
    }

    /// The text markers of the bus's records on this node's and the person's
    /// lanes: `marker -> [offset]`.
    fn bus_markers(&self) -> std::cell::Ref<'_, BTreeMap<String, Vec<u64>>> {
        let lanes = format!("/f/{FLEET}/in/");
        let log = self.log();
        self.reads.bus_markers.of(&log, |out, off, rec| {
            if rec.hidden || !rec.subject.starts_with(&lanes) {
                return;
            }
            let text = &rec.fabric().text;
            if is_marker(text) {
                out.entry(text.clone()).or_default().push(off as u64);
            }
        })
    }

    /// The records of the log as the model's tap reads them ([`conform::View::records`]).
    fn inbox_records(&self) -> std::cell::Ref<'_, Vec<InboxRecord>> {
        let lane = format!("/f/{FLEET}/in/{NODE}/");
        let log = self.log();
        self.reads.inbox_records.of(&log, |out, _, r| {
            out.push((|| {
                let rest = r.subject.strip_prefix(lane.as_str())?;
                let sid = rest.split('/').next()?;
                Some((sid.to_string(), r.fabric().text.clone()))
            })());
        })
    }

    /// Where each text of [`World::inbox_records`] stands ([`conform::View::texts`]).
    fn inbox_texts(&self) -> std::cell::Ref<'_, conform::Texts> {
        let records = self.inbox_records();
        let log = self.log();
        self.reads.inbox_texts.of(&log, |texts, off, _| {
            conform::add_text(texts, off, records[off].as_ref());
        })
    }

    /// What the model's tap reads of the world now ([`conform::View`]): `records` as
    /// [`World::inbox_records`] reads them, and the sessions of `ep`, the endpoint's state.
    fn view<'a>(
        &self,
        records: &'a [InboxRecord],
        texts: &'a conform::Texts,
        ep: &'a EpState,
    ) -> conform::View<'a> {
        let ident = {
            let s = lock(&self.core.broker.state);
            astream_broker::LogIdentity {
                id: s.id,
                generation: s.generation,
            }
        };
        let group = crate::subject::inbox_group(FLEET, NODE);
        let sessions = ep
            .sessions
            .values()
            .map(|s| {
                (
                    s.sid.as_str(),
                    conform::SessionView {
                        dedup: &s.dedup,
                        rows: &s.rows,
                        log: s.bus_log.as_deref(),
                    },
                )
            })
            .collect();
        conform::View {
            records,
            texts,
            gone: self.core.broker.gone(),
            log: ident.to_string(),
            // The identity's moves so far, another log's among them: astream numbers another
            // log's generations from 0 again, and to the model it is a move that kept nothing
            // a reader can continue from (`FabricExactlyOnce`'s `g`).
            generation: self.stats.moves + self.stats.another_logs,
            retired_logs: self.stats.retired_logs,
            forgot_remints: self.reminted_ids.intersection(&ep.forgot_across).count() as u64,
            group_start: self.core.broker.group_start(&group),
            alive: self.live.is_some(),
            owed: self
                .live
                .as_ref()
                .and_then(|l| l.bridge.owed_inbox_commit())
                .and_then(|(upto, on)| on.is_none_or(|on| on == ident).then_some(upto)),
            sessions,
        }
    }

    /// The last presence row of every session face under this node, `sid ->
    /// (state=, inc=, statedir=)`: the face is a last-value one, so the row at the
    /// highest offset is what every reader takes for the session.
    fn presence_rows(&self) -> std::cell::Ref<'_, BTreeMap<String, PresenceRow>> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let log = self.log();
        self.reads.presence_rows.of(&log, |out, _, rec| {
            if rec.hidden {
                return;
            }
            let Some(sid) = rec
                .subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/presence"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                return;
            };
            let b = rec.fabric();
            let state = b.unknown.get("state").cloned().unwrap_or_default();
            let inc = b.unknown.get("inc").and_then(|i| crate::parse_uint(i));
            let mark = b.unknown.get(crate::state::PRESENCE_MARK).cloned();
            out.insert(sid.to_string(), (state, inc, mark));
        })
    }

    /// The running bridge's `(me, witnessed)` incarnation pair read from the bus.
    fn running_bridge(&self) -> Option<(u64, Option<(u64, u64)>)> {
        self.live.as_ref()?;
        let rows = self.node_rows();
        let owner = self
            .mark
            .as_deref()
            .filter(|m| m.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|mark| u64::from_str_radix(mark, 16).ok())?;
        let (state, me, _) = rows
            .iter()
            .rev()
            .find(|(_, _, mark)| *mark == Some(owner))
            .cloned()?;
        let me = me?;
        (state == "live").then(|| (me, witnessed_by(&rows, me)))
    }

    /// THE SIBLING'S INCARNATION, once it attached: the scheduled one's
    /// ([`Opts::sibling`]) or the racing one's ([`Opts::race`]).
    fn sibling(&self) -> Option<u64> {
        self.sibling_inc.or_else(|| {
            lock(&self.core.broker.state)
                .race
                .as_ref()
                .and_then(|race| race.raced)
        })
    }

    /// Every row of this node's own presence face, in log order: `(state=,
    /// inc=)`.
    fn node_rows(&self) -> std::cell::Ref<'_, Vec<NodeRow>> {
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        let log = self.log();
        self.reads.node_rows.of(&log, |out, _, rec| {
            if rec.hidden || rec.subject != face {
                return;
            }
            let b = rec.fabric();
            let state = b.unknown.get("state").cloned().unwrap_or_default();
            let inc = b.unknown.get("inc").and_then(|i| crate::parse_uint(i));
            let mark = b
                .unknown
                .get(crate::state::PRESENCE_MARK)
                .filter(|m| m.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            out.push((state, inc, mark));
        })
    }

    /// PRESENCE, once settled: every hosted session's row says `live`, and no
    /// departed session's row the running bridge can prove its own still does.
    fn presence_gaps(&self) -> (Vec<String>, Vec<String>) {
        let rows = self.presence_rows();
        let running = self.running_bridge();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut left = Vec::new();
        if let (Some(sibling), Some((state, inc, _))) = (self.sibling(), rows.get(SIBLING_SID)) {
            let owner = self
                .sibling_mark
                .strip_prefix(" statedir=")
                .filter(|m| m.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            let node = self.node_rows();
            let own_mark = self
                .mark
                .as_deref()
                .filter(|m| m.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            let ours = node
                .iter()
                .filter(|(state, _, mark)| {
                    state == "live" && own_mark.is_some() && *mark == own_mark
                })
                .filter_map(|(_, i, _)| *i)
                .collect::<BTreeSet<u64>>();
            let witnessed = ours.iter().any(|i| {
                owner.is_some_and(|mark| witnessed_by(&node, *i) == Some((sibling, mark)))
            });
            if state != "live" && !witnessed {
                bad.push(format!(
                    "presence: the sibling's {SIBLING_SID} (inc={inc:?}) was retired, and no \
                     attach here witnessed its will: its row says {state:?}"
                ));
            }
            if state == "live"
                && running
                    .is_some_and(|(_, w)| owner.is_some_and(|mark| w == Some((sibling, mark))))
            {
                bad.push(format!(
                    "presence: the sibling's {SIBLING_SID} is dead and its row (inc={inc:?}) \
                     still says live, a ghost the running bridge may retire"
                ));
            }
        }
        for session in s.sessions.values() {
            let row = rows.get(&session.sid).map(|(state, _, _)| state.as_str());
            if row != Some("live") {
                bad.push(format!(
                    "presence: {} is hosted and its row says {row:?}",
                    session.sid
                ));
            }
        }
        for session in &s.gone {
            let Some((state, inc, mark)) = rows.get(&session.sid) else {
                continue;
            };
            if state != "live" {
                continue;
            }
            if inc.is_some_and(|inc| {
                running.is_some_and(|(me, witnessed)| {
                    (inc <= me && self.mark.is_some() && mark == &self.mark)
                        || mark
                            .as_deref()
                            .filter(|m| m.bytes().all(|b| b.is_ascii_hexdigit()))
                            .and_then(|mark| u64::from_str_radix(mark, 16).ok())
                            .is_some_and(|mark| witnessed == Some((inc, mark)))
                })
            }) {
                bad.push(format!(
                    "presence: {} is gone and its row (inc={inc:?}) still says live, a ghost \
                     the running bridge may retire",
                    session.sid
                ));
            } else {
                left.push(session.sid.clone());
            }
        }
        (bad, left)
    }

    /// A CLEAN QUIT'S CLOSE PRECEDES ITS WITHDRAWAL (fix unit Q1), judged as the quit ends,
    /// before anything can take a record from the log. Where the quit's closes landed before its
    /// bye (`bye_at`, the log's length as the bye was said), each session it closed (`closed`)
    /// has its own `out=app-quit` close on its `ev` face among the records since the quit began
    /// (`start`), below the bye and below every `exited` row the bridge withdrew it with. A quit
    /// that did not wait (its mirror could not land) or whose closes missed its passes is owed
    /// neither: its withdrawal tells the quit from a crash, and a close the mirror lands after
    /// it is still the session's own (a reader takes the close first).
    fn judge_quit_order(&self, closed: &[String], start: u64, bye_at: Option<u64>) -> Vec<String> {
        let Some(bye_at) = bye_at else {
            return Vec::new();
        };
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let log = self.log();
        let since = || {
            log.iter()
                .enumerate()
                .skip(usize::try_from(start).unwrap_or(usize::MAX))
                .filter(|(_, rec)| !rec.hidden)
        };
        let mut bad = Vec::new();
        for sid in closed {
            let face = format!("{prefix}{sid}/ev");
            let close = since().find_map(|(off, rec)| {
                (rec.subject == face
                    && rec.slog().is_some_and(|b| {
                        b.rec.op == aterm_slog::Op::Close
                            && b.rec.node == aterm_slog::Node::SESSION
                            && b.rec.out() == Some("app-quit")
                    }))
                .then_some(off as u64)
            });
            let row = format!("{prefix}{sid}/presence");
            let withdrawn = since().find_map(|(off, rec)| {
                (rec.subject == row
                    && rec.fabric().unknown.get("state").map(String::as_str) == Some("exited"))
                .then_some(off as u64)
            });
            if !close.is_some_and(|c| c < bye_at) {
                bad.push(format!(
                    "quit order: {sid}'s quit close landed before the bye @{bye_at} but the \
                     log holds it at {close:?}"
                ));
            }
            if withdrawn.is_some_and(|w| close.is_some_and(|c| c > w)) {
                bad.push(format!(
                    "quit order: {sid} withdrawn @{withdrawn:?} before its quit close @{close:?}"
                ));
            }
        }
        bad
    }

    /// THE READER'S VIEW of every session's `ev` face, decoded and deduped by
    /// `(ep, seq)` as every reader does: per `ms`, the distinct records that
    /// hold it, and the covers.
    fn mirror_faces(&self) -> std::cell::Ref<'_, BTreeMap<String, Face>> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let log = self.log();
        self.reads.mirror_faces.of(&log, |out, _, rec| {
            if rec.hidden {
                return;
            }
            let Some(sid) = rec
                .subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/ev"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                return;
            };
            let Some(b) = rec.slog() else {
                return;
            };
            let Some(ms) = b.ms else { return };
            let face = out.entry(sid.to_string()).or_default();
            match b.rec.f.get_u(aterm_slog::Key::N) {
                Some(n) if b.rec.op == aterm_slog::Op::Gap => face.covers.push((ms, n)),
                _ => {
                    face.recs.entry(ms).or_default().insert((b.ep, b.rec.seq));
                    *face.raw.entry((b.ep, b.rec.seq)).or_default() += 1;
                }
            }
        })
    }

    /// THE MIRROR'S AT-MOST-ONCE, at every step (`LogMirror`'s
    /// `ReaderViewAtMostOnce`): no `ms` of a session's log is two records in
    /// the reader's view.
    fn mirror_twice(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for (sid, face) in self.mirror_faces().iter() {
            for (ms, ids) in &face.recs {
                if ids.len() > 1 {
                    bad.push(format!(
                        "mirror: {sid}'s ms {ms} is {} records in the reader's view {ids:?}",
                        ids.len()
                    ));
                }
            }
        }
        bad
    }

    /// Checks that the mirror never stopped for its pin.
    fn mirror_stopped_for_its_pin(&self) -> Option<String> {
        let n = lock(&self.ep.state).pin_stops;
        (n > 0).then(|| format!("mirror: stopped for its pin {n} times"))
    }

    /// Across every boot, at every step: every take was answered while the media held a pin of
    /// this build ([`Device::pin_fenced`], critic item D06), however the loop ordered the pass
    /// against its deliveries. That is what an open's fence is for: what the media keeps of the
    /// pin, whatever a power cut does, is this build's. The law judges the media, not each
    /// open: an open that skipped its fence while an earlier open's fenced pin is still the
    /// durable one leaves the media as the fence would, so it breaks nothing stated here. It
    /// counts no send: a slot goes out of a batch a take pinned (`StateDir::set_logpin`, durable
    /// while the open does not hold the fence) or one recovered from the pin on the state dir,
    /// so in these worlds, which seed no older build's pin, no send can precede a durable pin
    /// of this build. D06's review counted such sends over every world of this suite and found
    /// none, nor any in the world where `Mutant::TakeAheadOfTheFence`, which skips the fence
    /// on both paths, breaks this law: a count no rule switched off can raise is no law. The
    /// node's takes named ONE alias key over its whole life, so an edge has
    /// one alias on the bus; and an engaged mirror's total is never below the highest total any
    /// pin of the node was written with ([`Device::pin_total`]) — whatever a power cut tore or
    /// rolled back.
    fn mirror_key_and_total(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let unfenced = lock(&self.ep.state).takes_unfenced;
        if unfenced > 0 {
            bad.push(format!(
                "mirror fence: {unfenced} take(s) answered while the media held no pin of this \
                 build: a take ran ahead of its open's durable pin"
            ));
        }
        let keys = lock(&self.ep.state).take_keys.clone();
        if keys.len() > 1 {
            bad.push(format!(
                "mirror: the node's takes named {} alias keys {keys:?}",
                keys.len()
            ));
        }
        if let Some(total) = self
            .live
            .as_ref()
            .and_then(|live| live.bridge.mirror_engaged_total())
        {
            let counted = self.core.device.pin_total();
            if total < counted {
                bad.push(format!(
                    "mirror total: the running bridge counts {total} bytes, below the {counted} \
                     a pin of the node was written with"
                ));
            }
        }
        bad
    }

    /// THE MIRROR, once settled (`LogMirror`'s `QuiescentMirrorComplete`):
    /// every hosted session's log landed through its `ms` head, and every
    /// `ms` up to it is a record in the reader's view or covered by a gap —
    /// across every move of the log, whose cuts the mirror must say again,
    /// and across every power cut, a torn pin's included.
    fn mirror_gaps(&mut self) -> Vec<String> {
        let faces = self.mirror_faces();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut mirrored = 0;
        for session in s.sessions.values() {
            let Some(log) = session.log.as_ref() else {
                continue;
            };
            let head = log.ms_head();
            if !log.mirror_complete() {
                bad.push(format!(
                    "mirror: {} landed {:?} of ms head {head}",
                    session.sid,
                    log.mirror_landed()
                ));
            }
            let face = faces.get(&session.sid);
            let missing: Vec<u64> = (1..=head)
                .filter(|ms| !face.is_some_and(|f| f.holds(*ms)))
                .collect();
            mirrored += head - missing.len() as u64;
            if let Some(first) = missing.first() {
                bad.push(format!(
                    "mirror: {}'s ms {first} ({} of {head} in all) is neither on the bus nor \
                     covered, landed {:?}",
                    session.sid,
                    missing.len(),
                    log.mirror_landed()
                ));
            }
        }
        drop(s);
        drop(faces);
        self.stats.mirrored = mirrored;
        bad
    }

    /// THE MIRROR'S PIN, once settled, in a world whose log never moved: no
    /// record of a session log is on its `ev` face twice before the reader's
    /// dedup.
    fn mirror_twice_raw(&self) -> Vec<String> {
        let dropped = lock(&self.core.broker.state).ret.dropped.clone();
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        // Each copy's producer key, of each record (sid, ep, seq) of a session log on its face.
        let mut copies: BTreeMap<(String, u32, u64), Keys> = BTreeMap::new();
        for rec in self.log().iter().filter(|rec| !rec.hidden) {
            let key = rec.key;
            let Some(sid) = rec
                .subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/ev"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let Some(b) = rec.slog() else {
                continue;
            };
            if b.ms.is_none()
                || (b.rec.op == aterm_slog::Op::Gap && b.rec.f.get_u(aterm_slog::Key::N).is_some())
            {
                continue;
            }
            copies
                .entry((sid.to_string(), b.ep, b.rec.seq))
                .or_default()
                .push(key);
        }
        let mut bad = Vec::new();
        for ((sid, ep, seq), keys) in copies {
            // A batch republished at its pinned sequences appends a copy again only once a
            // cut dropped the first copy's dedup key (sim item B05): astream's exactly-once
            // holds within its window.
            let first = keys.first().copied().flatten();
            let again =
                first.is_some_and(|f| dropped.contains(&f) && keys.iter().all(|k| *k == Some(f)));
            if keys.len() > 1 && !again {
                bad.push(format!(
                    "mirror: {sid}'s record {ep}:{seq} is on the bus {} times (keys {keys:?}), \
                     and the log never moved",
                    keys.len()
                ));
            }
        }
        bad
    }

    /// Whether the mirror said last that it waits on its window budget (`link mirror budget`)
    /// over a retained broker.
    fn mirror_waits_on_its_budget(&self) -> bool {
        self.opts.retained && lock(&self.ep.state).mirror_said == "budget"
    }

    /// Whether a post stands queued or a receipt owed, in the world `now` fingerprints
    /// ([`World::fingerprint`]), while a stray record's dedup key stands at or above the base
    /// ([`Opts::strays`]) over a retained broker: what a sender held behind it waits for.
    fn waits_on_a_stray(&self, now: (usize, usize, usize, usize, usize)) -> bool {
        if !self.opts.strays || !self.opts.retained || (now.2 == 0 && now.3 == 0) {
            return false;
        }
        let base = self.core.broker.base();
        self.core.broker.strays().iter().any(|&off| off >= base)
    }

    /// Whether the mirror is owed its sessions' logs once settled: a `turns` mirror always,
    /// an `auto` one over a broker whose last attach found it retaining.
    fn mirror_owed(&self) -> bool {
        !self.opts.auto
            || self
                .live
                .as_ref()
                .and_then(|live| live.bridge.broker_retention_read())
                == Some(true)
    }

    /// AN `auto` MIRROR RUNS ONLY OVER A BROKER THAT RETAINS (sim item B05): no record of a
    /// session log lands on its face while no open set the broker's rules.
    fn auto_mirror_unretained(&self) -> Vec<String> {
        if !(self.opts.mirror && self.opts.auto) {
            return Vec::new();
        }
        let unretained = lock(&self.core.broker.state).ret.unretained.clone();
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        self.log()
            .iter()
            .enumerate()
            .filter(|(off, rec)| {
                // A record of a session's log (the bridge's own events on the face are not).
                !rec.hidden
                    && unretained.contains(&(*off as u64))
                    && rec
                        .subject
                        .strip_prefix(prefix.as_str())
                        .and_then(|rest| rest.strip_suffix("/ev"))
                        .is_some_and(|sid| sid != "node" && !sid.contains('/'))
                    && rec.slog().is_some_and(|b| b.ms.is_some())
            })
            .map(|(off, rec)| {
                format!(
                    "mirror: an auto mirror wrote {} @{off} onto a broker that retains nothing",
                    rec.subject
                )
            })
            .collect()
    }

    /// Whether what settles slowly has: presence (a ghost waits out the
    /// bridge's patience) and the mirror.
    fn converged(&mut self) -> bool {
        let (bad, left) = self.presence_gaps();
        bad.is_empty()
            && left.is_empty()
            && (!self.mirror_owed() || self.mirror_gaps().is_empty())
            && !self.deadlines_pending()
    }

    /// One step of the world, through the model's tap.
    fn observe_model(&mut self) {
        let new: Vec<String> = {
            let records = self.inbox_records();
            let texts = self.inbox_texts();
            let ep = lock(&self.ep.state);
            let view = self.view(&records, &texts, &ep);
            let sent = &self.sent;
            let marker = |text: &str| sent.get(text).is_some_and(|s| s.to.is_some());
            let mut tap = self.tap.borrow_mut();
            let before = tap.violations.len();
            tap.observe(&view, &marker);
            tap.violations[before..].to_vec()
        };
        self.violations.extend(new);
    }

    /// THE CHECKS THAT HOLD AT EVERY STEP.
    fn check_always(&mut self) {
        self.observe_model();
        let mut bad = Vec::new();
        {
            let s = lock(&self.ep.state);
            for session in s.sessions.values().chain(s.gone.iter()) {
                let mut seen: BTreeMap<&str, u64> = BTreeMap::new();
                for row in &session.rows {
                    if is_marker(&row.text) {
                        let n = seen.entry(row.text.as_str()).or_default();
                        *n += 1;
                        if *n == 2 {
                            bad.push(format!(
                                "dup delivery: {} holds {} twice (rows @{})",
                                session.sid, row.text, row.off
                            ));
                        }
                    }
                }
            }
        }
        if self.opts.mirror {
            bad.extend(self.mirror_twice());
            bad.extend(self.mirror_key_and_total());
            bad.extend(self.auto_mirror_unretained());
        }
        if self.opts.fetches {
            bad.extend(self.fetch_answers());
            bad.extend(self.fetch_read_past());
        }
        if self.opts.deadlines {
            bad.extend(self.expired_once());
        }
        if self.opts.topics {
            bad.extend(self.topic_rows());
        }
        bad.extend(self.routed_after_exit());
        bad.extend(self.key_copies());
        let gone = self.core.broker.gone();
        for (marker, records) in self.bus_markers().iter() {
            // Every copy but the newest given up by retention (sim item B05): the replay of an
            // entry whose record went unread (an own record is never delivered while its entry
            // stands) publishes the one copy a reader is handed. The rows' law is `dup delivery`.
            let once_read = records
                .split_last()
                .is_some_and(|(_, older)| older.iter().all(|r| gone.contains(r)));
            if self.sent.get(marker).is_some_and(|s| s.post.is_some())
                && records.len() > 1
                && !once_read
            {
                bad.push(format!(
                    "dup post: {marker} is on the bus {} times {records:?}",
                    records.len()
                ));
            }
        }
        for (off, subject, removed) in self.core.broker.take_retired() {
            bad.extend(self.judge_retire(off, &subject, &removed));
        }
        let idle_held = std::mem::take(&mut *lock(&self.core.idle_held));
        for (off, subject, body) in idle_held {
            let (body, _) = Body::decode(&body);
            if subject.starts_with(&format!("/f/{FLEET}/in/"))
                && self.sent.get(&body.text).is_some_and(|s| s.post.is_some())
            {
                bad.push(format!(
                    "idle: the loop parked while the broker held the answer to post {} @{off}",
                    body.text
                ));
            }
        }
        for b in bad {
            if !self.violations.contains(&b) {
                self.violations.push(b);
            }
        }
    }

    /// THE KEY'S LAW (critic item F23, [`Opts::keys`]): a key's posts are one post on the bus. A
    /// re-post astream's dedup answers lands nothing of its own (aterm is told it landed at its
    /// key's record, `dup=1`); one sent after retention gave that record up, its dedup key with
    /// it, lands anew. So a key's copies are one where every older copy went with retention.
    fn key_copies(&self) -> Vec<String> {
        if self.keyed.is_empty() {
            return Vec::new();
        }
        let markers = self.bus_markers();
        let gone = self.core.broker.gone();
        self.keyed
            .iter()
            .filter_map(|(key, k)| {
                let mut copies: Vec<u64> = k
                    .markers
                    .iter()
                    .filter_map(|m| markers.get(m))
                    .flatten()
                    .copied()
                    .collect();
                copies.sort_unstable();
                let once = copies
                    .split_last()
                    .is_none_or(|(_, older)| older.iter().all(|off| gone.contains(off)));
                (!once).then(|| {
                    format!(
                        "key: {}'s key {key} is on the bus {} times {copies:?}",
                        k.from,
                        copies.len()
                    )
                })
            })
            .collect()
    }

    /// THE DOOR LOOKS BEFORE IT COMMITS (69979209f1): a post first listed after its recipient
    /// left is published to that recipient's lane on this node, where nothing would read it and
    /// its sender would be told it landed, by no bridge still holding the roster that names it
    /// ([`super::endpoint::EpState::listed_after_exit`]).
    fn routed_after_exit(&self) -> Vec<String> {
        let listed = lock(&self.ep.state).listed_after_exit.clone();
        if listed.is_empty() {
            return Vec::new();
        }
        let lanes = format!("/f/{FLEET}/in/{NODE}/");
        self.log()
            .iter()
            .enumerate()
            .filter(|(_, rec)| !rec.hidden)
            .filter_map(|(off, rec)| {
                let sid = rec
                    .subject
                    .strip_prefix(lanes.as_str())?
                    .split('/')
                    .next()?;
                let marker = &rec.fabric().text;
                (listed.get(marker).map(String::as_str) == Some(sid)).then(|| {
                    format!(
                        "routing: {marker} was first listed after {sid} left, and landed on \
                         its lane @{off}"
                    )
                })
            })
            .collect()
    }

    /// THE RECLAIM, AT EVERY RETIRE (sim item B02): the node gives back only what a
    /// departed session held. Its presence row goes only while it reads `exited` (so a
    /// successor's `live` row written since the sweep read it stays), and nothing of a
    /// session aterm hosts goes. `removed` is the body of the newest record the retire at
    /// `off` took out of the index.
    fn judge_retire(&self, off: u64, subject: &str, removed: &[u8]) -> Option<String> {
        let hosted = |sid: &str| lock(&self.ep.state).sessions.values().any(|x| x.sid == sid);
        let segs: Vec<&str> = subject.split('/').collect();
        match segs.as_slice() {
            ["", "f", fleet, "pub", node, sid, "presence"]
                if *fleet == FLEET && *node == NODE && *sid != "node" =>
            {
                let state = Body::decode(removed).0.unknown.get("state").cloned();
                (hosted(sid) || state.as_deref() != Some("exited")).then(|| {
                    format!(
                        "reclaim: @{off} gave {sid}'s presence row back, which read {state:?}{}",
                        if hosted(sid) {
                            ", a session aterm hosts"
                        } else {
                            ""
                        }
                    )
                })
            }
            ["", "f", fleet, "in", node, sid, src, _]
                if *fleet == FLEET && *node == NODE && *src == NODE && hosted(sid) =>
            {
                Some(format!(
                    "reclaim: @{off} gave {subject} back, a lane to a session aterm hosts"
                ))
            }
            _ => None,
        }
    }

    /// THE INDEX, ONCE SETTLED: every hosted session's presence row is in the last-value
    /// index and reads `live`, what another node routes by; none was given back.
    fn presence_indexed(&self) -> Vec<String> {
        let sids: Vec<String> = lock(&self.ep.state)
            .sessions
            .values()
            .map(|x| x.sid.clone())
            .collect();
        sids.into_iter()
            .filter_map(|sid| {
                let face = crate::subject::session_face(FLEET, NODE, &sid, "presence");
                let state = self
                    .core
                    .broker
                    .last_row(&face)
                    .map(|(_, body)| Body::decode(&body).0.unknown.get("state").cloned());
                (state != Some(Some("live".to_string()))).then(|| {
                    format!("presence: {sid} is hosted and the last-value index holds {state:?}")
                })
            })
            .collect()
    }

    /// THE CHECKS THAT HOLD ONCE SETTLED.
    fn check_settled(&mut self) {
        let mut bad = Vec::new();
        let markers = self.bus_markers();
        let gone = self.core.broker.gone();
        let s = lock(&self.ep.state);
        let live: BTreeMap<&str, &super::endpoint::Session> =
            s.sessions.values().map(|x| (x.sid.as_str(), x)).collect();
        for (marker, sent) in &self.sent {
            let Some(to) = sent.to.as_deref() else {
                continue;
            };
            let Some(session) = live.get(to) else {
                continue;
            };
            if markers.contains_key(marker) && !session.rows.iter().any(|r| r.text == *marker) {
                // GIVEN UP, NEVER SILENT (sim item B05): a mail retention gave up before its
                // session had it is no loss, once the bridge told aterm a range holding it
                // that aterm tells that session (`link gap`: from astream's gap notice to the
                // inbox group, every session's; or, B12, `sid=` this session's own lane's).
                let offs = &markers[marker];
                if offs.iter().all(|off| gone.contains(off)) {
                    if !offs
                        .iter()
                        .any(|off| s.gaps.iter().any(|g| g.tells_mail_of(to, *off)))
                    {
                        bad.push(format!(
                            "given up untold: {marker} for {to} at {offs:?} went with \
                             retention, and no `link gap` aterm was told holds it: {:?}",
                            s.gaps
                        ));
                    }
                    continue;
                }
                bad.push(format!(
                    "lost: {marker} is on the bus for {to} and not in its ring"
                ));
            }
        }
        for session in s.sessions.values() {
            for p in &session.posts {
                let marker = String::from_utf8_lossy(&p.body).into_owned();
                match p.retired.as_deref() {
                    None => bad.push(format!(
                        "post {} #{} {marker} never retired",
                        session.sid, p.id
                    )),
                    Some(r)
                        if r.contains("reason=unroutable")
                            && p.to.strip_prefix('@').is_some_and(|to| {
                                s.gone.iter().any(|g| g.sid == to)
                                    && !s.sessions.values().any(|l| l.sid == to)
                            }) => {}
                    Some(r) if r.starts_with("off=-") => {
                        bad.push(format!(
                            "post {} #{} {marker} retired unlanded: {r}",
                            session.sid, p.id
                        ));
                    }
                    Some(r) => {
                        let off: Option<u64> = r
                            .split_whitespace()
                            .find_map(|t| t.strip_prefix("off="))
                            .and_then(crate::parse_uint);
                        let on_bus = markers.get(&marker);
                        match (off, on_bus) {
                            (Some(off), Some(records)) if records.contains(&off) => {}
                            (Some(_), None) => {}
                            (Some(off), Some(_)) if self.lost_floor.is_some_and(|f| off >= f) => {}
                            _ => bad.push(format!(
                                "post {} #{} {marker} retired `{r}` but the bus has it at {:?}",
                                session.sid, p.id, on_bus
                            )),
                        }
                    }
                }
            }
            for r in &session.receipts {
                if r.retired.is_none() {
                    bad.push(format!(
                        "receipt {} rid={} never retired",
                        session.sid, r.rid
                    ));
                }
            }
        }
        let mut by_sender: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
        for (marker, sent) in &self.sent {
            let Some((from, id)) = &sent.post else {
                continue;
            };
            if let Some(records) = markers.get(marker) {
                if let Some(first) = records.first() {
                    by_sender
                        .entry(from.as_str())
                        .or_default()
                        .push((*first, *id));
                }
            }
        }
        for (from, mut posts) in by_sender {
            posts.sort_unstable();
            if posts.windows(2).any(|w| w[0].1 > w[1].1) {
                bad.push(format!(
                    "order: {from}'s posts on the bus out of order: {posts:?}"
                ));
            }
        }
        let records = self.core.broker.records();
        let by_off: BTreeMap<u64, &String> = records.iter().map(|(o, s, _, _)| (*o, s)).collect();
        let mut answers: BTreeMap<(u64, String), Vec<u64>> = BTreeMap::new();
        for (off, subject, body, _) in &records {
            if !subject.ends_with("/ack") {
                continue;
            }
            let (body, _) = Body::decode(body);
            if let (Some(re), Some(from)) = (body.re, body.from.clone()) {
                let asked = by_off.get(&re).is_some_and(|s| {
                    s.starts_with(&format!("/f/{FLEET}/in/{NODE}/{from}/")) && s.ends_with("/ask")
                });
                if !asked {
                    bad.push(format!(
                        "receipt: the answer @{off} from {from} names @{re}, which is not its \
                         ask (@{re} is {:?})",
                        by_off.get(&re)
                    ));
                }
                answers.entry((re, from)).or_default().push(*off);
            }
        }
        // Every copy but the newest given up by retention before its asker was handed it (sim
        // item B05): the replay of an entry whose record went unread (an own record is never
        // delivered while its entry stands) publishes the one copy a reader is handed, as a
        // post's does. A copy its asker holds or has fetched is an answer it was given
        // ([`holds_answer`]): an `ack` at that offset, from its sender, naming the same ask.
        // A row at that offset for another session, kind, ask or sender is another record.
        let gone = self.core.broker.gone();
        let by_offset = self.opts.mutants.contains(&Mutant::AnswerHandedByOffset);
        let handed = |off: u64, re: u64, from: &str| {
            if by_offset {
                return s
                    .sessions
                    .values()
                    .chain(s.gone.iter())
                    .any(|session| session.rows.iter().any(|row| row.off == off));
            }
            by_off
                .get(&off)
                .and_then(|subject| in_lane(subject))
                .is_some_and(|(asker, _, _)| holds_answer(&s, asker, off, "ack", re, Some(from)))
        };
        for ((re, from), offs) in answers {
            let once_given = offs.split_last().is_some_and(|(_, older)| {
                older
                    .iter()
                    .all(|off| gone.contains(off) && !handed(*off, re, &from))
            });
            if offs.len() > 1 && !once_given {
                bad.push(format!(
                    "receipt: {from}'s decision on @{re} answered {} times {offs:?}",
                    offs.len()
                ));
            }
        }
        let standing = {
            let records = self.core.broker.records();
            let halt = crate::subject::person_halt(FLEET, PERSON);
            records
                .iter()
                .rfind(|(_, subject, _, _)| *subject == halt)
                .map(|(_, _, body, _)| {
                    let (b, _) = Body::decode(body);
                    b.unknown.get("state").is_some_and(|v| v == "on")
                })
        };
        let ever_on = self.halt_rows.iter().any(|(_, on)| *on);
        for session in s.sessions.values() {
            if standing == Some(true) && session.fleet_hold.is_none() {
                bad.push(format!("halt: {} runs under a standing halt", session.sid));
            }
            if !ever_on && session.fleet_hold.is_some() {
                bad.push(format!(
                    "halt: {} is held with no halt ever on",
                    session.sid
                ));
            }
        }
        drop(s);
        drop(markers);
        let alias_collision = lock(&self.core.broker.state)
            .race
            .as_ref()
            .filter(|race| race.alias)
            .and_then(|race| race.collision);
        for (_, subject, body, _) in self.core.broker.records() {
            if !subject.starts_with(&format!("/f/{FLEET}/pub/{NODE}/")) {
                continue;
            }
            let text = crate::pct::decode(&String::from_utf8_lossy(&body));
            for alarm in ["cap-compromised", "forged-self"] {
                if text.contains(alarm) {
                    let (parsed, _) = Body::decode(&body);
                    if alias_collision.is_some_and(|(seq, off)| {
                        alarm == "cap-compromised"
                            && subject == crate::subject::node_face(FLEET, NODE, "ev")
                            && parsed.unknown.get("ev").map(|ev| crate::pct::decode(ev))
                                == Some(format!(
                                    "cap-compromised seq={seq} off={off} reason=deduped"
                                ))
                    }) {
                        continue;
                    }
                    bad.push(format!("alarm: {subject}: {text}"));
                }
            }
        }
        let (presence, left) = self.presence_gaps();
        bad.extend(presence);
        bad.extend(self.presence_indexed());
        self.stats.ghosts_left = left.len() as u64;
        let sibling = self.sibling();
        let sibling_row = sibling
            .and_then(|_| self.presence_rows().get(SIBLING_SID).cloned())
            .map(|(state, _, _)| state);
        let outranked = sibling.is_some_and(|sibling| {
            self.node_rows()
                .iter()
                .any(|(_, inc, _)| *inc > Some(sibling))
        });
        self.stats.sibling_kept = u64::from(outranked && sibling_row.as_deref() == Some("live"));
        self.stats.sibling_retired = u64::from(sibling_row.is_some_and(|state| state != "live"));
        self.stats.sibling_raced = u64::from(self.opts.race && sibling.is_some());
        self.stats.sibling_alias_collided = u64::from(self.core.broker.alias_collision_completed());
        self.stats.alias_ghost_retired = u64::from(self.alias_ghost.as_ref().is_some_and(|sid| {
            self.presence_rows()
                .get(sid)
                .is_some_and(|(state, _, _)| state == "exited")
        }));
        if self.mirror_owed() {
            bad.extend(self.mirror_gaps());
        }
        bad.extend(self.mirror_stopped_for_its_pin());
        if self.stats.moves == 0 && self.stats.another_logs == 0 {
            bad.extend(self.mirror_twice_raw());
        }
        if self.opts.deadlines {
            bad.extend(self.deadlines_fired());
        }
        if self.opts.topics {
            bad.extend(self.topics_followed());
        }
        self.stats.ghosts_retired = self
            .core
            .broker
            .records()
            .iter()
            .filter(|(_, subject, body, _)| {
                subject.starts_with(&format!("/f/{FLEET}/pub/{NODE}/"))
                    && crate::pct::decode(&String::from_utf8_lossy(body))
                        .contains("presence-retired")
            })
            .count() as u64;
        let quarantined: Vec<String> = std::fs::read_dir(
            std::path::Path::new(&self.cfg.state_dir).join(crate::state::QUARANTINE),
        )
        .map(|dir| {
            dir.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
        self.stats.retired = quarantined
            .iter()
            .filter(|n| n.starts_with(crate::state::RETIRED_PREFIX))
            .count() as u64;
        self.stats.set_aside = quarantined.len() as u64 - self.stats.retired;
        self.stats.pins_set_aside = quarantined
            .iter()
            .filter(|n| *n == crate::state::LOGPIN || n.starts_with("logpin."))
            .count() as u64;
        self.stats.set_aside_told = lock(&self.ep.state).set_aside_told;
        if self.stats.set_aside_told > self.stats.set_aside {
            bad.push(format!(
                "set-aside: the bridges told aterm of {} torn files, and the quarantine holds {}",
                self.stats.set_aside_told, self.stats.set_aside
            ));
        }
        let held_said = lock(&self.ep.state).held_said;
        if lock(&self.ep.state).rules.lane >= 5 && held_said != Some(self.stats.set_aside) {
            bad.push(format!(
                "quarantine: the newest bridge told aterm it holds {held_said:?} torn files, and \
                 it holds {}",
                self.stats.set_aside
            ));
        }
        self.stats.delivered = {
            let s = lock(&self.ep.state);
            s.sessions
                .values()
                .chain(s.gone.iter())
                .map(|x| x.rows.len() as u64)
                .sum()
        };
        self.stats.deliveries_pipelined = lock(&self.ep.state).deliveries_pipelined;
        self.stats.remints_proved = lock(&self.ep.state).proved_logs;
        self.stats.remints_forgotten = self.remints_forgotten();
        self.stats.retires_pipelined = lock(&self.ep.state).retires_pipelined;
        for b in bad {
            if !self.violations.contains(&b) {
                self.violations.push(b);
            }
        }
    }
}

/// The name a quarantined file's copies share: the one its source's path
/// gives it (`StateDir::move_into_quarantine`: `/` as `.`, a `retired.`
/// prefix), without the `.<n>` a name already taken adds.
fn quarantine_base(name: &str) -> &str {
    let parts = match name.split('.').next() {
        Some("logpin") => 1,
        Some("inflight" | "keys" | "unhandled" | "journal") => 2,
        Some("sent" | "acks" | "retired") => 3,
        _ => return name,
    };
    name.match_indices('.')
        .nth(parts - 1)
        .map_or(name, |(at, _)| &name[..at])
}

/// THE QUARANTINE'S SECOND COPIES ([`World::copies_since_launch`]): each pair of
/// regular files under one source's name ([`quarantine_base`]) that hold the
/// same bytes.
fn quarantine_copies(dir: &std::path::Path, names: &[String]) -> Vec<String> {
    let mut by_base: BTreeMap<&str, Vec<(&str, Vec<u8>)>> = BTreeMap::new();
    for name in names {
        let path = dir.join(name);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            by_base
                .entry(quarantine_base(name))
                .or_default()
                .push((name.as_str(), bytes));
        }
    }
    let mut bad = Vec::new();
    for copies in by_base.values() {
        for (i, (a, bytes)) in copies.iter().enumerate() {
            for (b, other) in &copies[i + 1..] {
                if bytes == other {
                    bad.push(format!(
                        "quarantine copies: {a} and {b} hold the same bytes, one file moved twice"
                    ));
                }
            }
        }
    }
    bad
}

/// Worlds that may run at once in this process: one per core the binary may use (its grant per
/// libtest thread times its width, [`aterm_cores::binary_parallelism`]), at most six. Each
/// world's state dirs are real folders whose every write is a create and a rename, and past six
/// worlds at once those metadata calls queue in the kernel: on 18 cores the lib's tests at nine
/// took 224 CPU-s for 25 s, at six 170 CPU-s for 27 s, the same instructions either way. Every
/// test thread's worlds share these slots, and each sweep already takes at most its thread's
/// grant of workers (`aterm_spec::check::each`), so the slots bind only below the binary's grant.
/// At one world per two cores they did: a sweep's workers waited for a slot while cores the
/// binary was granted sat idle.
static WORLD_SLOTS: LazyLock<(Mutex<usize>, Condvar)> = LazyLock::new(|| {
    let cores = aterm_cores::binary_parallelism().map_or(4, std::num::NonZeroUsize::get);
    (Mutex::new(cores.clamp(1, 6)), Condvar::new())
});

/// One of [`WORLD_SLOTS`], given back when the world ends (a panic included).
struct WorldSlot;

impl WorldSlot {
    fn take() -> Self {
        let (free, freed) = &*WORLD_SLOTS;
        let mut free = lock(free);
        while *free == 0 {
            free = freed.wait(free).unwrap_or_else(PoisonError::into_inner);
        }
        *free -= 1;
        Self
    }
}

impl Drop for WorldSlot {
    fn drop(&mut self) {
        let (free, freed) = &*WORLD_SLOTS;
        *lock(free) += 1;
        freed.notify_one();
    }
}

/// Run one seed: the fault phase, the settle phase, the oracle.
pub(crate) fn run(opts: &Opts) -> Outcome {
    let _slot = WorldSlot::take();
    let mut w = World::new(opts.clone());
    if opts.alias {
        w.open_one();
    }
    w.launch();
    if opts.alias {
        w.settle();
        let before = w.presence_rows().clone();
        let first_inc = w.running_bridge().map(|(inc, _)| inc);
        w.kill();
        w.exit_one();
        let departed = lock(&w.ep.state)
            .gone
            .last()
            .map(|session| session.sid.clone());
        w.alias_ghost = departed.filter(|sid| {
            before.get(sid).is_some_and(|(state, inc, mark)| {
                state == "live" && inc.is_some() && *inc == first_inc && mark == &w.mark
            })
        });
        w.launch();
    }
    for step in 0..opts.steps {
        if w.sibling_attaches_at == Some(step) {
            w.sibling_attaches();
        }
        if w.sibling_dies_at == Some(step) {
            w.sibling_dies();
        }
        w.fault_step();
        if w.violations
            .iter()
            .any(|v| v.starts_with("the bridge panicked"))
        {
            break;
        }
    }
    if !w
        .violations
        .iter()
        .any(|v| v.starts_with("the bridge panicked"))
    {
        w.settle();
        w.collect_notes();
        w.check_settled();
        w.copies_since_launch();
    }
    let trace = w.core.trace();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    (
        w.stats.acks_held,
        w.stats.tries_not_yet,
        w.stats.seen_before_ack,
    ) = w.core.broker.held_counts();
    (w.stats.commits_corked, w.stats.commits_alone) = w.core.broker.commit_writes();
    (w.stats.gap_fenced, w.stats.fences_lifted) = w.core.broker.fence_counts();
    w.stats.rate_limited = w.core.broker.rate_limited_total();
    w.stats.settles = w.core.broker.settles();
    {
        let b = lock(&w.core.broker.state);
        w.stats.poisoned = b.poisons;
        w.stats.poisoned_refusals = b.poisoned_refusals;
        w.stats.stampless_starts = b.stampless_opens;
    }
    w.stats.pin_durable_replaces = w.core.device.pin_durable_replaces();
    // THE LAW OF THE LIMIT'S WAIT (critic item E06, `bus::State::rate_laws`): judged at the door,
    // where a bridge that retried a refusal for the rate at once meets it.
    w.violations.extend(w.core.broker.take_rate_laws());
    {
        let s = lock(&w.core.broker.state);
        w.stats.retention_ticks = s.ret.ticks;
        w.stats.retention_cuts = s.ret.cuts;
        w.stats.given_up = s.ret.gone.len() as u64;
        w.stats.retained_from = s.ret.refusals;
        w.stats.gaps_noticed = s.ret.told.len() as u64;
    }
    {
        let ep = lock(&w.ep.state);
        w.stats.gaps_told = ep.gaps.len() as u64;
        w.stats.mirror_waits = ep.mirror_waits;
    }
    {
        let mut law = lock(&w.core.refills);
        w.stats.refills_judged = law.judged;
        w.stats.refill_rows = law.rows;
        w.stats.refills_together = law.together;
        w.stats.refill_together_sessions = law.together_sessions;
        w.stats.refills_below_oldest = law.below_oldest;
        w.violations.append(&mut law.violations);
    }
    w.stats.cuts_after_dedup = w.core.broker.cuts_after_dedup();
    w.stats.strays = w.core.broker.strays().len() as u64;
    if !w.keyed.is_empty() {
        let on_bus = w.bus_markers().clone();
        let reposts: BTreeSet<&String> = w
            .keyed
            .values()
            .flat_map(|k| k.markers.iter().skip(1))
            .collect();
        w.stats.reposts_appended =
            reposts.iter().filter(|m| on_bus.contains_key(**m)).count() as u64;
        let ep = lock(&w.ep.state);
        w.stats.reposts_deduped = ep
            .sessions
            .values()
            .chain(ep.gone.iter())
            .flat_map(|session| session.posts.iter())
            .filter(|p| {
                p.key.is_some() && p.retired.as_deref().is_some_and(|r| r.contains("dup=1"))
            })
            .count() as u64;
    }
    for (&tap, &n) in lock(&w.core.reservations).iter() {
        match tap {
            crate::fx::ReservationTap::Verified => w.stats.reservations_verified += n,
            crate::fx::ReservationTap::Collision => w.stats.reservation_collisions += n,
            crate::fx::ReservationTap::ReplayedAtTheWinner => w.stats.replayed_at_the_winner += n,
            crate::fx::ReservationTap::UndecidedTaken => w.stats.undecided_taken += n,
            crate::fx::ReservationTap::Unverifiable(case) => {
                let slot = usize::from(case)
                    .checked_sub(1)
                    .and_then(|i| w.stats.unverifiable.get_mut(i))
                    .unwrap_or_else(|| {
                        panic!("an unverifiable case {case} the stats keep no slot for")
                    });
                *slot += n;
            }
        }
    }
    let errors = w.core.device.write_errors();
    w.stats.writes_failed = errors.writes;
    w.stats.unlinks_failed = errors.unlinks;
    w.stats.flushes_failed = errors.flushes;
    w.stats.renamed_unflushed = errors.renamed_unflushed;
    w.stats.appends_cut = errors.appends_cut;
    w.stats.disk_full_refused = errors.disk_full;
    let tap = w.tap.into_inner();
    w.stats.tap_past_a_retire = tap.checked_past_a_retire;
    Outcome {
        seed: opts.seed,
        tier: w.tier,
        violations: w.violations,
        trace,
        stats: w.stats,
        model: (tap.opened, tap.checked, tap.fired),
        journal: w.core.broker.take_journal(),
    }
}

/// What [`run_reclaim`] found.
#[derive(Debug)]
pub(crate) struct Reclaimed {
    pub(crate) violations: Vec<String>,
    /// The subjects the node spent ([`bus::Broker::spent`]): settled with aterm's two
    /// sessions; with the scenario's sessions open, each having mailed another and been
    /// mailed by the peer; and once they left and the reclaim's wait passed.
    pub(crate) baseline: u64,
    pub(crate) peak: u64,
    pub(crate) after: u64,
    /// The session revived between the sweep's read and its retire, once it was.
    pub(crate) revived: Option<String>,
    pub(crate) trace: Vec<String>,
    /// The broker's journal ([`Opts::journal`]).
    pub(crate) journal: Vec<bus::Event>,
}

/// THE RECLAIM SCENARIO (sim item B02): aterm opens `n` sessions beside its two, each
/// mails the next and is mailed by the peer, all `n` leave, and the bridge runs until
/// its reclaim's wait has passed twice over. With `revive`, the first of them is served
/// again just as the sweep retires its presence row: a successor's `live` row reaches
/// the broker first ([`bus::Revive`]), and aterm hosts the session again, rows and posts
/// as it left them; the peer then mails it. Every law of a world holds throughout, and at
/// every retire [`World::judge_retire`]'s. Faults are the opts' (none, for the committed
/// runs).
pub(crate) fn run_reclaim(opts: &Opts, n: usize, revive: bool) -> Reclaimed {
    let _slot = WorldSlot::take();
    let mut w = World::new(opts.clone());
    let me = producer(NODE);
    w.launch();
    w.settle();
    let baseline = w.core.broker.spent(me);
    let opened: Vec<u64> = (0..n).map(|_| w.open_session()).collect();
    w.settle();
    let sids: Vec<String> = opened.iter().filter_map(|l| w.ep.sid(*l)).collect();
    for (i, &local) in opened.iter().enumerate() {
        let to = sids[(i + 1) % sids.len()].clone();
        w.post_as(local, &sids[i], &format!("@{to}"), "note", Some(to));
        w.peer_mail_to(sids[i].clone());
    }
    w.settle();
    let peak = w.core.broker.spent(me);
    // Each exit's `session-exited` line reaches the bridge before its next roster read, as
    // a closed tab's does on a real instance.
    for &local in &opened {
        w.exit_local(local);
        w.pass();
    }
    w.settle();
    let revivable = sids.first().cloned().filter(|_| revive);
    if let Some(sid) = &revivable {
        w.peer_seq += 1;
        let body = format!(
            "v=1 t={} inc=0 epoch=- gen=- state=live hold=0",
            w.core.wall_ms()
        );
        lock(&w.core.broker.state).revive = Some(bus::Revive {
            subject: crate::subject::session_face(FLEET, NODE, sid, "presence"),
            body: body.into_bytes(),
            producer: producer(PEER),
            seq: w.peer_seq,
            landed: None,
        });
    }
    let until = w.core.elapsed() + 2 * (crate::bridge::RECLAIM_AFTER + crate::bridge::GHOST_SWEEP);
    let mut revived = None;
    for _ in 0..100_000 {
        if w.core.elapsed() >= until {
            break;
        }
        if w.live.is_none() {
            w.launch();
            continue;
        }
        match w.core.readable().first() {
            Some(&(conn, _)) => {
                w.core.pump(conn);
            }
            None => w.pass(),
        }
        w.collect_notes();
        w.check_always();
        let landed = lock(&w.core.broker.state)
            .revive
            .as_ref()
            .and_then(|r| r.landed);
        if let (Some(sid), Some(_), None) = (&revivable, landed, &revived) {
            if w.revive(sid) {
                revived = Some(sid.clone());
            }
        }
    }
    if w.core.elapsed() < until {
        w.violations
            .push("liveness: the reclaim's wait never passed".to_string());
    }
    w.settle();
    if let Some(sid) = &revived {
        w.peer_mail_to(sid.clone());
        w.settle();
    }
    w.collect_notes();
    w.check_settled();
    let after = w.core.broker.spent(me);
    let trace = w.core.trace();
    let journal = w.core.broker.take_journal();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    Reclaimed {
        violations: w.violations,
        baseline,
        peak,
        after,
        revived,
        trace,
        journal,
    }
}

/// What [`run_reclaim_across_a_cut`] found.
#[derive(Debug)]
pub(crate) struct ReclaimedAcrossACut {
    pub(crate) violations: Vec<String>,
    /// The sender's post was on the bus and reserved, its retire not landed, when the power went.
    pub(crate) pinned_at_the_cut: bool,
    /// The two sessions' subjects this node is charged for (each one's presence row and the
    /// lane this node wrote to it), each with whether the last-value index held it when the
    /// power went and once the next boot's reclaim was due.
    pub(crate) subjects: Vec<(String, bool, bool)>,
    pub(crate) trace: Vec<String>,
}

/// A RESERVATION A POWER CUT LEFT (item C14): aterm opens two sessions beside its two; the
/// second mails the first, then the first's post to the second is pinned and lands, but its
/// retire does not (aterm answers its `outbox sent` as for a session it no longer finds,
/// [`super::endpoint::EpState::unretired`]) before the power goes, the state dir's device
/// keeping every write. The next boot's bridge runs until its reclaim's wait has passed twice
/// over after the ghost sweep's: its first start's sweep of the earlier boot retires that
/// reservation, so neither the sender nor the lane its post named is owed, and the reclaim
/// gives both sessions' presence rows and lanes back. Every law of a world and every retire's
/// ([`World::judge_retire`]) holds throughout.
pub(crate) fn run_reclaim_across_a_cut(opts: &Opts) -> ReclaimedAcrossACut {
    let _slot = WorldSlot::take();
    let mut w = World::new(Opts {
        lossless_device: true,
        ..opts.clone()
    });
    w.launch();
    w.settle();
    let (sender, recipient) = (w.open_session(), w.open_session());
    w.settle();
    let (Some(from), Some(to)) = (w.ep.sid(sender), w.ep.sid(recipient)) else {
        unreachable!("the two sessions just opened are hosted");
    };
    w.post_as(
        recipient,
        &to,
        &format!("@{from}"),
        "note",
        Some(from.clone()),
    );
    w.settle();
    lock(&w.ep.state).unretired.insert(from.clone());
    w.post_as(sender, &from, &format!("@{to}"), "note", Some(to.clone()));
    let marker = format!("m{}", w.marker);
    let id = w
        .sent
        .get(&marker)
        .and_then(|s| s.post.as_ref())
        .map(|(_, id)| *id);
    // Until the bridge asked aterm to retire the post (its publish answered), which aterm did not.
    let asked = id.map(|id| format!("ctl outbox sent {from} {id} "));
    for _ in 0..1_000 {
        if asked
            .as_ref()
            .is_none_or(|asked| w.core.trace().iter().any(|l| l.starts_with(asked)))
        {
            break;
        }
        if w.live.is_none() {
            break;
        }
        match w.core.readable().first() {
            Some(&(conn, _)) => {
                w.core.pump(conn);
            }
            None => w.pass(),
        }
        w.collect_notes();
        w.check_always();
    }
    let pinned = id.is_some_and(|id| {
        crate::state::StateDir::peek(&w.cfg.state_dir)
            .post_pin(&from, id)
            .is_ok_and(|pin| pin.is_some())
    });
    let unretired = lock(&w.ep.state)
        .sessions
        .values()
        .filter(|x| x.sid == from)
        .flat_map(|x| &x.posts)
        .any(|p| Some(p.id) == id && p.retired.is_none());
    let pinned_at_the_cut = pinned && unretired && w.bus_markers().contains_key(&marker);
    let subjects: Vec<String> = [&from, &to]
        .into_iter()
        .flat_map(|sid| {
            [
                crate::subject::session_face(FLEET, NODE, sid, "presence"),
                crate::subject::post_lane(FLEET, NODE, sid, NODE, "note"),
            ]
        })
        .collect();
    let at_the_cut: Vec<bool> = subjects
        .iter()
        .map(|s| w.core.broker.last_row(s).is_some())
        .collect();
    w.power_cut();
    // The ghost sweep's patience, then the reclaim's (the same wait), each judged at a sweep.
    let wait = 2 * crate::bridge::RECLAIM_AFTER + crate::bridge::GHOST_SWEEP;
    let until = w.core.elapsed() + 2 * wait;
    for _ in 0..100_000 {
        if w.core.elapsed() >= until {
            break;
        }
        if w.live.is_none() {
            w.launch();
            continue;
        }
        match w.core.readable().first() {
            Some(&(conn, _)) => {
                w.core.pump(conn);
            }
            None => w.pass(),
        }
        w.collect_notes();
        w.check_always();
    }
    if w.core.elapsed() < until {
        w.violations
            .push("liveness: the reclaim's wait never passed".to_string());
    }
    w.settle();
    w.collect_notes();
    w.check_settled();
    let subjects = subjects
        .into_iter()
        .zip(at_the_cut)
        .map(|(s, then)| {
            let now = w.core.broker.last_row(&s).is_some();
            (s, then, now)
        })
        .collect();
    let trace = w.core.trace();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    ReclaimedAcrossACut {
        violations: w.violations,
        pinned_at_the_cut,
        subjects,
        trace,
    }
}

/// What [`run_reworded`] found.
#[derive(Debug)]
pub(crate) struct Reworded {
    pub(crate) violations: Vec<String>,
    /// The post was pinned, its first attempt landed or refused as the scenario asked, and its
    /// `re=` rewritten before its retry.
    pub(crate) reworded: bool,
    /// The copies of the post on the bus once the world settled.
    pub(crate) copies: usize,
    /// Whether aterm retired the post.
    pub(crate) retired: bool,
    pub(crate) trace: Vec<String>,
}

/// A RETRY WHOSE WORDS NO LONGER MATCH ITS PIN (ATERM-KI-1111-7): aterm opens two sessions
/// beside its two, and the first posts a reply (`re=`) to the second (`other_node` false: one of
/// this node's own lanes) or to the person (`other_node`: a lane of another principal's, where
/// nothing is journaled in flight). Its first attempt takes a pin and then either lands
/// (`landed`: aterm answers its `outbox sent` as for a session it no longer finds,
/// [`super::endpoint::EpState::unretired`], so the post stays queued at its pin, and the bridge
/// is killed) or is refused (the broker rolls its batches back until the seam, [`bus::Broker::refuse_mail`]).
/// Then the seam: aterm lists the post with another `re=` (as a mesh route maps a reply's offset
/// to another Mac's, or a newer build encodes the post otherwise), so its retry encodes words its
/// pin does not name (`Bridge::unverified_publication`'s case 1). The world settles, and every law
/// is judged: a hold that never ends fails `liveness`, and a second copy `dup post`.
pub(crate) fn run_reworded(opts: &Opts, other_node: bool, landed: bool) -> Reworded {
    let _slot = WorldSlot::take();
    let mut w = World::new(Opts {
        lossless_device: true,
        ..opts.clone()
    });
    w.launch();
    w.settle();
    let (sender, recipient) = (w.open_session(), w.open_session());
    w.settle();
    let (Some(from), Some(to)) = (w.ep.sid(sender), w.ep.sid(recipient)) else {
        unreachable!("the two sessions just opened are hosted");
    };
    let (address, target) = if other_node {
        (PERSON.to_string(), None)
    } else {
        (format!("@{to}"), Some(to.clone()))
    };
    if landed {
        lock(&w.ep.state).unretired.insert(from.clone());
    } else {
        w.core.broker.refuse_mail(true);
    }
    w.post_as(sender, &from, &address, "note", target);
    let marker = format!("m{}", w.marker);
    let id = w
        .sent
        .get(&marker)
        .and_then(|s| s.post.as_ref())
        .map(|(_, id)| *id);
    let set_re = |w: &World, re: u64| {
        let mut s = lock(&w.ep.state);
        if let Some(p) = s
            .sessions
            .values_mut()
            .filter(|x| x.sid == from)
            .flat_map(|x| x.posts.iter_mut())
            .find(|p| Some(p.id) == id)
        {
            p.re = Some(re);
        }
    };
    set_re(&w, 1);
    let pinned = |w: &World| {
        id.is_some_and(|id| {
            crate::state::StateDir::peek(&w.cfg.state_dir)
                .post_pin(&from, id)
                .is_ok_and(|pin| pin.is_some())
        })
    };
    // Until the first attempt is pinned and concluded: answered (aterm asked to retire it), or
    // refused.
    let asked = id.map(|id| format!("ctl outbox sent {from} {id} "));
    let mut concluded = false;
    for _ in 0..1_000 {
        concluded = pinned(&w)
            && if landed {
                asked
                    .as_ref()
                    .is_some_and(|asked| w.core.trace().iter().any(|l| l.starts_with(asked)))
            } else {
                w.core.broker.mail_refused() > 0
            };
        if concluded || w.live.is_none() {
            break;
        }
        match w.core.readable().first() {
            Some(&(conn, _)) => {
                w.core.pump(conn);
            }
            None => w.pass(),
        }
        w.collect_notes();
        w.check_always();
    }
    let first = w.bus_markers().get(&marker).map_or(0, Vec::len);
    let reworded = concluded && first == usize::from(landed);
    set_re(&w, 2);
    w.note(format!("seam: {from}'s post {id:?} now lists re=2"));
    if landed {
        w.kill();
        lock(&w.ep.state).unretired.remove(&from);
        w.launch();
    } else {
        w.core.broker.refuse_mail(false);
    }
    w.settle();
    w.collect_notes();
    w.check_settled();
    let copies = w.bus_markers().get(&marker).map_or(0, Vec::len);
    let retired = id.is_some_and(|id| w.ep.retired(&from, id).is_some());
    let trace = w.core.trace();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    Reworded {
        violations: w.violations,
        reworded,
        copies,
        retired,
        trace,
    }
}

/// What [`run_busy`] found.
pub(crate) struct Busy {
    pub(crate) violations: Vec<String>,
    /// The bridge held the sender's own note in hand while aterm answered its `outbox sent` busy.
    pub(crate) held: bool,
    /// Another session's mail, posted after the note, reached its ring while the note waited.
    pub(crate) other_while_held: bool,
    /// The `outbox sent` lines aterm answered busy.
    pub(crate) answered_busy: u64,
    /// The links said down for an error (`link down reason=error`, every one at a record this
    /// world holds: it refuses no write).
    pub(crate) drops: u64,
    /// The note reached its recipient's ring once the world settled.
    pub(crate) delivered: bool,
    /// With `kill_while_held`, the inbox group's commit had passed the peer's mail held behind
    /// the note when the bridge was killed (only `Mutant::CommitPastAHeldOwnRecord` sends one).
    pub(crate) committed_past_the_held: bool,
    /// With `kill_while_held`, the records the bridge held in hand when it was killed.
    pub(crate) held_at_the_kill: usize,
    pub(crate) trace: Vec<String>,
}

/// One step of a scenario world: a readable connection pumped, or a pass of the bridge.
fn step(w: &mut World) {
    match w.core.readable().first() {
        Some(&(conn, _)) => {
            w.core.pump(conn);
        }
        None => w.pass(),
    }
    w.collect_notes();
    w.check_always();
}

/// Whether session `sid`'s ring holds a row of `marker`.
fn in_ring(w: &World, sid: &str, marker: &str) -> bool {
    lock(&w.ep.state)
        .sessions
        .values()
        .any(|s| s.sid == sid && s.rows.iter().any(|r| r.text == marker))
}

/// ATERM'S UPDATE COMMIT FENCES A SENDER'S RETIREMENTS (ATERM-KI-1111-3): aterm opens three
/// sessions beside its two; the first's `outbox sent` is answered `ERR busy update-committing`
/// ([`super::endpoint::EpState::busy`]) while it posts a note to the second, one of this node's
/// own lanes, so the note's in-flight entry stands for the drain's retry when the live tail
/// meets its record. The second then posts to the third, whose mail comes on the node's same
/// inbox subscription. Then aterm's commit ends, its retirements answer again, and the world
/// settles: every law is judged, the model's tap among them (a commit past the held note
/// before it is delivered), and `liveness`. With `kill_while_held` a peer's mail to the note's
/// session is held behind the note, the third session's mail is delivered (a commit's chance
/// to pass both), and the bridge is killed while it holds them; the next bridge settles the
/// world, whose laws (`lost:` and `dup delivery` among them) hold both delivered once: the
/// note's in-flight entry offers it again, and the peer's mail, held in the killed bridge's
/// memory alone, is offered again by the next bridge's refill of its lane.
pub(crate) fn run_busy(opts: &Opts, kill_while_held: bool) -> Busy {
    let _slot = WorldSlot::take();
    let mut w = World::new(Opts {
        lossless_device: true,
        ..opts.clone()
    });
    w.launch();
    w.settle();
    let (a, b, c) = (w.open_session(), w.open_session(), w.open_session());
    w.settle();
    let (Some(from), Some(to), Some(other)) = (w.ep.sid(a), w.ep.sid(b), w.ep.sid(c)) else {
        unreachable!("the three sessions just opened are hosted");
    };
    lock(&w.ep.state).busy.insert(from.clone());
    w.post_as(a, &from, &format!("@{to}"), "note", Some(to.clone()));
    let note = format!("m{}", w.marker);
    let mut held = false;
    for _ in 0..2_000 {
        held = w.live.as_ref().is_some_and(|l| l.bridge.held_in_hand() > 0);
        if held || w.live.is_none() || w.stats.held_for_the_disk > 0 {
            break;
        }
        step(&mut w);
    }
    w.note(format!("seam: {from}'s note {note} held in hand: {held}"));
    w.post_as(b, &to, &format!("@{other}"), "note", Some(other.clone()));
    let mail = format!("m{}", w.marker);
    let mut other_while_held = false;
    for _ in 0..2_000 {
        if in_ring(&w, &other, &mail) {
            other_while_held = !in_ring(&w, &to, &note);
            break;
        }
        if w.live.is_none() {
            break;
        }
        step(&mut w);
    }
    w.note(format!(
        "seam: {other} handed {mail} while {note} waited: {other_while_held}; aterm's commit ends"
    ));
    let mut committed_past_the_held = false;
    let mut held_at_the_kill = 0;
    if kill_while_held {
        // A peer's mail to the note's session, held behind the note in memory alone (no
        // in-flight entry stands for it, as one does for the note), then the third session's
        // mail, whose delivery is the next commit's chance to pass it.
        w.peer_mail_to(to.clone());
        let behind = format!("m{}", w.marker);
        for _ in 0..2_000 {
            if w.live.as_ref().is_none_or(|l| l.bridge.held_in_hand() >= 2) {
                break;
            }
            step(&mut w);
        }
        w.post_as(b, &to, &format!("@{other}"), "note", Some(other.clone()));
        let after = format!("m{}", w.marker);
        for _ in 0..2_000 {
            if w.live.is_none() || in_ring(&w, &other, &after) {
                break;
            }
            step(&mut w);
        }
        let group = crate::subject::inbox_group(FLEET, NODE);
        let at = w
            .bus_markers()
            .get(&behind)
            .and_then(|offs| offs.first().copied());
        for _ in 0..200 {
            committed_past_the_held = at.is_some_and(|off| w.core.broker.group_start(&group) > off);
            if committed_past_the_held || w.live.is_none() {
                break;
            }
            step(&mut w);
        }
        held_at_the_kill = w.live.as_ref().map_or(0, |l| l.bridge.held_in_hand());
        w.note(format!(
            "seam: the bridge is killed holding {held_at_the_kill} records, {note} and {behind} \
             behind it; the commit passed {behind}: {committed_past_the_held}"
        ));
        w.kill();
    }
    lock(&w.ep.state).busy.remove(&from);
    w.settle();
    w.collect_notes();
    w.check_settled();
    let delivered = in_ring(&w, &to, &note);
    let answered_busy = lock(&w.ep.state).answered_busy;
    let trace = w.core.trace();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    Busy {
        violations: w.violations,
        held,
        other_while_held,
        answered_busy,
        drops: w.stats.held_for_the_disk,
        delivered,
        committed_past_the_held,
        held_at_the_kill,
        trace,
    }
}

/// Whether `text` is one of the world's message markers (`m<digits>`).
fn is_marker(text: &str) -> bool {
    text.strip_prefix('m')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The incarnation and writer mark whose WILL was the newest node row when `inc`'s attach read
/// the face — the row just before `inc`'s first in `rows` (a node face's rows
/// in log order, [`World::node_rows`]) — if that row is a will.
fn witnessed_by(rows: &[(String, Option<u64>, Option<u64>)], inc: u64) -> Option<(u64, u64)> {
    let first = rows.iter().position(|(_, i, _)| *i == Some(inc))?;
    let (state, dead, mark) = rows.get(first.checked_sub(1)?)?;
    crate::glance::is_will(state)
        .then(|| dead.zip(*mark))
        .flatten()
}

#[cfg(test)]
#[path = "world_tests.rs"]
mod tests;
