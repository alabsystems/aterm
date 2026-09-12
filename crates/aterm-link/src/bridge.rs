// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BRIDGE — `aterm-link serve`.
//!
//! One process, one node, N broker connections and two inherited aterm
//! descriptors. What it does, in the order §11.2 lists it:
//!
//! * publishes `ev`, `presence` and outbound `post`s under the node's bound cap,
//!   with the producer sequence persisted BEFORE each publish and the
//!   incarnation taken from `max(local state, its own last presence row) + 1`;
//! * registers the `Will` on every connect, so a `kill -9` becomes exactly one
//!   `state=gone` and a reconnect's `live inc+1` structurally suppresses it;
//! * drains `/f/<F>/in/<node>/>` as a durable group into `deliver`, committing
//!   only once the record is ACCOUNTED FOR — an `OK`, or an `ERR` that produced
//!   an `ev` and a sender notice. A transport failure leaves the cursor where it
//!   is, because at-least-once cursor plus an idempotent sink is what makes the
//!   whole path exactly-once and that argument needs the commit to be
//!   conditional ([`Bridge::on_inbox_record`]);
//! * drains the `fleet/>` tail AHEAD of that group on every round, so a halt
//!   never queues behind a redelivery backlog;
//! * mirrors the fleet halt into `hold` and acks it;
//! * records `undeliverable` and `refused` verdicts as `ev` records;
//! * persists `seen_off` and refills a session's ring after an instance relaunch.
//!
//! ## A NODE'S DISTINCT-SUBJECT BUDGET IS FINITE, AND NOTHING RECLAIMS IT
//!
//! The broker bounds one producer at `MAX_SUBJECTS_PER_PRODUCER` = 4096, rebuilt
//! from the log on every open, and a node's producer id is derived from an id the
//! state dir mints once and keeps forever — so the budget does not clear on
//! restart. This node mints one distinct subject per hosted session on each of
//! `presence`, `ev` and `control`, plus `screen` and one per `say/<kind>`: call it
//! three to five per session over the node's lifetime, so a long-lived instance
//! exhausts the budget somewhere past a thousand sessions. Past the bound every
//! publish to a NEW subject is refused, which means a newly spawned session gets
//! no presence row, no `ev` face and no `control` row while existing sessions keep
//! working and nothing on the bus says why.
//!
//! DESIGN §5.2's stated mitigation — "the bridge collapses a session's `exited`
//! presence row into an `ev` record after `--exited-keep <n>` (default 64)
//! sessions" — IS NOT IMPLEMENTED, and `--exited-keep` is refused by `serve`'s
//! own argument parser as an unknown flag. That is written here rather than left
//! for an operator to discover at 4096, because this crate's docs are its only
//! evidence: the ceiling is real, it is roughly a thousand sessions per node id,
//! and the recovery today is minting a new node id (which abandons that node's
//! mail lane by design).
//!
//! ## The two lanes to aterm, and why losing either one must END this process
//!
//! The bridge holds TWO inherited descriptors: the VERB lane (fd 3) and the
//! PUSH lane (fd 4). aterm's fail-closed guard fires when EITHER closes — every
//! session the bridge ever touched is held `reason=fabric-lost origin=fleet` —
//! and §11.2's stated recovery is that "the instance relaunches the child with
//! back-off". That recovery only exists if the child exits.
//!
//! A3 watched one lane. `Item::Closed(Source::Aterm)` came from the push
//! reader alone, and every verb-lane failure was swallowed at its own call
//! site, so a verb lane that died on its own left a live child draining and
//! committing the inbox group into a dead socket while `child.wait()` blocked
//! forever and the whole instance stayed halted. Both halves are now closed:
//! [`crate::ctl::Ctl`] refuses to write a line aterm would drop the connection
//! for, and [`Bridge::ctl_request`] turns a lane that IS lost into the closure
//! notice the loop already knows how to act on.
//!
//! ## The path to a PTY, and the wall around it
//!
//! Exactly one function writes to a PTY — [`Bridge::feed`] — and exactly two
//! call it. [`Bridge::on_term_record`] is reachable only from the `term`
//! subscription and checks §6.6's four conditions in one place;
//! [`Bridge::on_inbox_record`] cannot reach either. That is not a comment — it
//! is the module structure, and
//! `no_inbox_record_ever_reaches_the_pty_and_a_stale_epoch_is_refused` in
//! `tests/bridge_e2e.rs` is the claim over it. (The name is PINNED by
//! `this_modules_doc_and_unsafe_surface_match_what_it_ships`: aterm ships no
//! evidence manifest, so a doc comment IS the claim, and this header used to
//! cite a test that has never existed under that name — an auditor following it
//! got `0 passed; 0 filtered out`, a green run over an empty set.)
//!
//! The second caller is [`Bridge::resolve_pending_feed`], and it is named here
//! rather than buried because it is a second way a bus record becomes a
//! keystroke. Its authority is narrower than the first's, not wider: it feeds
//! only a record whose offset [`Bridge::on_term_record`] already journalled
//! AFTER passing the four conditions, only under the key journalled with it, and
//! only after re-fetching that exact offset off the bus and re-parsing its
//! subject. The epoch is re-checked structurally (it is inside the key) and so
//! is the hold (the endpoint's gate precedes any write); the holder and the
//! generation are deliberately not, for the reasons written on that function.
//! Nothing else in this crate can put an offset in that journal.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astream_broker::Record as BrokerRecord;

use crate::body::{via_ok, Body};
use crate::ctl::{Ctl, Reply, REQUEST_LINE_MAX};
use crate::handoff::{self, decide_control, Decision, Event as HandoffEvent};
use crate::mailbox::{Item, Mailbox, Source};
use crate::state::StateDir;
use crate::subject::{self, Reject};
use crate::transport::{self, Closer, Conn, Transport};

/// How long the loop parks with nothing to do before running its periodic
/// duties (a reconnect attempt, a presence refresh). NOT a poll interval:
/// nothing is discovered by waking up, and every real input wakes the mailbox.
const IDLE_TICK: Duration = Duration::from_millis(250);

/// The reconnect back-off floor and ceiling. §7: "a bridge therefore reconnects
/// with jittered back-off and opens its publisher connection *before* its drain
/// connection", because `MAX_CONNS` can refuse a herd and the publisher's
/// `live inc+1` is what suppresses a fenced will.
const RECONNECT_MIN: Duration = Duration::from_millis(100);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// How far behind the log's head a refill starts when a session's `seen`
/// watermark is missing.
///
/// A bound rather than zero: `Fetch` is a linear scan, so a floor of zero is one
/// whole-log walk per freshly seen session — and a brand-new sid, which is the
/// common case for a missing watermark, has no history to find. This is the
/// window a genuinely lost watermark can be hiding delivered-but-unseen rows in,
/// and re-offering them is free because `deliver` is idempotent on `off=`.
const REFILL_FLOOR_SPAN: u64 = 4096;

/// How often an unfinished feed is re-asked from the MAIN loop.
///
/// The budget is [`FEED_BUDGET`], a DURATION; this is only the gap between
/// attempts inside it, and it is deliberately the idle tick's own period so a
/// busy bridge and a quiet one re-ask at the same rate. It is not itself a
/// budget and it no longer has to be long enough for anything: it used to be
/// documented as "long enough for a `turn` to finish or a hold to lift", which
/// at 250 ms it plainly is not, and the count it multiplied into was written
/// against a two-second cadence.
const FEED_RETRY: Duration = Duration::from_millis(250);

/// HOW LONG a `term/in` whose outcome was never obtained is re-asked for before
/// the bridge stops asking and publishes what it knows.
///
/// ## A COUNT OF RETRIES IS NOT A BUDGET
///
/// [`FEED_TRIES_MAX`] was eight, and eight meant fourteen to sixteen seconds
/// because `resolve_pending_feed` ran once per idle roster round (8 ×
/// `IDLE_TICK`). Round 2 gave the retry its own [`FEED_RETRY`] deadline —
/// correctly, because a busy bridge never reaches the idle arm — and the same
/// eight attempts silently became 1.75 s, while both constants' docs went on
/// saying "long enough for a hold or a lease to clear". It is the trap
/// `DRIVEN_KEEP` and `SETTLE_QUIET` were converted away from, one block down,
/// in the same commit: EVERY WINDOW IN THIS FILE IS A DURATION, because a count
/// means whatever the scheduler is doing this month.
///
/// And the cost of getting it wrong is not a shorter wait. Exhausting the
/// budget RETIRES the journal, and §6.5 makes the record it publishes terminal
/// — reported, never replayed. So a keystroke the endpoint refused CLEANLY on
/// every attempt, before a byte could move, was published to the fleet as "it
/// may have typed and nothing can tell" and became unrecoverable, 1.75 s into
/// a refusal that outlives that by design: `LEASE_TTL_MS` is 30 s, an `ERR
/// busy` is a whole `turn`, and `ERR halted` is lifted by a human.
///
/// Forty-five seconds covers a lease TTL and a long `turn` with room to spare,
/// and still puts the news on `ev` inside a minute. What was NOT extended is
/// the arithmetic that ties it to a period: nothing here multiplies a count by
/// a cadence, so [`FEED_RETRY`] can move again without moving this.
const FEED_BUDGET: Duration = Duration::from_secs(45);

/// How often the roster and the LOCAL observations are re-read.
///
/// FROM A DEADLINE, NOT FROM THE IDLE ARM. It used to be `ticks % 8` where
/// `ticks` advanced only in the mailbox's `None` branch — reachable only after a
/// full [`IDLE_TICK`] with every queue empty — so a bridge receiving one item
/// per 250 ms never ran it at all. The same commit that made this argument for
/// `renew_leases`, `resolve_pending_feed` and `publish_screens` left these
/// behind, and what starves is not bookkeeping: [`Bridge::sample_local_control`]
/// is the ONLY producer of §6.6 row 4 (the conservative pause, which takes the
/// keyboard from a remote holder once a human has touched the session), the only
/// producer of row 5 (the local-lease mirror), and the only place `attention=` is
/// re-sampled — the field `notify --on attention` and `glance` read. A peer
/// posting four times a second is enough to hold a node in that state
/// indefinitely.
///
/// The period is the one the tick gate used to produce (8 × 250 ms), so a quiet
/// bridge pays exactly what it paid before and a busy one now pays it too.
const ROSTER_REFRESH: Duration = Duration::from_millis(2_000);

/// How often the §6.6 row-4 observation is taken for a session a remote
/// principal HOLDS.
///
/// ## A SAMPLER IS ONLY AS GOOD AS WHAT IT CAN STILL SEE
///
/// Row 4's evidence is an EDGE, not a level. `status revision=` advances only
/// when aterm's classifier PUBLISHES a phase change, and a phase is only
/// published while the thing that caused it is still true: the classifier reads
/// a foreground-job Boolean, so `sleep 2` is `phase=running` for two seconds and
/// `phase=idle` before and after. Nothing anywhere records that it ran. A
/// sampler slower than the command therefore does not see the change LATE — it
/// never sees it at all, and §9.3's structural "a human always wins" silently
/// does not hold.
///
/// That is not hypothetical and it is not only about long periods. Sharing
/// [`ROSTER_REFRESH`]'s 2 s deadline also PHASE-LOCKED this observation to the
/// loop's other periodic duties: 2 s divides [`LEASE_RENEW`]'s 10 s exactly, so
/// the roster arm ran in the same iteration as the lease renewal, every time,
/// and a session was therefore sampled at the same point in every cycle. A
/// stroboscope. A person who typed just after a renewal was invisible for the
/// whole two seconds that followed — reproducibly, not by luck.
///
/// So this observation gets its own deadline at aterm's own classification
/// interval (250 ms — the rate at which `status` can produce a NEW verdict, so a
/// faster sampler could learn nothing more), and it visits only the sessions
/// row 4 applies to: the HELD ones. A free session's row-4 verdict is discarded
/// unread, and its baseline is re-taken by [`Bridge::baseline_local`] the moment
/// a holder appears — so the cost is one `status` per HELD session per 250 ms,
/// which is the rate [`FEED_RETRY`] and the screen face already run at, and not
/// a per-session poll of the whole roster.
///
/// [`Bridge::sample_local_control`] still runs the WHOLE local sweep — the
/// `attention=` re-read and row 5's local-lease mirror as well — on
/// [`ROSTER_REFRESH`]. Those two are levels, not edges: they are still true when
/// they are looked at late.
const LOCAL_OBSERVE: Duration = Duration::from_millis(250);

/// The furthest behind the head the drive face will RESUME from.
///
/// The durable cursor (`state.rs`: `term-off`) is the resume point, and this
/// bounds what one long absence can cost: a bridge that was down for a day comes
/// back to a window it can walk rather than to the whole log, and the offsets it
/// skips are NAMED in an `ev` rather than passed over silently. A3's head-resume
/// skipped every one of them and named none.
const TERM_RESUME_SPAN: u64 = 4096;

/// The most pages one last-value walk may take before it is called a failure.
///
/// A liveness bound, not a size one: the resume cursor advances every page, so a
/// walk that has not finished in this many has met something pathological, and
/// the honest answer to a caller that must not read absence as evidence is an
/// error rather than a short list. 256 rows a page puts the ceiling at a million
/// rows.
const LAST_PAGES_MAX: usize = 4096;

/// How many of this node's own acked offsets the self-lane check remembers.
const SELF_ACK_KEEP: usize = 4096;

/// The TTL the `control` row's mirror lease is taken with, and how often it is
/// RENEWED (§6.6: "renewed while the row stands"). A3 issued the acquire once
/// and never renewed it, so after 30 s aterm's `who` stopped saying
/// `driving=lease:fabric:<p>` and a competing local `turn` stopped answering
/// `ERR busy` — the mirror silently lapsed while the bus row still stood.
///
/// The renewal is a LOCAL verb call, not a republished record: a TTL kept alive
/// on the bus would mean a retained record per held session per period, forever.
const LEASE_TTL_MS: u64 = 30_000;
const LEASE_RENEW: Duration = Duration::from_millis(LEASE_TTL_MS / 3);

/// The reserved TOP of an incarnation's sequence space — the will's sequence, so
/// no ordinary publish of that incarnation can collide with it (§7).
const WILL_SEQ_LOW: u64 = 0xFFFF_FFFF;

/// The kinds an unlisted principal may not speak AS. A `task` or a `control`
/// from a principal that is neither a human nor named on `--accept-from` is
/// delivered `kind=note demoted=<k>`: a stranger may put words in front of an
/// agent, never an instruction (§8.4).
///
/// EVERY `h-*` IS ACCEPTED WITHOUT BEING LISTED, and this constant's doc used to
/// say otherwise — see [`Bridge::classify_kind`], which is where the predicate
/// lives and argues itself. `h-` is a CAP-FORCED `<src>` segment, so a principal
/// can only speak as a human if the broker's own grant binds it to one; the
/// carve-out is what makes §6.6's row 1 (a human's `claim`, "granted
/// unconditionally") reachable on a node whose operator listed nobody.
/// `--accept-from` therefore adds NON-human principals to the set, and adds
/// nothing for a human who is already in it.
///
/// `answer` is deliberately not here — an answer's authority is the `re=` the
/// receiver itself minted, so it is answerable by whoever the asker asked.
const DEMOTE_UNLESS_ACCEPTED: [&str; 2] = ["task", "control"];

/// The kinds that ARE a verdict, and therefore may never earn one.
///
/// `notify_sender_undeliverable` publishes a refused record's verdict onto a
/// lane the fabric actually drains — which is what makes the verdict useful, and
/// what makes it a record like any other: it is addressed, delivered, and
/// refusable. Nothing exempted a verdict from being answered with a verdict, so
/// two saturated per-sender quotas closed the loop: the refusal of a
/// `kind=undeliverable` published another `kind=undeliverable` under the same
/// `from=` that was already at quota, forever, two durable `fsync`ed records per
/// round trip on an append-forever log. A `post` is an ordinary `Scoped` verb, so
/// 65 of them from any in-session client armed it with no capability at all
/// (§8.5's stated adversary).
///
/// The property pinned here is the narrow one: A VERDICT NEVER GENERATES A
/// VERDICT. The refusal is still recorded as an `ev`, so the fate of the notice
/// is on the log; what does not happen is another notice.
const VERDICT_KINDS: [&str; 2] = ["undeliverable", "expired"];

/// The most bytes a `reason=` token may carry onto the bus.
const REASON_TOKEN_MAX: usize = 32;

/// How the bridge reached aterm — and therefore what it is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// The inherited descriptors: `Scope::Bridge`, the real thing.
    Inherited,
    /// A hand-started `--sock` connection with the Owner token. OBSERVER MODE:
    /// presence and `ev` only, no `deliver`, no `hold` — and it says so rather
    /// than discovering it one refusal at a time.
    Observer,
}

/// One sample of the LOCAL facts §6.6's last two rows are computed from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LocalSample {
    /// Whether `revision` is a real observation yet. THE FIRST SAMPLE IS A
    /// BASELINE, NOT A CHANGE: without this, every session whose classifier had
    /// ever moved would read as an unaccounted change the first time it was
    /// looked at, and the conservative pause would fire on a session nobody had
    /// touched.
    seen: bool,
    /// The `status revision=` this bridge last observed.
    revision: u64,
    /// When the classifier was last seen to MOVE — or, for a session that has
    /// only ever been sampled once, when it was first looked at. A session that
    /// has not been quiet since then is still settling from something already
    /// accounted for — its own launch, or the keystroke before this one — and an
    /// advance while it settles is not news. A launching session goes
    /// `starting → idle` on its own within a second, which is exactly the burst
    /// that would otherwise pause every freshly handed-over session.
    ///
    /// A CLOCK, NOT A COUNT OF SAMPLES. See [`SETTLE_QUIET`].
    moved_at: Option<Instant>,
    /// When the bridge applied a `term/in` whose effect on the revision has not
    /// been observed yet. Consumed by the sample that SEES the advance — never
    /// by the one right after the write, because the output a keystroke causes
    /// lands after the verb returns. A clock, for [`DRIVEN_KEEP`]'s reason.
    driven_at: Option<Instant>,
}

/// How long an unconsumed `driven` mark survives.
///
/// EVERY WINDOW IN THIS BLOCK IS A DURATION, AND IT USED TO BE A COUNT OF
/// SAMPLES. `DRIVEN_KEEP_QUIET = 2` and `SETTLED_QUIET = 1` were counts, so what
/// they actually meant was "two sampling periods" and "one sampling period" —
/// numbers that lived in the SCHEDULER, not here. That coupling is silent and it
/// is a trap: [`LOCAL_OBSERVE`] exists precisely because the observation period
/// had to change, and changing it under a count-based rule moves every one of
/// these windows by the same factor without a line of this file being touched.
/// (Measured: with the windows still counted in samples, dropping the period to
/// 250 ms shrank the settling window from ~2 s to 250 ms and the conservative
/// pause fired on a session's own handover burst.) A rule whose meaning is a
/// span of time says so in seconds.
///
/// Four seconds is the span the old two-sample rule produced at the old ~2 s
/// period, and the reason for a span at all is unchanged: the sample immediately
/// after a `feed-bin` routinely sees nothing yet, and clearing the mark there
/// would pause the session on its own keystroke.
const DRIVEN_KEEP: Duration = Duration::from_millis(4_000);

/// How long a session must have been seen quiet before an advance counts as an
/// unaccounted change. See [`DRIVEN_KEEP`] for why this is a duration.
///
/// One second is what the rule's own justification names — "a launching session
/// goes `starting → idle` on its own within a second" — rather than whatever the
/// sampling period happens to be.
const SETTLE_QUIET: Duration = Duration::from_millis(1_000);

/// TEST-ONLY fault injection, armed by `$ATERM_LINK_FAULT` — and TEST-ONLY is
/// enforced, not merely documented: [`Fault::from_env`] reads the variable only
/// in a build with `debug_assertions`, so a released `aterm-link serve` honours
/// no fault at all.
///
/// It had to become enforcement. The knob was documented test-only and shipped
/// in every build, while `ATERM_LINK_FAULT` is on neither `ENV_DENY_VARS` nor
/// `ENV_DENY_PREFIXES` — so it survives the PTY child-shell seam and
/// `fabric_launch::filter_child_env`, and a prompt-injected agent inside a
/// session could arm `kill-in-feed-window` on a nested instance's bridge and
/// leave every session that bridge governs held `fabric-lost`. §8.5 states the
/// intended rule ("the deny-list keeps every fabric selector (`ATERM_LINK_*`)
/// from surviving a hop"); the deny-list names three of the five variables, and
/// widening it lives in `aterm-types`, so this crate closes its own half here.
///
/// The A3 rung asserts what survives a CRASH *between* two specific steps —
/// `deliver` and `Commit`. That window is microseconds wide and there is no
/// honest way to hit it from outside the process: a test that raced it would be
/// the flake this codebase keeps refusing to accept. So the bridge kills ITSELF
/// at exactly the named point. The crash is real — [`Fault::fire`] calls
/// `abort()`, which raises `SIGABRT` and runs no destructor; only the timing is
/// chosen, and it is chosen by the process whose progress defines the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// `SIGKILL` after `deliver` answers `OK`, before `Commit`.
    KillAfterDeliver,
    /// `SIGKILL` after a post's `Publish` is acked, before `outbox sent`.
    KillAfterPublish,
    /// `SIGKILL` inside the FEED WINDOW: after the journal names the `term/in`
    /// and its key, after `feed-bin` answered `OK`, and before the outcome is
    /// recorded (§6.5). This is the window A3 lost a keystroke in — silently,
    /// because its drive face resumed at the head — and the one A7 asserts
    /// "applied ONCE" across.
    KillInFeedWindow,
    /// LOSE THE VERB LANE, without killing the process, immediately before the
    /// first `deliver` — the failure this bridge used to be unable to see.
    ///
    /// It is not a kill because the point is what happens while the process
    /// LIVES: the verb lane (fd 3) is gone, the push lane (fd 4) is not, and the
    /// bridge must (a) not commit the group cursor past a record the endpoint
    /// never took and (b) exit anyway, so its supervisor relaunches it. Racing
    /// that from outside is impossible — there is no way to close ONE inherited
    /// descriptor of another process — so the process closes its own, at the
    /// named point.
    LoseAtermBeforeDeliver,
    /// Answer the FIRST `feed` with a TRANSIENT refusal without asking aterm.
    ///
    /// `ERR busy turn=<id>` is what a live endpoint answers while a local turn
    /// holds the write-block lease, and it is the verdict
    /// [`Verdict::is_final`] deliberately calls non-final: the journal is kept
    /// with NO `ev` published so the record can be asked again. It is a REACHABLE
    /// state and not an impossible one, which an earlier version of this doc had
    /// backwards: only the CLAIM-FIRST order is exclusive (a `turn` started under
    /// the bridge's live cooperative lease is refused `ERR busy lease=`), and in
    /// the TURN-FIRST order the human's `control` claim lands anyway —
    /// [`Bridge::acquire_lease`] records the refusal as an `ev lease-refused`
    /// rather than failing the handoff — so the bridge drives a session whose
    /// turn refuses every keystroke. What the fault buys is DETERMINISM: the
    /// reply, not the state. Everything downstream of it is the shipped path.
    RefuseFirstFeed,
    /// Answer EVERY `feed` with the same transient refusal for as long as a
    /// marker file (`<state>/refuse-feeds`) exists — a refusal WINDOW rather
    /// than a single reply, and one whose length the test controls from the
    /// observable the bridge itself writes.
    ///
    /// [`FEED_BUDGET`] is a span of wall clock, and the property it exists for
    /// is that a keystroke refused CLEANLY for longer than a scheduler round is
    /// still there when the refusal lifts. Asserting that means holding a
    /// transient refusal open across many attempts — a real `turn` would have to
    /// hold a session's write-block lease while the bridge holds that session's
    /// cooperative lease for the driving human, which the design makes mutually
    /// exclusive — and then lifting it at a point defined by the bridge's OWN
    /// progress (`tries=` in the journal) rather than by a clock the test races.
    /// Everything downstream of the reply is the shipped path.
    ///
    /// It is NOT one-shot: the window is the whole point, so it neither clears
    /// itself nor writes `fault-fired`.
    RefuseFeedsWhileMarked,
    /// Answer every `@<sid> status` with a REFUSAL for as long as a marker file
    /// (`<state>/fail-status`) exists, so [`Bridge::status_sample`] answers
    /// `None` — the unreadable state whose old handling folded into
    /// `revision = 0`.
    ///
    /// A real one is a session mid-relaunch or a verb lane that errored inside
    /// one round trip: microseconds wide, and racing it from outside is the
    /// flake this codebase refuses. The window is opened and closed by the test,
    /// and everything the bridge does with the `None` is the shipped path.
    FailStatusWhileMarked,
    /// Fail the `Publish` of every outbound POST for as long as a marker file
    /// (`<state>/fail-post-publish`) exists — after the durable sequence has
    /// been reserved, which is the state that leaves a reservation on disk.
    ///
    /// It is the precondition of the leak the `route =>` arm had: a drain that
    /// resolved a post, reserved its sequence and could not publish keeps the
    /// file (correctly — the retry needs it), and a LATER drain that resolves
    /// the same address differently retires the post and used to walk away
    /// from the file forever. Staging it needs the broker to refuse one publish
    /// at one instant; a test cannot reach inside the connection, so the bridge
    /// stages it.
    FailPostPublishWhileMarked,
    /// PAD THE `deliver` REQUEST LINE past [`REQUEST_LINE_MAX`], once.
    ///
    /// Every variable-length field the line carries is now bounded — `via=` by
    /// [`crate::body::VIA_MAX_HOPS`], the body by the remaining budget, the rest by
    /// `subject::is_principal` or a closed set — so the total-line check that
    /// catches a field a LATER rung adds cannot be reached by any record a peer
    /// can publish. That is the point of it, and it is also why it would
    /// otherwise ship untested: the guard exists for the case the next author
    /// creates, and the one honest way to stage that case is for the process to
    /// stage it itself. What follows the padding is the shipped path.
    OversizeDeliverLine,
}

impl Fault {
    /// ONE SHOT, across restarts. The marker lives in the state dir because the
    /// crash is the whole point: the process that would remember is gone, and a
    /// fault that re-armed on every relaunch would kill the bridge at the same
    /// step forever — the test would then be asserting about a bridge that never
    /// got past it, which is not the property.
    fn from_env(state: &StateDir) -> Self {
        // A RELEASED BINARY HONOURS NO FAULT. See the type's own doc: the knob is
        // documented test-only and was gated by nothing, in a process whose death
        // is the fail-closed halt of every session it governs.
        if !cfg!(debug_assertions) {
            return Fault::None;
        }
        if state.root().join("fault-fired").exists() {
            return Fault::None;
        }
        match std::env::var("ATERM_LINK_FAULT").as_deref() {
            Ok("kill-after-deliver") => Fault::KillAfterDeliver,
            Ok("kill-after-publish") => Fault::KillAfterPublish,
            Ok("kill-in-feed-window") => Fault::KillInFeedWindow,
            Ok("lose-aterm-before-deliver") => Fault::LoseAtermBeforeDeliver,
            Ok("refuse-first-feed") => Fault::RefuseFirstFeed,
            Ok("refuse-feeds-while-marked") => Fault::RefuseFeedsWhileMarked,
            Ok("fail-status-while-marked") => Fault::FailStatusWhileMarked,
            Ok("fail-post-publish-while-marked") => Fault::FailPostPublishWhileMarked,
            Ok("oversize-deliver-line") => Fault::OversizeDeliverLine,
            _ => Fault::None,
        }
    }

    /// Mark a NON-FATAL fault as fired. One shot within the process and across
    /// restarts, for the same reason [`Fault::fire`] is: a fault that re-armed
    /// on every relaunch would leave the test asserting about a bridge that
    /// never got past it.
    fn fired(self, state: &StateDir) {
        let _ = std::fs::write(state.root().join("fault-fired"), b"1\n");
        eprintln!("aterm-link: ATERM_LINK_FAULT={self:?} — firing now");
    }

    /// Die HERE, running no destructors. Not `exit`: an orderly exit would run
    /// destructors, flush the connection and publish a clean goodbye — which is
    /// the opposite of what the rung is testing.
    ///
    /// `abort()` and NOT a raw `kill(2)`, for the reason [`crate::notify`]'s
    /// identical one-shot fault gives in as many words: it keeps this module
    /// free of `unsafe`. §11.2 pins aterm's unsafe surface to the cordon that
    /// already owns raw-descriptor syscalls (`aterm-uds`), and the bridge's run
    /// loop is not that cordon — a reviewer auditing by the documented rule
    /// would never look here. `abort` satisfies every property this fault
    /// needs: the process dies at this instruction, no destructor runs, and
    /// nothing is flushed.
    fn fire(self, at: Fault, state: &StateDir) -> ! {
        assert_eq!(self, at);
        let _ = std::fs::write(state.root().join("fault-fired"), b"1\n");
        eprintln!("aterm-link: ATERM_LINK_FAULT={at:?} — killing this process now");
        std::process::abort();
    }
}

/// One minted capability, as `asb mint` prints it and `--cap-file` reads it.
#[derive(Debug, Clone)]
pub struct Cap {
    pub grant: String,
    pub tag: Vec<u8>,
}

