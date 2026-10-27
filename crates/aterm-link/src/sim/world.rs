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
//! before any session was handed it; an `auto` mirror writes nothing onto a broker that retains
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
use super::endpoint::{Endpoint, EpRules};
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
    /// A post left queued by a kill takes a fresh sequence on its retry instead of the one pinned.
    FreshSeqOnRetry,
    /// A receipt left owed by a kill takes a fresh sequence on its retry.
    FreshReceiptSeq,
    /// A sender's next post goes out before its last one is answered.
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
    /// The inbox commit a round held rides the next publish unclamped: past the first
    /// record still owed to a session or left unaccounted (`Bridge::take_held_commit`).
    HeldCommitPastTheFloor,
    /// The wind-down drops a held inbox commit: neither landed nor left for the next bridge.
    DropHeldCommitAtWindDown,
    /// A drain peeks the outbox with a post still in flight, its answer not read first.
    PeekPastOwedPost,
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
    /// An offset of ours below the self-lane window's oldest is added to it.
    RememberBelowTheWindow,
    /// A recovery step stopped by a write error still writes `run` after it (`Bridge::recover`).
    BeginRunPastAFailedRecovery,
    /// The mirror judges its pin at the hello before recovery completes (`Bridge::mirror_hello_answered`).
    JudgePinBeforeRecovery,
    /// An own record whose `acked` write or in-flight entry's unlink failed is delivered with its entry standing (`Bridge::settled_before_delivery`).
    DeliverPastAStandingEntry,
    /// The mirror's floor is trusted as it reads.
    TrustTheFloorThatReads,
    /// An attach that could not write the state dir's witness of the log goes on and delivers on it.
    AttachPastAnUnwrittenWitness,
    /// A bridge whose recovery after an unclean stop is owed stops its run cleanly all the same.
    CleanStopOverAnOwedRecovery,
    /// Each try of the recovery's moves publishes a copy of every file again.
    CopyOnEveryTry,
    /// The sweep gives no departed session's subject back (`Bridge::reclaim_subjects`).
    NoSubjectRetire,
    /// The sweep gives a subject back with the unconditional `/a/retire`, whatever landed on it since it read it.
    RetireUnconditionally,
    /// An in-flight entry is published again at its pinned sequence without reading its
    /// record back first (`Bridge::landed_since`), trusting astream's dedup past a cut.
    ReplayWithoutReadingBack,
    /// A post or receipt retried at a pin an earlier attempt took is published again without
    /// reading its record back first (`Bridge::landed_at_its_pin`).
    RetryWithoutReadingBack,
    /// A read back before a republish goes on at the floor (`transport::fetch_kept`), never
    /// told: a record retention still holds below its subject's oldest is missed.
    ReadBackWithoutTheHeld,
    /// A read of one offset that its filter's oldest lies above is taken for no record there,
    /// never read again told (`Bridge::read_at`; the `read_back_rows` of before finding F3): a
    /// record retention still holds there reads as gone, so a session's fetch of it is
    /// answered `retained` and a dedup's read back finds nothing to verify.
    ReadAtWithoutTheHeld,
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
    /// A hand-off whose heads the disk refused unlinks the journal all the same, as before item
    /// F4 (`StateDir::hand_posts_to_files`): the next bridge publishes each record in flight
    /// again at its pin without reading it back, and past a cut that dropped its key a post or
    /// a receipt lands twice.
    HandOffWithoutTheHeads,
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
    /// F4 (`Bridge::post_landed`): a bridge killed before the table is written again leaves
    /// the next one no deadline to sweep, and the ask never expires.
    RetireOverAnUnwrittenDeadline,
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
}

/// The deadlines an ask draws ([`Opts::deadlines`]), in ms: from one that passes before its
/// target can decide it to one a decision meets in time.
pub(crate) const DEADLINES_MS: [u64; 5] = [1_000, 5_000, 20_000, 60_000, 180_000];

/// The broadcast topics of a world of [`Opts::topics`].
pub(crate) const TOPICS: [&str; 2] = ["alpha", "beta"];

/// The peer's session that says on [`TOPICS`].
pub(crate) const SAYER: &str = "s-sayer";

/// How long past an ask's deadline (judged from where its post retired) the sweep may take
/// to say so, the settle's clocks being the bridge's: its roster refresh re-arms it, and a
/// sweep the link was down for runs again once it is back.
const DEADLINE_SLACK_MS: u64 = 30_000;