/// Read a `--cap-file`: `<grant> <tag-hex>` lines, split at the LAST whitespace.
///
/// The last, not the first: the wire admits a SPACE inside a filter, so a grant
/// may hold one, and the tag is the fixed-width whitespace-free tail. Splitting
/// at the first whitespace turns `ro:/f/F/pub a/> <tag>` into a parse error that
/// takes the whole keyring down with it — `asb` learned this the same way.
///
/// # Errors
///
/// The file read, or a line that is not `<grant> <hex>`.
pub fn read_cap_file(path: &str) -> io::Result<Vec<Cap>> {
    let text = std::fs::read_to_string(path)?;
    let mut caps = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((grant, tag_hex)) = line.rsplit_once(char::is_whitespace) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path}: a cap line is `<grant> <tag-hex>`"),
            ));
        };
        let tag = hex_to_bytes(tag_hex).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{path}: the tag is not hex"),
            )
        })?;
        caps.push(Cap {
            grant: grant.trim().to_string(),
            tag,
        });
    }
    Ok(caps)
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || s.is_empty() {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// Everything `serve` was told.
#[derive(Debug, Clone)]
pub struct Config {
    pub fleet: String,
    /// The broker's endpoint: a Unix socket path, or `<host>:<port>` for the
    /// two TCP transports.
    pub broker: String,
    /// How to reach that endpoint (§8.6).
    pub transport: Transport,
    pub cap_files: Vec<String>,
    pub state_dir: String,
    /// Principals whose `task`/`control` are delivered as themselves rather than
    /// demoted to `note`.
    pub accept_from: Vec<String>,
    /// `--screen <sid|all>`: sessions whose SCREEN is published to the bus
    /// (§3.3's `/f/<F>/term/<node>/<sid>/screen`, "a full `DELTA screen`
    /// snapshot, opt-in, ≤ 4/s").
    ///
    /// OPT-IN, and empty by default, because the log is a secrets store: §14's
    /// second open question is exactly "`term/screen` cadence and who may hold
    /// `ro:…/screen`", and T12 says an at-rest exfil of the log is total until
    /// encrypt-at-rest lands. A fabric that published every screen because it
    /// could would have decided that question by accident.
    pub screen: Vec<String>,
    /// `--sock <path>`: hand-started observer mode.
    pub sock: Option<String>,
    /// The instance token, for observer mode only.
    pub token: Option<String>,
}

/// The bridge's live state.
pub struct Bridge {
    cfg: Config,
    node: String,
    producer_id: u64,
    inc: u64,
    state: StateDir,
    caps: Vec<Cap>,
    attachment: Attachment,
    fault: Fault,
    /// The verb connection to aterm (fd 3, or the observer socket).
    ctl: Ctl,
    /// The publisher/commit connection. `None` while the broker is unreachable —
    /// and that is the whole of "the broker is down" as far as this process is
    /// concerned: holds stay, posts stay queued, nothing is lifted.
    conn: Option<Conn>,
    /// `local id -> sid`, refreshed from `sessions`. The events digest names the
    /// LOCAL id; every subject names the sid.
    locals: BTreeMap<u64, String>,
    /// `sid -> epoch` (the session's public launch nonce, verbatim).
    epochs: BTreeMap<String, String>,
    /// `sid -> the principal holding its keyboard` — the local twin of the
    /// last-value `control` row, RESTORED from the bus on every connect
    /// ([`Bridge::restore_control_rows`]) so a bridge restart does not silently
    /// hand a driving human's keyboard back to nobody.
    holders: BTreeMap<String, String>,
    /// `sid -> the principal whose `request` is waiting on the holder` (§6.6
    /// row 2). Carried on the `control` row as `pending=` so the holder's next
    /// wake shows it.
    pending: BTreeMap<String, String>,
    /// `sid -> what the local instance last said about it`, for the two rows of
    /// §6.6 that are LOCAL observations rather than bus events: the conservative
    /// pause and the mirrored `lease acquire`.
    local: BTreeMap<String, LocalSample>,
    /// The sessions this bridge has NEWLY seen and not yet admitted to the
    /// fabric: presence published and, above all, §6.2's ring refill run.
    ///
    /// IT IS A QUEUE BECAUSE DISCOVERY AND ADMISSION HAPPEN AT DIFFERENT TIMES.
    /// [`Bridge::refresh_sessions`] is called from five places and only ONE of
    /// them used its return value; the other four discarded it with `let _ =`,
    /// including [`Bridge::run`]'s very first call — which happens before the
    /// broker is attached and had therefore already inserted every session the
    /// instance hosts by the time anything could refill one. The roster tick's
    /// `fresh` list was empty for those sessions forever after, and aterm's
    /// `sessions` push stream only reports a `session-created` for a session
    /// that appears AFTER the subscription, so no event arrived for them
    /// either. A session that existed when the bridge started was never
    /// refilled — which is exactly the seamless-update case §6.2's refill was
    /// written for, where the sid survives, the endpoint's ring does not, and
    /// the durable group cursor is already committed past every row.
    ///
    /// So discovery ADDS here, wherever it happens, and admission drains it
    /// once there is a connection to admit it on. It is pruned to the live
    /// roster beside the other per-sid maps, so a long disconnection cannot
    /// grow it.
    pending_admit: BTreeSet<String>,
    /// When the mirror leases are next renewed.
    lease_due: Instant,
    /// When the roster, the local §6.6 observations and the halt backstop are
    /// next re-read. See [`ROSTER_REFRESH`]: a deadline rather than a count of
    /// idle ticks, because a busy bridge has none.
    roster_due: Instant,
    /// When the §6.6 row-4 observation is next taken for the HELD sessions. Its
    /// own deadline, not the roster's — see [`LOCAL_OBSERVE`].
    observe_due: Instant,
    /// When the next screen snapshot may be published — the ≤ 4/s bound of §3.3,
    /// enforced by the clock rather than by how often the loop happens to idle.
    screen_due: Instant,
    /// `sid -> the `attention=` last PUBLISHED on its presence row`. A row is
    /// republished when this moves, and only then: presence is a last-value
    /// face on an append-forever log, so a periodic republish would be one
    /// record per session per period forever for no new information.
    attention: BTreeMap<String, String>,
    /// When the unfinished feed is next re-asked. FROM THE LOOP, not only from
    /// the idle arm: `ticks` advances only in the mailbox's `None` branch, so a
    /// bridge that is receiving records never idles and never reached the retry
    /// at all — which made the feed's budget one that could not be spent and
    /// left a transiently-refused keystroke waiting on a quiet moment that a
    /// busy node does not have.
    feed_retry_due: Instant,
    /// `sid -> the generation last published on its `screen` face`. A snapshot
    /// is published only when the generation MOVED: a retained face republished
    /// on a timer would put one record per session per period on an
    /// append-forever log, forever, for no new information.
    screen_gen: BTreeMap<String, String>,
    /// `human -> (their halt state, the reason they published)`. The fleet halt
    /// is in force iff ANY says on, and the reason the sessions are held under
    /// is the FIRST standing halter's, in principal order — deterministic, and
    /// stable while that halter's row stands (§5.3 has one `reason=` per halt
    /// record and one `hold … reason=` per session, so N halters have to
    /// collapse to one string somewhere; here, visibly, rather than by whichever
    /// record arrived last).
    halts: BTreeMap<String, (bool, String)>,
    /// Whether the halt is currently mirrored into the endpoint.
    halt_applied: bool,
    /// The `reason=` the endpoint is currently holding under. Kept because the
    /// mirror is now a PAIR: a second human halting with a different reason,
    /// or the same human re-publishing with a new one, must reach the agent
    /// rather than be swallowed by a state check that only looked at the flag.
    halt_reason: String,
    /// Offsets already committed on the inbox group.
    committed: u64,
    /// Offsets this bridge has acked on its OWN lanes, for the self-lane check.
    self_acked: BTreeSet<u64>,
    /// The live subscriptions' closers. On a reconnect every one is closed
    /// FIRST: two group subscriptions on one cursor would deliver the same
    /// record twice and could walk the commit backwards, which is the one way
    /// this design's exactly-once could actually break.
    ///
    /// Built at CONNECT time rather than from the `Subscription`, because
    /// `Subscription::closer` exists only for the concrete `UnixStream` case and
    /// this bridge also speaks sealed TCP — see [`crate::transport`].
    closers: Vec<Closer>,
    mailbox: Arc<Mailbox>,
}

impl Bridge {
    /// Build a bridge: state dir, node id, aterm connection, and the whoami that
    /// tells it whether it IS the bridge.
    ///
    /// # Errors
    ///
    /// Anything that makes the bridge unable to run at all.
    pub fn new(cfg: Config) -> io::Result<Self> {
        let state = StateDir::open(&cfg.state_dir)?;
        let node = state.node_id(mint_node_id)?;
        let caps = cfg
            .cap_files
            .iter()
            .map(|p| read_cap_file(p))
            .collect::<io::Result<Vec<_>>>()?
            .concat();
        let (mut ctl, attachment) = match (&cfg.sock, &cfg.token) {
            (Some(sock), Some(token)) => (Ctl::connect(sock, token)?, Attachment::Observer),
            _ => (
                Ctl::adopt(aterm_uds::spawnfd::BRIDGE_VERB_FD)?,
                Attachment::Inherited,
            ),
        };
        // THE SCOPE IS PROBED, not assumed — and it is probed with a BRIDGE-ONLY
        // verb, because that is exactly the authority in question. `outbox` is
        // the harmless one: it moves no watermark and drops nothing, so asking
        // costs nothing, and an `ERR denied` is the whole answer.
        //
        // Not `whoami`: it reports the CONNECTION's own session and the bridge's
        // connection has none, so it would answer about whichever session
        // happened to be active — a question about the wrong thing.
        let probe = ctl.request("outbox")?;
        let attachment = match (attachment, probe.ok()) {
            (Attachment::Inherited, true) => Attachment::Inherited,
            (_, _) => {
                eprintln!(
                    "aterm-link: OBSERVER MODE — aterm answered `{}` to a bridge-plane verb. \
                     Presence and ev only: no deliver, no hold, no drive.",
                    probe.header()
                );
                Attachment::Observer
            }
        };
        let producer_id = astream_cap::producer_id_of(&node);
        // The pid file is how a test (or an operator) reaches this process. It is
        // written after the node id so a reader that sees it can trust both.
        let _ = std::fs::write(
            state.root().join("pid"),
            format!("{}\n", std::process::id()),
        );
        let fault = Fault::from_env(&state);
        Ok(Self {
            node,
            producer_id,
            inc: 0,
            self_acked: state.self_acked(),
            state,
            caps,
            attachment,
            fault,
            ctl,
            conn: None,
            locals: BTreeMap::new(),
            epochs: BTreeMap::new(),
            holders: BTreeMap::new(),
            pending: BTreeMap::new(),
            local: BTreeMap::new(),
            pending_admit: BTreeSet::new(),
            lease_due: Instant::now() + LEASE_RENEW,
            roster_due: Instant::now() + ROSTER_REFRESH,
            observe_due: Instant::now() + LOCAL_OBSERVE,
            attention: BTreeMap::new(),
            feed_retry_due: Instant::now() + FEED_RETRY,
            screen_due: Instant::now(),
            screen_gen: BTreeMap::new(),
            halts: BTreeMap::new(),
            halt_applied: false,
            halt_reason: String::new(),
            committed: 0,
            closers: Vec::new(),
            mailbox: Arc::new(Mailbox::default()),
            cfg,
        })
    }

    /// This node's id.
    #[must_use]
    pub fn node(&self) -> &str {
        &self.node
    }

    // -----------------------------------------------------------------------
    // the aterm verb lane — EVERY call goes through here
    // -----------------------------------------------------------------------

    /// One verb on aterm's VERB lane (fd 3), with the one thing every caller
    /// must do on an I/O failure: say so where the loop can see it.
    ///
    /// ## Why this wrapper exists at all
    ///
    /// `Item::Closed(Source::Aterm)` — the only thing that makes [`Bridge::run`]
    /// return — used to be produced in exactly ONE place, the reader that pumps
    /// the PUSH descriptor (fd 4). Nothing produced it for the verb descriptor.
    /// So when the verb lane died on its own — an over-long line, a framing
    /// desync, a half-closed socket — every failure was swallowed at its own
    /// call site (`deliver failed`, `outbox failed`, `write_hold` answering
    /// false) and the child kept running: still attached to the broker, still
    /// draining and committing the inbox group into a dead socket, still
    /// publishing `state=live`. `fabric_launch::supervise` blocks in
    /// `child.wait()`, so no relaunch was ever attempted. Meanwhile the near
    /// end's `BridgeLostGuard` had already fired, holding every session the
    /// bridge governed under `fabric-lost`. §11.2's stated recovery — "until a
    /// bridge reconnects … the instance relaunches the child with back-off" —
    /// could not happen, because the child never exited.
    ///
    /// A LOST LANE IS NOT A REFUSED LINE. [`Ctl`] latches the first I/O failure
    /// and reports it through [`Ctl::lost`]; a line this client declined to
    /// write (over [`REQUEST_LINE_MAX`]) wrote nothing and left the connection
    /// framed and alive, so it is an error the caller records and carries on
    /// from. Only the former ends the process.
    fn ctl_request(&mut self, line: &str) -> io::Result<Reply> {
        let reply = self.ctl.request(line);
        note_ctl_loss(&self.ctl, &self.mailbox, reply.as_ref().err());
        reply
    }

    /// [`Bridge::ctl_request`] with a length-prefixed body — the `feed-bin`
    /// frame. Same loss discipline.
    fn ctl_request_with_body(&mut self, line: &str, body: &[u8]) -> io::Result<Reply> {
        let reply = self.ctl.request_with_body(line, body);
        note_ctl_loss(&self.ctl, &self.mailbox, reply.as_ref().err());
        reply
    }

    // -----------------------------------------------------------------------
    // broker plumbing
    // -----------------------------------------------------------------------

    /// One authenticated connection to the broker over the configured transport,
    /// with every cap attached — and the closer that can end it from another
    /// thread.
    ///
    /// A SEALED connect fails HERE when the key is wrong: the handshake confirms
    /// the key each way before a single Frame is written, so a peer without it
    /// is refused inside the handshake rather than on the first verb (§8.6).
    fn connect(&self) -> io::Result<(Conn, Closer)> {
        let (mut c, closer) = transport::connect(&self.cfg.transport, &self.cfg.broker)?;
        for cap in &self.caps {
            c.attach(&cap.grant, &cap.tag)?;
        }
        Ok((c, closer))
    }

    /// Reserve the next producer sequence, PERSISTING it before it is used.
    ///
    /// A crash between the write and the publish BURNS that number — never a
    /// duplicate, but the record is simply never published. That is the same
    /// trade `asb pub --seq-file` makes, and it is the right way round: a
    /// duplicate would be deduped away by the broker and silently lost, while a
    /// burned number costs nothing at all.
    fn next_seq(&mut self) -> io::Result<u64> {
        let base = self.inc << 32;
        let last = self.state.sequence().max(base);
        let next = last + 1;
        // Never reach the reserved top: that sequence belongs to the will.
        if next >= base | WILL_SEQ_LOW {
            return Err(io::Error::other(
                "this incarnation's sequence space is exhausted",
            ));
        }
        self.state.reserve_sequence(next)?;
        Ok(next)
    }

    /// Publish one record under the node's bound cap at a FRESH sequence.
    fn publish(&mut self, subject: &str, body: &[u8]) -> io::Result<u64> {
        let seq = self.next_seq()?;
        self.publish_at(seq, subject, body).map(|(off, _)| off)
    }

    /// Publish at an EXACT sequence, answering `(offset, deduped)`.
    ///
    /// `deduped` is not a failure and the offset is not a guess: the broker
    /// answers a re-send of `(producer_id, producer_seq)` with the ORIGINAL
    /// record's offset and appends nothing. That is what makes a republish after
    /// a crash exactly-once rather than a second copy — see
    /// [`crate::state::StateDir::post_seq`].
    fn publish_at(&mut self, seq: u64, subject: &str, body: &[u8]) -> io::Result<(u64, bool)> {
        let Some(conn) = self.conn.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        conn.publish(self.producer_id, seq, subject, body)
    }

    /// One `ev` record on the node's own digest. NEVER carries a body — that is
    /// the rule the whole digest leans on, and it is why `undeliverable` says
    /// only which offset and why.
    fn publish_ev(&mut self, payload: &str) {
        self.publish_ev_for(None, payload);
    }

    /// One `ev` record on the face that OWNS it: a session's when the event is
    /// about a session, the node's otherwise.
    ///
    /// §3.3 makes `ev` a per-owner face (`/f/<F>/pub/<owner>/ev`, owner =
    /// `<node>/<sid>` or `<node>/node`) and §10 says an applied `term/in`
    /// "leaves an `ev` record `applied re=M seq=<n>` on the SESSION's `ev`
    /// face". A3 published every one of them on the node face with the session
    /// named only inside the pct-encoded payload, and `replay::session_of`
    /// answers `None` for that subject — so the causal pointer §10 asks for
    /// belonged to no partition in this crate's own consistent-cut machinery
    /// and contributed no `CrossEdge`. A reader scoped to
    /// `ro:/f/<F>/pub/<n>/<sid>/>` could not see its own session's `ev` either.
    fn publish_ev_for(&mut self, sid: Option<&str>, payload: &str) {
        let subject = self.ev_face(sid);
        let body = format!(
            "v=1 t={} ev={}",
            crate::now_ms(),
            crate::pct::encode(payload)
        );
        if let Err(e) = self.publish(&subject, body.as_bytes()) {
            eprintln!("aterm-link: could not publish ev {payload:?}: {e}");
        }
    }

    /// The `ev` face one record belongs on.
    fn ev_face(&self, sid: Option<&str>) -> String {
        match sid {
            Some(sid) => subject::session_face(&self.cfg.fleet, &self.node, sid, "ev"),
            None => subject::node_face(&self.cfg.fleet, &self.node, "ev"),
        }
    }

    /// One `ev` record carrying a `re=` — the CAUSAL POINTER §10 asks an applied
    /// `term/in` to leave behind.
    ///
    /// `re=` is a first-class body field (§4.1: "the offset this record
    /// answers"), so it is a token a reader parses structurally rather than a
    /// substring inside the pct-encoded `ev=` payload. That is what lets a reader
    /// rebuild the fabric's causal edges from the stored bytes alone — the
    /// aterm-seam analogue of astream's durable `caused_by`
    /// (`term.fleet.durable-watermark`), at the watermark grade §10 gives it and
    /// not a step higher.
    ///
    /// IT ANSWERS WHETHER THE RECORD LANDED, because the feed journal is retired
    /// on the strength of it: an entry cleared after a publish that never
    /// happened is a keystroke whose fate is on nobody's log.
    fn publish_ev_re(&mut self, sid: Option<&str>, payload: &str, re: u64) -> bool {
        let subject = self.ev_face(sid);
        let body = format!(
            "v=1 t={} re={re} ev={}",
            crate::now_ms(),
            crate::pct::encode(payload)
        );
        match self.publish(&subject, body.as_bytes()) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("aterm-link: could not publish ev {payload:?}: {e}");
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // presence and the incarnation
    // -----------------------------------------------------------------------

    /// Bring the node's presence up: read the bus's own opinion of our last
    /// incarnation, take `max(local, bus) + 1`, register the will at that
    /// incarnation's reserved top, then publish `live`.
    ///
    /// THE ORDER IS THE FENCE. Every sequence of incarnation n+1 is above
    /// incarnation n's reserved top, so when a half-open old connection finally
    /// dies its will is suppressed structurally — no reader fold needed, which
    /// matters because `Last` returns one record per subject and a reader could
    /// never see a `live` hidden behind a later `gone` (§7).
    fn bring_presence_up(&mut self) -> io::Result<()> {
        let subject = subject::node_face(&self.cfg.fleet, &self.node, "presence");
        let bus_inc = {
            let conn = self
                .conn
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let (rows, _) = conn.last(&subject, "", 8)?;
            rows.iter()
                .filter(|(_, s, _)| *s == subject)
                .filter_map(|(_, _, b)| inc_of(b))
                .max()
                .unwrap_or(0)
        };
        self.inc = self.state.incarnation().max(bus_inc) + 1;
        self.state.set_incarnation(self.inc)?;
        let gone = format!(
            "v=1 t={} state=gone inc={} fabric=disconnected",
            crate::now_ms(),
            self.inc
        );
        let will_seq = (self.inc << 32) | WILL_SEQ_LOW;
        {
            let conn = self
                .conn
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            conn.will(self.producer_id, will_seq, &subject, gone.as_bytes())?;
        }
        let live = format!(
            "v=1 t={} state=live inc={} fabric=connected host={} pid={}",
            crate::now_ms(),
            self.inc,
            crate::pct::encode(&hostname()),
            std::process::id()
        );
        self.publish(&subject, live.as_bytes())?;
        Ok(())
    }

    /// Publish one hosted session's roster row.
    ///
    /// IT CARRIES `observer=1` IN OBSERVER MODE, and that flag is load-bearing
    /// rather than decorative. §11.2 says a hand-started `--sock` bridge still
    /// publishes presence; §6.1 says a bare `@s-<sid>` routes only when EXACTLY
    /// ONE node advertises that sid. Taken together and unmarked, running an
    /// observer beside a real bridge would make every session it can see
    /// ambiguous fleet-wide — a working fabric broken by a read-only tool. The
    /// flag says "I do not host this, I am watching it", and
    /// [`Bridge::advertisers`] excludes such rows from routing. Faking it can
    /// only REMOVE a node from the candidate set, never add one, so it fails
    /// safe against a rogue too.
    ///
    /// ## `attention=` HAS A WRITER, and `fabric=` no longer has a false one
    ///
    /// §4.2's presence body specifies `attention=`; A10's whole rung is
    /// `notify --on attention`, A8's glance guarantees the column, and §9.3
    /// sells `ls --attention` as the way to see every escalation on every host.
    /// Nothing in the fabric ever published the field, so in a real fleet every
    /// one of those readers had no writer: the notifier could never fire and
    /// the column was always `-`. It is read from the session's own `meta`
    /// (aterm's `attention` is "the typed needs-human escalation") on the roster
    /// round, capped at [`crate::glance::ATTENTION_CAP`], and carried through
    /// pct-encoded exactly as it arrives.
    ///
    /// `fabric=` is GONE from a session row, and that is the honest direction.
    /// It was the literal `connected`, and the only `Will` this bridge registers
    /// is on the NODE face — so when a bridge died the node row flipped to
    /// `state=gone fabric=disconnected` while every session row stayed retained
    /// at `fabric=connected` until a bridge came back. `glance.rs` names the
    /// field one of the two a human actually reads and defines it as "whether
    /// that session's node can still be reached", and glance is a pass-through:
    /// a constant that the documented question can never be answered by is worse
    /// than an absent token, because `GUARANTEED` renders a missing one as `-`
    /// — "unknown" — which is the truth. The node's own row still carries it,
    /// where the will keeps it honest.
    fn publish_session_presence(&mut self, sid: &str, state: &str) {
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "presence");
        let epoch = self.epochs.get(sid).cloned().unwrap_or_else(|| "-".into());
        let hold = u8::from(self.halt_applied);
        let holder = self
            .holders
            .get(sid)
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        let gen = self.live_gen(sid).unwrap_or_else(|| "-".to_string());
        let attention = self.attention_of(sid);
        self.attention.insert(sid.to_string(), attention.clone());
        let mut body = format!(
            "v=1 t={} inc={} epoch={epoch} gen={gen} state={state} hold={hold} \
             holder={holder} attention={attention}",
            crate::now_ms(),
            self.inc
        );
        if self.attachment == Attachment::Observer {
            body.push_str(" observer=1");
        }
        if let Err(e) = self.publish(&subject, body.as_bytes()) {
            eprintln!("aterm-link: could not publish presence for {sid}: {e}");
        }
    }

    // -----------------------------------------------------------------------
    // the aterm side
    // -----------------------------------------------------------------------

    /// Refresh `local -> sid` and `sid -> epoch` from aterm's own roster.
    fn refresh_sessions(&mut self) -> io::Result<Vec<String>> {
        let reply = self.ctl_request("sessions")?;
        // A NON-`OK` REPLY IS NOT AN EMPTY ROSTER. `Ctl::read_reply` answers a
        // bare `Reply::Status` for any header that does not start with `OK`, and
        // `Reply::rows()` answers `&[]` for a `Status` — so an `ERR …` was
        // indistinguishable from "this instance hosts nothing", and the pruning
        // below then emptied `epochs`, `holders` and `pending`: the two maps the
        // one path to a PTY compares against, with `holders` rebuilt ONLY by
        // `restore_control_rows` inside an attach. Every `term/in` from the
        // legitimate remote holder was refused `reason=holder` until the next
        // broker reconnect, with their claim still standing on the bus — and a
        // fleet halt arriving in that window was recorded as applied over zero
        // sessions. Every other reply-consumer in this file already checks.
        if !reply.ok() {
            return Err(io::Error::other(format!(
                "aterm refused the roster: {}",
                reply.header()
            )));
        }
        let mut fresh = Vec::new();
        self.locals.clear();
        for row in reply.rows() {
            let mut cols = row.split_whitespace();
            let (Some(local), Some(sid)) = (cols.next(), cols.next()) else {
                continue;
            };
            let Ok(local) = local.parse::<u64>() else {
                continue;
            };
            self.locals.insert(local, sid.to_string());
            // THE EPOCH comes off the roster row's `nonce=` — the session's
            // public launch nonce, verbatim, which is what §7 makes `epoch=`.
            // `whoami` carries it too but only for the connection's OWN session,
            // and the bridge's connection is not a session's.
            //
            // SEARCHED AFTER THE FIXED COLUMNS, NOT ACROSS THE WHOLE ROW. The row is
            // `<local> <sid> <parent> <state> <title> meta=.. nonce=.. ...`, and the
            // title is the one field a PROGRAM controls: it is whatever the PTY last
            // set with OSC 0/2. A whole-row `find_map` picked the first `nonce=` token
            // in the line, which put an attacker-authored field UPSTREAM of the real
            // one in scan order. It is not exploitable today — `pct_encode` escapes
            // every non-graphic byte, so a title is exactly ONE token and can never
            // introduce a second — but that is an invariant of a function three crates
            // away, and this fence must not be one careless emitter away from letting
            // screen content choose the epoch it is checked against. `parent` and
            // `state` are ours; `cols` has already taken `local` and `sid`, so dropping
            // three more lands past the title with no second pass over the row.
            let nonce = cols
                .skip(3)
                .find_map(|t| t.strip_prefix("nonce="))
                .unwrap_or("-")
                .to_string();
            if self.epochs.insert(sid.to_string(), nonce).is_none() {
                fresh.push(sid.to_string());
                // AND IT IS REMEMBERED, not merely returned. Four of this
                // function's five callers discard the return value; see
                // [`Bridge::pending_admit`].
                self.pending_admit.insert(sid.to_string());
            }
        }
        // PRUNED TO THE ROSTER. `epochs` and `holders` are the two maps the one
        // path to a PTY compares against, and A3 only ever added to them:
        // `session-exited` removed one entry when its line arrived, and nothing
        // reconciled either map with the roster. `holders` is also RESTORED from
        // the bus at every attach, including rows naming sids from a previous
        // launch, so the two maps could disagree about which sessions exist —
        // and a drive record for a sid `holders` knew and `epochs` did not was
        // the state the epoch fence used to pass by comparing two `None`s.
        // Dropping both together keeps them one answer rather than two.
        //
        // ALL SIX PER-SID MAPS, and the queue. The first fix pruned three of
        // them and left `local`, `attention` and `screen_gen` — declared in the
        // same struct, written on the same roster round, keyed by the same
        // sids, removed by nothing anywhere (`session-exited` drops `epochs`
        // and `holders` only). Sids are 128-bit and never reused and the bridge
        // is resident for the life of the instance, so those three grew with
        // every tab ever opened. All three are pure CACHES of an observation
        // about a live session — the last `status` sample, the last published
        // `attention=`, the last published screen generation — so dropping a
        // departed session's entry can lose nothing: the next holder of that
        // sid does not exist.
        let live: BTreeSet<String> = self.locals.values().cloned().collect();
        self.epochs.retain(|sid, _| live.contains(sid));
        self.holders.retain(|sid, _| live.contains(sid));
        self.pending.retain(|sid, _| live.contains(sid));
        self.local.retain(|sid, _| live.contains(sid));
        self.attention.retain(|sid, _| live.contains(sid));
        self.screen_gen.retain(|sid, _| live.contains(sid));
        self.pending_admit.retain(|sid| live.contains(sid));
        Ok(fresh)
    }

    /// ADMIT every session discovery has queued: presence on the bus, and
    /// §6.2's ring refill.
    ///
    /// THE ONE PLACE BOTH DUTIES HAPPEN, so a discovery path cannot do one and
    /// forget the other — which is what four `let _ = self.refresh_sessions()`
    /// call sites did (see [`Bridge::pending_admit`]). It needs a broker, so a
    /// queued session waits rather than being consumed by a call that could not
    /// have refilled it: [`Bridge::refill`] answers a missing connection by
    /// returning, and a sid dropped there would never be offered again.
    fn admit_fresh_sessions(&mut self) {
        if self.conn.is_none() {
            return;
        }
        for sid in std::mem::take(&mut self.pending_admit) {
            if !self.epochs.contains_key(&sid) {
                continue;
            }
            self.publish_session_presence(&sid, "live");
            self.refill(&sid);
        }
    }

    /// REFILL one session's ring after an instance relaunch (§6.2).
    ///
    /// The group cursor is per NODE and is already past every row this session
    /// was delivered, so without this a SIGKILLed `aterm-gui` would lose every
    /// delivered-but-unseen row: the bus still holds them, but nothing would ever
    /// hand them over again. `Fetch` from the persisted `seen_off + 1` is what
    /// hands them over, and `deliver`'s idempotency on `off=` is what makes doing
    /// it unconditionally safe.
    ///
    /// A MISSING `seen/<sid>` REFILLS FROM A BOUNDED FLOOR, which is the
    /// direction the state dir's own table has always claimed ("the refill
    /// starts lower and the ring dedups on `off=`"). A3 returned instead, so a
    /// half-lost state dir — a restore from a backup taken before `seen/`
    /// existed, a partial `rm`, a disk error — silently made every
    /// delivered-but-unseen row unreachable forever: the group cursor is per
    /// node and is already past them. Returning failed in the LOSING direction.
    ///
    /// FROM THE FLOOR, NOT FROM ZERO, and the difference is the whole care. The
    /// broker's `Fetch` is a linear scan bounded per page by its own scan budget
    /// (`store.rs`: "`O(scan_max)` whatever the filter matches"), so refilling
    /// from zero would walk the entire log once per freshly seen session —
    /// unbounded work on the session-creation path, bought to repair a state
    /// dir that is usually intact. [`REFILL_FLOOR_SPAN`] behind the head is
    /// bounded, is strictly more than the nothing A3 offered, and covers the
    /// window a lost watermark can actually be hiding rows in.
    fn refill(&mut self, sid: &str) {
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let from = match self.state.seen_off(sid) {
            Some(off) => off + 1,
            None => {
                // `max=0` is the broker's head query and scans nothing.
                let Some(conn) = self.conn.as_mut() else {
                    return;
                };
                let Ok((_, (_, head))) = conn.fetch(0, &filter, 0) else {
                    return;
                };
                head.saturating_sub(REFILL_FLOOR_SPAN)
            }
        };
        let mut cursor = from;
        loop {
            let page = {
                let Some(conn) = self.conn.as_mut() else {
                    return;
                };
                match conn.fetch(cursor, &filter, 256) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("aterm-link: refill of {sid} stopped: {e}");
                        return;
                    }
                }
            };
            let (rows, (next, head)) = page;
            for (off, subj, body) in rows {
                // A LOST LANE means there is nothing to refill INTO, and
                // carrying on would write every remaining row into a dead
                // socket. It is the only outcome that stops the refill: a record
                // this bridge refuses locally is `Accounted` and the refill walks
                // on, because one poison record must not be able to truncate a
                // session's whole post-relaunch recovery on every relaunch,
                // forever.
                if self.deliver_record(off, &subj, &body) == Delivery::Unaccounted {
                    return;
                }
            }
            if next >= head || next <= cursor {
                return;
            }
            cursor = next;
        }
    }

    // -----------------------------------------------------------------------
    // the inbox plane
    // -----------------------------------------------------------------------

    /// One record off the durable inbox group: validate, classify, deliver,
    /// COMMIT — and the commit happens ONLY when the record was accounted for.
    ///
    /// The commit is last on purpose: at-least-once cursor plus an idempotent
    /// sink is what makes the whole path exactly-once. That argument requires
    /// the commit to be CONDITIONAL on the sink having answered. A3 committed
    /// unconditionally, so a `deliver` whose reply never arrived — the socket
    /// died mid-request, aterm was mid-teardown — advanced the durable group
    /// cursor past a record the endpoint never took. Nothing recovered it:
    /// `refill` starts from a persisted `seen_off`, which only ever covers rows
    /// the endpoint acknowledged, and no `ev` was published, so the sender was
    /// not told either. The record stayed on the log forever and no bridge ever
    /// offered it again.
    ///
    /// [`Delivery::Unaccounted`] is exactly the LOST LANE — the one outcome that
    /// says nothing about whether the endpoint saw the row. Leaving the cursor
    /// where it is redelivers it on the next attach, which is safe because
    /// `deliver` is idempotent on `off=`; that idempotency is the whole reason
    /// the commit is supposed to be last.
    ///
    /// AND THAT ARGUMENT ONLY WORKS BECAUSE THE LANE IS GONE. `commit_upto` is an
    /// ABSOLUTE group commit, so a later record that delivers commits past a
    /// skipped offset and no attach ever offers it again — "redelivered on the
    /// next attach" is true only when nothing later can commit, which is the case
    /// precisely when the lane is lost and this process is on its way out
    /// ([`note_ctl_loss`] has already pushed `Closed(Aterm)`). A record this
    /// bridge refuses locally therefore may NOT take this path, and does not:
    /// see [`Bridge::refuse_locally`].
    fn on_inbox_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, body) = rec;
        let outcome = self.deliver_record(*off, subject, body);
        if self.fault == Fault::KillAfterDeliver {
            self.fault.fire(Fault::KillAfterDeliver, &self.state);
        }
        match outcome {
            Delivery::Accounted => self.commit_upto(*off),
            Delivery::Unaccounted => eprintln!(
                "aterm-link: not committing {off}: the aterm lane is gone, so the group \
                 cursor stays put and a replacement bridge redelivers the record"
            ),
        }
    }

    /// Validate and hand one record to the endpoint. Shared by the live group
    /// drain and the post-relaunch refill, so a refilled row is classified by
    /// exactly the code that classified it the first time.
    fn deliver_record(&mut self, off: u64, subject: &str, raw: &[u8]) -> Delivery {
        // OBSERVER MODE delivers nothing (§11.2: "presence and `ev` only, no
        // delivery, no hold"). It is checked HERE rather than left to the
        // endpoint's refusal so an observer produces one honest record per
        // message instead of a stream of `ERR denied`.
        if self.attachment == Attachment::Observer {
            self.publish_ev_for(None, &format!("undeliverable off={off} reason=observer"));
            return Delivery::Accounted;
        }
        let addr = match subject::parse_in(&self.cfg.fleet, &self.node, subject) {
            Ok(a) => a,
            Err(reject) => {
                self.record_undeliverable(off, None, reject);
                return Delivery::Accounted;
            }
        };
        if !self.epochs.contains_key(&addr.sid) {
            // A session this bridge has not heard of yet is not the same thing
            // as one this node does not host: the roster may simply be one
            // `session-created` behind. Ask ONCE, then decide — the cursor
            // commits either way, so a wrong "not hosted" here is a message
            // nobody ever gets.
            let _ = self.refresh_sessions();
            if !self.epochs.contains_key(&addr.sid) {
                self.record_undeliverable(off, Some(&addr.sid.clone()), Reject::NotHosted);
                return Delivery::Accounted;
            }
        }
        // THE SELF-LANE CHECK (§6.2). A record on our own lane, under our own
        // `<src>`, at an offset we never got a `PublishAck` for is a forgery by a
        // co-holder of the node cap — nobody else could have published it, and we
        // did not. It is recorded and escalated, never delivered.
        //
        // IT FAILS OPEN BELOW THE WINDOW, and that is the whole difference
        // between a compromise report and a false one. `self_acked` remembers
        // only the newest [`SELF_ACK_KEEP`] offsets — durably AND in memory —
        // and two local sessions messaging each other route through the bus
        // under this node's own `<src>`, so self-lane records are the ordinary
        // intra-instance case. An offset OLDER than the oldest one remembered is
        // therefore not evidence of a forgery: it is a question this bridge can
        // no longer answer, and answering "compromised" to it would tell an
        // operator to rotate the node cap over a message the node sent itself.
        if addr.src == self.node && self.is_forged_self(off) {
            self.record_undeliverable(off, Some(&addr.sid.clone()), Reject::ForgedSelf);
            return Delivery::Accounted;
        }
        let (body, _raw_tail) = Body::decode(raw);
        // THE RELAY CHAIN IS BOUNDED BEFORE IT REACHES A LINE — see
        // [`crate::body::VIA_MAX_HOPS`], which is where the bound lives so that
        // this side and the outbound `post` side cannot hold two numbers for one
        // field. It is applied HERE, on the writer, because a line the writer
        // refuses never reaches the endpoint check that would have refused it.
        if !body.via.as_deref().is_none_or(via_ok) {
            return self.refuse_locally(&addr, &body, off, "via");
        }
        let (kind, demoted) = self.classify_kind(&addr, &body);
        let trust = trust_of(&addr.src, body.via.is_some());
        let from = self.render_from(&addr, &body);
        // A `control` claim is the one inbox kind that also MOVES something: it
        // hands the keyboard over (§6.6). It is still delivered as a row — the
        // agent must see that the human took the wheel — and the handover
        // happens BESIDE the delivery, never instead of it.
        //
        // WHICH IS WHY IT IS NOT DONE HERE. This call used to run before the
        // line was even built, and every refusal below it — the request-line
        // budget, an `ERR quota` from a full ring, an `ERR too large` — returns
        // `Accounted`, so `commit_upto` moves the durable group cursor past the
        // record and no bridge ever offers it again. The handover then happened
        // INSTEAD of the delivery, permanently: `on_control_message` is not a
        // classification but a mutation — it moves `holders`, publishes the
        // last-value `control` row, re-baselines the sampler, takes aterm's
        // mirror lease and sends `lost`/`request by=` notices — while
        // `notify_sender_undeliverable` told the human `state=refused` for a
        // claim that had just taken their keyboard, and the agent whose wheel
        // moved saw no row and no `dropped=`.
        //
        // So the effect follows the record: it is applied only on the outcome
        // that means the endpoint took the row.
        let is_control = kind == "control";
        let mut line = format!(
            "deliver {} off={off} from={from} kind={kind} trust={trust}",
            addr.sid
        );
        if let Some(re) = body.re {
            line.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = body.dl {
            line.push_str(&format!(" dl={dl}"));
        }
        if let Some(d) = demoted {
            line.push_str(&format!(" demoted={d}"));
        }
        if let Some(via) = &body.via {
            line.push_str(&format!(" via={via}"));
        }
        // THE BODY IS BOUNDED HERE, AGAINST THE SAME NUMBER THE WRITER ENFORCES.
        //
        // This is the line the whole wound was made of. aterm's control server
        // drops — with no reply — any connection whose request line reaches
        // 64 KiB, and this connection's loss is the fail-closed `bridge_lost`
        // halt of every session the bridge governs. The endpoint meanwhile
        // accepts a 256 KiB `post` body, and a remote peer needs no aterm at
        // all: the broker's record ceiling is 16 MiB. Three components, three
        // implicit limits, and one line that carried a whole body.
        //
        // The bound now comes from ONE place — [`REQUEST_LINE_MAX`], the number
        // [`Ctl::request`] itself refuses to write past — so the two sides
        // cannot disagree, and the remaining budget is whatever this line has
        // not already spent. A body that does not fit is delivered TRUNCATED
        // with `len=` naming its true size, which is the field `deliver`'s own
        // grammar has for exactly this ("Body length in bytes, before
        // truncation") and which every `msg` row prints — so the receiving
        // agent sees `len=` exceed what it was given rather than reading a
        // shortened message as a whole one. The cut is recorded as an `ev` too.
        // A body over the endpoint's own `BODY_MAX` answers `ERR too large`,
        // which is a verdict with a sender notice, not a dropped connection.
        // AND THE BUDGET IS TAKEN OVER THE WHOLE LINE, not over its last field.
        // `saturating_sub` answered 0 for a prefix that was ALREADY past the
        // bound, so cutting the body to nothing still left an unsendable line —
        // which `Ctl` then correctly refused, and which nothing accounted for.
        // `checked_sub` is the same arithmetic without the lie: `None` means the
        // metadata alone does not fit, which is a verdict, not a body to cut.
        let Some(budget) = REQUEST_LINE_MAX.checked_sub(line.len() + " len= text=".len() + 20)
        else {
            return self.refuse_locally(&addr, &body, off, "oversize");
        };
        let (encoded, cut) = encode_bounded(&body.text, budget);
        if cut {
            line.push_str(&format!(" len={}", body.text.len()));
        }
        line.push_str(&format!(" text={encoded}"));
        if self.fault == Fault::OversizeDeliverLine {
            self.fault = Fault::None;
            Fault::OversizeDeliverLine.fired(&self.state);
            line.push_str(&"x".repeat(REQUEST_LINE_MAX));
        }
        // THE LAST WORD ON THE BOUND, and it is deliberately not a
        // `debug_assert`. Every field above is bounded by construction, so this
        // can only fire on a field a later rung adds — which is precisely how
        // this defect arrived the first time. A line that does not fit earns a
        // verdict here rather than an `InvalidInput` nobody accounted for.
        if line.len() > REQUEST_LINE_MAX {
            return self.refuse_locally(&addr, &body, off, "oversize");
        }
        if self.fault == Fault::LoseAtermBeforeDeliver {
            self.fault = Fault::None;
            Fault::LoseAtermBeforeDeliver.fired(&self.state);
            let _ = self.ctl.get_ref().shutdown(std::net::Shutdown::Both);
        }
        let mut delivered = false;
        let outcome = match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => {
                delivered = true;
                Delivery::Accounted
            }
            Ok(reply) => {
                // A REFUSAL IS DATA. `ERR quota` is the ring telling us one peer
                // has had its say; the design's answer is to tell the SENDER so,
                // on their own lane, rather than to drop the record silently.
                // The reason travels as ONE TOKEN — see [`reason_token`].
                let why = reason_token(reply.header());
                self.refuse_locally(&addr, &body, off, &why)
            }
            Err(e) => {
                // THREE OUTCOMES, NOT TWO, and [`Ctl::lost`] is the question that
                // separates the last two. An error with the lane still ALIVE
                // means nothing was written — the only such error this client can
                // raise is its own over-long-line refusal, which is deliberately
                // not latched — so the endpoint definitively did not see the
                // record and never will: retrying it is a guaranteed loop, and
                // the honest outcome is a verdict, exactly as an `ERR` is. Only a
                // LOST lane is [`Delivery::Unaccounted`], and the process is on
                // its way out when that happens ([`note_ctl_loss`] has already
                // pushed `Closed(Aterm)`), so the cursor it leaves behind is a
                // cursor a replacement will re-read from.
                eprintln!("aterm-link: deliver failed: {e}");
                if self.ctl.lost() {
                    Delivery::Unaccounted
                } else {
                    self.refuse_locally(&addr, &body, off, "refused")
                }
            }
        };
        // THE HANDOFF, IF THE ROW LANDED. See the note above `is_control`.
        if is_control && delivered {
            self.on_control_message(&addr, &body, trust);
        }
        if cut && outcome == Delivery::Accounted {
            self.publish_ev_for(
                Some(&addr.sid),
                // BOTH NUMBERS IN THE SAME UNIT. `len=` is the body's decoded
                // length; `sent=` used to be the pct-ENCODED prefix's, and an
                // escape is three bytes per non-graphic one, so the row that
                // exists to make the loss auditable overstated what arrived by
                // up to 3x. The prefix is decoded back to answer in the unit
                // `len=` is already in.
                &format!(
                    "truncated off={off} len={} sent={}",
                    body.text.len(),
                    crate::pct::decode(&encoded).len()
                ),
            );
        }
        outcome
    }

    /// Whether a record on our own lane at `off` is a FORGERY rather than one of
    /// ours we can no longer remember. See the self-lane check's own note.
    fn is_forged_self(&self, off: u64) -> bool {
        forged_self(&self.self_acked, off)
    }

    /// The `kind` the endpoint is told, and the `demoted=` that explains it.
    ///
    /// TWO DEMOTIONS, and the first is unconditional. A relayed message is
    /// `kind=note demoted=<k>` whatever the recipient's allowlist says (§6.7):
    /// `via=` is a claim on the relayer's word, never authority, so it can never
    /// become a `task`, a `control` or an `answer`. The second is the allowlist
    /// itself: an unlisted principal's `task`/`control` is a `note` too.
    ///
    /// ## EVERY HUMAN IS ACCEPTED, and that is the same rule `hook::accepted`
    /// already implements
    ///
    /// `--accept-from` is EMPTY by default. With no carve-out, that default made
    /// §6.6's row 1 — an `h-*` `claim` "granted unconditionally" — unreachable:
    /// [`Bridge::deliver_record`] decides whether a record moves the keyboard
    /// from the CLASSIFIED kind, so a human's `control` demoted to `note` never
    /// reached [`decide_control`] at all, `holders` was never populated, and
    /// every following `term/in` under that claim was refused `reason=holder`.
    /// The human away from the machine — §9.3's "answer from a phone", the whole
    /// point of the drive face — could not take the keyboard from any node whose
    /// operator had not pre-listed them by name. §8.4's own spelling of the
    /// mitigation, `--accept-from h-*`, is refused at startup by
    /// `subject::is_principal` (`*` is not in `[a-z0-9-]`), so there was no way
    /// to express "every human" either.
    ///
    /// This crate's OTHER implementation of the same policy, `hook::accepted`,
    /// has always read `owner.starts_with("h-") || listed.contains(owner)`, and
    /// its help text says "beside every `h-*`". The two modules now agree, and
    /// the one that governs the keyboard is no longer the narrower.
    ///
    /// It is not a widening of anyone's authority: `h-` is a CAP-FORCED `<src>`
    /// segment (§4.3), so a principal can only speak as a human if the broker's
    /// own grant binds it to one — the same fact `trust_of` already computes
    /// `trust=human` from.
    fn classify_kind(&self, addr: &subject::InAddr, body: &Body) -> (String, Option<String>) {
        if body.via.is_some() {
            return ("note".to_string(), Some(addr.kind.clone()));
        }
        if DEMOTE_UNLESS_ACCEPTED.contains(&addr.kind.as_str()) && !self.accepted(&addr.src) {
            return ("note".to_string(), Some(addr.kind.clone()));
        }
        (addr.kind.clone(), None)
    }

    /// Whether `src` may speak a `task`/`control` AS ITSELF — every human, plus
    /// whatever `--accept-from` lists. The single source of truth for §8.4's
    /// rule inside this module; `hook::accepted` is its twin on the wake path.
    fn accepted(&self, src: &str) -> bool {
        src.starts_with("h-") || self.cfg.accept_from.iter().any(|p| p == src)
    }

    /// The `from=` the endpoint prints, built from the delivered SUBJECT and the
    /// node-attested `from=<sid>` — never from anything else in the body (§4.3).
    fn render_from(&self, addr: &subject::InAddr, body: &Body) -> String {
        match &body.from {
            // A node writing on behalf of a session it hosts. Attested because
            // the node's `<src>` segment is cap-forced: it could type as that
            // session anyway, so claiming to is no escalation (§8.3).
            // A SESSION, and only a session. §4.1 permits exactly one attested
            // exception — "a node writing on behalf of one of its SESSIONS adds
            // `from=<sid>`" — and §8.3's stated residual is "a node can
            // attribute a post to any of its OWN SESSIONS". `is_principal`
            // alone also accepts `h-`, `a-` and `n-`, so a node holding nothing
            // but its own §8.2 ring could render `h-andrew@n-rogue` and forge
            // HUMAN provenance: `hook::accepted` reads the `h-` prefix of this
            // string to decide whether a row may wake a parked `Stop` hook, and
            // `InboxRow::is_human` reads it for the eviction order §6.2 promises
            // "never evicts an `h-*` row ahead of anyone else's". Claiming to be
            // a human is outside the residual, so it is refused at the door and
            // the record is attributed to the node itself.
            Some(sid)
                if addr.src.starts_with("n-")
                    && sid.starts_with("s-")
                    && subject::is_principal(sid) =>
            {
                format!("{sid}@{}", addr.src)
            }
            _ => addr.src.clone(),
        }
    }

    /// Record a refusal: an `ev` on the owning digest, and — where there is a
    /// lane to answer on — an `undeliverable` row for the sender.
    fn record_undeliverable(&mut self, off: u64, sid: Option<&str>, reject: Reject) {
        self.publish_ev_for(
            sid,
            &format!("undeliverable off={off} reason={}", reject.token()),
        );
        if reject == Reject::ForgedSelf {
            // A forged record on our own lane means somebody else holds the node
            // cap; that is not a delivery problem, it is a compromise, and §9.2's
            // remedy for this row is "rotate the node cap". It is therefore only
            // ever raised on EVIDENCE — see [`Bridge::is_forged_self`], which
            // fails open below the remembered window rather than accusing.
            self.publish_ev_for(sid, &format!("cap-compromised off={off}"));
        }
    }

    /// Refuse one record HERE, with a verdict, and ACCOUNT FOR IT.
    ///
    /// A record this bridge will not put on the wire is not a transport failure.
    /// Nothing was written, the lane is framed and alive, and the same record
    /// will be refused identically on every redelivery — so
    /// [`Delivery::Unaccounted`], whose whole meaning is "the endpoint may or
    /// may not have seen this", is the one answer that must never be given for
    /// it. It was: every `Err` from `ctl_request` mapped to `Unaccounted`,
    /// including the writer's own over-long-line refusal, and because
    /// `commit_upto` is an ABSOLUTE group commit the next record that delivered
    /// moved the durable cursor straight past the refused one. No `ev`, no
    /// sender notice, no inbox row — the silent loss the conditional commit
    /// exists to end, reachable by a stranger's oversized `via=`.
    ///
    /// So a local refusal produces exactly what an endpoint `ERR` produces: an
    /// `ev` on the addressed session's face, a verdict on the sender's lane, and
    /// `Accounted`, because the cursor moving past a record whose fate is on the
    /// bus is the correct outcome and the only one that terminates.
    fn refuse_locally(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        why: &str,
    ) -> Delivery {
        self.publish_ev_for(
            Some(&addr.sid),
            &format!("undeliverable off={off} reason={why}"),
        );
        self.notify_sender_undeliverable(addr, body, off, why);
        Delivery::Accounted
    }

    /// Tell a sender their message did not land, ON THEIR OWN LANE.
    ///
    /// §6.2 makes the per-sender quota safe by promising the sender a verdict:
    /// "the 65th is refused at `deliver` and the bridge records `undeliverable
    /// re=<off> reason=quota` on the sender's lane". A3 built that lane two ways
    /// and both were wrong for the traffic that actually arrives:
    ///
    /// * a non-`s-` sender got `/f/<F>/in/p/<src>/…`, but the `<src>` of a
    ///   message from a session behind a bridge is that node's principal (§6.1),
    ///   and a node drains `/f/<F>/in/<node>/>` — never the `p/` owner path,
    ///   which is §6.3's DIRECT-principal lane. The verdict was published,
    ///   authorized, and drained by nobody;
    /// * an `s-` sender got THIS node's own subtree under THIS node's own
    ///   `<src>`, so the notice came straight back through this bridge's own
    ///   group drain, tripped the self-lane check (the publish offset was never
    ///   registered the way `drain_outbox` registers one) and published
    ///   `cap-compromised` — a false compromise alarm the node raised against
    ///   itself, on a path an attacker triggers at will by filling a peer's
    ///   quota.
    ///
    /// The address is now built from what the record actually carries. A remote
    /// node's message names the session it spoke for (`from=<sid>`, attested by
    /// the cap-forced `<src>`), and THAT session's lane on THAT node is where the
    /// answer goes. A local `s-` sender is routed through [`Bridge::resolve_to`],
    /// the one place that knows where an address lives — and its offset is
    /// registered as ours before anything else can see it.
    fn notify_sender_undeliverable(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        why: &str,
    ) {
        // A VERDICT NEVER EARNS A VERDICT — see [`VERDICT_KINDS`]. The notice is
        // a record like any other: addressed, delivered, and refusable. With two
        // saturated quotas, answering a refused `undeliverable` with another one
        // is a publish loop with a durable `fsync` per iteration and nothing that
        // bounds it. The refusal is still recorded as an `ev` by the caller; what
        // does not happen is another notice.
        if VERDICT_KINDS.contains(&addr.kind.as_str()) {
            self.publish_ev_for(
                Some(&addr.sid),
                &format!("unnotified off={off} src={} reason=verdict", addr.src),
            );
            return;
        }
        let Some(prefix) = self.sender_lane(addr, body) else {
            // NO LANE, SAID OUT LOUD. A node that spoke for no session has no
            // deliverable inbox owner path (`<node>/node` is not a `<sid>`, and
            // parsing one would be refused as malformed at the far end), so the
            // honest record is that the verdict could not be addressed — not a
            // publish into a lane nobody drains.
            self.publish_ev_for(
                Some(&addr.sid),
                &format!("unnotified off={off} src={} reason={why}", addr.src),
            );
            return;
        };
        let subject = format!("{prefix}/undeliverable");
        let mut notice = Body::new(crate::now_ms());
        notice.re = Some(off);
        notice.from = Some(addr.sid.clone());
        notice.text = format!("state=refused reason={why}");
        let encoded = notice.encode(None);
        self.publish_own(&subject, &encoded);
    }

    /// The six-segment `in` prefix a verdict for this record's SENDER goes to,
    /// or `None` when there is no lane that can be drained.
    fn sender_lane(&mut self, addr: &subject::InAddr, body: &Body) -> Option<String> {
        let fleet = self.cfg.fleet.clone();
        if addr.src.starts_with("s-") {
            // A session address: routed, pin and roster and all.
            return match self.resolve_to(&format!("@{}", addr.src), None) {
                Route::To(prefix) => Some(prefix),
                _ => None,
            };
        }
        if addr.src.starts_with("n-") {
            // A NODE spoke, on behalf of one of its sessions. That session's
            // lane on that node is the only owner path a bridge drains.
            let sid = body
                .from
                .as_deref()
                .filter(|s| s.starts_with("s-") && subject::is_principal(s))?;
            return Some(format!("/f/{fleet}/in/{}/{sid}/{}", addr.src, self.node));
        }
        // A human or a service holds its own `ro:/f/<F>/in/p/<p>/>` lane (§6.3).
        Some(format!("/f/{fleet}/in/p/{}/{}", addr.src, self.node))
    }

    /// Publish a record and REMEMBER IT AS OURS when it lands on one of this
    /// node's own inbox lanes.
    ///
    /// The self-lane check reads `self_acked`, so a record this bridge published
    /// onto its own subtree and did not register comes back through its own
    /// group drain as `reason=forged-self` plus a `cap-compromised` escalation.
    /// `drain_outbox` has always done this; the two other publishers onto our own
    /// lanes — a sender verdict and a `control` notice for a locally hosted
    /// holder — did not, which is why both are routed through here now.
    fn publish_own(&mut self, subject: &str, body: &[u8]) {
        let mine = format!("/f/{}/in/{}/", self.cfg.fleet, self.node);
        match self.publish(subject, body) {
            Ok(off) if subject.starts_with(&mine) => {
                self.remember_self_ack(off);
            }
            Ok(_) => {}
            Err(e) => eprintln!("aterm-link: could not publish {subject}: {e}"),
        }
    }

    /// Record one offset as ours, in memory and durably, keeping both halves the
    /// SAME size. The in-memory set used to grow one entry per publish for the
    /// life of the process while its durable twin kept only the newest
    /// [`SELF_ACK_KEEP`]; a restart then reloaded the short one and the check
    /// answered "forged" to records the long one had called ours.
    fn remember_self_ack(&mut self, off: u64) {
        self.self_acked.insert(off);
        while self.self_acked.len() > SELF_ACK_KEEP {
            let Some(oldest) = self.self_acked.first().copied() else {
                break;
            };
            self.self_acked.remove(&oldest);
        }
        let _ = self.state.note_self_acked(off, SELF_ACK_KEEP);
    }

    /// Commit the group cursor.
    fn commit_upto(&mut self, off: u64) {
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        match conn.commit(&group, off) {
            Ok(_) => self.committed = off,
            Err(e) => eprintln!("aterm-link: commit failed: {e}"),
        }
    }

    // -----------------------------------------------------------------------
    // the fleet halt
    // -----------------------------------------------------------------------

    /// One `/f/<F>/fleet/<h>/<kind>` record.
    fn on_fleet_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, raw) = rec;
        let segs: Vec<&str> = subject.split('/').collect();
        // `["", "f", "<F>", "fleet", "<src>", "<kind>"]`
        if segs.len() != 6 || segs[3] != "fleet" {
            return;
        }
        let (human, kind) = (segs[4], segs[5]);
        if kind != "halt" || !human.starts_with("h-") {
            return;
        }
        let (body, _) = Body::decode(raw);
        let on = body.unknown.get("state").is_some_and(|s| s == "on");
        // THE HUMAN'S OWN WORDS, carried through. A3 read `state=` and dropped
        // `reason=` on the floor, so every held agent was told `reason=fleet-halt`
        // — true, and useless: §5.3's whole point is that the `PreToolUse` hook
        // prints this string to the model, and "main broken, hold everything" is
        // what stops an agent from re-trying its way around a halt it cannot see
        // the cause of. It arrives pct-encoded (a body token has no spaces) and
        // is passed on in that form, which is exactly what `hold … reason=<pct>`
        // takes; [`halt_reason_token`] bounds and sanitizes it anyway, because a
        // reason is a stranger's bytes.
        let reason = halt_reason_token(body.unknown.get("reason").map(String::as_str));
        self.halts.insert(human.to_string(), (on, reason));
        // IN FORCE IFF ANY human says on, and each human lifts their OWN (§4.2).
        let effective = self
            .halts
            .values()
            .find(|(on, _)| *on)
            .map(|(_, reason)| reason.clone());
        match effective {
            Some(reason) => self.apply_halt(true, &reason),
            None => self.apply_halt(false, ""),
        }
        let effective = self.halt_applied;
        // THE ACK IS ONE RETAINED SUBJECT PER NODE, NAMING THE BARRIER IT
        // ANSWERS — not one subject per barrier.
        //
        // A3 published to `…/node/ack/<off>`, a fresh subject for every halt
        // record on AND off. The broker bounds a producer at
        // `MAX_SUBJECTS_PER_PRODUCER` = 4096, rebuilt from the log on every
        // open, and a node's producer id is derived from an id the state dir
        // mints once and keeps forever — so a fleet whose humans toggled the
        // halt two thousand times, or one cap-holding human publishing 4096
        // halt records in a burst, permanently spent that node's whole subject
        // budget. Past the bound EVERY publish from the node fails, including
        // the `live` presence row `attach_broker` refuses to continue without,
        // so no bridge on that node can ever attach again and every session it
        // hosts stays held `fabric-lost`. There is no recovery short of minting
        // a new node id, which abandons that node's mail lane by design. T9's
        // "log flooding" residual became permanent damage.
        //
        // One subject carrying `re=<off>` costs the producer exactly one subject
        // for the life of the node, and a quorum fold over
        // `Last{/f/<F>/pub/*/node/ack}` counts the members whose `re=` names the
        // barrier in question.
        //
        // IT IS A DEVIATION FROM §5.4, AND IT IS NARROWER THAN THE DESIGN'S
        // PROMISE. The design's count is `Last{/f/<F>/pub/*/*/ack/<B>}` over "one
        // `ack/<B>` per (member, barrier) ever issued", and §5.3 says a node
        // "acknowledges a halt it applied with `pub/<node>/node/ack/<halt-offset>`
        // so the issuer's tool counts who actually held". Neither query matches
        // anything now, and the one row that does exist answers only the NEWEST
        // barrier this node saw: the answer to an earlier one has been overwritten.
        // A counter must therefore read the single row and compare its `re=`, and
        // must treat a node whose `re=` names a later barrier as UNKNOWN for the
        // earlier one rather than as absent — "who is missing = presence roster
        // minus acked" is exactly the reading that turns this into a false report.
        //
        // The design's own mitigation for the subject cost (§5.4: answer only a
        // principal on the member's `--accept-from` list) was not taken, because it
        // bounds who can spend the budget rather than how much of it a halt costs,
        // and the failure past `MAX_SUBJECTS_PER_PRODUCER` is permanent: every
        // publish from the node fails, including the `live` presence row
        // `attach_broker` will not continue without. Sizing the ack at one subject
        // is the fail-safe direction; losing a per-barrier history is the price,
        // and it is written here because the design file and `tui.rs`'s
        // `/barrier` text still describe the per-barrier shape.
        //
        // AND IT IS NOT REPUBLISHED FOR HISTORY. The fleet face resubscribes
        // from offset 0 on every reconnect, and A3 kept no memory of what it had
        // answered, so each reconnect re-appended one ack per historical halt.
        // The watermark is durable for the same reason the sequence is.
        if self.state.halt_acked().is_some_and(|have| have >= *off) {
            return;
        }
        let ack_subject = subject::node_face(&self.cfg.fleet, &self.node, "ack");
        let state = if effective { "held" } else { "ready" };
        let ack = format!("v=1 t={} re={off} state={state}", crate::now_ms());
        match self.publish(&ack_subject, ack.as_bytes()) {
            Ok(_) => {
                let _ = self.state.set_halt_acked(*off);
            }
            Err(e) => eprintln!("aterm-link: could not ack the halt at {off}: {e}"),
        }
    }

    /// Mirror the fleet halt into every hosted session's endpoint hold.
    ///
    /// STICKY UNDER A DEAD BROKER, structurally: the hold lives in the endpoint
    /// and is lifted only by a `state=off` record arriving here. A broker that
    /// cannot be reached cannot send one, so an unreachable broker leaves the
    /// halt exactly where it was — which is the only safe direction for it to
    /// fail.
    fn apply_halt(&mut self, on: bool, reason: &str) {
        // THE STATE IS THE PAIR. A flag-only check would swallow a second
        // halter's different reason, or the same halter re-publishing a sharper
        // one, and the agent would keep reading the first words anybody wrote.
        if on == self.halt_applied && (!on || reason == self.halt_reason) {
            return;
        }
        self.write_holds(on, reason);
    }

    /// Write the hold to every session this instance hosts, UNCONDITIONALLY.
    ///
    /// [`Bridge::apply_halt`] is this with a state check in front of it. The
    /// unconditional form exists because two callers must write even when this
    /// process already believes the endpoint agrees with them: a session that
    /// appeared under a standing halt ([`Bridge::reassert_halt`]), and an attach
    /// after a crash ([`Bridge::reconcile_halt`]), where the endpoint is holding
    /// `fabric-lost` and the bridge's own memory of the halt died with the last
    /// incarnation.
    fn write_holds(&mut self, on: bool, reason: &str) {
        // OBSERVER MODE holds nothing, and says so once rather than being
        // refused per session: `write_hold` always names `origin=fleet`, and the
        // FLEET hold is the inherited bridge connection's alone — `hold` is
        // owner-class, but `origin=fleet` from an Owner-token connection, which
        // is what an observer holds, is `ERR denied`.
        if self.attachment == Attachment::Observer {
            self.publish_ev(&format!("hold-skipped observer state={}", u8::from(on)));
            self.halt_applied = on;
            self.halt_reason = reason.to_string();
            return;
        }
        // ASK FIRST. A halt must reach every session this instance hosts, and a
        // roster one `session-created` out of date would leave one of them
        // driving through a fleet halt — the one failure this whole mechanism
        // exists to prevent. One round trip is a cheap price for that.
        let roster = self.refresh_sessions();
        let sids: Vec<String> = self.locals.values().cloned().collect();
        for sid in sids {
            if !self.write_hold(&sid, on, reason) {
                return;
            }
        }
        // A ROSTER THAT COULD NOT BE READ IS NOT A ROSTER. The holds above went
        // to every session this bridge knew of, which may not be every session
        // the instance hosts — so the transition is NOT recorded, and the next
        // halt record (or `reassert_halt`) applies it again rather than being
        // swallowed by `apply_halt`'s state check.
        if let Err(e) = roster {
            eprintln!("aterm-link: the roster could not be read while holding: {e}");
            self.publish_ev(&format!("hold-unconfirmed state={}", u8::from(on)));
            return;
        }
        self.halt_applied = on;
        self.halt_reason = reason.to_string();
    }

    /// One session's hold. `false` means the verb could not be written at all,
    /// which is the aterm connection dying — the caller stops rather than
    /// claiming a state it did not reach.
    fn write_hold(&mut self, sid: &str, on: bool, reason: &str) -> bool {
        let line = if on {
            format!("hold {sid} on reason={reason} origin=fleet")
        } else {
            format!("hold {sid} off origin=fleet")
        };
        match self.ctl_request(&line) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("aterm-link: {line} failed: {e}");
                false
            }
        }
    }

    /// CONVERGE the endpoint's hold on one session with the fleet's standing
    /// halt, whichever way they disagree.
    ///
    /// ## The race this exists for, and why ordering cannot close it
    ///
    /// A2's fail-closed drop guard holds every session a dying bridge governed,
    /// `reason=fabric-lost origin=fleet`, from the SERVING thread — and it is
    /// deliberately unconditional, because a halt a `kill -9` lifts is not a
    /// halt. The instance also relaunches the bridge. Nothing orders those two:
    /// the replacement can attach, read `Last{/f/<F>/fleet/*/halt}`, find no
    /// standing halt and write `hold off` — and the previous incarnation's guard
    /// can then finish unwinding and put the hold straight back. The endpoint is
    /// then held for a bridge that is alive, watching, and certain it is not.
    ///
    /// Nothing lifts it. `apply_halt` is a state check over the bridge's own
    /// belief, and the belief says "not held"; the fleet publishes nothing,
    /// because nothing changed on the bus. A3 shipped that deadlock and never met
    /// it, because nothing it did needed a PTY after a bridge crash. A7 does: the
    /// first thing the replacement bridge tries is the interrupted keystroke, and
    /// it answers `ERR halted` forever.
    ///
    /// **The fix is convergence, not ordering.** Tightening the order would have
    /// meant teaching the guard to skip a hold when a newer bridge had already
    /// attached, which trades a deadlock for a window in which a standing fleet
    /// halt is not enforced — strictly worse, and against the one property the
    /// guard exists to keep. Instead the belief is reconciled against the
    /// OBSERVED endpoint state, continuously: the endpoint's `hold=` is already
    /// in the `status` reply the local sampler reads every roster tick, so this
    /// costs no round trip, and `EVENT <local> hold` makes it prompt.
    ///
    /// It is still fail-closed. The bridge lifts only what its own successful
    /// `Last` over the fleet's halt face says is unjustified, and only while it
    /// holds a broker connection; with the broker away it converges nothing and
    /// a fleet-origin hold stays exactly where it is (§5.3).
    fn converge_hold(&mut self, sid: &str, endpoint_holds: bool) {
        if self.attachment == Attachment::Observer || self.conn.is_none() {
            return;
        }
        if endpoint_holds == self.halt_applied {
            return;
        }
        let (on, reason) = (self.halt_applied, self.halt_reason.clone());
        eprintln!(
            "aterm-link: {sid}: the endpoint's hold ({}) disagrees with the standing halt ({}); correcting",
            u8::from(endpoint_holds),
            u8::from(on)
        );
        self.write_hold(sid, on, &reason);
    }

    /// Re-apply the standing halt to every session, including one that appeared
    /// after it. It writes through [`Bridge::write_holds`] rather than
    /// [`Bridge::apply_halt`] because the state check would answer "already
    /// applied" and the NEW session would never be held — and the REASON is
    /// carried across, because a session that joined a halt must be told the
    /// same thing every other one was.
    fn reassert_halt(&mut self) {
        if !self.halt_applied {
            return;
        }
        let reason = self.halt_reason.clone();
        self.write_holds(true, &reason);
    }

    /// AT EVERY ATTACH: make the endpoint's hold agree with the fleet's standing
    /// halt (§5.3), whichever way that goes.
    ///
    /// ## The hold a bridge crash leaves behind, and who is supposed to lift it
    ///
    /// When either bridge fd closes, the instance holds every session the bridge
    /// ever touched with `reason=fabric-lost origin=fleet` — deliberately
    /// unconditional, because a halt a `kill -9` lifts is not a halt (§11.2, and
    /// `fabric.rs` `bridge_lost`). §11.2 says that hold stands "until a bridge
    /// reconnects … or a human lifts it at the GUI", and the endpoint's own note
    /// says a reconnecting bridge "lifts what it wants lifted with `hold off`".
    ///
    /// **A3 never wrote that `hold off`.** Its only lift was inside
    /// [`Bridge::apply_halt`], behind a state check that a fresh process always
    /// reads as "not held" — so nothing was ever written, and every session a
    /// crashed bridge had touched stayed drive-held for the life of the
    /// instance. It cost A3 and A5 nothing because neither drove a PTY after a
    /// crash; A7 does, and a keystroke replayed into a session still held
    /// `fabric-lost` answers `ERR halted` forever. Both halves of §6.5's repair
    /// depend on this.
    ///
    /// ## Why it is still fail-closed
    ///
    /// The lift is authorized by EVIDENCE, not by the absence of it:
    /// `Last{/f/<F>/fleet/*/halt}` is the retained state of every human's halt
    /// flag, one round trip, and it is the same query §5.3 says "a worker spawned
    /// after the halt sees it at its first `Last`". A `Last` that fails is not an
    /// answer, so it refuses the attach outright — the bridge lifts nothing, the
    /// hold stays, and the next round tries again. A broker that cannot be
    /// reached still cannot lift a halt, which is the property §5.3 asks for.
    fn reconcile_halt(&mut self) -> bool {
        let filter = format!("/f/{}/fleet/*/halt", self.cfg.fleet);
        let Some(conn) = self.conn.as_mut() else {
            return false;
        };
        // A WALK THAT DID NOT FINISH IS NOT AN ANSWER, and this is the one
        // caller where that distinction lifts a fleet halt. The old loop stopped
        // on an empty page, which the broker's own API says is not the end —
        // `/f/<F>/fleet/` shares an index with a subtree that grows one entry per
        // (member, barrier) and per session ever spawned, so a scan-bound cut is
        // reachable — and the "no halt row exists" that follows an incomplete
        // walk issues `hold off` to every session this bridge hosts.
        let Ok(rows) = last_all(conn, &filter) else {
            return false;
        };
        // REBUILT FROM THE BUS, not merged into what this process remembered: the
        // retained rows ARE the standing state, and a human whose row is gone is
        // a human who is not halting.
        self.halts.clear();
        for (_, subject, raw) in rows {
            let segs: Vec<&str> = subject.split('/').collect();
            let Some(human) = segs.get(4).filter(|h| h.starts_with("h-")) else {
                continue;
            };
            let (body, _) = Body::decode(&raw);
            let on = body.unknown.get("state").is_some_and(|s| s == "on");
            let reason = halt_reason_token(body.unknown.get("reason").map(String::as_str));
            self.halts.insert((*human).to_string(), (on, reason));
        }
        match self
            .halts
            .values()
            .find(|(on, _)| *on)
            .map(|(_, reason)| reason.clone())
        {
            Some(reason) => self.write_holds(true, &reason),
            None => self.write_holds(false, ""),
        }
        true
    }

    // -----------------------------------------------------------------------
    // the drive face — THE ONLY PATH TO A PTY
    // -----------------------------------------------------------------------

    /// One `control` message off the inbox plane, run through §6.6's table.
    ///
    /// THE EPOCH IS CHECKED FIRST, and it is not a policy question: a `control`
    /// message minted against a session that has since relaunched must never
    /// land on its successor, whoever sent it. Only then does the message
    /// become a [`HandoffEvent`] for [`decide_control`].
    ///
    /// The op is the body's `text=`, one of `claim`, `request`, `release` and
    /// `grant <p>`. Anything else is recorded and dropped rather than guessed
    /// at — A3 treated every `control` message from a human as a claim, which
    /// worked only because `claim` was the only op it implemented.
    fn on_control_message(&mut self, addr: &subject::InAddr, body: &Body, trust: &str) {
        let Some(epoch) = &body.epoch else {
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=control reason=epoch", addr.sid),
            );
            return;
        };
        if self.epochs.get(&addr.sid) != Some(epoch) {
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=control reason=epoch", addr.sid),
            );
            return;
        }
        let mut words = body.text.split_whitespace();
        let event = match (words.next(), words.next()) {
            // A `claim` is the HUMAN row. The same word from an agent is a
            // `request`, because §6.6 grants row 1 unconditionally and an agent
            // that could spell its way into it would own every keyboard.
            (Some("claim"), _) if trust == "human" => HandoffEvent::Claim {
                by: addr.src.clone(),
            },
            (Some("claim" | "request"), _) => HandoffEvent::Request {
                by: addr.src.clone(),
            },
            (Some("release"), _) => HandoffEvent::Release {
                by: addr.src.clone(),
            },
            (Some("grant"), Some(to)) if subject::is_principal(to) => HandoffEvent::Grant {
                by: addr.src.clone(),
                to: to.to_string(),
            },
            _ => {
                self.publish_ev_for(
                    Some(&addr.sid.clone()),
                    &format!("refused sid={} face=control reason=op", addr.sid),
                );
                return;
            }
        };
        self.apply_handoff(&addr.sid, &event);
    }

    /// What this bridge believes about one session's keyboard, in the shape
    /// [`decide_control`] reads.
    fn handoff_state(&mut self, sid: &str) -> handoff::State {
        let holder = self.holders.get(sid).cloned();
        let holder_live = match &holder {
            None => true,
            Some(h) => self.holder_is_live(h),
        };
        handoff::State {
            holder,
            holder_live,
            halted: self.halt_applied,
        }
    }

    /// Whether a holder can still act — §6.6's `expired`, OBSERVED rather than
    /// timed (see [`crate::handoff`]).
    ///
    /// A human or a service is live by definition: there is nothing local to
    /// observe about `h-andrew`, and "live" is the CONSERVATIVE answer here
    /// because it is what makes an agent's `request` wait rather than take the
    /// wheel. A session is live while this node hosts it, or while some node's
    /// roster row still says `state=live` — the same one `Last` read §6.1's
    /// routing uses, so "who can act" and "where do I send" can never disagree.
    fn holder_is_live(&mut self, holder: &str) -> bool {
        if !holder.starts_with("s-") {
            return true;
        }
        if self.epochs.contains_key(holder) {
            return true;
        }
        // THE SAME SET §6.1's ROUTING USES, not a second reading of the same
        // rows: [`Bridge::advertisers`] is the one place `state=live` is
        // spelled, so the sentence above cannot come apart from the code.
        !self.advertisers(holder).is_empty()
    }

    /// Run one §6.6 event through the table and DO what it says: move the local
    /// holder, publish the last-value `control` row, mirror the lease, and tell
    /// whoever lost the wheel.
    fn apply_handoff(&mut self, sid: &str, event: &HandoffEvent) {
        let state = self.handoff_state(sid);
        match decide_control(&state, event) {
            Decision::Nothing => {}
            Decision::Refuse { reason } => {
                self.publish_ev_for(
                    Some(sid),
                    &format!("refused sid={sid} face=control reason={reason}"),
                );
            }
            Decision::Pending { by } => {
                self.pending.insert(sid.to_string(), by.clone());
                self.publish_control_row(sid, "request");
                // The holder learns about it on its own lane, which is what
                // makes "a pending row the holder's next wake shows" a thing the
                // holder can actually see from another host.
                if let Some(holder) = state.holder.clone() {
                    self.notify_control(&holder, sid, &format!("request by={by}"));
                }
            }
            Decision::Hold {
                holder,
                evidence,
                lost,
            } => {
                self.holders.insert(sid.to_string(), holder.clone());
                self.pending.remove(sid);
                self.publish_control_row(sid, evidence);
                // RE-BASELINE. The new holder is answerable for what the session
                // does from HERE; whatever moved the classifier before the
                // handoff is not its unaccounted change.
                self.baseline_local(sid);
                // A MIRRORED LOCAL LEASE IS NOT TAKEN BACK — and the rule for
                // that lives in [`Bridge::acquire_lease`], not here. It was
                // written as `(evidence != "lease")` on THIS line, and three
                // other paths take the same lease without ever reading this
                // one: [`Bridge::renew_leases`] every [`LEASE_RENEW`],
                // [`Bridge::restore_control_rows`] on every attach, and
                // [`Bridge::hand_lease_over`]'s `next`. One holder, one rule,
                // one place — a guard that only one of four callers applies is
                // not a guard.
                self.hand_lease_over(sid, state.holder.as_deref(), Some(holder.as_str()));
                if let Some(lost) = lost {
                    self.notify_control(&lost, sid, "lost");
                }
            }
            Decision::Free { evidence, lost } => {
                self.holders.remove(sid);
                self.pending.remove(sid);
                self.publish_control_row(sid, evidence);
                self.hand_lease_over(sid, state.holder.as_deref(), None);
                if let Some(lost) = lost {
                    self.notify_control(&lost, sid, "lost");
                }
            }
        }
    }

    /// Publish the session's last-value `control` row: the wire twin of aterm's
    /// `ControlToken`, and the row `on_term_record` compares a drive record's
    /// `<src>` against at APPLY time.
    fn publish_control_row(&mut self, sid: &str, evidence: &str) {
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "control");
        let holder = self
            .holders
            .get(sid)
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        let mut row = format!(
            "v=1 t={} holder={holder} evidence={evidence}",
            crate::now_ms()
        );
        if let Some(by) = self.pending.get(sid) {
            row.push_str(&format!(" pending={by}"));
        }
        let _ = self.publish(&subject, row.as_bytes());
    }

    /// Read the `control` rows back off the bus (§6.6's row is last-value state,
    /// not process state) and re-take the mirror lease for each one.
    ///
    /// ONLY THIS NODE'S OWN SUBTREE: `/f/<F>/pub/<node>/*/control` is written by
    /// this node alone, so nothing another principal can publish reaches
    /// `holders` — which is the map the one path to a PTY compares against.
    fn restore_control_rows(&mut self) {
        let filter = format!("/f/{}/pub/{}/*/control", self.cfg.fleet, self.node);
        let page = {
            let Some(conn) = self.conn.as_mut() else {
                return;
            };
            // PAGED ON THE RESUME CURSOR (see [`last_all`]): `holders` is the map
            // the one path to a PTY compares against, and a partial restore of it
            // refuses the legitimate holder's every `term/in` with
            // `reason=holder` while their claim still stands on the bus.
            match last_all(conn, &filter) {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("aterm-link: could not read the control rows back: {e}");
                    return;
                }
            }
        };
        {
            for (_, subject, raw) in &page {
                // `["", "f", "<F>", "pub", "<node>", "<sid>", "control"]`
                let segs: Vec<&str> = subject.split('/').collect();
                let Some(sid) = segs.get(5).filter(|_| segs.len() == 7) else {
                    continue;
                };
                let (body, _) = Body::decode(raw);
                let holder = body.unknown.get("holder").cloned().unwrap_or_default();
                if holder.is_empty() || holder == "-" {
                    self.holders.remove(*sid);
                    continue;
                }
                self.holders.insert((*sid).to_string(), holder.clone());
                if let Some(p) = body.unknown.get("pending") {
                    self.pending.insert((*sid).to_string(), p.clone());
                }
                // Re-take the mirror only for a session this instance actually
                // hosts now: a row naming a sid from a previous launch has no
                // lease to take, and asking for one would be an `ERR no such
                // session` per row on every reconnect.
                if self.epochs.contains_key(*sid) {
                    self.acquire_lease(sid, &holder);
                }
            }
        }
    }

    /// Take (or RENEW) aterm's own cooperative lease for a fabric holder, so
    /// every local driver sees `driving=lease:fabric:<p>` in `who` and a
    /// competing `turn` gets `ERR busy` (§6.6).
    ///
    /// `lease acquire` by the SAME holder is a renewal, so this one call is both
    /// verbs. Nothing is done for the conservative pause: `human?` is not a
    /// principal and there is nobody to hold a lease for — the pause's whole
    /// content is that the bridge does not know who is driving.
    ///
    /// AND NOTHING IS DONE FOR A MIRRORED LOCAL LEASE, WHICH IS THE OTHER
    /// HOLDER THAT IS NOT A LEASE TO TAKE. §6.6 row 5 records what a socket
    /// driver ALREADY holds ([`is_mirrored`]); asking aterm for it under a
    /// `fabric:` name is refused for as long as the driver keeps it — one
    /// durable `ev lease-refused` per session per [`LEASE_RENEW`] on an
    /// append-forever log — and, the moment the driver's own TTL lapses, it
    /// SUCCEEDS: the bridge would then hold aterm's lease as
    /// `fabric:owner-cli:<h>`, renew it every 10 s forever, answer the real
    /// driver's reconnect `ERR lease held`, and report `driving=` a CLI driver
    /// that holds nothing — which is the misattribution
    /// `control.rs`'s `lease_forges_fabric_holder` refuses a caller, performed
    /// by the bridge itself, with `lease release force` as the only escape.
    /// [`Bridge::local_lease_holder`] filters `fabric:` holders out, so the
    /// bridge could not even see that it was the holder.
    ///
    /// The rule is HERE rather than at the one call site that used to make it,
    /// because `renew_leases`, `restore_control_rows` and `hand_lease_over`
    /// reach this function too and none of them read that line.
    fn acquire_lease(&mut self, sid: &str, holder: &str) {
        if self.attachment == Attachment::Observer
            || holder == handoff::PAUSED
            || is_mirrored(holder)
        {
            return;
        }
        let line = format!("@{sid} lease acquire holder=fabric:{holder} ttl={LEASE_TTL_MS}");
        match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => {}
            // A LOCAL DRIVER MAY BE HOLDING IT. That is `ERR lease held` / `ERR
            // busy`, and it is not a reason to fail the handoff: the bus row is
            // the fabric's own answer to "who drives", and `send`/`key`/`feed`
            // are advisory against a lease anyway (§6.6's honest bound). It is
            // recorded so the disagreement is visible rather than assumed away.
            Ok(reply) => {
                let why = reason_token(reply.header());
                self.publish_ev_for(
                    Some(sid),
                    &format!("lease-refused sid={sid} holder={holder} reason={why}"),
                );
            }
            Err(e) => eprintln!("aterm-link: {line} failed: {e}"),
        }
    }

    /// Move the mirror lease from one fabric holder to the next.
    ///
    /// THE RELEASE IS NAMED, NEVER FORCED. `lease acquire` refuses a DIFFERENT
    /// live holder, so a handover that only acquired would be refused for the
    /// remaining 30 s of the old holder's TTL and the local half of the design
    /// would quietly say the wrong name. But `lease release force` steals ANY
    /// cooperative hold, including a local driver's — so the release names the
    /// mirror this bridge itself took (`fabric:<prev>`) and nothing else. If a
    /// local driver holds the lease instead, the release does nothing, the
    /// acquire is refused, and `ev lease-refused` records the disagreement
    /// rather than resolving it by force: §6.6's honest bound is that the lease
    /// is advisory, and a bridge that forced it would be making a claim the
    /// design does not.
    fn hand_lease_over(&mut self, sid: &str, prev: Option<&str>, next: Option<&str>) {
        if self.attachment == Attachment::Observer {
            return;
        }
        // AND NEVER RELEASED FOR A MIRROR THIS BRIDGE NEVER TOOK. A row-5
        // holder's `fabric:owner-cli:<h>` lease does not exist ([`is_mirrored`]
        // in [`Bridge::acquire_lease`]), so releasing it is a verb round trip
        // that can only ever answer "not held" — and the same skip on both
        // halves is what keeps the pair symmetrical.
        if let Some(prev) = prev.filter(|p| *p != handoff::PAUSED && !is_mirrored(p)) {
            let _ = self.ctl_request(&format!("@{sid} lease release holder=fabric:{prev}"));
        }
        if let Some(next) = next {
            self.acquire_lease(sid, next);
        }
    }

    /// RENEW every standing mirror lease. §6.6 says the lease is "renewed while
    /// the row stands"; the TTL is 30 s, so a bridge that acquired once and
    /// never renewed let the mirror lapse while the bus row still stood — the
    /// local half of the design silently stopped being true.
    ///
    /// EVERY holder in the map, and the filtering is [`Bridge::acquire_lease`]'s
    /// alone: a row-5 mirror and the conservative pause are not leases this
    /// bridge took, so they are not leases it renews. This function used to be
    /// the counter-example — it renewed a mirrored `owner-cli:` holder every
    /// [`LEASE_RENEW`] under a rule written at ONE of the four call sites.
    fn renew_leases(&mut self) {
        let standing: Vec<(String, String)> = self
            .holders
            .iter()
            .filter(|(sid, _)| self.epochs.contains_key(*sid))
            .map(|(sid, holder)| (sid.clone(), holder.clone()))
            .collect();
        for (sid, holder) in standing {
            self.acquire_lease(&sid, &holder);
        }
    }

    /// Tell a principal something about a session's keyboard, on its own lane:
    /// `lost` when it was displaced, `request by=<p>` when somebody is waiting.
    fn notify_control(&mut self, principal: &str, sid: &str, text: &str) {
        if principal == handoff::PAUSED || !subject::is_principal(principal) {
            return;
        }
        // ROUTED, not assumed. A displaced holder can be a session on ANOTHER
        // node, and addressing it by name on this node's own lane would put the
        // notice where nobody reads it. `resolve_to` is the one place that
        // answers "where does this address live", pin, roster and all — and a
        // contested or unknown one is dropped rather than sent somewhere wrong.
        let to = if principal.starts_with("s-") {
            format!("@{principal}")
        } else {
            principal.to_string()
        };
        let Route::To(prefix) = self.resolve_to(&to, None) else {
            return;
        };
        let subject = format!("{prefix}/control");
        let mut body = Body::new(crate::now_ms());
        body.from = Some(sid.to_string());
        body.text = format!("control {text} sid={sid}");
        let encoded = body.encode(None);
        // THROUGH `publish_own`: a holder hosted on THIS node is addressed on
        // this node's own lane, and an offset we published there but did not
        // remember comes back as `forged-self` plus a `cap-compromised` alarm.
        self.publish_own(&subject, &encoded);
    }

    /// THE LOCAL HALF OF §6.6 — one sample of what the instance says about
    /// EVERY session this node hosts, on the roster tick.
    ///
    /// Two rows of the table are local observations rather than bus events, and
    /// this function is where ROW 5 is read: it visits `self.locals` — every
    /// hosted session, held or free — because row 5's arm is the one for a
    /// session that has NO holder (a local socket driver's `lease acquire`,
    /// mirrored as `holder=owner-cli:<h>`), and filtering to held sessions here
    /// would delete it. It also re-reads `attention=` for every session, and
    /// takes row 4's baseline for the ones that are held.
    ///
    /// ROW 4 ITSELF IS NOT ON THIS DEADLINE. The conservative pause moved to
    /// [`Bridge::watch_held_control`] at [`LOCAL_OBSERVE`] (250 ms) over the
    /// HELD sessions alone, because its evidence is an EDGE that a 2 s sampler
    /// does not see late but does not see at all — see [`LOCAL_OBSERVE`]. What
    /// this function still does for row 4 is belt and braces: the baseline has
    /// to be current at the moment a holder appears, and
    /// [`Bridge::apply_handoff`] re-baselines on `Decision::Hold` as well.
    ///
    /// The row-4 accounting below is documented here because this is where the
    /// baseline is taken; the verdict is drawn in
    /// [`Bridge::observe_local_control`], which both deadlines call.
    ///
    /// * an unaccounted `status revision=` advance — something moved the session
    ///   that this bridge did not cause. The row is parked at `human?` until
    ///   somebody claims it again. `docs/RFC-operator-2026-08-15.md:297-301` is
    ///   explicit that screen state detects such a change and CANNOT attribute
    ///   it (repaint, resize, another client, non-echoed typing), so the verdict
    ///   is labelled inferred and the remedy is a re-claim, not an accusation.
    /// * a local `lease acquire` by a socket driver, mirrored as
    ///   `holder=owner-cli:<h>`.
    ///
    /// THE ACCOUNTING, and its honest bounds. Two things stop this from firing
    /// on ordinary life, and neither makes it miss a change it can see:
    ///
    /// * the session must have been QUIET for [`SETTLE_QUIET`]. A classifier
    ///   that is still moving is settling from something already accounted for
    ///   — a fresh session goes `starting → idle` on its own within a second,
    ///   and pausing a handover on that would make every handover useless.
    /// * the bridge marks a session `driven` when it applies a `term/in`, and
    ///   the sample that SEES the advance consumes the mark. A sample that sees
    ///   nothing does not, because the output a keystroke causes lands after the
    ///   verb returns; the mark expires after [`DRIVEN_KEEP`] instead, so it
    ///   cannot silently account for a human's change an hour later.
    ///
    /// WHAT IT STILL GETS WRONG, in one direction only: a keystroke whose effect
    /// on the classifier arrives more than [`DRIVEN_KEEP`] later reads as
    /// unaccounted, and a session whose own program changes phase while a remote
    /// principal holds the keyboard reads as unaccounted too. Both cost one
    /// re-claim.
    ///
    /// And one in the OTHER direction, which is why [`Bridge::baseline_local`]
    /// keeps the settling clock across a handoff: an advance the settling rule
    /// suppresses is CONSUMED, never deferred, so a change that lands before the
    /// session has been seen quiet is not merely late — it is gone. The
    /// window is now only what it has to be (a session that really is still
    /// moving), rather than every session for one round after every handoff. That is the direction the RFC chose — "pause the target,
    /// re-read, require expectation re-confirmation" — because the alternative
    /// is a keystroke applied to a screen somebody else already changed.
    ///
    /// AND WHERE THE PERIOD ITSELF IS THE BOUND: row 4 is checked for the HELD
    /// sessions at [`LOCAL_OBSERVE`] rather than here, because an evidence
    /// source that only exists while the change is happening is not merely
    /// served late by a slow sampler — it is not served at all.
    fn sample_local_control(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let sids: Vec<String> = self.locals.values().cloned().collect();
        for sid in sids {
            // OBSERVED FOR EVERY SESSION, not only held ones: the baseline has to
            // be current at the moment a holder appears, or the first sample
            // after a handoff reads the classifier's whole history as one
            // unaccounted change. (It is BELT AND BRACES now rather than the only
            // guard — [`Bridge::apply_handoff`] re-baselines on `Decision::Hold`
            // — which is what lets [`Bridge::watch_held_control`] visit the held
            // sessions alone at [`LOCAL_OBSERVE`].)
            self.observe_local_control(&sid);
            // THE ESCALATION IS A ROSTER OBSERVATION TOO. A10's notifier and
            // A8's glance read `attention=` off the presence row, and a session
            // sets it locally with `meta set attention …` — nothing on the bus
            // announces that. It is sampled here, on the round that already pays
            // for a `status`, and the row is republished ONLY when the string
            // moved: a retained face rewritten on a timer is an unbounded write
            // for no new information.
            //
            // It stays on THIS deadline, not row 4's: `attention=` is a LEVEL. It
            // is still set when it is read late, so a slower sampler sees it late
            // rather than not at all.
            let attention = self.attention_of(&sid);
            if self.attention.get(&sid) != Some(&attention) {
                self.publish_session_presence(&sid, "live");
            }
        }
    }

    /// §6.6 ROW 4, FOR THE SESSIONS IT APPLIES TO, ON ITS OWN FAST DEADLINE.
    ///
    /// The conservative pause is the one local observation that can be missed
    /// ENTIRELY rather than merely served late, because `status revision=` only
    /// advances while the change that moved it is still happening. See
    /// [`LOCAL_OBSERVE`] for the measurement and the phase-lock that made this a
    /// reproducible blindness rather than a rare one.
    ///
    /// Only HELD sessions: row 4's verdict is discarded unread for a free one
    /// ([`Bridge::observe_local_control`]'s `None` arm is row 5), so visiting the
    /// whole roster four times a second would be load with no answer attached.
    fn watch_held_control(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let held: Vec<String> = self
            .locals
            .values()
            .filter(|sid| self.holders.contains_key(*sid))
            .cloned()
            .collect();
        for sid in held {
            self.observe_local_control(&sid);
        }
    }

    /// ONE session's §6.6 row 4 (held) or row 5 (free), off one `status`.
    ///
    /// ONE `status`, TWO answers. `revision=` is the conservative pause's input;
    /// `hold=` is the endpoint's own opinion of whether it is held, which is the
    /// only thing that can catch the drop guard landing after a reconcile
    /// ([`Bridge::converge_hold`]). A status that cannot be read reconciles
    /// nothing — an unread state is not a disagreement.
    ///
    /// AND AN UNREAD STATUS IS NOT A REVISION EITHER. That reasoning used to
    /// stop at `converge_hold`: the failed read then fell through as
    /// `revision = 0` — a value in the SAME RANGE as a real one — and installed
    /// it as a baseline with `seen = true`. The next successful read was
    /// therefore `revision > 0`, i.e. an ADVANCE, on a session nothing had
    /// touched; one settled round later that is `UnaccountedChange`, which
    /// parks the row at `human?`, tells the holder it lost the wheel and
    /// refuses their every `term/in` `reason=holder`. One unreadable `status`
    /// — a session mid-relaunch, a busy verb lane — was enough. So the sample
    /// is taken FIRST and a `None` returns before any state moves: `seen`,
    /// `revision`, `moved_at` and `driven_at` are all left exactly as the last
    /// real observation left them.
    fn observe_local_control(&mut self, sid: &str) {
        let holder = self.holders.get(sid).cloned();
        let Some((revision, held)) = self.status_sample(sid) else {
            return;
        };
        self.converge_hold(sid, held);
        let now = Instant::now();
        let entry = self.local.entry(sid.to_string()).or_default();
        let advanced = entry.seen && revision > entry.revision;
        // BOTH WINDOWS ARE SPANS OF TIME, so they mean the same thing however
        // often this runs. See [`DRIVEN_KEEP`].
        let settled = entry
            .moved_at
            .is_some_and(|at| now.saturating_duration_since(at) >= SETTLE_QUIET);
        let driven = entry
            .driven_at
            .is_some_and(|at| now.saturating_duration_since(at) < DRIVEN_KEEP);
        let unaccounted = advanced && settled && !driven;
        entry.seen = true;
        if revision > entry.revision {
            entry.revision = revision;
            entry.moved_at = Some(now);
            entry.driven_at = None;
        } else if entry.moved_at.is_none() {
            // A first sighting starts the settling clock, so the very first
            // advance a session shows is never news. This is the `quiet = 0` the
            // count-based rule opened with.
            entry.moved_at = Some(now);
        }
        match holder {
            // A MIRRORED ONE (row 5) IS NEITHER OF THE OTHER TWO CASES.
            //
            // It is not row 4: the whole content of the row is that a LOCAL
            // driver is driving, so the `status revision=` it advances is the
            // most accounted-for change there is. Reading it as unaccounted
            // parked the row at `human?` on the driver's first keystroke — and
            // `human?` is not a mirror, so row 5 could never fire again and the
            // real driver never reappeared on the bus. (Nothing is weakened by
            // skipping the pause here: no `<src>` can equal an `owner-cli:`
            // holder, so [`Bridge::on_term_record`] already refuses every bus
            // record for a mirrored session, exactly as it does for `human?`.)
            //
            // And it is not row 5's acquire either — it is row 5's WITHDRAWAL.
            // The mirror is a last-value row that outlives the lease it mirrors:
            // once `holders[sid]` was set, this arm was `Some(_)` forever, so a
            // driver that exited or let its TTL lapse left a bus row naming it
            // as the holder for the life of the node, an agent's `request`
            // Pending against a holder that holds nothing, and — before
            // [`Bridge::acquire_lease`] learned to skip it — a bridge-held
            // `fabric:owner-cli:` lease renewed over the top of it every 10 s.
            // The withdrawal goes through §6.6's own table as the holder's own
            // `release`, so nothing here decides who may hold the row.
            Some(holder) if is_mirrored(&holder) => {
                let still = self
                    .local_lease_holder(sid)
                    .map(|h| format!("{MIRRORED_PREFIX}{h}"));
                if still.as_deref() != Some(holder.as_str()) {
                    self.apply_handoff(sid, &HandoffEvent::Release { by: holder });
                }
            }
            // A HELD SESSION: watch for a change the bridge did not cause.
            Some(_) => {
                if unaccounted {
                    self.apply_handoff(sid, &HandoffEvent::UnaccountedChange);
                }
            }
            // A FREE ONE: mirror whoever holds aterm's own lease.
            None => {
                if let Some(local) = self.local_lease_holder(sid) {
                    self.apply_handoff(sid, &HandoffEvent::LocalLease { holder: local });
                }
            }
        }
    }

    /// THE PERIODIC BACKSTOP: the local §6.6 observations, the roster re-read,
    /// the halt reassert for anything new, and the unfinished feed.
    ///
    /// Every part of it is a thing DISCOVERED BY LOOKING rather than by an
    /// arrival, which is why it cannot live on the idle arm (see
    /// [`ROSTER_REFRESH`]) — and why it is also what a `GAP` frame should run.
    /// A `GAP` is aterm's push lane saying in as many words "your picture of this
    /// node has a hole in it"; publishing that and taking no recovery action left
    /// the whole repair to a path that a busy node — the only node whose push
    /// lane coalesces — does not reach.
    ///
    /// The order matters and is the order the idle arm used to state: the local
    /// sample first (it is where a stale `fabric-lost` hold is corrected), the
    /// roster next, then the feed retry, so a retry is never spent on a hold this
    /// round could have lifted.
    fn roster_backstop(&mut self) {
        self.sample_local_control();
        let before: Vec<String> = self.locals.values().cloned().collect();
        let _ = self.refresh_sessions();
        self.admit_fresh_sessions();
        // A session that appeared while a halt was standing must be held too — a
        // SET question, not a cardinality one. A3 compared `before.len()` with
        // the new length, so a round in which one session exited and another was
        // created left the new one un-held: same count, no reassert.
        let after: Vec<String> = self.locals.values().cloned().collect();
        if before != after {
            self.reassert_halt();
        }
        // AND THE UNFINISHED FEED, if there is one — the retry cadence for a
        // `term/in` whose outcome was never obtained. Bounded by
        // [`FEED_BUDGET`]; it also has its own faster deadline in the loop.
        self.resolve_pending_feed();
        // AND THE OUTBOX, WHICH THE RUN LOOP HAS BEEN CLAIMING THIS FUNCTION
        // DRAINED. It said so in as many words — the outbox "has its prompt
        // trigger on the push lane (`EVENT <sid> post`) and its own backstop in
        // [`Bridge::roster_backstop`]" — and there was no such call: the only
        // two triggers were that push-lane event and the idle arm. Both are
        // starvable by exactly the traffic that makes the idle arm unreachable
        // (`Queues::take` drains `closed > fleet > term > events > inbox`, so a
        // steady `term`/`fleet` stream outranks the `post` event), and a `GAP`
        // — aterm saying the very line that carries that event may have been
        // coalesced away — routes HERE for its repair. The endpoint leans on
        // the same sentence for its 4 MiB drain budget (`cmd_outbox`: "a drain
        // stopped by the budget is resumed by the next call"). One call makes
        // all three true.
        self.drain_outbox();
    }

    /// Take a fresh `status revision=` baseline for one session, discarding any
    /// pending `driven` mark with it.
    ///
    /// "OBSERVED QUIET AT LEAST ONCE" SURVIVES A HANDOFF, and dropping it was a
    /// hole in §9.3's structural "human always wins".
    ///
    /// [`Bridge::sample_local_control`] will not call an advance unaccounted
    /// until the session has been seen quiet once — the settling rule, and it is
    /// the right rule: a session that is still moving is settling from something
    /// already accounted for, and pausing a handover on that would make every
    /// handover useless. But a suppressed advance is CONSUMED, not deferred: the
    /// baseline moves to the new revision and that particular change is never
    /// reconsidered. So restarting the settling clock here — on a session the
    /// last sample had already seen QUIET, whose classifier has not moved since
    /// — threw away the very observation the rule asks for, and made §6.6 row 4
    /// depend on where the sampling phase happened to fall: a human who typed
    /// once, in the round after taking or granting the keyboard, was never
    /// noticed at all. (Twice and they were: the second advance meets a settled
    /// session. A guarantee that needs the person to type twice is not the
    /// guarantee §9.3 states.)
    ///
    /// So `moved_at` is KEPT when this baseline reads the same revision the last
    /// sample did — nothing moved, so nothing is settling — and restarted only
    /// when the classifier moved INTO the baseline, which is the settling case
    /// the rule exists for. A session with no prior sample starts its clock
    /// here, which is the un-settled state the count-based rule opened with.
    fn baseline_local(&mut self, sid: &str) {
        // AND AN UNREADABLE `status` TAKES NO BASELINE AT ALL. Folding the
        // failed read into `revision = 0` installed `seen: true` over a real
        // observation, so the next successful read was an advance from zero and
        // the first settled round after it parked the session at `human?` —
        // see [`Bridge::observe_local_control`], which had the same hole. The
        // prior sample is the best answer available and is kept: its `revision`
        // is a real reading, so a genuine advance past it is still seen, and a
        // `driven_at` it carries is bounded by [`DRIVEN_KEEP`] anyway.
        let Some((revision, _)) = self.status_sample(sid) else {
            return;
        };
        let prior = self.local.get(sid).copied().unwrap_or_default();
        let moved_at = if prior.seen && revision == prior.revision {
            prior.moved_at
        } else {
            Some(Instant::now())
        };
        self.local.insert(
            sid.to_string(),
            LocalSample {
                seen: true,
                revision,
                moved_at,
                driven_at: None,
            },
        );
    }

    /// The session's `status revision=` and `hold=`, or `None` when the verb
    /// could not be read at all.
    ///
    /// `None` IS NOT A ZERO, and no caller may turn it into one: `revision` is
    /// compared against the last one seen, so a failed read folded into `0`
    /// makes the next successful read an advance. See
    /// [`Bridge::observe_local_control`].
    fn status_sample(&mut self, sid: &str) -> Option<(u64, bool)> {
        if self.fault == Fault::FailStatusWhileMarked
            && self.state.root().join("fail-status").exists()
        {
            return None;
        }
        let reply = self.ctl_request(&format!("@{sid} status")).ok()?;
        if !reply.ok() {
            return None;
        }
        let mut revision = 0u64;
        let mut hold = false;
        for tok in reply.header().split_whitespace() {
            if let Some(v) = tok.strip_prefix("revision=") {
                revision = v.parse().unwrap_or(0);
            } else if let Some(v) = tok.strip_prefix("hold=") {
                hold = v == "1";
            }
        }
        Some((revision, hold))
    }

    /// The holder of aterm's own cooperative lease, when it is a LOCAL driver
    /// rather than this bridge's own `fabric:` mirror.
    fn local_lease_holder(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} lease status")).ok()?;
        if !reply.ok() {
            return None;
        }
        reply
            .header()
            .split_whitespace()
            .find_map(|t| t.strip_prefix("holder="))
            .filter(|h| !h.starts_with("fabric:"))
            .map(str::to_string)
    }

    /// THE ONE PATH FROM THE BUS TO A PTY, and every condition on it, in one
    /// place (§6.6):
    ///
    /// > applied iff `<src>` equals the `control` holder **at apply time** ∧
    /// > `hold=0` ∧ the body's `epoch=` equals the live session's launch nonce ∧
    /// > (`gen=` absent or equal to the live `content_seq:fp16`).
    ///
    /// A rejected attempt leaves no `In` and is recorded `ev refused
    /// reason=holder|hold|epoch|gen`. `re=` is dense and guessable and `gen=` is
    /// observable from presence, so a body that could TRIGGER keystrokes on their
    /// strength would let any lane-writer drive a worker — which is why no other
    /// record shape reaches this function at all. A `re=` on an applied record is
    /// carried into the `ev` as CAUSALITY and is read by nothing else; it can no
    /// more unlock this function than it could before.
    ///
    /// ## The window, and the journal around it (§6.5)
    ///
    /// The feed is bracketed: [`StateDir::set_feed_intent`] writes the offset,
    /// the session and the key BEFORE the verb; the outcome is published as an
    /// `ev` after it; the intent is cleared only then. A `SIGKILL` anywhere
    /// inside that bracket leaves the intent on disk, and
    /// [`Bridge::resolve_pending_feed`] asks the endpoint — with the SAME key —
    /// which side of the window it fell on. That is the difference between this
    /// rung and A3, whose drive face resumed at the head and therefore lost the
    /// record without ever saying so.
    fn on_term_record(&mut self, rec: &BrokerRecord) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let (off, subject, raw) = rec;
        let off = *off;
        let Ok(addr) = subject::parse_term_in(&self.cfg.fleet, &self.node, subject) else {
            return;
        };
        let (body, Some(bytes)) = Body::decode(raw) else {
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=term reason=malformed", addr.sid),
            );
            return;
        };
        // `epoch=` IS MANDATORY, and the check is presence-then-equality rather
        // than `Option == Option`. `body.rs` documents it as "MANDATORY on
        // `term/in` and `control`" and `on_control_message` enforces presence
        // explicitly; the DRIVE face — the one path to a PTY — compared two
        // `Option`s, so a record carrying NO `epoch=` at all passed whenever
        // this bridge happened to have no entry for that sid. `holders` is
        // restored from the bus and can outlive `epochs`, so that state is
        // reachable: a record from the recorded holder with no epoch was
        // journalled and fed with nothing epoch-fenced about it.
        let mut reason = if self.halt_applied {
            Some("hold")
        } else if self.holders.get(&addr.sid).map(String::as_str) != Some(addr.src.as_str()) {
            Some("holder")
        } else if !matches!(
            (body.epoch.as_deref(), self.epochs.get(&addr.sid)),
            (Some(claimed), Some(live)) if claimed == live
        ) {
            Some("epoch")
        } else {
            None
        };
        // THE LIVE GENERATION, read ONCE. It is both the fourth condition's
        // right-hand side and the `seq=` §10 wants on the `applied` row, so
        // reading it twice would be two round trips describing two instants.
        let live = self.live_gen(&addr.sid);
        // THE FOURTH CONDITION, and the one A3 left out: `gen=` absent, or equal
        // to the live generation. It is read HERE, at apply time, from the
        // instance — never from the presence row this bridge published, which
        // could be a round old. A `gen=` that no longer matches means the sender
        // answered a screen that has since moved, which is exactly the approval
        // prompt §6.6 names (`docs/OPERATOR-EMBEDDED.md:253-256`).
        if reason.is_none() {
            if let Some(claimed) = body.gen.clone() {
                if live.as_deref() != Some(claimed.as_str()) {
                    reason = Some("gen");
                }
            }
        }
        if let Some(reason) = reason {
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=term reason={reason}", addr.sid),
            );
            return;
        }
        // §10's `seq=<n>`: the session's `content_seq` BASELINE, read immediately
        // before the write, so "the screen that followed is `term/out` after seq
        // n" is a statement a reader can check. `feed-bin`'s own reply carries no
        // `seq=` — only the line-dispatched input verbs are stamped
        // (`control.rs` `stamp_input_seq`) and the framed path never reaches it —
        // so the bridge reads the baseline rather than inventing one. `-` when
        // the screen could not be read at all, which is honest: an unknown
        // baseline is not a zero.
        let baseline = live
            .as_deref()
            .and_then(|g| g.split(':').next())
            .unwrap_or("-")
            .to_string();
        // JOURNAL BEFORE THE VERB. The key is minted from what is already on the
        // record and in this process: the epoch just checked, this node's bound
        // producer id, and the record's own bus offset + 1 (offsets are dense and
        // monotone on a subscription, so they are already the monotone sequence
        // A6's mark wants — and `+1` because sequence 0 is A6's "consumed
        // nothing"). The same record therefore mints the same key on a replay,
        // which is the whole mechanism.
        // NO OFFSET LEAVES THE JOURNAL WITHOUT A VERDICT.
        //
        // The journal is ONE slot (`state.rs`: one file, `feeding`), and A6's
        // retry path deliberately KEEPS an entry across records: a non-final
        // verdict — `ERR busy` while a local `turn` holds the lease, `ERR
        // halted`, `ERR rate`, or an aterm that did not answer — publishes no
        // `ev` and leaves the entry armed to be asked again. A3 then wrote the
        // next drive record's intent over it unconditionally, so a second
        // keystroke arriving before the retry erased the first: never fed,
        // never retried, and never reported — no `applied`, no `refused`, no
        // `in-doubt`. That is exactly the silent loss `pty_idem.rs`'s header
        // and `Verdict::is_final`'s doc both say this rung exists to end.
        //
        // So a pending entry is RESOLVED before a new one is minted, and if it
        // still cannot be resolved this round it is published as `in-doubt …
        // reason=superseded` and retired. Superseded is the honest word: the
        // bridge does not know whether those bytes reached the PTY, and §6.5's
        // rule for that state is "reported, never silently replayed".
        if !self.settle_pending_feed_before(off) {
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=term reason=pending", addr.sid),
            );
            return;
        }
        let epoch = body.epoch.clone().unwrap_or_default();
        let intent = crate::state::FeedIntent {
            off,
            sid: addr.sid.clone(),
            key: format!("{epoch}:{}:{}", self.producer_id, off + 1),
            tries: 1,
            // THE BUDGET IS A DURATION AND IT STARTS HERE. See [`FEED_BUDGET`].
            first_at: crate::now_ms(),
            doubt: false,
        };
        if let Err(e) = self.state.set_feed_intent(&intent) {
            // NOT FED. An unjournalled feed is exactly the in-doubt window this
            // rung closes, so a state dir that cannot be written refuses the
            // keystroke and says so, rather than typing it blind.
            eprintln!("aterm-link: could not journal the feed intent: {e}");
            self.publish_ev_for(
                Some(&addr.sid.clone()),
                &format!("refused sid={} face=term reason=no-journal", addr.sid),
            );
            return;
        }
        let reply = self.feed(&intent, &bytes);
        // THE WINDOW, timed from inside it. The keystroke is on the PTY and this
        // process has not yet said so anywhere: the intent is on disk, the `ev`
        // is not published, and nothing but the journal knows what happened. A
        // test that raced this from outside would be a flake; the process whose
        // progress defines the window is the one that ends it.
        if self.fault == Fault::KillInFeedWindow
            && reply.as_deref().is_some_and(|r| r.starts_with("OK"))
        {
            self.fault.fire(Fault::KillInFeedWindow, &self.state);
        }
        self.record_feed_outcome(&intent, &feed_verdict(reply.as_deref()), &baseline, false);
    }

    /// Write one journalled `term/in`'s bytes to the PTY under its key.
    ///
    /// `None` is an I/O failure against aterm itself — the one outcome that says
    /// nothing at all about the PTY, and therefore the one the journal exists
    /// for.
    fn feed(&mut self, intent: &crate::state::FeedIntent, bytes: &[u8]) -> Option<String> {
        if self.fault == Fault::RefuseFirstFeed {
            self.fault = Fault::None;
            Fault::RefuseFirstFeed.fired(&self.state);
            return Some("ERR busy turn=0".to_string());
        }
        if self.fault == Fault::RefuseFeedsWhileMarked
            && self.state.root().join("refuse-feeds").exists()
        {
            return Some("ERR busy turn=0".to_string());
        }
        // `id=` is a TRAILING token on `feed-bin` (the frame announces its length
        // first, `control.rs` `parse_feed_bin`), unlike the leading `id=` the
        // line verbs take. One wrong position here is an `ERR usage` on every
        // keystroke, so it is written once, in one place.
        let line = format!("@{} feed-bin {} id={}", intent.sid, bytes.len(), intent.key);
        match self.ctl_request_with_body(&line, bytes) {
            Ok(reply) => Some(reply.header().to_string()),
            Err(e) => {
                eprintln!("aterm-link: feed-bin failed: {e}");
                None
            }
        }
    }

    /// Turn one feed's reply into the record §10 asks for, then retire the
    /// journal entry.
    ///
    /// Four outcomes, and every one of them is SAID:
    ///
    /// | reply | `ev` | meaning |
    /// |---|---|---|
    /// | `OK …` | `applied re=<off> seq=<n>` | it ran; this is §10's causal pointer |
    /// | `OK dup=1` | `applied re=<off> dup=1` | a replay found it already landed |
    /// | `ERR in-doubt seq=<n>` | `in-doubt re=<off> …` | it MAY have typed; an operator's problem, never a silent retry |
    /// | `ERR busy idem=<n>`, or no reply at all | retried, then `in-doubt … reason=unresolved` | an attempt under this key is in the seam, or aterm never answered: it MAY have typed |
    /// | anything else | `refused re=<off> reason=<first token>` | refused before a byte could move |
    ///
    /// The two middle rows and every other transient reply are RETRIED under the
    /// same key first ([`FEED_BUDGET`]); what a spent budget publishes is the
    /// sticky `doubt` bit's business, below, not the last reply's.
    ///
    /// The journal is cleared last, and only after the `ev` publish was
    /// attempted. A crash between the publish and the clear costs one duplicate
    /// `applied … dup=1` row on the next replay and nothing else; a crash the
    /// other way round would cost the record that says what happened.
    fn record_feed_outcome(
        &mut self,
        intent: &crate::state::FeedIntent,
        verdict: &Verdict,
        baseline: &str,
        replayed: bool,
    ) {
        // AN UNANSWERED QUESTION IS NOT AN ANSWER. A transient refusal consumed
        // no sequence, so the record is still pending: keep the journal and ask
        // again on the next round, under the same key. Past the budget the
        // silence itself is the news, and it is published.
        //
        // THE ENTRY THEREFORE OUTLIVES THIS CALL, and the journal has one slot.
        // [`Bridge::settle_pending_feed_before`] is what stops the next drive
        // record from erasing it without a verdict, and the loop's retry
        // deadline is what stops a BUSY bridge — one that never reaches the
        // idle arm — from never asking again.
        if !verdict.is_final() {
            // DOUBT IS STICKY, AND IT IS DURABLE. An attempt aterm never
            // answered may have reached the PTY; a later `ERR busy turn=`
            // refuses THAT attempt before the mark is claimed and says nothing
            // about the earlier one, so the doubt cannot be argued away by what
            // came after it. Written back with the journal entry it belongs to.
            // (`ERR busy idem=` is not one of those later refusals — it is
            // doubt in its own right, and [`feed_verdict`] says why.)
            let doubt = intent.doubt || matches!(verdict, Verdict::InDoubt { .. });
            let elapsed = crate::now_ms().saturating_sub(intent.first_at);
            let spent = intent.first_at > 0 && elapsed >= FEED_BUDGET.as_millis() as u64;
            if !spent && intent.tries < FEED_TRIES_MAX {
                if doubt != intent.doubt {
                    let marked = crate::state::FeedIntent {
                        doubt,
                        ..intent.clone()
                    };
                    if let Err(e) = self.state.set_feed_intent(&marked) {
                        eprintln!("aterm-link: could not journal the feed's doubt: {e}");
                    }
                }
                return;
            }
            // WHAT THE SILENCE ACTUALLY SAYS. `in-doubt` is §6.5's "it may have
            // typed and nothing can tell", and publishing it for a record that
            // was refused CLEANLY on every attempt — the key's sequence never
            // consumed, which is the argument `Verdict::is_final` itself makes
            // — asserts an unknown the bridge does not have. A budget spent
            // entirely on pre-write refusals is a REFUSAL, named by the last
            // reason the endpoint gave, and it is recoverable by whoever reads
            // it; an in-doubt is terminal.
            //
            // WHICH TURNS ENTIRELY ON `doubt` BEING COMPLETE, and the one reply
            // that hides inside a refusal is `ERR busy idem=`: it is answered by
            // the mark, not before it, so it is read as doubt at the source
            // ([`feed_verdict`]) rather than filtered out here — by the time a
            // verdict reaches this branch `reason_token` has already reduced
            // every `ERR busy` to the same word.
            let exhausted = if doubt {
                Verdict::InDoubt {
                    seq: String::new(),
                    why: Some("unresolved"),
                }
            } else {
                Verdict::Refused {
                    why: match verdict {
                        Verdict::Refused { why } => why.clone(),
                        _ => "unresolved".to_string(),
                    },
                }
            };
            eprintln!(
                "aterm-link: giving up on {} after {} attempts over {elapsed} ms; escalating",
                intent.render(),
                intent.tries
            );
            if self.publish_ev_re(
                Some(&intent.sid.clone()),
                &exhausted.ev(&intent.sid, intent.off, baseline, replayed),
                intent.off,
            ) {
                self.state.clear_feed_intent();
            }
            return;
        }
        // ACCOUNT FOR IT. The next `status revision=` advance this session shows
        // is the bridge's own doing, so it must not read as the unaccounted
        // change that parks the row (§6.6 row 4). A `dup=1` typed nothing, so it
        // accounts for nothing.
        if *verdict == (Verdict::Applied { dup: false }) {
            self.local.entry(intent.sid.clone()).or_default().driven_at = Some(Instant::now());
        }
        // THE JOURNAL IS RETIRED ONLY ONCE ITS VERDICT IS ON THE BUS. A clear
        // after a publish that failed (an unreachable broker) would retire the
        // record with its fate on nobody's log — the silent loss again, one
        // layer down. Keeping it costs at most one duplicate `applied … dup=1`
        // on the next round, which is what A6's mark is for.
        if self.publish_ev_re(
            Some(&intent.sid.clone()),
            &verdict.ev(&intent.sid, intent.off, baseline, replayed),
            intent.off,
        ) {
            self.state.clear_feed_intent();
        }
    }

    /// Clear the way for a NEW drive record at `next_off`: resolve whatever the
    /// single-slot journal is still holding, and if it cannot be resolved, say
    /// so on the bus before overwriting it.
    ///
    /// See the note at the call site. The `in-doubt` is not a formality: the
    /// entry survives only when the last attempt's verdict was non-final, which
    /// means the bridge genuinely does not know whether those bytes reached the
    /// PTY, and an offset that vanished from the journal with no `ev` is
    /// unrecoverable by anyone.
    /// Answers whether the journal is now FREE. `false` means the pending entry
    /// could not be resolved AND its supersession could not be recorded, so the
    /// caller must refuse the new record rather than erase the old one — losing
    /// a keystroke that was never fed is strictly better than losing one that
    /// may already be on a PTY.
    fn settle_pending_feed_before(&mut self, next_off: u64) -> bool {
        let Some(pending) = self.state.feed_intent() else {
            return true;
        };
        if pending.off == next_off {
            // The same record, being re-armed by a replay: not a supersession.
            return true;
        }
        self.resolve_pending_feed();
        let Some(still) = self.state.feed_intent() else {
            return true;
        };
        eprintln!(
            "aterm-link: superseding an unresolved feed: {}",
            still.render()
        );
        // THE SAME DISTINCTION THE BUDGET MAKES. A record every attempt refused
        // before a byte could move is not in doubt just because a newer record
        // has taken the slot: nothing typed, the key's sequence was never
        // consumed, and `refused` is the recoverable verdict a reader can act
        // on. `in-doubt` is kept for the entry that has genuinely been in
        // doubt — an attempt aterm never answered — which is the state
        // `FeedIntent::doubt` records and an old journal line assumes.
        let superseded = if still.doubt {
            Verdict::InDoubt {
                seq: String::new(),
                why: Some("superseded"),
            }
        } else {
            Verdict::Refused {
                why: "superseded".to_string(),
            }
        };
        if !self.publish_ev_re(
            Some(&still.sid.clone()),
            &superseded.ev(&still.sid, still.off, "-", true),
            still.off,
        ) {
            return false;
        }
        self.state.clear_feed_intent();
        true
    }

    /// ASK THE ENDPOINT which side of the feed window a journalled record fell
    /// on, and record the answer (§6.5).
    ///
    /// ## When it runs, and how often
    ///
    /// It is a no-op unless a `feeding` entry is present. An entry is present
    /// for TWO reasons, and A3's doc named only the first: this process died
    /// between the journal write and the outcome record, OR the last attempt's
    /// verdict was not final ([`Verdict::is_final`]) and
    /// [`Bridge::record_feed_outcome`] deliberately kept the entry to ask again.
    ///
    /// So it does NOT run once. It runs at every attach, on a deadline from the
    /// main loop, and on the idle roster round, re-asking under the SAME key
    /// until the journal retires — bounded by [`FEED_BUDGET`], a span of wall
    /// clock measured from the first attempt and carried in the journal so a
    /// restart cannot buy a fresh one, with [`FEED_TRIES_MAX`] as the backstop
    /// for the restart loop in which no time passes; the attempt count is
    /// written durably BEFORE each attempt. Nothing is published on the
    /// intermediate rounds; the `ev` comes on a final verdict, on budget
    /// exhaustion, or when a new drive record supersedes the entry
    /// ([`Bridge::settle_pending_feed_before`]).
    ///
    /// **The property that makes repeating it safe is not that it happens once.**
    /// It is A6's per-session, per-producer, monotone mark: the replay re-sends
    /// the same key, so the endpoint answers `OK dup=1` (it landed — write
    /// nothing), `ERR in-doubt` (it may have; refuse and escalate) or applies it
    /// fresh (it did not land — the repair).
    ///
    /// **The replay re-sends the same key, and that is the whole safety
    /// argument.** A6's mark is per-session, per-producer and monotone, so the
    /// endpoint answers `OK dup=1` (it landed — write nothing), `ERR in-doubt`
    /// (it may have; refuse and escalate) or applies it fresh (it did not land —
    /// the repair). What the replay does NOT re-check is deliberate and bounded:
    ///
    /// * the EPOCH is re-checked, structurally — it is inside the key, and a key
    ///   minted against a session that has since relaunched is `ERR epoch` at the
    ///   endpoint before any byte moves;
    /// * the HOLD is re-checked, structurally — the endpoint's gate refuses
    ///   `feed-bin` under a hold before A6's mark is even claimed, so a halt that
    ///   landed while this bridge was down stops the replay dead;
    /// * the HOLDER is NOT re-consulted. The apply was authorized when it began;
    ///   a claim that arrives afterwards does not retroactively un-authorize an
    ///   operation already in flight, and treating it as though it did would turn
    ///   every crash into a silent loss — the exact failure this rung exists to
    ///   end;
    /// * the `gen=` is NOT re-checked, because the keystroke's OWN effect is what
    ///   moved the screen. A replay that re-checked it could never repair
    ///   anything.
    ///
    /// Whatever it finally learns is published as an `ev` and the journal is
    /// retired. What it must never do is retire an entry SILENTLY — a keystroke
    /// that left the journal with no record of its fate is the silent loss §6.5
    /// forbids, and every exit from this function publishes one.
    fn resolve_pending_feed(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let Some(intent) = self.state.feed_intent() else {
            return;
        };
        // THE BYTES COME BACK FROM THE LOG, not from the state dir. The record is
        // still there — the bus is append-forever (§10) — so the journal never
        // has to hold a stranger's payload at rest, and the replay is over the
        // same bytes the broker has rather than a copy that could have drifted.
        let filter = subject::term_filter(&self.cfg.fleet, &self.node);
        let found = self
            .conn
            .as_mut()
            .and_then(|c| c.fetch(intent.off, &filter, 1).ok())
            .and_then(|(rows, _)| rows.into_iter().next())
            .filter(|(off, _, _)| *off == intent.off);
        let Some((_, subject, raw)) = found else {
            // The record cannot be re-read, so it cannot be resolved. Say so and
            // retire the entry rather than carrying it forever: a journal that
            // never empties is a bridge that replays on every attach.
            self.record_feed_outcome(&intent, &Verdict::UNREADABLE, "-", true);
            return;
        };
        // The subject is re-parsed rather than trusted from the journal: this is
        // still the ONE path to a PTY, and it may not widen just because the
        // record has been seen before.
        let same_session = subject::parse_term_in(&self.cfg.fleet, &self.node, &subject)
            .is_ok_and(|addr| addr.sid == intent.sid);
        let (_, bytes) = Body::decode(&raw);
        let (true, Some(bytes)) = (same_session, bytes) else {
            self.record_feed_outcome(&intent, &Verdict::UNREADABLE, "-", true);
            return;
        };
        eprintln!(
            "aterm-link: resolving an unfinished feed: {}",
            intent.render()
        );
        // THE ATTEMPT IS COUNTED BEFORE IT IS MADE, and durably, so a crash
        // inside the retry is bounded by the same budget as a refusal is. A
        // counter that only advanced on a completed attempt would let a bridge
        // that dies at the same step relaunch into it forever.
        let intent = crate::state::FeedIntent {
            tries: intent.tries + 1,
            // A JOURNAL LINE FROM BEFORE `first=` EXISTED starts its budget now
            // rather than at the epoch, which would read as instantly spent.
            first_at: if intent.first_at == 0 {
                crate::now_ms()
            } else {
                intent.first_at
            },
            ..intent
        };
        if let Err(e) = self.state.set_feed_intent(&intent) {
            eprintln!("aterm-link: could not journal the replay attempt: {e}");
            return;
        }
        let reply = self.feed(&intent, &bytes);
        let baseline = self
            .live_gen(&intent.sid)
            .and_then(|g| g.split(':').next().map(str::to_string))
            .unwrap_or_else(|| "-".to_string());
        self.record_feed_outcome(&intent, &feed_verdict(reply.as_deref()), &baseline, true);
    }

    // -----------------------------------------------------------------------
    // the screen face (opt-in)
    // -----------------------------------------------------------------------

    /// Whether this session's screen is published (`--screen <sid>` or
    /// `--screen all`).
    fn screens(&self, sid: &str) -> bool {
        self.cfg
            .screen
            .iter()
            .any(|s| s == "all" || s == sid || s.trim_start_matches('@') == sid)
    }

    /// Publish a `screen` snapshot for every opted-in session whose generation
    /// moved (§3.3, §10).
    ///
    /// This is the face a consistent-cut replay re-folds a screen from. It is a
    /// LAST-VALUE face carrying a whole frame rather than a `term/out` stream of
    /// deltas, which is what §3.3 specifies and what keeps the cost of the
    /// feature one record per changed screen per period instead of one per byte
    /// the shell wrote.
    ///
    /// Three bounds, all of them because this is the one fabric face that
    /// carries screen CONTENT:
    ///
    /// * **opt-in** — `--screen` is empty by default (§14's open question 2);
    /// * **rate** — at most one publish per changed session per
    ///   [`SCREEN_PERIOD`], which is the "≤ 4/s" §3.3 states of each
    ///   `…/<sid>/screen` subject. Taken from the clock rather than from how
    ///   often the loop happens to run, so a busy bridge pays no more than an
    ///   idle one — but the clock gates the SWEEP, so the bound is per SUBJECT
    ///   and not per node: N changed screens are N records a period, which is
    ///   the cost the paragraph above names;
    /// * **change** — a generation that has not moved publishes nothing, so a
    ///   quiet session costs one record and then nothing at all. On an
    ///   append-forever log (§10) a periodic republish would be an unbounded
    ///   write for no new information, which is the shape of defect five audit
    ///   rounds keep finding.
    ///
    /// A frame over [`SCREEN_MAX`] is SKIPPED with an `ev`, never truncated: a
    /// half-frame is a document that parses and lies.
    ///
    /// One echo, deliberately unremarked-upon anywhere else: this subject is
    /// inside the bridge's own `term/<node>/>` subscription, so every snapshot
    /// comes back through the mailbox. [`Bridge::on_term_record`] drops it at
    /// `parse_term_in` — a `screen` subject is seven segments and the drive face
    /// is eight, with a literal `in` at position six — so it costs one parse and
    /// cannot loop. Narrowing the subscription to exclude it would need a second
    /// filter and a second connection to carry it, which is a worse trade than a
    /// parse four times a second.
    fn publish_screens(&mut self) {
        if self.attachment == Attachment::Observer || self.cfg.screen.is_empty() {
            return;
        }
        if Instant::now() < self.screen_due {
            return;
        }
        self.screen_due = Instant::now() + SCREEN_PERIOD;
        let sids: Vec<String> = self
            .locals
            .values()
            .filter(|sid| self.screens(sid))
            .cloned()
            .collect();
        for sid in sids {
            let Ok(reply) = self.ctl_request(&format!("@{sid} text --json")) else {
                continue;
            };
            let Some(frame) = reply.rows().first().cloned() else {
                continue;
            };
            let Some(gen) = gen_of_frame(&frame) else {
                continue;
            };
            if self.screen_gen.get(&sid) == Some(&gen) {
                continue;
            }
            if frame.len() > SCREEN_MAX {
                self.publish_ev_for(
                    Some(&sid.clone()),
                    &format!(
                        "screen-skipped sid={sid} reason=too-large bytes={}",
                        frame.len()
                    ),
                );
                // The generation is recorded anyway: a screen too large to
                // publish is not a screen to re-ask about four times a second.
                self.screen_gen.insert(sid, gen);
                continue;
            }
            let epoch = self.epochs.get(&sid).cloned().unwrap_or_else(|| "-".into());
            let subject = format!("/f/{}/term/{}/{sid}/screen", self.cfg.fleet, self.node);
            let mut body = format!(
                "v=1 t={} epoch={epoch} gen={gen} len={}\n",
                crate::now_ms(),
                frame.len()
            )
            .into_bytes();
            body.extend_from_slice(frame.as_bytes());
            match self.publish(&subject, &body) {
                Ok(_) => {
                    self.screen_gen.insert(sid, gen);
                }
                Err(e) => eprintln!("aterm-link: could not publish {sid}'s screen: {e}"),
            }
        }
    }

    /// The session's LIVE generation, `<content_seq>:<fp16>` (§6.6).
    ///
    /// One round trip: `text --json` carries both halves of the same instant —
    /// the engine's `content_seq` as `"seq"`, and the visible rows the
    /// fingerprint is taken over. The fingerprint is FNV-1a-64 over the ROWS
    /// ARRAY of that frame — the raw JSON slice between `{"rows":[` and
    /// `],"cursor":`, see [`gen_of_frame`] — so the two halves are the pair a
    /// sender saw rather than a number a sender could guess: `content_seq`
    /// alone is dense and predictable, and the fingerprint alone repeats
    /// whenever a screen returns to a previous state.
    ///
    /// IT IS NOT aterm's `turn` `hash=`, AND THE TWO MUST NEVER BE COMPARED.
    /// The algorithm is the same FNV-1a-64; the INPUT is not. `turn` hashes the
    /// plain visible rows joined by `\n` (`control_session.rs`'s `screen`),
    /// this hashes the JSON encoding of the rows array, and the two therefore
    /// disagree for every screen there is. §6.6 says so in as many words —
    /// `fp16` "is deliberately NOT byte-identical to aterm's own `turn`
    /// `hash=` … so the two must never be compared" — and this comment used to
    /// say the opposite, which is an invitation to build a `gen=`-fenced
    /// approval out of a `hash=` a driver already has and watch every record it
    /// fences be refused `reason=gen` for a units mismatch two doc comments
    /// asserted away.
    ///
    /// `None` when the screen cannot be read, and that REFUSES a `gen=`-bearing
    /// record rather than admitting it — an unreadable screen is not a matching
    /// one.
    fn live_gen(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} text --json")).ok()?;
        gen_of_frame(reply.rows().first()?)
    }

    /// The session's `meta attention=` — §4.1's typed needs-human escalation —
    /// as one bounded, already-pct-encoded presence token.
    ///
    /// `-` for unset, for a session that cannot be read, and for a value this
    /// build will not carry: aterm's own reply is pct-encoded and capped at 256
    /// bytes, and this clamps again at [`crate::glance::ATTENTION_CAP`] because
    /// the string is a stranger's and it ends up on an append-forever log and in
    /// a notifier's argv. A clamp that would split a `%XX` escape trims past it,
    /// the same rule [`halt_reason_token`] keeps.
    fn attention_of(&mut self, sid: &str) -> String {
        let Ok(reply) = self.ctl_request(&format!("@{sid} meta")) else {
            return "-".to_string();
        };
        if !reply.ok() {
            return "-".to_string();
        }
        let raw = reply
            .header()
            .split_whitespace()
            .find_map(|t| t.strip_prefix("attention="))
            .unwrap_or("-");
        let mut out = String::with_capacity(crate::glance::ATTENTION_CAP);
        for byte in raw.bytes() {
            if out.len() == crate::glance::ATTENTION_CAP {
                break;
            }
            if byte.is_ascii_graphic() {
                out.push(byte as char);
            }
        }
        while out.ends_with('%') || (out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%') {
            out.pop();
        }
        if out.is_empty() {
            return "-".to_string();
        }
        out
    }

    // -----------------------------------------------------------------------
    // the outbound plane
    // -----------------------------------------------------------------------

    /// Drain aterm's outbox and publish each post under the node's bound cap.
    ///
    /// A post whose address cannot be resolved is retired `off=-` with an
    /// `undeliverable` row rather than left in the queue forever: a queue that
    /// never empties is how an outbound bound turns into an outbound leak. A post
    /// the BROKER cannot take is left exactly where it is — the broker will come
    /// back, and the whole point of the queue is to survive that.
    fn drain_outbox(&mut self) {
        if self.conn.is_none() {
            return;
        }
        let drained = match self.ctl_request("outbox") {
            Ok(reply) if reply.ok() => reply.body().to_vec(),
            Ok(reply) => {
                eprintln!("aterm-link: outbox refused: {}", reply.header());
                return;
            }
            Err(e) => {
                eprintln!("aterm-link: outbox failed: {e}");
                return;
            }
        };
        for post in parse_outbox(&drained) {
            match self.resolve_to(&post.to, Some(&post.sid)) {
                Route::To(subject) => {
                    let subject = format!("{subject}/{}", post.kind);
                    let mut body = Body::new(crate::now_ms());
                    body.from = Some(post.sid.clone());
                    body.re = post.re;
                    body.dl = post.dl;
                    body.via = post.via.clone();
                    body.text = String::from_utf8_lossy(&post.body).into_owned();
                    let encoded = body.encode(None);
                    // THE SEQUENCE IS PINNED TO THE POST, reserved durably before
                    // the publish and reused verbatim on every retry. `outbox` is
                    // a peek, so a bridge that died between the `Publish` and the
                    // `outbox sent` re-reads this same post — and a FRESH sequence
                    // would put a second copy on the bus, because the broker's
                    // dedup key is `(producer_id, producer_seq)`.
                    let seq = match self.state.post_seq(&post.sid, post.id) {
                        Some(seq) => seq,
                        None => {
                            let Ok(seq) = self.next_seq() else { return };
                            if self.state.set_post_seq(&post.sid, post.id, seq).is_err() {
                                return;
                            }
                            seq
                        }
                    };
                    let published = if self.fault == Fault::FailPostPublishWhileMarked
                        && self.state.root().join("fail-post-publish").exists()
                    {
                        Err(io::Error::other("ATERM_LINK_FAULT: post publish refused"))
                    } else {
                        self.publish_at(seq, &subject, &encoded)
                    };
                    match published {
                        Ok((off, _deduped)) => {
                            // The offset is the RECORD's either way: a deduped
                            // re-send answers the original one. Remember it as
                            // OURS before anything else can see it — the
                            // self-lane check reads this set, and a record acked
                            // but not remembered would be refused as a forgery of
                            // ourselves.
                            self.remember_self_ack(off);
                            if self.fault == Fault::KillAfterPublish {
                                self.fault.fire(Fault::KillAfterPublish, &self.state);
                            }
                            let retired = self.ctl_request(&format!(
                                "outbox sent {} {} off={off}",
                                post.sid, post.id
                            ));
                            // Forget the reservation only once the endpoint has
                            // taken the retirement: the file must outlive every
                            // moment in which a retry could still need it.
                            if retired.is_ok_and(|r| r.ok()) {
                                self.state.clear_post_seq(&post.sid, post.id);
                            }
                        }
                        Err(e) => {
                            eprintln!("aterm-link: publish of post {} failed: {e}", post.id);
                            return;
                        }
                    }
                }
                // TWO WAYS A POST CAN DIE AT THE DOOR, and the sender is told
                // WHICH. `outbox sent … off=- reason=<why>` is what turns a
                // parked `post --wait` into `ERR ambiguous` rather than a
                // uniform `ERR undeliverable`: §6.1 makes the two-claimant case
                // its own verdict precisely because the remedy is different — a
                // bad address is the sender's mistake, a contested sid is a
                // fleet problem somebody has to look at.
                route => {
                    let why = route.reason();
                    let retired = self.ctl_request(&format!(
                        "outbox sent {} {} off=- reason={why}",
                        post.sid, post.id
                    ));
                    // AND THE RESERVATION IS FORGOTTEN HERE TOO, ON THE SAME
                    // CONDITION AS THE OTHER ARM: the endpoint has taken the
                    // retirement, so nothing can ever need the sequence again.
                    //
                    // A post reaches THIS arm holding a reservation whenever an
                    // earlier drain resolved it, reserved the sequence and then
                    // failed to publish (the arm above returns, keeping the
                    // file — correctly, for the retry) and a later drain
                    // resolves the same address differently: the roster moved,
                    // the advertising node went away, or a second node started
                    // claiming the sid and `resolve_to` now answers
                    // `Route::Ambiguous`. The post id is retired, so
                    // `sent/<sid>.<id>` is never read again — and `StateDir`
                    // has no sweep, so it survived for the life of the node
                    // with no operator undo but deleting files by hand.
                    if retired.is_ok_and(|r| r.ok()) {
                        self.state.clear_post_seq(&post.sid, post.id);
                    }
                    self.publish_ev(&format!("undeliverable to={} reason={why}", post.to));
                }
            }
        }
    }

    /// Resolve a `to=` address to the six-segment owner+src prefix of an `in`
    /// subject (the kind is appended by the caller), or to the sender's own
    /// `say` face.
    ///
    /// `@s-<sid>` is accepted only when this node hosts it, or when exactly one
    /// node advertises it AND the TOFU pin agrees (§6.1);
    /// `@s-<sid>@n-<node>` names the node explicitly; a bare principal is a
    /// `p/<principal>` owner path; `say` is the sender's own broadcast face.
    ///
    /// ## THE PIN IS AN OBSERVATION, NEVER A SENDER'S ARGUMENT
    ///
    /// §6.1 makes the pin a security mechanism against T15 (sid hijack): a bare
    /// `@s-<sid>` routes only when exactly one node advertises that sid "**and**
    /// the sender's bridge has pinned that (sid → node) pair **on first sight**"
    /// — first sight meaning a PRESENCE ADVERTISEMENT, which the claiming node
    /// had to hold its own cap to publish. A3 also pinned on the EXPLICIT-node
    /// arm, where the node segment comes straight out of a session's own
    /// `post to=` argument: `post` is owner-class and every in-session
    /// `aterm-ctl @self` is Owner (§8.1), so the stated adversary — a
    /// prompt-injected worker with a shell — could write the durable pin table
    /// with one message and no capability at all, permanently making a peer's
    /// sid `ERR ambiguous` for every sender on the node (or, before the real
    /// host came up, routing its mail onto a lane of the attacker's choosing).
    /// The table survives every restart and §11.2's `aterm-link pin` override is
    /// not implemented, so there was no undo.
    ///
    /// An address that NAMES its node needs no pin — the sender already said
    /// where it is going. So the explicit arm now reads the table and never
    /// writes it: a sid pinned to a DIFFERENT node is the §6.1 conflict and is
    /// refused rather than silently redirected (which is what returning the
    /// pinned node used to do to a sender who had explicitly asked for another),
    /// and an unpinned sid routes where it was told without leaving a mark.
    fn resolve_to(&mut self, to: &str, from_sid: Option<&str>) -> Route {
        let to = crate::pct::decode(to);
        let fleet = self.cfg.fleet.clone();
        // §4.2/§5.1's broadcast face: `/f/<F>/pub/<owner>/say/<kind>`, owner =
        // `<node>/<sid>`. The verb table, `POST_USAGE` and §11.2's serve spec
        // all name `to=say`, `cmd_post` accepts it and answers `OK <id>`, and
        // the tui renders and publishes it — but A3's `resolve_to` answered
        // `Unroutable`, so every such post was retired `off=- reason=unroutable`
        // and vanished from `inbox` with no verdict a non-waiting sender ever
        // sees. A verb row promising a target that always fails is a first-rank
        // doc defect in a crate whose catalog IS its contract, so the route is
        // implemented rather than the promise withdrawn — the withdrawal would
        // have to happen in `cmd_post` and the verb table, which are the
        // endpoint's.
        if to == "say" {
            return match from_sid.filter(|s| subject::is_principal(s)) {
                Some(sid) => Route::To(format!("/f/{fleet}/pub/{}/{sid}/say", self.node)),
                // A `say` with no session behind it (a bridge-internal notice)
                // has no owner face to speak under.
                None => Route::Unroutable,
            };
        }
        if let Some(rest) = to.strip_prefix('@') {
            let (sid, node) = match rest.split_once('@') {
                Some((sid, node)) => (sid.to_string(), Some(node.to_string())),
                None => (rest.to_string(), None),
            };
            if !subject::is_principal(&sid) || !sid.starts_with("s-") {
                return Route::Unroutable;
            }
            // ASK BEFORE GIVING UP. A session spawned a moment ago may not be in
            // this bridge's roster yet — the `session-created` event and the
            // `post` event are two lines on the same push lane, and the post can
            // reach `outbox` first. Refusing on that would retire a perfectly
            // routable message as undeliverable, which is a message the sender
            // was told nothing useful about.
            let mut hosted = self.epochs.contains_key(&sid);
            if !hosted {
                let _ = self.refresh_sessions();
                hosted = self.epochs.contains_key(&sid);
            }
            let node = match (node, hosted) {
                (Some(n), _) => {
                    if !subject::is_principal(&n) || !n.starts_with("n-") {
                        return Route::Unroutable;
                    }
                    match self.state.pins().get(&sid) {
                        Some(pinned) if *pinned != n => return Route::Ambiguous,
                        _ => n,
                    }
                }
                (None, true) => self.node.clone(),
                (None, false) => match self.pinned_node_for(&sid) {
                    Ok(node) => node,
                    Err(route) => return route,
                },
            };
            return Route::To(format!("/f/{fleet}/in/{node}/{sid}/{}", self.node));
        }
        if subject::is_principal(&to) {
            Route::To(format!("/f/{fleet}/in/p/{to}/{}", self.node))
        } else {
            Route::Unroutable
        }
    }

    /// §6.1's rule for a BARE `@s-<sid>`, in one place: "accepted only when
    /// exactly one node advertises that sid **and** the sender's bridge has
    /// pinned that (sid → node) pair on first sight. A second node advertising a
    /// pinned sid makes `post` answer `ERR ambiguous`".
    ///
    /// WHY BOTH HALVES. A node's cap covers its whole `pub/<n>/` subtree, so a
    /// rogue node CAN publish a presence row for a sid it does not host. The pin
    /// alone would stop that from re-routing an established peer, but a rogue
    /// that got there first would own the sid forever; the live advertiser set
    /// alone would let the rogue's row silently split the traffic. Together they
    /// answer the only honest verdict for two claimants: refuse, and say why.
    fn pinned_node_for(&mut self, sid: &str) -> Result<String, Route> {
        let advertisers = self.advertisers(sid);
        let pinned = self.state.pins().get(sid).cloned();
        match (&pinned, advertisers.len()) {
            // A pinned sid that somebody ELSE now advertises is the conflict
            // §6.1 names — whichever of them is lying.
            (Some(node), _) if advertisers.iter().any(|n| n != node) => Err(Route::Ambiguous),
            (Some(node), _) => Ok(node.clone()),
            (None, 0) => Err(Route::Unroutable),
            (None, 1) => {
                let node = advertisers.into_iter().next().unwrap_or_default();
                self.state.pin(sid, &node).map_err(|_| Route::Unroutable)
            }
            (None, _) => Err(Route::Ambiguous),
        }
    }

    /// Which nodes advertise `sid`, from `Last{/f/<F>/pub/*/<sid>/presence}` —
    /// one bounded round trip, no filesystem scan and no saved peer names (§7).
    ///
    /// OBSERVER ROWS ARE EXCLUDED. A hand-started `--sock` bridge publishes
    /// presence for sessions it watches but does not host
    /// ([`Bridge::publish_session_presence`]); counting those would make every
    /// watched session ambiguous the moment somebody opened an observer.
    ///
    /// AND SO ARE THE ROWS THAT SAY THE SESSION IS GONE. This took the map's
    /// KEYS and threw the `state=` away, while [`Bridge::holder_is_live`] read
    /// the same rows and answered on `state == "live"` — so a session whose
    /// only row is the `state=exited` withdrawal its own node published was
    /// simultaneously not live ("who can act") and the single routing
    /// candidate ("where do I send"), which is the disagreement
    /// `holder_is_live`'s doc says can never happen. It also wrote a PERMANENT
    /// TOFU pin for a sid that no longer exists. One predicate, one word, one
    /// place: `holder_is_live` now asks this function.
    fn advertisers(&mut self, sid: &str) -> BTreeSet<String> {
        self.roster_rows(sid)
            .into_iter()
            .filter(|(_, state)| state == "live")
            .map(|(node, _)| node)
            .collect()
    }

    /// Every non-observer roster row for `sid`, as `node -> state=`. One
    /// bounded `Last` per call and no paging state kept — the roster is
    /// presence, not history.
    fn roster_rows(&mut self, sid: &str) -> BTreeMap<String, String> {
        let filter = format!("/f/{}/pub/*/{sid}/presence", self.cfg.fleet);
        let mut out = BTreeMap::new();
        {
            let Some(conn) = self.conn.as_mut() else {
                return out;
            };
            // PAGED ON THE RESUME CURSOR (see [`last_all`]). A short walk here
            // misses a second node advertising the sid, so §6.1's `ERR ambiguous`
            // never fires and the post routes to whichever node the pin holds.
            let page = match last_all(conn, &filter) {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("aterm-link: could not read the roster for {sid}: {e}");
                    return out;
                }
            };
            for (_, subject, raw) in &page {
                // `["", "f", "<F>", "pub", "<node>", "<sid>", "presence"]`
                let segs: Vec<&str> = subject.split('/').collect();
                if segs.len() != 7 || segs.get(5) != Some(&sid) {
                    continue;
                }
                let (body, _) = Body::decode(raw);
                if body.unknown.get("observer").is_some_and(|v| v == "1") {
                    continue;
                }
                if let Some(node) = segs.get(4).filter(|n| subject::is_principal(n)) {
                    let state = body
                        .unknown
                        .get("state")
                        .cloned()
                        .unwrap_or_else(|| "-".to_string());
                    out.insert((*node).to_string(), state);
                }
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // aterm's events digest
    // -----------------------------------------------------------------------

    /// One `EVENT …` line off the push lane.
    fn on_event(&mut self, line: &str) {
        let mut toks = line.split_whitespace();
        match toks.next() {
            Some("EVENT") => {}
            // A GAP IS PUBLISHED, NOT DROPPED (§4.2). aterm's push lane emits it
            // when a watcher fell behind and frames were coalesced away; a
            // reader of the fleet digest must be able to see that its picture of
            // this node has a hole in it, and the alternative — swallowing it —
            // is the bug `fleet_cli.rs:361` is on record for.
            Some("GAP") => {
                let rest = line.strip_prefix("GAP").unwrap_or("").trim();
                self.publish_ev(&format!("gap {rest}"));
                // AND IT IS ACTED ON, not only published. A `GAP` is the one
                // signal that says recovery is needed: frames were coalesced
                // away, so a `session-created`, a `hold` or an `inbox-seen` line
                // may simply not exist any more. Publishing it and doing nothing
                // left the repair entirely to the periodic backstop — which is
                // reached on a schedule, while a session created inside the gap
                // is ungoverned until then: no presence row, no refill, and no
                // hold under a standing fleet halt.
                self.roster_backstop();
                self.roster_due = Instant::now() + ROSTER_REFRESH;
                return;
            }
            _ => return,
        }
        let Some(target) = toks.next() else { return };
        let Some(kind) = toks.next() else { return };
        match kind {
            "post" => self.drain_outbox(),
            "inbox-seen" => {
                // `EVENT <local> inbox-seen <id> off=<n>` — the digest names the
                // LOCAL id; `seen_off` is keyed by sid, so the map is the join.
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                let off =
                    toks.find_map(|t| t.strip_prefix("off=").and_then(|n| n.parse::<u64>().ok()));
                if let Some(off) = off {
                    let _ = self.state.set_seen_off(&sid, off);
                }
            }
            "session-created" => {
                let _ = self.refresh_sessions();
                // A session with a REMEMBERED watermark is a relaunched
                // instance's session, and its ring needs refilling before the
                // first `inbox` (§6.2) — through the one admission path, so
                // this arm and the roster tick cannot disagree about what a
                // newly seen session is owed.
                self.admit_fresh_sessions();
                // A halt already in force must reach a session that appeared
                // after it.
                self.reassert_halt();
            }
            // `EVENT <local> hold <0|1> reason=<pct> origin=<>` (§11.2). The
            // endpoint says its hold moved; if that disagrees with the standing
            // fleet halt this bridge has verified, correct it NOW rather than at
            // the next roster tick. This is the prompt half of
            // [`Bridge::converge_hold`] — the tick is the backstop for an event
            // that never arrived.
            "hold" => {
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                let held = toks.next() == Some("1");
                self.converge_hold(&sid, held);
            }
            "session-exited" => {
                let sid = toks.next().unwrap_or(target).to_string();
                self.publish_session_presence(&sid, "exited");
                self.epochs.remove(&sid);
                self.holders.remove(&sid);
                let _ = self.refresh_sessions();
            }
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // the loop
    // -----------------------------------------------------------------------

    /// Connect to the broker, bring presence up, open every subscription, and
    /// start the reader threads. Answers `false` when the broker is unreachable —
    /// which is a normal state, not an error: the holds stay, the posts stay
    /// queued, and the next round tries again.
    fn attach_broker(&mut self) -> bool {
        self.close_subscriptions();
        // Forget the old connection's queued inputs and closure notices before
        // opening a new one: see `Mailbox::reset_broker_sources`.
        self.mailbox.reset_broker_sources();
        // The PUBLISHER connection first, before the drain connection: a
        // post-restart herd can be refused by `MAX_CONNS`, and it is the
        // publisher's `live inc+1` that suppresses a fenced will (§7).
        let Ok((conn, _pub_closer)) = self.connect() else {
            return false;
        };
        self.conn = Some(conn);
        if let Err(e) = self.bring_presence_up() {
            eprintln!("aterm-link: presence could not come up: {e}");
            self.conn = None;
            return false;
        }
        // BEFORE ANY RECORD IS TAKEN. The endpoint may be holding `fabric-lost`
        // from the last incarnation's death, and until that is reconciled with
        // the fleet's standing halt no session on this instance can be driven at
        // all — including by the replay of a feed the crash interrupted.
        if !self.reconcile_halt() {
            eprintln!("aterm-link: the standing fleet halt could not be read; not attaching");
            self.conn = None;
            return false;
        }
        let sids: Vec<String> = self.locals.values().cloned().collect();
        for sid in sids {
            self.publish_session_presence(&sid, "live");
        }
        // AND THE REFILL THOSE SESSIONS ARE OWED. Presence for every local
        // session has just been published, so what is left of the admission is
        // §6.2's refill — and this is the first moment in the process's life
        // there is a broker to run one against. Before the live drain starts,
        // so the rows a previous incarnation delivered and the endpoint lost
        // are re-offered in offset order ahead of anything new.
        self.pending_admit
            .retain(|sid| self.epochs.contains_key(sid));
        for sid in std::mem::take(&mut self.pending_admit) {
            self.refill(&sid);
        }
        let fleet_filter = subject::fleet_filter(&self.cfg.fleet);
        let term_filter = subject::term_filter(&self.cfg.fleet, &self.node);
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        let inbox_filter = subject::inbox_filter(&self.cfg.fleet, &self.node);
        // THE DRIVE FACE RESUMES AT ITS DURABLE CURSOR, and the fleet face from
        // zero.
        //
        // The asymmetry is still the point, and it has moved by one step. A halt
        // is idempotent state — replaying every halt record on a reconnect
        // reaches the same standing verdict. A keystroke is not idempotent BY
        // ITSELF, which is why A3 resumed at the head; but A6 gave every `term/in`
        // a key (`{epoch}:{producer}:{off+1}`) that the endpoint's monotone mark
        // answers `dup` to without writing, and the producer id survives a bridge
        // restart while the epoch does not survive an aterm one. So a replay can
        // no longer retype anything, and the head-resume's own cost — a `term/in`
        // published while the bridge was between incarnations, never seen, never
        // fed, never refused, named by no `ev` — is a silent loss with nothing
        // left to justify it. See `state.rs`'s `term_off`.
        //
        // BOUNDED, AND WHAT IS SKIPPED IS SAID. A cursor further behind than
        // [`TERM_RESUME_SPAN`] resumes at the floor instead and publishes the
        // span it passed over, because a bridge that was down for a day must not
        // walk the whole log — and must not pretend it walked it either.
        // `max=0` is the broker's head query (R2).
        let head = match self.conn.as_mut() {
            Some(conn) => match conn.fetch(0, &term_filter, 0) {
                Ok((_, (_, head))) => head,
                Err(_) => return false,
            },
            None => return false,
        };
        let floor = head.saturating_sub(TERM_RESUME_SPAN);
        let term_from = match self.state.term_off() {
            Some(cursor) => {
                let resume = cursor.saturating_add(1);
                if resume < floor {
                    self.publish_ev(&format!("term-skipped from={resume} to={}", floor - 1));
                    floor
                } else {
                    resume
                }
            }
            // No cursor at all: a first launch, or a state dir that lost the
            // file. The head is A3's behaviour and the conservative direction —
            // it can only skip records, never re-offer one twice.
            None => head,
        };
        for (filter, source, from) in [
            (fleet_filter, Source::Fleet, 0),
            (term_filter, Source::Term, term_from),
        ] {
            let Ok((client, closer)) = self.connect() else {
                return false;
            };
            let Ok(sub) = client.subscribe(from, &filter) else {
                return false;
            };
            self.closers.push(closer);
            spawn_reader(sub, source, self.mailbox.clone());
        }
        let Ok((client, closer)) = self.connect() else {
            return false;
        };
        let Ok(sub) = client.subscribe_group(&group, &inbox_filter) else {
            return false;
        };
        self.closers.push(closer);
        spawn_reader(sub, Source::Inbox, self.mailbox.clone());
        // THE KEYBOARD SURVIVES THE BRIDGE. The `control` row is last-value bus
        // state written only by this node, so it is read back rather than
        // reinvented: a restart that forgot it would leave a human's claim
        // standing on the bus while every `term/in` under it was refused
        // `reason=holder`.
        self.restore_control_rows();
        // AND THE FEED THE LAST INCARNATION DIED INSIDE. It needs the publisher
        // connection (to re-read the record and to publish the verdict) and the
        // restored holders are irrelevant to it, so it runs last — and it runs
        // before the loop takes another `term` record, because the term
        // subscription's reader has only just been spawned and the mailbox is
        // drained on this thread.
        self.resolve_pending_feed();
        true
    }

    /// Close every live subscription. Called before a reconnect and on the way
    /// out, so a reader thread never outlives the bridge state it feeds.
    fn close_subscriptions(&mut self) {
        for closer in self.closers.drain(..) {
            closer.close();
        }
    }

    /// Run until aterm goes away.
    ///
    /// # Errors
    ///
    /// Only a failure to set the push lane up: everything after that is handled
    /// in the loop, because a bridge that exits on a broker hiccup is a bridge
    /// that lifts the fleet halt by dying.
    pub fn run(mut self) -> io::Result<()> {
        // The push lane: `subscribe @* events,sessions` on the SECOND inherited
        // descriptor. It is a separate connection because a `subscribe` holds its
        // stream forever, and the verb connection must stay answerable.
        if self.attachment == Attachment::Inherited {
            let push = Ctl::adopt(aterm_uds::spawnfd::BRIDGE_PUSH_FD)?;
            spawn_event_reader(push, self.mailbox.clone());
        }
        let _ = self.refresh_sessions();
        let mut backoff = RECONNECT_MIN;
        loop {
            if self.conn.is_none() {
                if self.attach_broker() {
                    backoff = RECONNECT_MIN;
                    // Whatever queued while the broker was away goes now.
                    self.drain_outbox();
                } else {
                    self.conn = None;
                    // PARK ON THE MAILBOX for the back-off rather than sleeping
                    // through it. Exactly one thing can still arrive while the
                    // broker is unreachable and it is the one that matters:
                    // aterm going away. A bridge that slept through that would
                    // spin forever as an orphan, holding nothing and reachable
                    // by nobody. Any other item is from the connection that just
                    // died and is dropped with it.
                    if let Some(Item::Closed(Source::Aterm)) = self.mailbox.take(backoff) {
                        eprintln!("aterm-link: aterm closed the bridge connection; exiting");
                        return Ok(());
                    }
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                    continue;
                }
            }
            // THE MIRROR LEASE IS RENEWED FROM THE LOOP, not from the idle
            // branch: a busy bridge never idles, and a lease that lapsed under
            // load is exactly the state §6.6 says must not exist while the row
            // stands. It is a bounded local call on a deadline, so a quiet
            // bridge pays it three times a minute per held session and a busy
            // one pays it no more often.
            if Instant::now() >= self.lease_due {
                self.renew_leases();
                self.lease_due = Instant::now() + LEASE_RENEW;
            }
            // AND THE LOCAL OBSERVATIONS, FOR THE SAME REASON — the reason this
            // loop has now made four times. Two of §6.6's six rows and the whole
            // `attention=` escalation are discovered by LOOKING, not by anything
            // arriving, so a bridge that only looks when nothing is arriving does
            // not implement them on a busy node. See [`ROSTER_REFRESH`]. It runs
            // BEFORE the feed retry, because it is where a stale `fabric-lost`
            // hold gets corrected and retrying into a hold this round could have
            // lifted would burn the budget on a refusal of our own making.
            if Instant::now() >= self.roster_due {
                self.roster_backstop();
                self.roster_due = Instant::now() + ROSTER_REFRESH;
            }
            // AND ROW 4 ON ITS OWN, FASTER DEADLINE. The roster's period is fine
            // for everything that is still true when it is read late; the
            // conservative pause is not one of those things. See
            // [`LOCAL_OBSERVE`].
            if Instant::now() >= self.observe_due {
                self.watch_held_control();
                self.observe_due = Instant::now() + LOCAL_OBSERVE;
            }
            // AND THE UNFINISHED FEED, FROM THE LOOP. See [`FEED_RETRY`]: the
            // idle arm alone is unreachable on a node that is receiving
            // records, which is exactly the node whose keystrokes are being
            // refused `ERR busy`.
            if Instant::now() >= self.feed_retry_due {
                self.resolve_pending_feed();
                self.feed_retry_due = Instant::now() + FEED_RETRY;
            }
            // FROM THE LOOP, not from the idle branch: a busy bridge never
            // idles, and a screen face that only advanced while nothing was
            // happening would be blank at exactly the moments a reader wants it.
            // Its own clock bounds the rate.
            self.publish_screens();
            match self.mailbox.take(IDLE_TICK) {
                Some(Item::Fleet(r)) => self.on_fleet_record(&r),
                Some(Item::Term(r)) => {
                    let off = r.0;
                    self.on_term_record(&r);
                    // AFTER, NEVER BEFORE. A crash between the handling and this
                    // write replays the record, which A6's key answers `dup` to;
                    // a crash the other way round would lose it exactly the way
                    // the head-resume did.
                    if let Err(e) = self.state.set_term_off(off) {
                        eprintln!("aterm-link: could not record the drive cursor: {e}");
                    }
                }
                Some(Item::Event(line)) => self.on_event(&line),
                Some(Item::Inbox(r)) => self.on_inbox_record(&r),
                Some(Item::Closed(Source::Aterm)) => {
                    eprintln!("aterm-link: aterm closed the bridge connection; exiting");
                    return Ok(());
                }
                Some(Item::Closed(_)) => {
                    // A broker reader ended: the connection is gone. Drop the
                    // publisher too and reconnect the whole set, because a
                    // half-attached bridge is the state nothing is defined for.
                    eprintln!("aterm-link: a broker subscription ended; reconnecting");
                    self.conn = None;
                }
                None => {
                    // THE IDLE TICK, and it is now only ONE duty. Everything
                    // periodic moved onto its own deadline in the loop body,
                    // because "nothing is discovered by waking up" was never true
                    // of the things that are discovered by LOOKING — a screen
                    // revision advancing, a local `lease acquire`, an
                    // `attention=` a session set with `meta set`, a
                    // `session-created` line the push lane coalesced away. None
                    // of those wakes the mailbox, and a bridge under load never
                    // reaches this arm at all.
                    //
                    // What is left here is the one duty that is genuinely
                    // "there is a moment to spare": draining the outbox, which
                    // also has its prompt trigger on the push lane
                    // (`EVENT <sid> post`) and its own backstop in
                    // [`Bridge::roster_backstop`].
                    self.drain_outbox();
                }
            }
        }
    }
}

/// Whether a record on this node's own lane at `off` is a FORGERY, or one of
/// ours the bounded window can no longer vouch for.
///
/// `self_acked` remembers only the newest [`SELF_ACK_KEEP`] offsets, and two
/// local sessions messaging each other route through the bus under this node's
/// own `<src>`, so self-lane records are the ORDINARY intra-instance case. An
/// offset below the oldest one remembered is therefore not evidence of anything:
/// it is a question the window has aged out of. Answering "forged" to it
/// publishes `ev cap-compromised`, whose §9.2 remedy is "rotate the node cap" —
/// so the window's edge degrades to "cannot tell" rather than to an accusation.
fn forged_self(acked: &BTreeSet<u64>, off: u64) -> bool {
    if acked.contains(&off) {
        return false;
    }
    !matches!(acked.first(), Some(oldest) if off < *oldest)
}

/// Tell the loop aterm is gone, IF IT IS — the whole of [`Bridge::ctl_request`]'s
/// discipline, as a function, so it can be pinned by a test that needs no
/// inherited descriptors.
///
/// The notice goes through the MAILBOX rather than a flag, because the mailbox
/// is where the loop already learns this from the push lane and `Closed`
/// outranks every other item there: whatever this round was in the middle of,
/// the very next `take` returns the closure. A REFUSED LINE IS NOT A LOSS — the
/// bound in [`crate::ctl`] writes nothing and leaves the connection framed, so
/// its error must not end the process.
fn note_ctl_loss(ctl: &Ctl, mailbox: &Mailbox, err: Option<&io::Error>) {
    if err.is_some() && ctl.lost() {
        mailbox.push_aterm_closed();
    }
}

/// Whether one inbound record was ACCOUNTED FOR — the question the durable
/// group cursor turns on.
///
/// Accounted means the record's FATE IS ON THE BUS, by any of three routes: the
/// endpoint answered `OK`; the endpoint answered `ERR` and that produced an `ev`
/// and a sender notice; or this bridge refused the record itself and produced
/// the same pair ([`Bridge::refuse_locally`]). Re-offering any of them would
/// deliver nothing new.
///
/// Unaccounted is the LOST LANE alone — the reply never came and the connection
/// is gone, so nothing anywhere says what happened and the cursor must not move
/// past it. It used to mean "any `Err` from `ctl_request`", which folded in the
/// writer's own over-long-line refusal: a deterministic, permanent, local
/// refusal handled as a transient transport blip, with no `ev`, no verdict, and
/// an absolute group commit from the next record that stepped over it.
/// [`Ctl::lost`] is the question that separates the two, which is the whole
/// reason that flag exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Accounted,
    Unaccounted,
}

/// Read EVERY row of a last-value face, paging on the RESUME CURSOR.
///
/// §5.2 and `Client::last_page`'s own doc state the rule literally: the broker
/// clamps `max` and cuts each index scan after a fixed number of entries
/// VISITED, so a page shorter than `max` — AN EMPTY ONE INCLUDED — is not the
/// end of the answer. Only an empty `resume` is. Four readers in this crate
/// paged on "the page came back empty" instead, which reads a scan bound as
/// evidence of absence — and one of them ([`Bridge::reconcile_halt`]) LIFTS A
/// STANDING FLEET HALT on that evidence. `glance::read` already does it
/// correctly and says why; this is that loop, in one place, for the readers that
/// did not.
///
/// `Err` is returned for a walk that could not be COMPLETED, page bound
/// included, because a caller that cannot tell "no rows" from "I stopped
/// looking" is the caller that lifts the halt.
fn last_all(conn: &mut Conn, filter: &str) -> io::Result<Vec<BrokerRecord>> {
    let mut rows = Vec::new();
    let mut after = String::new();
    for _ in 0..LAST_PAGES_MAX {
        let (page, _, resume) = conn.last_page(filter, &after, 256)?;
        rows.extend(page);
        if resume.is_empty() {
            return Ok(rows);
        }
        after = resume;
    }
    Err(io::Error::other(format!(
        "the last-value walk of {filter} did not finish within {LAST_PAGES_MAX} pages"
    )))
}

/// The one-token `reason=` a refusal header becomes.
///
/// §3.3 spells the field as a TOKEN (`ev undeliverable off=<n> reason=malformed`)
/// and `Reject::token` obeys it — but the endpoint's refusals are sentences:
/// `ERR no such session`, and `DELIVER_USAGE` is a 150-character usage line full
/// of spaces, `<`, `>`, `|` and `[`. Splicing one into a `key=value` line put the
/// endpoint's whole private usage grammar onto an append-forever log at a
/// stranger's choosing, and handed any reader parsing by the design's grammar a
/// `reason=usage:` plus five stray bare tokens. The endpoint enforces the
/// mirror-image rule on the way IN (`valid_dead_reason`: "a bridge is a separate
/// process and its output is still input"); this is the same rule on the way out,
/// in ONE place, for the three sites that each needed it.
///
/// The charset filter is the second half: the first word of a refusal is aterm's
/// today, but a `reason=` is a wire field, and a token that can carry a space or
/// an `=` is a token that can invent a second field.
fn reason_token(header: &str) -> String {
    let mut words = header.split_whitespace();
    let first = words.next().unwrap_or_default();
    // A bare `ERR` names no reason at all, so it must not BECOME one.
    let word = if first == "ERR" {
        words.next().unwrap_or_default()
    } else {
        first
    };
    let cleaned: String = word
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(REASON_TOKEN_MAX)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// pct-encode `text` into at most `budget` bytes, cutting on a CHARACTER
/// boundary. Answers the encoded text and whether anything was cut.
///
/// The cut is per-character rather than per-byte for two reasons that are the
/// same reason: an escape is three bytes and half of one is not a `%XX`, and a
/// multi-byte character is several escapes and half of one is not a character.
/// Cutting the SOURCE and re-encoding makes both impossible by construction
/// rather than by a trailing fix-up.
#[must_use]
pub fn encode_bounded(text: &str, budget: usize) -> (String, bool) {
    let whole = crate::pct::encode(text);
    if whole.len() <= budget {
        return (whole, false);
    }
    let mut out = String::with_capacity(budget);
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let piece = crate::pct::encode(ch.encode_utf8(&mut buf));
        if out.len() + piece.len() > budget {
            break;
        }
        out.push_str(&piece);
    }
    (out, true)
}

/// Where one outbound post is going — or why it is going nowhere (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The six-segment owner+src prefix of the `in` subject to publish under.
    To(String),
    /// No address this fleet can reach: not a principal, not a hosted sid, and
    /// nobody advertises it.
    Unroutable,
    /// TWO NODES CLAIM THE SID. Never a route: a node's cap covers its whole
    /// `pub/<n>/` subtree, so picking one would be picking whichever rogue
    /// published a presence row for a session it does not host, and sending the
    /// message into that rogue's own read lane (§6.1).
    Ambiguous,
}

impl Route {
    /// The `reason=` token this verdict retires a post with. `To` has none — it
    /// is not a verdict — and answers `unroutable` only so the type is total.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Route::Ambiguous => "ambiguous",
            Route::To(_) | Route::Unroutable => "unroutable",
        }
    }
}

/// `<content_seq>:<fp16>` out of one `text --json` frame — §6.6's `gen=`.
///
/// PUBLIC because the test that proves the fence must compute the value the
/// same way the fence does. A test with its own copy of this would pass the day
/// the two drifted apart, which is the day the fence stopped working.
///
/// The rows array is delimited by `{"rows":[` and `],"cursor":`, and neither
/// can occur inside a row: a `"` in a row is JSON-escaped, so a row can never
/// contain the unescaped quote either bracket needs.
#[must_use]
pub fn gen_of_frame(frame: &str) -> Option<String> {
    let seq = frame
        .split(",\"seq\":")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse::<u64>()
        .ok()?;
    let rows = frame
        .strip_prefix("{\"rows\":[")?
        .split_once("],\"cursor\":")?
        .0;
    Some(format!("{seq}:{:016x}", fnv1a_64(rows.as_bytes())))
}

/// FNV-1a over 64 bits — the same HASH aterm's own turn path uses
/// (`crates/aterm-gui/src/turn_ledger.rs`), recomputed here rather than
/// depended on: `aterm-gui` is a binary crate this one does not link, and §11.2
/// pins the dependency set.
///
/// The same function over DIFFERENT BYTES: `turn`'s `hash=` is taken over the
/// plain screen text and this one over a `text --json` rows array, so the two
/// values never agree and §6.6 forbids comparing them. See
/// [`Bridge::live_gen`].
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// What one `feed-bin` attempt's reply MEANS about the PTY (§6.5).
///
/// A pure function of the reply string, kept apart from the side effects so the
/// four outcomes can be pinned by a test that needs no aterm, no broker and no
/// PTY. Getting this wrong is not a cosmetic error: calling an in-doubt an
/// `applied` is a silent loss, and calling it a `refused` invites the retry the
/// design forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    /// It ran. `dup` means A6's mark recognized an already-consumed sequence and
    /// wrote NOTHING — the answer a replay of a landed keystroke gets.
    Applied { dup: bool },
    /// It may have typed, and nothing here can tell. Reported and stopped:
    /// §6.5's "in-doubt, reported, never replayed".
    InDoubt {
        /// The sequence the endpoint named, or `-`.
        seq: String,
        /// Why, when the doubt is the bridge's own rather than the endpoint's.
        why: Option<&'static str>,
    },
    /// Refused before anything could reach the PTY.
    Refused { why: String },
}