/// The sibling's one session ([`Opts::sibling`]).
pub(crate) const SIBLING_SID: &str = "s-sibling";

/// A scaled world's rules' bytes ([`Opts::scaled`]): the bulk stream's, the mail lanes' and
/// every other subject's, a few dozen of the world's records each.
pub(crate) const SCALED: (u64, u64, u64) = (2 * 1024, 3 * 1024, 12 * 1024);

/// A scaled world's mirror window, in body bytes ([`Opts::window`]): its records' bytes are
/// past what the bulk stream's rule keeps ([`SCALED`]), so the mirror reaches it only while
/// a tick has yet to cut, waits, and goes on once one has (one under the rule's bytes would
/// wait on the age alone, a week).
pub(crate) const SCALED_WINDOW: u64 = 1024;

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
#[derive(Debug)]
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
    pub(crate) clean_stops: u64,
    /// Kills that met the bridge holding an inbox commit for its next write (critic item B08).
    pub(crate) kills_holding: u64,
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
#[derive(Debug, Default)]
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
    epoch: u64,
    marker: u64,
    sent: BTreeMap<String, Sent>,
    /// Every halt row the person published, in order.
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
    tap: conform::Tap,
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
    /// The offsets of the `expired` records judged ([`World::judge_expiries`]).
    expiries_judged: BTreeSet<u64>,
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
        Some(from) if src.starts_with("n-") && from.starts_with("s-") => from.to_string(),
        _ => src.to_string(),
    }
}