impl Verdict {
    /// A journalled feed whose record cannot be read back off the bus, so its
    /// outcome can never be resolved. Reported once and retired — an entry kept
    /// for a record that will not come back would replay on every attach.
    const UNREADABLE: Verdict = Verdict::InDoubt {
        seq: String::new(),
        why: Some("unreadable"),
    };

    /// Whether this verdict SETTLES the record — nothing further can be learned
    /// by asking again.
    ///
    /// The three that do: it ran (`applied`, `dup=1` included), the endpoint
    /// itself declared the outcome unknowable (`ERR in-doubt` — it has kept the
    /// mark and will answer the same thing forever), and a refusal that can never
    /// become an acceptance because the record no longer addresses anything real.
    ///
    /// Everything else is TRANSIENT: `ERR halted` (a hold something will lift),
    /// `ERR busy turn=`/`lease=` (a lease something will drop), `ERR rate` (a
    /// floor that recovers), `ERR busy idem=` (this key's own attempt is still
    /// inside the seam) and an aterm that did not answer at all. Retiring the
    /// journal on any of them would be the silent loss this rung exists to end,
    /// and asking again is safe because the key makes it safe; asking again
    /// FOREVER is not, which is what [`FEED_BUDGET`] bounds — in seconds, not in
    /// scheduler rounds.
    ///
    /// THEY ARE NOT ALL THE SAME KIND OF UNANSWERED, and a spent budget has to
    /// tell them apart. The three REFUSALS were decided before a byte could
    /// move, so the key's sequence was not consumed and the question is simply
    /// unasked. The other two are not refusals at all: a silence says nothing
    /// about the PTY, and `ERR busy idem=` says an attempt under this very key
    /// is running right now — so [`feed_verdict`] reads both as `InDoubt` and
    /// `FeedIntent::doubt` keeps that sticky, which is what stops an exhausted
    /// budget from publishing `refused` over a keystroke that landed.
    fn is_final(&self) -> bool {
        match self {
            Verdict::Applied { .. } => true,
            Verdict::InDoubt { why, .. } => matches!(*why, None | Some("unreadable")),
            // A refusal the record can never outgrow. `epoch` means the session
            // relaunched — a different incarnation, and §6.6 says a key minted
            // against a dead one must never land on its successor. `usage` and
            // `no` (`ERR no such session`) mean this bridge and this endpoint
            // disagree about what was asked, which no retry repairs.
            Verdict::Refused { why } => matches!(why.as_str(), "epoch" | "usage" | "no"),
        }
    }