/// A subject of mail on this node's lanes, `/f/<F>/in/<node>/<sid>/<src>/<kind>`: `(sid, src,
/// kind)`.
fn in_lane(subject: &str) -> Option<(&str, &str, &str)> {
    let rest = subject.strip_prefix(&format!("/f/{FLEET}/in/{NODE}/"))?;
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
        for p in [NODE, PEER, PERSON] {
            broker.name_producer(producer(p), p);
        }
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
            epoch: 1,
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
            tap: conform::Tap::new(aterm_models_session::fabric_exactly_once_model()),
            quarantine_at_launch: BTreeSet::new(),
            stats: Stats::default(),
            ret_rng: Rng::new(seed ^ 0x7e7a_14ed_c075_0b05),
            rules_rng: Rng::new(seed ^ 0x01de_b011_d5ee_d0b5),
            fetch_rng: Rng::new(seed ^ 0x0f3f_e7c4_ed0f_f5e7),
            last_tick_ms: 0,
            ask_rng: Rng::new(seed ^ 0x0dea_d11e_0a5c_00f4),
            asks: BTreeMap::new(),
            expiries_judged: BTreeSet::new(),
            expired_over_given_up: Vec::new(),
            verdicts_lost: BTreeSet::new(),
            say_rng: Rng::new(seed ^ 0x05a7_7091_c5f4_0b0b),
            says: BTreeMap::new(),
            follows: Vec::new(),
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

    /// aterm's sessions of this boot: two, with sids of this epoch.
    fn open_sessions(&mut self) {
        for name in ["a", "b"] {
            let local = self.next_local;
            self.next_local += 1;
            let sid = format!("s-{name}{}", self.epoch);
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
        let sid = format!("s-n{local}e{}", self.epoch);
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
        let (near, far) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        self.ep.serve(far);
        let ctl = Ctl::from_stream(near).expect("the verb lane");
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
                if payload.is::<SimKill>() && live.bridge.holds_inbox_commit() {
                    self.stats.kills_holding += 1;
                }
                let process = Arc::clone(&live.process);
                drop(live);
                self.absorb(payload, &process);
            }
        }
    }

    /// Kill the bridge now (a `kill -9` between two of its steps).
    fn kill(&mut self) {
        if let Some(live) = self.live.take() {
            self.note(format!("kill pid={} now", live.process.pid));
            self.stats.kills_holding += u64::from(live.bridge.holds_inbox_commit());
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
                .publish_as(producer(PEER), self.peer_seq, &subject, body.as_bytes());
        self.note(format!("peer mail {marker} to {sid}: {landed:?}"));
        if landed.is_some() {
            self.stats.mail += 1;
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
    /// deadlines an ask to a session carries one three times in four ([`DEADLINES_MS`]).
    fn post_as(&mut self, local: u64, from: &str, to: &str, kind: &str, target: Option<String>) {
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
        let Some(id) = self.ep.post(local, to, kind, &marker, dl) else {
            return;
        };
        self.stats.posts += 1;
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
        self.note(format!(
            "post {from} #{id} {marker} to={to} kind={kind}{dl_word}"
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
                .and_then(|n| n.parse::<u64>().ok());
            match off {
                Some(off) if !r.contains("dup=1") => ask.landed = Some((off, wall, pid)),
                _ => ask.unnoted = true,
            }
        }
        let records = self.core.broker.records();
        let mut bad = Vec::new();
        for (e, subject, body, _) in &records {
            if self.expiries_judged.contains(e) {
                continue;
            }
            let Some((asker, src, "expired")) = in_lane(subject) else {
                continue;
            };
            self.expiries_judged.insert(*e);
            if src != NODE {
                continue;
            }
            let Some(re) = Body::decode(body).0.re else {
                bad.push(format!(
                    "deadline: @{e} on {asker}'s lane expired no record"
                ));
                continue;
            };
            let ask = records.iter().find(|r| r.0 == re).and_then(|(_, s, b, _)| {
                let (target, src, kind) = in_lane(s)?;
                let (b, _) = Body::decode(b);
                (sender(src, &b) == asker && WAITING.contains(&kind) && b.dl.is_some())
                    .then(|| (target.to_string(), b.text))
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
            for (a, s, b, _) in records.iter().filter(|r| r.0 < *e) {
                let Some((sid, src, kind)) = in_lane(s) else {
                    continue;
                };
                let (b, _) = Body::decode(b);
                if sid != asker
                    || sender(src, &b) != target
                    || !ANSWER_KINDS.contains(&kind)
                    || b.re != Some(re)
                {
                    continue;
                }
                if self.core.broker.holds(*a) {
                    bad.push(format!(
                        "deadline: @{e} expired @{re} ({marker}), answered by {target} at @{a}, \
                         which the broker holds"
                    ));
                } else {
                    self.expired_over_given_up.push((asker.to_string(), *a, *e));
                }
            }
            let s = lock(&self.ep.state);
            let handed = s
                .sessions
                .values()
                .chain(s.gone.iter())
                .filter(|x| x.sid == asker)
                .flat_map(|x| x.rows.iter())
                .find(|row| {
                    row.on_log
                        && row.off < *e
                        && row.re == Some(re)
                        && ANSWER_KINDS.contains(&row.kind.as_str())
                })
                .map(|row| row.off);
            drop(s);
            if let Some(a) = handed {
                bad.push(format!(
                    "deadline: @{e} expired @{re} ({marker}), whose answer @{a} {asker} was handed"
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
    /// only where every older copy went with retention before any session was handed it: the
    /// sweep that reads the lane for one then finds none (as a receipt's answer is excused).
    fn expired_once(&self) -> Vec<String> {
        let gone = self.core.broker.gone();
        let mut copies: BTreeMap<(String, u64), Vec<u64>> = BTreeMap::new();
        for (off, subject, body, _) in self.core.broker.records() {
            if let Some((asker, src, "expired")) = in_lane(&subject) {
                if let Some(re) = Body::decode(&body).0.re.filter(|_| src == NODE) {
                    copies.entry((asker.to_string(), re)).or_default().push(off);
                }
            }
        }
        let s = lock(&self.ep.state);
        let handed: BTreeSet<u64> = s
            .sessions
            .values()
            .chain(s.gone.iter())
            .flat_map(|x| x.rows.iter().filter(|r| r.on_log).map(|r| r.off))
            .collect();
        copies
            .into_iter()
            .filter(|(_, offs)| {
                offs.len() > 1
                    && !offs.split_last().is_some_and(|(_, older)| {
                        older
                            .iter()
                            .all(|o| gone.contains(o) && !handed.contains(o))
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
        let records = self.core.broker.records();
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
                let on_bus = records.iter().any(|(o, s, b, _)| {
                    let (b, _) = Body::decode(b);
                    *o == off
                        && in_lane(s).is_some_and(|(to, src, kind)| {
                            to == a.target && sender(src, &b) == a.asker && kind == "ask"
                        })
                        && b.text == *marker
                });
                if !on_bus {
                    return None;
                }
                let (mut answered, mut expired) = (false, false);
                for (_, s, b, _) in &records {
                    let Some((sid, src, kind)) = in_lane(s) else {
                        continue;
                    };
                    let (b, _) = Body::decode(b);
                    if sid != a.asker || b.re != Some(off) {
                        continue;
                    }
                    answered |= sender(src, &b) == a.target && ANSWER_KINDS.contains(&kind);
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
                .publish_as(producer(PEER), self.peer_seq, &subject, body.as_bytes());
        self.note(format!("say {marker} on {topic}: {landed:?}"));
        if landed.is_some() {
            self.stats.says += 1;
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
        let landed = self.core.broker.publish_as(
            producer(PERSON),
            self.person_seq,
            &subject,
            body.as_bytes(),
        );
        self.note(format!(
            "halt {} {marker}: {landed:?}",
            if on { "on" } else { "off" }
        ));
        if landed.is_some() {
            self.stats.halts += 1;
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
        if r.generation_after != r.generation_before {
            self.stats.moves += 1;
        }
        self.stats.lost_records += r.before.len() as u64 - r.kept;
        if r.kept < r.before.len() as u64 {
            self.lost_floor = Some(self.lost_floor.map_or(r.kept, |f| f.min(r.kept)));
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
        for v in bus::law_check(&r, &oracle) {
            self.violations.push(format!("broker law: {v}"));
        }
    }

    /// The broker opens again. In a world of retention's draws one open in four sets no
    /// rules, as `aterm link broker` serves when setting them fails, and of the rest one in
    /// four sets changed rules ([`changed_rules`], as `asb serve --retain-file` would, and the
    /// open after it aterm's again); one that sets them ticks at once, as astream's first tick
    /// is asked for when its rules are set.
    fn broker_start(&mut self) {
        let drawn = self.opts.retained && self.opts.cuts;
        if drawn && self.ret_rng.chance(1, 4) {
            self.stats.unretained_starts += 1;
            self.core.broker.start_unretained();
        } else if drawn && self.rules_rng.chance(1, 4) {
            self.stats.changed_starts += 1;
            self.core.broker.start_with(changed_rules(self.opts.scaled));
        } else {
            self.core.broker.start();
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
        self.ep.power_cut();
        self.boot = self.boot.wrapping_add(1).max(1);
        self.mark = boot_mark(std::path::Path::new(&self.cfg.state_dir), self.boot);
        self.epoch += 1;
        self.core.broker.start();
        self.tick_now();
        self.open_sessions();
    }

    /// Quits and restarts aterm in the same boot ([`Opts::quits`]).
    fn quit(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        self.stats.quits += 1;
        self.note("aterm quits");
        live.mailbox
            .note_bye(aterm_types::fabric_grammar::Bye::Quit);
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
        self.kill();
        let run = crate::state::StateDir::peek(&self.cfg.state_dir).last_run();
        if matches!(run, Ok(Some(run)) if run.clean) {
            self.stats.clean_stops += 1;
        }
        self.ep.power_cut();
        self.epoch += 1;
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
                    lock(&self.core.broker.state).refuse_next = n;
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
        // Before a tick can take what an `expired` is judged against.
        self.judge_expiries();
        self.retention_step(true);
        self.collect_notes();
        self.check_always();
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

    /// The text markers of the bus's records on this node's and the person's
    /// lanes: `marker -> [(offset, subject, body)]`.
    fn bus_markers(&self) -> BTreeMap<String, Vec<(u64, String, Body)>> {
        let mut out: BTreeMap<String, Vec<(u64, String, Body)>> = BTreeMap::new();
        for (off, subject, body, _) in self.core.broker.records() {
            if !subject.starts_with(&format!("/f/{FLEET}/in/")) {
                continue;
            }
            let (body, _) = Body::decode(&body);
            if is_marker(&body.text) {
                out.entry(body.text.clone())
                    .or_default()
                    .push((off, subject, body));
            }
        }
        out
    }

    /// What the model's tap reads of the world now ([`conform::View`]).
    fn view(&self) -> conform::View {
        let lane = format!("/f/{FLEET}/in/{NODE}/");
        let (records, ident) = {
            let s = lock(&self.core.broker.state);
            let records = s
                .log
                .iter()
                .map(|r| {
                    let rest = r.subject.strip_prefix(lane.as_str())?;
                    let sid = rest.split('/').next()?;
                    let (body, _) = Body::decode(&r.body);
                    Some((sid.to_string(), body.text))
                })
                .collect();
            let ident = astream_broker::LogIdentity {
                id: s.id,
                generation: s.generation,
            };
            (records, ident)
        };
        let group = crate::subject::inbox_group(FLEET, NODE);
        let sessions = lock(&self.ep.state)
            .sessions
            .values()
            .map(|s| {
                (
                    s.sid.clone(),
                    conform::SessionView {
                        dedup: s.dedup.clone(),
                        rows: s.rows.iter().map(|r| (r.off, r.text.clone())).collect(),
                        log: s.bus_log.clone(),
                    },
                )
            })
            .collect();
        conform::View {
            records,
            gone: self.core.broker.gone(),
            log: ident.to_string(),
            generation: ident.generation,
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
    fn presence_rows(&self) -> BTreeMap<String, (String, Option<u64>, Option<String>)> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let mut out = BTreeMap::new();
        for (_, subject, body, _) in self.core.broker.records() {
            let Some(sid) = subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/presence"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let (b, _) = Body::decode(&body);
            let state = b.unknown.get("state").cloned().unwrap_or_default();
            let inc = b.unknown.get("inc").and_then(|i| i.parse().ok());
            let mark = b.unknown.get(crate::state::PRESENCE_MARK).cloned();
            out.insert(sid.to_string(), (state, inc, mark));
        }
        out
    }

    /// The running bridge's `(me, witnessed)` incarnation pair read from the bus.
    fn running_bridge(&self) -> Option<(u64, Option<(u64, u64)>)> {
        self.live.as_ref()?;
        let rows = self.node_rows();
        let owner = self
            .mark
            .as_deref()
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
    fn node_rows(&self) -> Vec<(String, Option<u64>, Option<u64>)> {
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        self.core
            .broker
            .records()
            .into_iter()
            .filter(|(_, subject, _, _)| *subject == face)
            .map(|(_, _, body, _)| {
                let (b, _) = Body::decode(&body);
                let state = b.unknown.get("state").cloned().unwrap_or_default();
                let inc = b.unknown.get("inc").and_then(|i| i.parse().ok());
                let mark = b
                    .unknown
                    .get(crate::state::PRESENCE_MARK)
                    .and_then(|mark| u64::from_str_radix(mark, 16).ok());
                (state, inc, mark)
            })
            .collect()
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
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            let node = self.node_rows();
            let own_mark = self
                .mark
                .as_deref()
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

    /// THE READER'S VIEW of every session's `ev` face, decoded and deduped by
    /// `(ep, seq)` as every reader does: per `ms`, the distinct records that
    /// hold it, and the covers.
    fn mirror_faces(&self) -> BTreeMap<String, Face> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let mut out: BTreeMap<String, Face> = BTreeMap::new();
        for (_, subject, body, _) in self.core.broker.records() {
            let Some(sid) = subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/ev"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&body)) else {
                continue;
            };
            let Some(ms) = b.ms else { continue };
            let face = out.entry(sid.to_string()).or_default();
            match b.rec.f.get_u(aterm_slog::Key::N) {
                Some(n) if b.rec.op == aterm_slog::Op::Gap => face.covers.push((ms, n)),
                _ => {
                    face.recs.entry(ms).or_default().insert((b.ep, b.rec.seq));
                    *face.raw.entry((b.ep, b.rec.seq)).or_default() += 1;
                }
            }
        }
        out
    }

    /// THE MIRROR'S AT-MOST-ONCE, at every step (`LogMirror`'s
    /// `ReaderViewAtMostOnce`): no `ms` of a session's log is two records in
    /// the reader's view.
    fn mirror_twice(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for (sid, face) in self.mirror_faces() {
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

    /// Across every boot, at every step: the node's takes
    /// named ONE alias key over its whole life, so an edge has one alias on
    /// the bus; and an engaged mirror's total is never below the highest
    /// total any pin of the node was written with ([`Device::pin_total`]) —
    /// whatever a power cut tore or rolled back.
    fn mirror_key_and_total(&self) -> Vec<String> {
        let mut bad = Vec::new();
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
        for (_, subject, body, key) in self.core.broker.records() {
            let Some(sid) = subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/ev"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&body)) else {
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
        self.core
            .broker
            .records()
            .into_iter()
            .filter(|(off, subject, body, _)| {
                // A record of a session's log (the bridge's own events on the face are not).
                unretained.contains(off)
                    && subject
                        .strip_prefix(prefix.as_str())
                        .and_then(|rest| rest.strip_suffix("/ev"))
                        .is_some_and(|sid| sid != "node" && !sid.contains('/'))
                    && aterm_slog::decode_bus(&String::from_utf8_lossy(body))
                        .is_ok_and(|b| b.ms.is_some())
            })
            .map(|(off, subject, _, _)| {
                format!("mirror: an auto mirror wrote {subject} @{off} onto a broker that retains nothing")
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
        let view = self.view();
        let markers: BTreeSet<String> = self
            .sent
            .iter()
            .filter(|(_, s)| s.to.is_some())
            .map(|(m, _)| m.clone())
            .collect();
        let before = self.tap.violations.len();
        self.tap.observe(&view, &markers);
        let new: Vec<String> = self.tap.violations[before..].to_vec();
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
        }
        if self.opts.deadlines {
            bad.extend(self.expired_once());
        }
        if self.opts.topics {
            bad.extend(self.topic_rows());
        }
        let gone = self.core.broker.gone();
        for (marker, records) in self.bus_markers() {
            // Every copy but the newest given up by retention (sim item B05): the replay of an
            // entry whose record went unread (an own record is never delivered while its entry
            // stands) publishes the one copy a reader is handed. The rows' law is `dup delivery`.
            let once_read = records
                .split_last()
                .is_some_and(|(_, older)| older.iter().all(|r| gone.contains(&r.0)));
            if self.sent.get(&marker).is_some_and(|s| s.post.is_some())
                && records.len() > 1
                && !once_read
            {
                let offs: Vec<u64> = records.iter().map(|r| r.0).collect();
                bad.push(format!(
                    "dup post: {marker} is on the bus {} times {offs:?}",
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
                let offs: Vec<u64> = markers[marker].iter().map(|r| r.0).collect();
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
                            .and_then(|n| n.parse().ok());
                        let on_bus = markers.get(&marker);
                        match (off, on_bus) {
                            (Some(off), Some(records)) if records.iter().any(|x| x.0 == off) => {}
                            (Some(_), None) => {}
                            (Some(off), Some(_)) if self.lost_floor.is_some_and(|f| off >= f) => {}
                            _ => bad.push(format!(
                                "post {} #{} {marker} retired `{r}` but the bus has it at {:?}",
                                session.sid,
                                p.id,
                                on_bus.map(|v| v.iter().map(|x| x.0).collect::<Vec<_>>())
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
                        .push((first.0, *id));
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
        // Every copy but the newest given up by retention before any session was handed it
        // (sim item B05): the replay of an entry whose record went unread (an own record is
        // never delivered while its entry stands) publishes the one copy a reader is handed,
        // as a post's does. A copy some session holds is an answer it was given.
        let gone = self.core.broker.gone();
        let handed: BTreeSet<u64> = s
            .sessions
            .values()
            .chain(s.gone.iter())
            .flat_map(|session| session.rows.iter().map(|row| row.off))
            .collect();
        for ((re, from), offs) in answers {
            let once_given = offs.split_last().is_some_and(|(_, older)| {
                older
                    .iter()
                    .all(|off| gone.contains(off) && !handed.contains(off))
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
            .and_then(|_| self.presence_rows().remove(SIBLING_SID))
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
        if self.stats.moves == 0 {
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

/// Worlds that may run at once in this process: one per two cores, at most six. Each world's
/// state dirs are real folders whose every write is a create and a rename, and past six worlds
/// at once those metadata calls queue in the kernel: on 18 cores the lib's tests at nine took
/// 224 CPU-s for 25 s, at six 170 CPU-s for 27 s, the same instructions either way.
static WORLD_SLOTS: LazyLock<(Mutex<usize>, Condvar)> = LazyLock::new(|| {
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    (Mutex::new((cores / 2).clamp(1, 6)), Condvar::new())
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
        let before = w.presence_rows();
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
    let errors = w.core.device.write_errors();
    w.stats.writes_failed = errors.writes;
    w.stats.unlinks_failed = errors.unlinks;
    w.stats.flushes_failed = errors.flushes;
    w.stats.renamed_unflushed = errors.renamed_unflushed;
    w.stats.appends_cut = errors.appends_cut;
    w.stats.disk_full_refused = errors.disk_full;
    Outcome {
        seed: opts.seed,
        tier: w.tier,
        violations: w.violations,
        trace,
        stats: w.stats,
        model: (w.tap.opened, w.tap.checked, w.tap.fired.clone()),
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