    /// The `ev` payload this verdict is published as.
    ///
    /// `applied` carries §10's two fields: the offset it applied (`re=`) and the
    /// `content_seq` baseline the screen that followed is measured from
    /// (`seq=`). `replay=1` marks a verdict that came from the journal rather
    /// than from a live record, so a reader can tell a repair from a first pass.
    fn ev(&self, sid: &str, off: u64, baseline: &str, replayed: bool) -> String {
        let tail = if replayed { " replay=1" } else { "" };
        match self {
            Verdict::Applied { dup } => {
                let dup = if *dup { " dup=1" } else { "" };
                format!("applied sid={sid} face=term re={off} seq={baseline}{dup}{tail}")
            }
            Verdict::InDoubt { seq, why } => {
                let seq = if seq.is_empty() { "-" } else { seq.as_str() };
                let why = why.map(|w| format!(" reason={w}")).unwrap_or_default();
                format!("in-doubt sid={sid} face=term re={off} seq={seq}{why}{tail}")
            }
            // `re=` LIKE THE OTHER TWO. A feed verdict names the offset it is
            // about: the record's own body carries `re=` (see
            // [`Bridge::publish_ev_re`]), but the payload is what an `ev` reader
            // prints, and this was the one of the three verdicts a reader could
            // not correlate to a keystroke. It matters more now that a budget
            // spent entirely on clean pre-write refusals is published as
            // `refused` rather than as an `in-doubt` that always carried it.
            //
            // The `refused` rows the DRIVE GATE publishes (`reason=holder|hold|
            // epoch|gen`, before anything is journalled) are a different call
            // path — [`Bridge::publish_ev_for`] — and are unchanged: there is no
            // journal entry behind them to correlate.
            Verdict::Refused { why } => {
                format!("refused sid={sid} face=term re={off} reason={why}{tail}")
            }
        }
    }
}

/// Read one `feed-bin` reply.
///
/// `None` is an I/O failure against aterm itself — the socket died, or the reply
/// never came. That says NOTHING about the PTY, which is the in-doubt shape seen
/// from the other side of the socket, so it is one.
fn feed_verdict(reply: Option<&str>) -> Verdict {
    let Some(reply) = reply else {
        return Verdict::InDoubt {
            seq: String::new(),
            why: Some("no-reply"),
        };
    };
    if reply.starts_with("OK") {
        return Verdict::Applied {
            dup: reply.split_whitespace().any(|t| t == "dup=1"),
        };
    }
    if reply.starts_with("ERR in-doubt") {
        return Verdict::InDoubt {
            seq: reply
                .split_whitespace()
                .find_map(|t| t.strip_prefix("seq="))
                .unwrap_or_default()
                .to_string(),
            why: None,
        };
    }
    // `ERR busy idem=<seq>` IS NOT A PRE-WRITE REFUSAL, and it is the one
    // `ERR busy` that is not. The other two are decided before the seam — the
    // turn lease (`turn=`) and the mirrored drive lease (`lease=`) — but this
    // one is `pty_idem`'s own answer for a key AT the mark with a RUNNING tip:
    // an attempt under this very key is inside the seam right now and its bytes
    // may already be on the PTY. The bridge cannot reach that state by itself
    // (`ctl_request` is serial), it reaches it by DYING inside the feed window
    // and replaying the journalled key into an endpoint still writing it —
    // which is the case this whole rung exists for.
    //
    // Read as a clean refusal it would be worse than useless: `reason_token`
    // keeps only the first word, so it would arrive as `Refused{"busy"}`,
    // indistinguishable from the lease refusals, and a budget spent entirely on
    // it would publish `refused` — a definite non-delivery — for a keystroke
    // that landed. A human reading that re-types, the re-type mints a NEW key,
    // and A6's mark cannot dedupe it: the silent duplicate `pty_idem` exists to
    // remove. So it is DOUBT, and doubt is sticky in the journal.
    //
    // Nothing else changes: `InDoubt` with a `why` is non-final, so the retry
    // loop is untouched. The `idem=` clears as soon as the attempt behind it
    // settles, and the next replay reads `OK dup=1` and is `applied`.
    //
    // THE SEQUENCE IT NAMES IS NOT COMPARED against this attempt's own, though
    // it could be: `pty_idem` also answers this while refusing a HIGHER key
    // because an older one is still running, and that one is a clean refusal of
    // the key asked about. Reading both as doubt can only over-report doubt,
    // which §6.5 makes a reader's problem rather than a keystroke's, and it
    // keeps this a pure function of the reply — the same conservative direction
    // `FeedIntent::parse` takes for a `doubt=` token it cannot find.
    if let Some(seq) = reply.strip_prefix("ERR busy ").and_then(|rest| {
        rest.split_whitespace()
            .find_map(|t| t.strip_prefix("idem="))
    }) {
        return Verdict::InDoubt {
            seq: seq.to_string(),
            why: Some("in-flight"),
        };
    }
    // Every other refusal. Only the FIRST word of the reason travels: a `turn`
    // reply is a whole screen and an `ev` never carries content (§11.2).
    Verdict::Refused {
        why: reason_token(reply),
    }
}

/// The floor on the gap between `screen` snapshots — §3.3's "≤ 4/s", as a
/// period rather than a rate so the bound holds however often the loop runs.
const SCREEN_PERIOD: Duration = Duration::from_millis(250);

/// The largest `text --json` frame published on the `screen` face. A frame over
/// it is skipped with an `ev` rather than truncated: half a frame is a document
/// that parses and lies.
const SCREEN_MAX: usize = 256 * 1024;

/// The BACKSTOP on how many times one `term/in` is ever fed. The budget itself
/// is [`FEED_BUDGET`].
///
/// A retry under A6's key cannot duplicate, so the risk either bound guards is
/// not a double keystroke — it is a bridge that asks forever and reports
/// nothing. The wall clock answers that for every ordinary case; this answers
/// the one case a wall clock cannot, and it is the reason the count survives at
/// all: a bridge that dies INSIDE the feed and is relaunched into the same
/// journal entry can burn attempts without time passing, and a clock that
/// stepped backwards would leave `FEED_BUDGET` unreachable. At [`FEED_RETRY`]'s
/// cadence a 45 s budget is about 180 attempts, so this is set well above it —
/// it is a runaway stop, not a schedule.
const FEED_TRIES_MAX: u32 = 512;

/// The default a halt with no `reason=` is held under. It still names the
/// origin, which is the one thing a bare `state=on` record does say.
const HALT_REASON_DEFAULT: &str = "fleet-halt";

/// How many bytes of a halt's `reason=` reach the endpoint. The endpoint clamps
/// at the same 128 (`fabric.rs` `REASON_MAX`); clamping HERE too keeps the verb
/// line the bridge writes bounded by this crate's own rules rather than by a
/// number in another crate that could move.
const HALT_REASON_MAX: usize = 128;

/// One halt record's `reason=`, bounded and sanitized for the `hold` verb line.
///
/// The value arrives pct-encoded — a body token is delimited by spaces, so it can
/// hold none — and is passed on in that form, because `hold … reason=<pct>` is
/// what the endpoint's grammar names. What this adds is the receiver-side floor:
/// only ASCII graphic bytes survive, the result is clamped, and a clamp that
/// would split a `%XX` escape trims back past it. A raw control byte or a bidi
/// override in a halt reason would otherwise become every reader's problem —
/// the endpoint sanitizes too, and that is deliberate belt-and-braces at a seam
/// where the input is a stranger's.
fn halt_reason_token(raw: Option<&str>) -> String {
    let mut out = String::with_capacity(HALT_REASON_MAX);
    for byte in raw.unwrap_or_default().bytes() {
        if out.len() == HALT_REASON_MAX {
            break;
        }
        if byte.is_ascii_graphic() {
            out.push(byte as char);
        }
    }
    while out.ends_with('%') || (out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%') {
        out.pop();
    }
    if out.is_empty() {
        return HALT_REASON_DEFAULT.to_string();
    }
    out
}

/// The `inc=` on a presence body, if it has one.
fn inc_of(body: &[u8]) -> Option<u64> {
    let (b, _) = Body::decode(body);
    b.unknown.get("inc").and_then(|s| s.parse().ok())
}

/// The prefix §6.6 row 5 mirrors a LOCAL socket driver's cooperative lease
/// under, and the one place this file spells it.
///
/// It is minted by `handoff::decide_control`'s row-5 arm
/// (`format!("owner-cli:{holder}")`) and read back here by [`is_mirrored`]:
/// `bridge::tests::the_mirror_prefix_is_the_one_decide_control_mints` fails if
/// the two ever drift, because a holder this side stopped recognising would be
/// a holder [`Bridge::acquire_lease`] starts taking a `fabric:` lease for.
const MIRRORED_PREFIX: &str = "owner-cli:";

/// Whether a `control` holder is §6.6 row 5's MIRROR of a lease a local socket
/// driver already holds, rather than a fabric principal this bridge holds
/// anything for.
///
/// A mirror is an OBSERVATION the bridge published, never a hold it took. Every
/// path that would act on a holder as though the bridge owned it — taking
/// aterm's cooperative lease, releasing it, and §6.6 row 4's conservative pause
/// — asks this question first.
fn is_mirrored(holder: &str) -> bool {
    holder.starts_with(MIRRORED_PREFIX)
}

/// TRUST is a pure function of `(sender class, relay)` — never read from a body
/// (§4.3). There is no `attested` token and no "downgrade only" rule to police,
/// because no sender ever writes the label.
fn trust_of(src: &str, relayed: bool) -> &'static str {
    if relayed {
        return "relayed";
    }
    if src.starts_with("h-") {
        "human"
    } else {
        "agent"
    }
}

/// This host's name, for the presence row's `host=` — §7's second column of the
/// cross-host `ls`, and the field `main.rs`'s header calls "the real answer"
/// a session row's `-` points at.
///
/// `$HOSTNAME` ALONE WAS NEVER AN ANSWER ON THE SHIPPED PLATFORM. It is a bash
/// SHELL variable and is not exported; zsh — this repo's and macOS's default —
/// sets `HOST` instead and exports neither, and the bridge child's environment
/// is `env_clear()` plus a filtered copy of aterm-gui's (`fabric_launch.rs`),
/// so nothing puts it back. Every node's row read `host=-`, on both hosts of
/// the two-host fleet the column exists for.
///
/// So it is asked of the OS, in the order an operator would expect: an explicit
/// `$HOSTNAME` override first (that is what an operator setting it means),
/// then `/etc/hostname` where the platform keeps one, then `uname -n`, which is
/// the POSIX answer and the one macOS has. `-` survives as the last resort and
/// is honest: the field's readers (`glance`, `aterm-link ls`) render a missing
/// token as "unknown" already.
///
/// `gethostname(2)` would be one call rather than three attempts, and it is not
/// used: §11.2 confines this crate's raw-descriptor and `libc` surface to
/// `aterm-uds`, and `ctl.rs`'s adoption is the ONE `unsafe` block the crate
/// ships (`this_modules_doc_and_unsafe_surface_match_what_it_ships` fails on a
/// second). A `uname` a bridge runs at most once is the cheaper promise to
/// keep.
///
/// ONCE PER PROCESS. The node presence row is republished on every reconnect
/// and the answer cannot change under a running kernel's process, so the work
/// — including the one spawn — happens on the first row and never again.
fn hostname() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        if let Some(h) = std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()) {
            return h;
        }
        if let Some(h) = std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
        {
            return h;
        }
        for uname in ["/usr/bin/uname", "/bin/uname"] {
            let out = std::process::Command::new(uname)
                .arg("-n")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            if let Some(h) = out
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|h| !h.is_empty())
            {
                return h;
            }
        }
        "-".to_string()
    })
    .clone()
}

/// Mint a node id: `n-<16 hex>`, once, from the OS CSPRNG.
fn mint_node_id() -> String {
    let mut b = [0u8; 8];
    aterm_uds::rand::fill(&mut b).expect("the OS CSPRNG");
    let mut s = String::from("n-");
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// One post read out of an `outbox` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPost {
    pub sid: String,
    pub id: u64,
    pub to: String,
    pub kind: String,
    pub re: Option<u64>,
    pub dl: Option<u64>,
    pub via: Option<String>,
    pub body: Vec<u8>,
}

/// Parse an `outbox` frame: `post sid=… id=… to=… kind=… [re=] [dl=] [via=]
/// len=<n>` then exactly `n` body bytes, repeated.
///
/// TOTAL: a truncated or malformed frame yields the posts that parsed and stops.
/// The alternative — refusing the whole frame — would wedge the outbound queue on
/// one bad row forever.
#[must_use]
pub fn parse_outbox(frame: &[u8]) -> Vec<QueuedPost> {
    let mut out = Vec::new();
    let mut rest = frame;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            break;
        };
        let line = String::from_utf8_lossy(&rest[..nl]).into_owned();
        let tail = &rest[nl + 1..];
        let mut post = QueuedPost {
            sid: String::new(),
            id: 0,
            to: String::new(),
            kind: String::new(),
            re: None,
            dl: None,
            via: None,
            body: Vec::new(),
        };
        let mut len: Option<usize> = None;
        for tok in line.split_whitespace() {
            let Some((k, v)) = tok.split_once('=') else {
                continue;
            };
            match k {
                "sid" => post.sid = v.to_string(),
                "id" => post.id = v.parse().unwrap_or(0),
                "to" => post.to = v.to_string(),
                "kind" => post.kind = v.to_string(),
                "re" => post.re = v.parse().ok(),
                "dl" => post.dl = v.parse().ok(),
                "via" => post.via = Some(v.to_string()),
                "len" => len = v.parse().ok(),
                _ => {}
            }
        }
        let Some(len) = len.filter(|n| *n <= tail.len()) else {
            break;
        };
        post.body = tail[..len].to_vec();
        if post.sid.is_empty() || post.id == 0 {
            break;
        }
        out.push(post);
        rest = &tail[len..];
    }
    out
}

/// Pump one broker subscription into the mailbox, STAMPED WITH THE INCARNATION
/// IT BELONGS TO.
///
/// The generation is captured here, after `attach_broker` has already bumped it,
/// and every push carries it. A reader that outlives its own subscription — and
/// one always can, because `close_subscriptions` runs on the loop's thread while
/// the parked `recv` returns on this one — then pushes into a mailbox that
/// discards it, rather than into a fresh connection it would tear down. See
/// [`Mailbox::reset_broker_sources`].
fn spawn_reader<S>(mut sub: astream_broker::Subscription<S>, source: Source, mailbox: Arc<Mailbox>)
where
    S: std::io::Read + std::io::Write + Send + 'static,
{
    let generation = mailbox.broker_generation();
    let _ = std::thread::Builder::new()
        .name(format!("aterm-link-{source:?}"))
        .spawn(move || loop {
            match sub.recv() {
                Ok(Some(rec)) => match source {
                    Source::Fleet => mailbox.push_fleet(rec, generation),
                    Source::Term => mailbox.push_term(rec, generation),
                    Source::Inbox => mailbox.push_inbox(rec, generation),
                    Source::Aterm => {}
                },
                Ok(None) | Err(_) => {
                    mailbox.push_closed(source, generation);
                    return;
                }
            }
        });
}

/// Pump aterm's push lane into the mailbox.
fn spawn_event_reader(push: Ctl, mailbox: Arc<Mailbox>) {
    let _ = std::thread::Builder::new()
        .name("aterm-link-events".to_string())
        .spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            let Ok(stream) = push.get_ref().try_clone() else {
                mailbox.push_aterm_closed();
                return;
            };
            // `subscribe` is PUSH-framed: it never returns a reply, so it is
            // written straight to the socket rather than through `Ctl::request`,
            // which would park waiting for one.
            if push
                .get_ref()
                .write_all(b"subscribe @* events,sessions\n")
                .is_err()
            {
                mailbox.push_aterm_closed();
                return;
            }
            let _ = push.get_ref().flush();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        mailbox.push_aterm_closed();
                        return;
                    }
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        // `EVENT` is the digest; `GAP` is the digest ADMITTING it
                        // dropped frames, which §4.2 says must be published as an
                        // `ev` record rather than dropped ("a `GAP` frame is
                        // published, not dropped"). Both go through the loop.
                        if trimmed.starts_with("EVENT") || trimmed.starts_with("GAP") {
                            mailbox.push_event(trimmed);
                        }
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TRUST IS COMPUTED, and it is computed from the two things a sender cannot
    /// choose: the cap-forced class of its `<src>` segment, and whether the
    /// record went through a relay. Nothing in a body can reach it.
    #[test]
    fn trust_is_a_function_of_the_address_and_nothing_else() {
        assert_eq!(trust_of("h-andrew", false), "human");
        assert_eq!(trust_of("s-abc", false), "agent");
        assert_eq!(trust_of("n-abc", false), "agent");
        assert_eq!(trust_of("a-svc", false), "agent");
        // A RELAY DEMOTES A HUMAN TOO. `via=` is the relayer's word, so a relayed
        // message from a human is `relayed`, never `human`: the human's authority
        // did not travel with it.
        assert_eq!(trust_of("h-andrew", true), "relayed");
    }

    /// **EVERY PER-SID MAP ON `Bridge` IS RECONCILED WITH THE LIVE ROSTER.**
    ///
    /// aterm ships no evidence manifest, so a bound that is not enforced
    /// somewhere is a bound that lasts until the next author. `refresh_sessions`
    /// prunes the maps a departed session leaves behind, and the first version
    /// of that fix covered THREE of the six — `local`, `attention` and
    /// `screen_gen` were declared in the same struct, written on the same
    /// round, keyed by the same sids and reconciled by nothing, so the bridge's
    /// resident footprint grew with every tab ever opened for the life of the
    /// instance.
    ///
    /// This reads the struct rather than a list, so the SEVENTH map is covered
    /// the day it is added — which is the shape of the mistake, not the
    /// instance of it.
    #[test]
    fn every_per_sid_map_is_pruned_to_the_roster() {
        let src = include_str!("bridge.rs");
        let body = src
            .split_once("pub struct Bridge {")
            .expect("the struct is declared here")
            .1
            .split_once("\n}\n")
            .expect("and it ends")
            .0;
        let mut checked = 0;
        for line in body.lines() {
            let line = line.trim();
            // `<name>: BTreeMap<String, …>` — a map keyed by a sid. The two
            // maps keyed by something else (`halts` is per HUMAN, `locals` is
            // aterm's own local id) are named in the exception below.
            let Some((name, _)) = line.split_once(": BTreeMap<String, ") else {
                continue;
            };
            if matches!(name, "halts") {
                continue;
            }
            checked += 1;
            assert!(
                src.contains(&format!("self.{name}.retain(|sid, _| live.contains(sid))")),
                "`{name}` is keyed by a sid and nothing reconciles it with the roster: \
                 add it to `refresh_sessions`'s retain block, or say in its doc why a \
                 departed session's entry must outlive the session"
            );
        }
        assert!(checked >= 6, "the struct's per-sid maps were not found");
        // AND THE QUEUE BESIDE THEM, which is a set rather than a map and would
        // have slipped through the loop above.
        assert!(src.contains("self.pending_admit.retain(|sid| live.contains(sid))"));
    }

    /// **THE CLAIMS THIS CRATE MAKES ABOUT ITSELF, WHERE THEY HAVE NO OTHER
    /// GUARD.**
    ///
    /// aterm has no evidence manifest: its doc comments ARE its claims, and
    /// three of them said more than the code did. A doc test is the only thing
    /// that can fail when a sentence stops being true.
    #[test]
    fn the_docs_that_have_no_other_guard_still_match_the_code() {
        let src = include_str!("bridge.rs");
        // §6.6's `gen=` fingerprint is deliberately NOT aterm's `turn` `hash=`,
        // and the design says the two "must never be compared". Both doc
        // comments used to say the opposite — an invitation to fence a keystroke
        // with a `hash=` a driver already has and have every one refused
        // `reason=gen` for a units mismatch.
        assert!(
            src.contains("IT IS NOT aterm's `turn` `hash=`, AND THE TWO MUST NEVER BE COMPARED."),
            "`live_gen` must say what the design says about `gen=` versus `hash=`"
        );
        // SPLIT, so this test's own prose is not the counterexample — the same
        // care `this_modules_doc_and_unsafe_surface_match_what_it_ships` takes.
        assert!(
            !src.contains(concat!(
                "exactly as aterm's own turn ",
                "path hashes its screen"
            )),
            "the two hashes take the same algorithm over DIFFERENT bytes"
        );
        // The run loop's idle arm claims the outbox has "its own backstop in
        // [`Bridge::roster_backstop`]". There was no such call: the only two
        // triggers were the push-lane `post` event and the idle arm itself, and
        // both are starved by exactly the traffic that makes the idle arm
        // unreachable.
        let backstop = src
            .split_once("fn roster_backstop(&mut self) {")
            .expect("the backstop exists")
            .1
            .split_once("\n    }\n")
            .expect("and it ends")
            .0;
        assert!(
            backstop.contains("self.drain_outbox();"),
            "the idle arm and `cmd_outbox`'s drain budget both name this function as \
             the outbox's backstop; it must actually drain it"
        );
        // And the local sweep's header must not describe the pre-`LOCAL_OBSERVE`
        // shape: it visits EVERY session, and row 5's arm is the one for a
        // session with no holder — the case the old text said cost nothing.
        assert!(
            !src.contains(concat!(
                "both are read here, on the roster tick, ",
                "for the sessions that HAVE a"
            )),
            "row 4 moved to `watch_held_control`, and row 5 needs the sessions with NO holder"
        );
    }

    /// **`host=` HAS A WRITER ON THE PLATFORM THIS SHIPS ON.**
    ///
    /// It was `$HOSTNAME` alone — a bash SHELL variable, unexported, absent
    /// under zsh and absent from the bridge child's `env_clear()`ed
    /// environment — so §7's cross-host `ls` printed `host=-` on every row of
    /// every node, while `main.rs`'s header told the reader the node row
    /// "carries the real answer".
    ///
    /// The assertion is conditional on a `uname` existing, because that is the
    /// honest claim: where the OS can be asked, it is, and `-` is reserved for
    /// where it cannot.
    #[test]
    fn the_node_row_carries_a_real_host_name() {
        let askable = std::path::Path::new("/usr/bin/uname").exists()
            || std::path::Path::new("/bin/uname").exists()
            || std::path::Path::new("/etc/hostname").exists();
        if !askable {
            return;
        }
        let host = hostname();
        assert_ne!(
            host, "-",
            "the host name is readable on this machine, so the presence row must carry it"
        );
        // ONE TOKEN, always: the row is whitespace-delimited and `host=` is
        // pct-encoded at the call site, but a name with a space would still be
        // a bug worth catching here rather than on a fleet reader.
        assert!(
            !host.is_empty() && !host.contains(char::is_whitespace),
            "{host:?}"
        );
    }

    /// **THE MIRROR PREFIX THIS FILE READS IS THE ONE `decide_control` MINTS.**
    ///
    /// [`is_mirrored`] is the guard on three things that must never happen to a
    /// §6.6 row-5 holder: taking aterm's cooperative lease for it, releasing
    /// that lease, and row 4's conservative pause. All three read a PREFIX, and
    /// the string is minted in `handoff.rs` — two files, one literal. If the
    /// table's spelling ever moves, this side stops recognising a mirror and
    /// [`Bridge::renew_leases`] goes straight back to asking aterm for a lease
    /// a local driver already holds, every ten seconds, forever.
    #[test]
    fn the_mirror_prefix_is_the_one_decide_control_mints() {
        let free = handoff::State {
            holder: None,
            holder_live: true,
            halted: false,
        };
        let Decision::Hold {
            holder, evidence, ..
        } = decide_control(
            &free,
            &HandoffEvent::LocalLease {
                holder: "drv-7".to_string(),
            },
        )
        else {
            panic!("row 5 mirrors a local lease as a Hold");
        };
        assert_eq!(evidence, "lease");
        assert!(
            is_mirrored(&holder),
            "the mirror `{holder}` must be recognised by this file's own predicate"
        );
        // AND NOTHING ELSE IS A MIRROR. A fabric principal and the pause are
        // the two holders that must keep their existing treatment.
        assert!(!is_mirrored("h-andrew"));
        assert!(!is_mirrored("s-abcdef0123456789"));
        assert!(!is_mirrored(handoff::PAUSED));
        // The claim row 4's skip rests on: no cap-forced `<src>` can ever equal
        // a mirrored holder, so a mirrored session applies nothing off the bus
        // — the same reason the pause is safe.
        assert!(!subject::is_principal(&holder));
    }

    /// The `outbox` frame parses back, bodies with newlines included, and a
    /// truncated frame yields what parsed rather than nothing.
    #[test]
    fn an_outbox_frame_parses_including_bodies_with_newlines() {
        let frame = b"post sid=s-a id=1 to=@s-b kind=ask re=7 len=8\nline\none\
                      post sid=s-a id=2 to=h-x kind=note len=2\nhi";
        let posts = parse_outbox(frame);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].body, b"line\none");
        assert_eq!(posts[0].re, Some(7));
        assert_eq!(posts[1].to, "h-x");
        assert_eq!(posts[1].body, b"hi");
        // A LYING `len=` stops the parse rather than reading past the frame.
        assert!(parse_outbox(b"post sid=s-a id=1 to=x kind=note len=99\nhi").is_empty());
        assert!(parse_outbox(b"garbage").is_empty());
        assert!(parse_outbox(b"").is_empty());
    }

    /// A cap file's line splits at its LAST whitespace, so a grant holding a
    /// space reads back as the grant that was minted rather than taking the whole
    /// keyring down with it.
    #[test]
    fn a_cap_line_splits_at_the_last_whitespace() {
        let dir = std::env::temp_dir().join(format!("atlink-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("caps");
        std::fs::write(&path, "ro:/f/F/pub a/> 0a0b\nrw,p=n-1:/f/F/in/> ffee\n").expect("write");
        let caps = read_cap_file(path.to_str().expect("utf8")).expect("read");
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].grant, "ro:/f/F/pub a/>");
        assert_eq!(caps[0].tag, vec![0x0a, 0x0b]);
        assert_eq!(caps[1].grant, "rw,p=n-1:/f/F/in/>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minted node id is `n-` plus sixteen lowercase hex digits — a principal
    /// by §3.2's grammar, so it can be a subject segment and a cap principal
    /// without any further escaping.
    /// THE FOUR ANSWERS OF §6.5, READ CORRECTLY. Every one of these mappings is
    /// a safety decision: an in-doubt read as an `applied` is a keystroke
    /// silently lost, an in-doubt read as a `refused` invites the retry the
    /// design forbids, and a `dup=1` read as a fresh apply would tell the
    /// conservative pause that this bridge caused a screen change it did not.
    #[test]
    fn a_feed_reply_is_read_as_exactly_one_of_the_four_answers() {
        assert_eq!(
            feed_verdict(Some("OK 7 bytes")),
            Verdict::Applied { dup: false }
        );
        assert_eq!(
            feed_verdict(Some("OK dup=1")),
            Verdict::Applied { dup: true }
        );
        assert_eq!(
            feed_verdict(Some("ERR in-doubt seq=41")),
            Verdict::InDoubt {
                seq: "41".to_string(),
                why: None
            }
        );
        // aterm did not answer: nothing is known about the PTY, so it is doubt.
        assert_eq!(
            feed_verdict(None),
            Verdict::InDoubt {
                seq: String::new(),
                why: Some("no-reply")
            }
        );
        // `ERR busy turn=` and `ERR epoch` are ordinary refusals — decided
        // before the seam, so the endpoint never claimed the sequence. `ERR
        // busy idem=` is NOT one of them, and this test used to say it was:
        // see `a_busy_that_names_the_mark_is_doubt_and_not_a_refusal`.
        assert_eq!(
            feed_verdict(Some("ERR busy turn=3")),
            Verdict::Refused {
                why: "busy".to_string()
            }
        );
        assert_eq!(
            feed_verdict(Some("ERR epoch")),
            Verdict::Refused {
                why: "epoch".to_string()
            }
        );
        // ONLY THE FIRST WORD travels. A `turn` reply can be a whole screen and
        // an `ev` never carries content.
        assert_eq!(
            feed_verdict(Some("ERR halted reason=main%20broken origin=fleet")),
            Verdict::Refused {
                why: "halted".to_string()
            }
        );
        // A body token spelled `dup=1` inside something else is not a dup: the
        // reply is split on whitespace, not searched for a substring.
        assert_eq!(
            feed_verdict(Some("OK 4 bytes nodup=1")),
            Verdict::Applied { dup: false }
        );
    }

    /// The `ev` each verdict is published as — §10's `applied re=<M> seq=<n>`
    /// included, with `re=` also carried as a BODY field by the publisher so a
    /// reader rebuilds the edge structurally rather than by string-searching a
    /// pct-encoded payload.
    #[test]
    fn a_verdict_renders_the_ev_row_section_ten_asks_for() {
        let sid = "s-abc";
        assert_eq!(
            Verdict::Applied { dup: false }.ev(sid, 900, "42", false),
            "applied sid=s-abc face=term re=900 seq=42"
        );
        assert_eq!(
            Verdict::Applied { dup: true }.ev(sid, 900, "42", true),
            "applied sid=s-abc face=term re=900 seq=42 dup=1 replay=1"
        );
        assert_eq!(
            Verdict::InDoubt {
                seq: "901".to_string(),
                why: None
            }
            .ev(sid, 900, "-", true),
            "in-doubt sid=s-abc face=term re=900 seq=901 replay=1"
        );
        assert_eq!(
            Verdict::UNREADABLE.ev(sid, 900, "-", true),
            "in-doubt sid=s-abc face=term re=900 seq=- reason=unreadable replay=1"
        );
        assert_eq!(
            Verdict::Refused {
                why: "holder".to_string()
            }
            .ev(sid, 900, "-", false),
            // `re=` LIKE THE OTHER TWO: a feed verdict names the keystroke it
            // is about, in the payload an `ev` reader prints.
            "refused sid=s-abc face=term re=900 reason=holder"
        );
    }

    /// **AN `ERR busy` THAT NAMES THE MARK IS DOUBT, NOT A REFUSAL — and it is
    /// the only `ERR busy` that is.**
    ///
    /// `ERR busy turn=` and `ERR busy lease=` are decided before the seam, so a
    /// budget spent entirely on them is published as `refused`: recoverable, and
    /// true. `ERR busy idem=<seq>` is `pty_idem`'s answer for a key AT the mark
    /// with a RUNNING tip — an attempt under this very key is inside the seam
    /// and its bytes may already be on the PTY. `reason_token` keeps only the
    /// first word, so read as a refusal it is INDISTINGUISHABLE from the other
    /// two, and an exhausted budget would publish `refused` — a definite
    /// non-delivery — for a keystroke that landed. The re-type that invites
    /// mints a new key, which A6's mark cannot dedupe: the silent duplicate.
    ///
    /// So it is `InDoubt`, which is what sets `FeedIntent::doubt`, which is what
    /// keeps the exhausted verdict `in-doubt … reason=unresolved`. The last
    /// three assertions are that condition, read out of `record_feed_outcome`.
    #[test]
    fn a_busy_that_names_the_mark_is_doubt_and_not_a_refusal() {
        let in_flight = feed_verdict(Some("ERR busy idem=41"));
        assert_eq!(
            in_flight,
            Verdict::InDoubt {
                seq: "41".to_string(),
                why: Some("in-flight")
            }
        );
        // Non-final, so the retry loop is exactly as it was: the `idem=` clears
        // when the attempt behind it settles, and the next replay reads
        // `OK dup=1`.
        assert!(
            !in_flight.is_final(),
            "an attempt still inside the seam is asked about again"
        );
        // The other two `ERR busy` shapes are refusals, and keep their word.
        for reply in ["ERR busy turn=7", "ERR busy lease=agent-a"] {
            assert_eq!(
                feed_verdict(Some(reply)),
                Verdict::Refused {
                    why: "busy".to_string()
                },
                "{reply}"
            );
        }
        // THE CONDITION THE EXHAUSTED VERDICT TURNS ON, as `record_feed_outcome`
        // computes it: a history of lease refusals publishes `refused`, and one
        // `ERR busy idem=` anywhere in it does not.
        let doubt = |v: &Verdict| matches!(v, Verdict::InDoubt { .. });
        assert!(!doubt(&feed_verdict(Some("ERR busy turn=7"))));
        assert!(doubt(&in_flight));
        assert!(doubt(&feed_verdict(None)));
    }

    /// A LOST VERB LANE BECOMES THE LOOP'S EXIT, and a refused line does not.
    ///
    /// `Item::Closed(Source::Aterm)` is the only thing that makes `Bridge::run`
    /// return, and it used to be produced by the PUSH reader alone. A verb lane
    /// that died on its own left the child alive — draining and committing into
    /// a dead socket while `child.wait()` blocked forever and aterm's
    /// fail-closed guard held every session the bridge governed. The two halves
    /// of the distinction are pinned together here because getting either one
    /// wrong is a fleet outage: a loss that produced no notice is the permanent
    /// halt, and a REFUSAL that produced one would exit the bridge over a single
    /// over-long message.
    #[test]
    fn a_lost_verb_lane_reaches_the_loop_and_a_refused_line_does_not() {
        use std::os::unix::net::UnixStream;

        // A refusal: the line is over the bound, nothing is written, the lane is
        // alive. No closure notice may reach the mailbox.
        let (near, far) = UnixStream::pair().expect("socketpair");
        let mut ctl = Ctl::from_stream(near).expect("client");
        let mailbox = Mailbox::default();
        let err = ctl
            .request(&"x".repeat(REQUEST_LINE_MAX + 1))
            .expect_err("over the bound");
        note_ctl_loss(&ctl, &mailbox, Some(&err));
        assert!(
            mailbox.take(Duration::from_millis(0)).is_none(),
            "a refused line must not end the bridge"
        );
        drop(far);

        // A loss: the peer is gone, the failure latches, and the loop is told.
        let (near, far) = UnixStream::pair().expect("socketpair");
        drop(far);
        let mut ctl = Ctl::from_stream(near).expect("client");
        let err = ctl.request("sessions").expect_err("the peer is gone");
        note_ctl_loss(&ctl, &mailbox, Some(&err));
        assert!(
            matches!(
                mailbox.take(Duration::from_millis(0)),
                Some(Item::Closed(Source::Aterm))
            ),
            "a lost verb lane must reach the loop as Closed(Aterm)"
        );
    }

    /// THE DELIVER LINE IS CUT ON A CHARACTER, NEVER INSIDE AN ESCAPE.
    ///
    /// A body over the budget is delivered truncated with `len=` naming its true
    /// size; a cut that split a `%XX` would hand the endpoint's decoder a
    /// malformed escape, and one that split a multi-byte character would hand
    /// the agent half a character. Both are made impossible by cutting the
    /// SOURCE and re-encoding rather than by trimming the encoded string.
    #[test]
    fn a_bounded_encode_cuts_on_a_character_and_never_inside_an_escape() {
        // Under the budget: nothing is cut and the encoding is untouched.
        let (out, cut) = encode_bounded("two words", 64);
        assert_eq!((out.as_str(), cut), ("two%20words", false));

        // Every byte of every prefix is a whole escape or a whole graphic byte.
        let text = "é β 漢 x".repeat(40);
        for budget in 0..64 {
            let (out, cut) = encode_bounded(&text, budget);
            assert!(out.len() <= budget, "budget {budget}: {out}");
            assert!(cut, "budget {budget} cannot hold the whole text");
            assert!(
                !out.ends_with('%') && !(out.len() >= 2 && out.as_bytes()[out.len() - 2] == b'%'),
                "budget {budget} split an escape: {out}"
            );
            // It decodes back to a PREFIX of the original — never to mojibake.
            assert!(
                text.starts_with(&crate::pct::decode(&out)),
                "budget {budget} did not cut on a character: {out}"
            );
        }
    }

    /// THE SELF-LANE CHECK FAILS OPEN BELOW ITS WINDOW.
    ///
    /// A missing offset is a forgery only INSIDE the window the bridge can still
    /// answer for. `self_acked` is bounded at `SELF_ACK_KEEP` in memory and on
    /// disk, and an instance whose sessions message each other fills it with
    /// ordinary traffic — so past the bound the old code reported every one of
    /// its own older records as `forged-self` and published `cap-compromised`,
    /// telling an operator to rotate the node cap over mail the node sent
    /// itself. It is a refusal either way; the difference is a compromise alarm.
    #[test]
    fn the_self_lane_check_accuses_only_inside_the_window_it_remembers() {
        let acked: BTreeSet<u64> = [100, 200, 300].into_iter().collect();
        // Inside the window and remembered: ours.
        assert!(!forged_self(&acked, 200));
        // Inside the window and NOT remembered: a forgery, which is the whole
        // point of the check — this must still fire.
        assert!(forged_self(&acked, 250));
        assert!(forged_self(&acked, 9_000));
        // BELOW the window: aged out, not accused.
        assert!(!forged_self(&acked, 99));
        assert!(!forged_self(&acked, 0));
        // An empty set has no FLOOR to be below: a bridge that has published
        // nothing on its own lanes did not publish this, so the check still
        // fires. Fail-open is about the window's edge, not about disarming it.
        assert!(forged_self(&BTreeSet::new(), 7));
    }

    /// EVERY VARIABLE-LENGTH FIELD ON THE `deliver` LINE IS BOUNDED, and the one
    /// that was not is the one a stranger fills in.
    ///
    /// Round 1 put the request-line bound in one place and sized the BODY budget
    /// from it. `via=` sits on the same line, is copied verbatim off a record
    /// whose head runs to the broker's 16 MiB ceiling, and was checked by nothing
    /// on this side — so the prefix alone went past the bound, the body budget
    /// saturated to zero, and the finished line was still unsendable. The
    /// arithmetic is pinned here beside the grammar, because the two together are
    /// the property: a chain this predicate accepts cannot make the line
    /// unsendable, whatever else it carries.
    #[test]
    fn a_via_chain_is_bounded_by_grammar_and_by_length() {
        assert!(via_ok("h-andrew"));
        assert!(via_ok("h-andrew,n-a,s-b"));
        // The endpoint's own rule: every comma element a principal.
        assert!(!via_ok("notaprincipal"));
        assert!(!via_ok("h-andrew,,n-a"));
        assert!(!via_ok(""));
        assert!(!via_ok("h-andrew,h-WITHCAPS"));
        // The two bounds the endpoint does NOT have.
        let hop = format!("n-{}", "a".repeat(32));
        assert_eq!(hop.len(), subject::PRINCIPAL_MAX);
        let ok: Vec<String> = vec![hop.clone(); crate::body::VIA_MAX_HOPS];
        assert!(via_ok(&ok.join(",")));
        let too_many: Vec<String> = vec![hop.clone(); crate::body::VIA_MAX_HOPS + 1];
        assert!(!via_ok(&too_many.join(",")));
        // And the arithmetic that matters: the longest chain this admits, on the
        // longest line every OTHER field can build, still fits.
        let worst_via = ok.join(",");
        assert!(worst_via.len() <= crate::body::VIA_MAX_BYTES);
        let worst_prefix = format!(
            "deliver {sid} off={off} from={sid}@{sid} kind=undeliverable trust=relayed \
             re={off} dl={off} demoted=undeliverable via={worst_via} len={off} text=",
            sid = "s-".to_string() + &"a".repeat(32),
            off = u64::MAX,
        );
        assert!(
            worst_prefix.len() < REQUEST_LINE_MAX,
            "the worst metadata prefix is {} bytes, against a {REQUEST_LINE_MAX}-byte bound",
            worst_prefix.len()
        );
    }

    /// A `reason=` IS A TOKEN, wherever it comes from. The endpoint's refusals are
    /// sentences — `ERR no such session`, and a 150-character usage line full of
    /// spaces, `<`, `>` and `|` — and splicing one into a `key=value` line put
    /// that whole grammar onto an append-forever log at a stranger's choosing.
    #[test]
    fn a_refusal_reason_reaches_the_bus_as_one_clean_token() {
        assert_eq!(reason_token("ERR quota"), "quota");
        assert_eq!(reason_token("ERR no such session"), "no");
        assert_eq!(
            reason_token("ERR usage: deliver <sid> off=<n> | deliver <sid> landed=<id>"),
            "usage"
        );
        assert_eq!(reason_token("ERR"), "unknown");
        assert_eq!(reason_token(""), "unknown");
        // No token can ever invent a second field.
        for reply in [
            "ERR quota",
            "ERR no such session",
            "ERR usage: deliver <sid> off=<n> [via=<p,...>] | x",
            "ERR \u{1b}[31mred",
            "ERR a=b c=d",
        ] {
            let token = reason_token(reply);
            assert!(
                !token.is_empty()
                    && token.len() <= REASON_TOKEN_MAX
                    && token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "{reply:?} became {token:?}"
            );
        }
    }

    /// THIS MODULE'S DOC IS ITS CLAIM, and aterm has no evidence manifest to
    /// check it against — so two claims that were false are pinned here.
    ///
    /// `resolve_pending_feed` used to close by claiming it ran exactly once and
    /// that an entry which re-armed itself would be the silent retry §6.5
    /// forbids — attached to one of the two functions on the path to a PTY,
    /// while the same commit shipped a retry budget of eight and the run loop's
    /// own comment said so. An auditor asked "can a bus record be written to a
    /// PTY more than once?" was told by the function itself that it could not.
    ///
    /// And §11.2 pins aterm's unsafe surface to the `aterm-uds` cordon, so a raw
    /// `kill(2)` FFI in the bridge's run loop is an audit-surface defect even
    /// though it never misbehaved: a reviewer auditing by the documented rule
    /// would not look here. `notify.rs` solved the identical one-shot fault with
    /// `abort()` and said why.
    ///
    /// ## THE SCAN IS THE WHOLE CRATE NOW, AND THE PROMISE IS THE NARROWER ONE
    ///
    /// The cordon claim is made crate-wide by three docs, and this test used to
    /// check ONE FILE — the file that never had the problem. `ctl.rs` holds an
    /// one adoption of an inherited raw descriptor, which is exactly what the
    /// cordon exists for, so a reviewer auditing by the documented rule ("the
    /// raw-descriptor work is in aterm-uds") greps the cordon, finds it clean,
    /// and never opens the file where the crate's one block actually is.
    ///
    /// The right home is `aterm_uds::spawnfd`, beside the `BRIDGE_VERB_FD` /
    /// `BRIDGE_PUSH_FD` constants that place the very descriptor being adopted —
    /// that is a change in another crate. What is fixed HERE is the promise and
    /// its guard: the scan reads every file this crate ships, and the claim is
    /// the true narrow one — aterm-link's unsafe surface is EXACTLY the one
    /// adoption in `ctl.rs`, documented in place. A second block anywhere, or a
    /// second call site for that one, fails this test rather than falling silent.
    #[test]
    fn this_modules_doc_and_unsafe_surface_match_what_it_ships() {
        let src = include_str!("bridge.rs");
        // THE CITED TEST EXISTS. aterm ships no evidence manifest, so the header's
        // named guard is its evidence — and a name nothing answers to is worse
        // than no name, because `cargo test <name>` passes over an empty set.
        // SPLIT, and read out of the HEADER rather than the whole file. `src` is
        // this very file, so a single literal here satisfied `src.contains` by
        // itself: the assertion passed unchanged on a tree whose header still
        // cited the name that never existed, which is the exact defect it was
        // added to close.
        let cited = concat!(
            "no_inbox_record_ever_reaches_the_pty",
            "_and_a_stale_epoch_is_refused"
        );
        let header: String = src
            .lines()
            .take_while(|l| l.starts_with("//") || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            header.contains(cited),
            "the module header must cite the test that guards the one path to a PTY"
        );
        assert!(
            include_str!("../tests/bridge_e2e.rs").contains(&format!("fn {cited}")),
            "the cited guard must exist under exactly that name"
        );
        // Split so this test's own prose is not the counterexample.
        assert!(
            !src.contains(concat!("It runs ", "ONCE")),
            "the replay runs at every attach, on the loop's retry deadline and \
             on the idle roster round, bounded by FEED_BUDGET"
        );
        assert!(
            src.contains("bounded by [`FEED_BUDGET`]"),
            "the retry budget must be stated, as a DURATION, where the replay is documented"
        );
        // THE SCREEN FACE'S RATE BOUND IS PER SUBJECT. The bullet used to state
        // §3.3's "≤ 4/s" as a per-node aggregate while the shared clock gates
        // the SWEEP and every changed session publishes inside it — 4N/s under
        // `--screen all`, on the one face that carries screen CONTENT and the
        // one §14 names the largest retention risk. Split so this test's own
        // prose is not the counterexample.
        assert!(
            !src.contains(concat!("per [`SCREEN_PERIOD`] across", " all")),
            "the screen face's rate bullet must not restate a per-subject \
             bound as a per-node aggregate"
        );
        // EVERY FILE THIS CRATE SHIPS, read from the directory rather than from a
        // list — a list would silently stop covering the next file added, which
        // is the same shape of gap as scanning one file for a crate-wide claim.
        // Split so this array is not its own counterexample.
        let forms = [
            concat!("unsafe", " {"),
            concat!("unsafe", " extern"),
            concat!("unsafe", " fn"),
            concat!("unsafe", " impl"),
        ];
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut breaches: Vec<String> = Vec::new();
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).expect("the crate's own src/") {
            let path = entry.expect("a dir entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            files += 1;
            let body = std::fs::read_to_string(&path).expect("read a source file");
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for form in forms {
                for _ in 0..body.matches(form).count() {
                    breaches.push(name.clone());
                }
            }
        }
        assert!(
            files > 1,
            "the scan must read the whole crate, not one file"
        );
        // THE TRUE, NARROW CLAIM: exactly one adoption of an inherited descriptor,
        // in `ctl.rs`, and nothing else anywhere. Its right home is
        // `aterm_uds::spawnfd`; until it moves, the promise says what ships.
        assert_eq!(
            breaches,
            vec!["ctl.rs".to_string()],
            "aterm-link's unsafe surface is exactly one adopt in ctl.rs (§11.2 \
             wants even that in the aterm-uds cordon); found {breaches:?}"
        );
    }

    /// A halt's `reason=` is bounded, sanitized and NEVER empty — and a bare
    /// `state=on` still names its origin. The endpoint prints this string
    /// verbatim into `ERR halted …`, onto the digest and into `status`, so what
    /// a stranger can put in it is what every reader downstream has to survive.
    #[test]
    fn a_halt_reason_is_bounded_sanitized_and_never_empty() {
        assert_eq!(halt_reason_token(None), "fleet-halt");
        assert_eq!(halt_reason_token(Some("")), "fleet-halt");
        assert_eq!(halt_reason_token(Some("main%20broken")), "main%20broken");
        // A control byte, a newline and a non-ASCII byte are dropped rather than
        // passed on — the wire form is pct-encoded and therefore ASCII by
        // construction, so anything else is already a lie about the encoding.
        assert_eq!(halt_reason_token(Some("a\u{7}b\nc\u{202e}d")), "abcd");
        // Clamped, and the clamp never splits a `%XX` escape in half.
        let long = "x".repeat(200);
        assert_eq!(halt_reason_token(Some(&long)).len(), HALT_REASON_MAX);
        let escaped = format!("{}%41", "x".repeat(HALT_REASON_MAX - 2));
        let clamped = halt_reason_token(Some(&escaped));
        assert!(
            !clamped.ends_with('%') && !clamped.contains("%4"),
            "a clamp split an escape: {clamped}"
        );
    }

    #[test]
    fn a_minted_node_id_is_a_principal() {
        let id = mint_node_id();
        assert!(subject::is_principal(&id), "{id}");
        assert_eq!(id.len(), 18);
        assert!(id.starts_with("n-"));
        assert_ne!(id, mint_node_id(), "the CSPRNG is not a constant");
    }
}
