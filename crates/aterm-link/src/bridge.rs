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
//! * persists `seen_off` and refills a session's ring after an instance relaunch;
//! * REPORTS ITS BROKER LINK to the endpoint over the verb lane (`link up
//!   rtt=<ms>` / `link down reason=<token>`), on every change and after an ack
//!   that moved the round trip by more than 2x — never on a timer — so the
//!   endpoint's `fabric=` reads `connected` only while the broker actually
//!   answers and `stalled` while this process is alive but its link is not
//!   ([`Bridge::observe`], [`ACK_DEADLINE`]; round 13);
//! * WRITES PRESENCE WITH MEANING: every hosted session's row carries `role=
//!   detail= phase= [context=] title=` beside `attention=` — the phase read by
//!   the same reader `aterm drive phase` prints from, over the last 40 rows of
//!   the screen, re-read ONLY when the session's `status revision=` moved, and
//!   the row republished only when a field changed and at most once per 2 s.
//!   Never a word of the transcript ([`crate::presence`]; round 13).
//!
//! ## A NODE'S DISTINCT-SUBJECT BUDGET IS FINITE, AND NOTHING RECLAIMS IT
//!
//! The broker bounds one producer at `MAX_SUBJECTS_PER_PRODUCER` = 4096, rebuilt
//! from the log on every open, and a node's producer id is derived from an id the
//! state dir mints once and keeps forever — so the budget does not clear on
//! restart. This node mints one distinct subject per hosted session on
//! `presence` and `ev`, plus one per `say/<kind>`: call it two to three per
//! session over the node's lifetime — it was three to five before round 21 cut
//! the `control` and `screen` faces — so a long-lived instance exhausts the
//! budget somewhere past two thousand sessions. Past the bound every publish to
//! a NEW subject is refused, which means a newly spawned session gets no
//! presence row and no `ev` face while existing sessions keep working and
//! nothing on the bus says why.
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
//! ## The path to a PTY: there is none
//!
//! NO BUS RECORD REACHES A PTY BY ANY PATH. Not "is checked before it does" —
//! there is no function in this crate that writes to one. That is the round-21
//! cut: the `term/in` drive face, the only thing that ever converted a record
//! to keystrokes, is gone, and with it `feed`, `on_term_record`, the four
//! §6.6 conditions, the holder table, the epochs' fencing role, the mirror
//! leases and the feed journal. `no_bus_record_ever_reaches_a_pty` in
//! `tests/bridge_e2e.rs` is the claim over it, and it is now a claim about an
//! absence rather than about a check.
//!
//! (The name is PINNED by
//! `this_modules_doc_and_unsafe_surface_match_what_it_ships`: aterm ships no
//! evidence manifest, so a doc comment IS the claim, and this header once
//! cited a test that had never existed under that name — an auditor following
//! it got `0 passed; 0 filtered out`, a green run over an empty set.)
//!
//! THE SUBJECT STAYS RESERVED. `subject::parse_term_in` and
//! `subject::term_filter` remain, and the node ring still carries both `term`
//! grants, because an older node on this wire may still publish one and a
//! fleet that forgot the shape of the subject could not tell a stranger's
//! forgery from a peer's. Nothing in this crate parses or serves it: the
//! filter's only in-tree reference is its own test, which is the intended
//! end state.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use astream_broker::Record as BrokerRecord;

use crate::body::{via_ok, Body};
use crate::ctl::{Ctl, Reply, REQUEST_LINE_MAX};
use crate::mailbox::{Item, Mailbox, Source};
use crate::presence::{self, Fields, Mode, Slot};
use crate::state::{Asked, Deadline, StateDir, TopicCursor, ASKED_KEEP, DEADLINES_KEEP};
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

/// THE ACK DEADLINE: how long the publisher connection waits for the broker to
/// answer ONE request — a `Publish`, a `Fetch`, a `Last` page, the `Will`, an
/// `Attach`. Set on the socket ([`crate::transport::Closer::set_read_timeout`])
/// before the first frame, so a broker that accepts the connection and never
/// answers is a bridge that reports `link down reason=no-ack` after this long,
/// not a bridge parked in `attach` for the life of the process reporting
/// nothing. A timed-out request DROPS the connection: the late answer may still
/// arrive, and a stream with an unread reply on it is no longer framed.
///
/// The subscription connections get the same bound for their `attach` and
/// have it LIFTED once the subscription is open ([`Bridge::attach_broker`]):
/// parking in `recv` with nothing to deliver is their normal state, not a
/// stalled link.
///
/// Five seconds is far past a local broker's group-commit `fsync` on a loaded
/// machine and the width of [`RECONNECT_MAX`], so a link that stalls under a
/// wedged broker is reported within one back-off tick of the wedge being
/// noticed — the bound `aterm help fabric` states.
const ACK_DEADLINE: Duration = Duration::from_secs(5);

/// After an ack, how old the last `link up` report may be before the next ack
/// re-sends one anyway. NOT A HEARTBEAT: nothing is sent while nothing is
/// acked, and a quiet link's `fabric_link_age_ms=` simply grows. What this
/// bounds is the STALENESS of the endpoint's number on a BUSY link — a bridge
/// acking ten records a second would otherwise re-report only on a 2x move,
/// and `fabric_link_age_ms=` would read minutes on a link acking every 100 ms.
///
/// ONE BOUNDED EXCEPTION, and it is the ghost sweep ([`GHOST_SWEEP`]). Any
/// answered broker request is an ack, so a periodic READ is a periodic report
/// even though nothing was published — which is why the sweep runs only while
/// it has a candidate to retire and not on a free-running clock. While one is
/// standing this bridge does report about once a minute; when the set drains it
/// stops, and a fleet that has never had a ghost never starts. A round-21
/// review measured the version without that gate: every attached bridge on the
/// fleet reported every 60 s for ever, and `fabric_link_age_ms=` could no
/// longer exceed it.
const LINK_REFRESH: Duration = Duration::from_secs(2);

/// The least gap between two reports made because the round trip MOVED by
/// more than 2x. A local socket's ack jitters between 1 ms and 3 ms under
/// `fsync` alone, so an unbounded 2x rule would put a control-lane round trip
/// under every other publish of a burst; one per idle tick is the same rate
/// the loop's other periodic duties run at.
const LINK_MOVE_GAP: Duration = Duration::from_millis(250);

/// How far behind the log's head a refill starts when a session's `seen`
/// watermark is missing.
///
/// A bound rather than zero: `Fetch` is a linear scan, so a floor of zero is one
/// whole-log walk per freshly seen session — and a brand-new sid, which is the
/// common case for a missing watermark, has no history to find. This is the
/// window a genuinely lost watermark can be hiding delivered-but-unseen rows in,
/// and re-offering them is free because `deliver` is idempotent on `off=`.
const REFILL_FLOOR_SPAN: u64 = 4096;

/// How often the roster and the LOCAL observations are re-read.
///
/// FROM A DEADLINE, NOT FROM THE IDLE ARM. It used to be `ticks % 8` where
/// `ticks` advanced only in the mailbox's `None` branch — reachable only after a
/// full [`IDLE_TICK`] with every queue empty — so a bridge receiving one item
/// per 250 ms never ran it at all. The commit that made this argument for the
/// other periodic duties left these behind, and what starves is not
/// bookkeeping:
/// [`Bridge::sample_local_control`] is the only place `attention=` is
/// re-sampled — the field `notify --on attention` and `glance` read — and the
/// only producer of the per-session `detail=` and `revision` the presence row
/// carries. A peer posting four times a second is enough to hold a node in
/// that state indefinitely. (It was also the only producer of §6.6's rows 4
/// and 5, the conservative pause and the local-lease mirror; round 21 cut both
/// with the drive face they decided for.)
///
/// The period is the one the tick gate used to produce (8 × 250 ms), so a quiet
/// bridge pays exactly what it paid before and a busy one now pays it too.
const ROSTER_REFRESH: Duration = Duration::from_millis(2_000);

/// How often the retained presence rows under THIS node are swept for ghosts.
///
/// A minute, not [`ROSTER_REFRESH`]'s two seconds: this is a `Last` walk of a
/// whole subtree, and the thing it looks for cannot appear between two roster
/// rounds — a session that exits normally has its `exited` row published by
/// `refresh_sessions` within one of those rounds. What is left for this sweep
/// is only what NO round could have published.
const GHOST_SWEEP: Duration = Duration::from_secs(60);

/// How many `Fetch` pages one topic's backlog may walk per roster round.
///
/// A bound, not a budget: the scheduling loop is single-threaded and it is what
/// takes the fleet halt, so a late subscriber asking for `since=@0` against a
/// year of broadcasts must not be able to hold it. Four pages of 256 is up to a
/// thousand records a round — a deeper backlog simply takes more rounds, and
/// the cursor is persisted between them, so it also survives a restart
/// mid-catch-up.
const SAY_REPLAY_PAGES: usize = 4;

/// How long a `state=live` row under this node must have gone unhosted, AS
/// OBSERVED BY THIS BRIDGE, before its presence is retired.
///
/// Measured against this bridge's own sight of the roster rather than the
/// row's `t=`, because `t=` is when the row was last WRITTEN and presence is
/// published on change: a session that has been quietly busy for an hour has
/// an hour-old row and is perfectly alive. Five minutes is two orders of
/// magnitude past the two-second round that would have retired an ordinary
/// exit, so anything still standing at the end of it was published by an
/// incarnation that never got to say goodbye.
const GHOST_AFTER: Duration = Duration::from_secs(300);

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
/// carve-out is why a human's `task` or `control` arrives as itself rather than
/// demoted on a node whose operator listed nobody. (It also made §6.6's row 1,
/// a human's `claim` "granted unconditionally", reachable there; that row went
/// with the drive face in round 21, but the demotion carve-out is about mail
/// and stands on its own.)
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

/// The kinds that SETTLE an `ask`/`task` with a `dl=` (R8): a record of one of
/// these carrying `re=<off>` means the asker was answered, and no `expired` is
/// ever recorded for that offset. An `ask` answered by another `ask` is not
/// settled — that is a new question, not an answer.
const ANSWER_KINDS: [&str; 3] = ["answer", "report", "ack"];

/// The kinds that WAIT for a reply, and so are the only ones a `dl=` is a
/// deadline FOR. The same two `post` turns `--wait` on by default for.
const WAITING_KINDS: [&str; 2] = ["ask", "task"];

/// How often the deadline table is swept. The broker holds no timers (R8), so
/// the asker's OWN bridge is the only thing that can notice a deadline pass,
/// and it notices on this clock — from the run loop, not the idle arm, for the
/// reason every other periodic duty moved there: a busy bridge never idles.
const DEADLINE_TICK: Duration = Duration::from_millis(250);

/// The most verdicts one sweep publishes. Each one is a bounded `Fetch` of the
/// asker's lane plus a publish, so the sweep is bounded in work per tick and a
/// burst of expiries drains over a few ticks rather than stalling delivery.
const DEADLINE_SWEEP_MAX: usize = 16;

/// How many offsets this bridge remembers having recorded `expired` for, so a
/// reply that arrives afterwards is delivered `late=1`. In memory only: a
/// relaunched bridge delivers such a reply without the flag, which is the
/// honest direction (a missing `late=1` understates nothing the agent cannot
/// see — the `expired` row is in its inbox, and the reply's `re=` names it).
const EXPIRED_KEEP: usize = 4096;

/// The most bytes a `reason=` token may carry onto the bus.
const REASON_TOKEN_MAX: usize = 32;

/// The most body bytes one `inbox get @<off>` answer carries: the endpoint's
/// own `BODY_MAX` (256 KiB), the largest body it holds or returns. A record
/// over it is answered CUT there with `len=` naming its true size — which is
/// how the endpoint's `truncated=1 len=` reaches the reader — rather than
/// refused, because a refused answer is a read that never ends.
pub const FETCH_BODY_MAX: usize = 256 * 1024;

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

/// One sample of what the local instance says about a session: the `revision`
/// the screen reader is gated on ([`crate::presence::Slot::needs_screen`]) and
/// whether it has ever been read. It carried §6.6's last two rows until round
/// 21 cut them with the drive face.
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
}

/// One `status` reply's three tokens the bridge reads: `revision=`, `hold=`
/// and `detail=` (the running program, as `ls` prints it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StatusSample {
    revision: u64,
    hold: bool,
    detail: String,
}

/// TEST-ONLY fault injection, armed by `$ATERM_LINK_FAULT` — and TEST-ONLY is
/// enforced, not merely documented: [`Fault::from_env`] reads the variable only
/// in a build with `debug_assertions`, so a released `aterm-link serve` honours
/// no fault at all.
///
/// It had to become enforcement. The knob was documented test-only and shipped
/// in every build, while `ATERM_LINK_FAULT` is on neither `ENV_DENY_VARS` nor
/// `ENV_DENY_PREFIXES` — so it survives the PTY child-shell seam and
/// `fabric_launch::filter_child_env`, and a prompt-injected agent inside a
/// session could arm `kill-after-deliver` on a nested instance's bridge and
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
    /// DROP the push lane's `session-exited` line for as long as a marker file
    /// (`<state>/drop-session-exited`) exists — the line that never arrives.
    ///
    /// It is not an artificial state. A `GAP` is aterm saying a watcher fell
    /// behind and frames were coalesced away — §4.2's own words for what that
    /// costs are that "a `session-created`, a `hold` or an `inbox-seen` line may
    /// simply not exist any more"; a bridge that attaches after a session has
    /// already gone never had the line at all; and either fd of the verb/push
    /// pair can be lost while the process lives
    /// ([`Fault::LoseAtermBeforeDeliver`]). Every one of those leaves the same
    /// state: the endpoint's roster has moved and the only PUSHED notice of it
    /// is gone, so the repair has to come from LOOKING. Racing that window from
    /// outside is the flake this crate refuses; the marker holds it open for as
    /// long as the assertion takes, and everything downstream of the drop is the
    /// shipped path.
    DropSessionExitedWhileMarked,
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
    /// Sweep ghost presence rows with NO patience for as long as a marker file
    /// (`<state>/sweep-ghosts-now`) exists: [`GHOST_AFTER`] is treated as zero
    /// and [`Bridge::retire_ghost_presence`] runs on every roster round rather
    /// than on [`GHOST_SWEEP`].
    ///
    /// The two constants are five minutes and one minute, and they are those
    /// lengths on purpose — the sweep's whole safety argument is that it waits
    /// far longer than any ordinary exit takes to be published. A test cannot
    /// assert about a bound by waiting it out, and a test that shortened the
    /// bound by building a different bridge would be asserting about a bridge
    /// nobody ships. So the marker collapses the WAIT and nothing else: which
    /// rows qualify, the `inc=` guard, the record published and the `ev` beside
    /// it are all the shipped path.
    SweepGhostsAtOnceWhileMarked,
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
            Ok("lose-aterm-before-deliver") => Fault::LoseAtermBeforeDeliver,
            Ok("fail-status-while-marked") => Fault::FailStatusWhileMarked,
            Ok("fail-post-publish-while-marked") => Fault::FailPostPublishWhileMarked,
            Ok("drop-session-exited-while-marked") => Fault::DropSessionExitedWhileMarked,
            Ok("oversize-deliver-line") => Fault::OversizeDeliverLine,
            Ok("sweep-ghosts-at-once-while-marked") => Fault::SweepGhostsAtOnceWhileMarked,
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
/// What this bridge last TOLD the endpoint about its broker link — the `link`
/// verb's state on the sending side, so the record goes out on CHANGE and not
/// on a clock (§7 forbids a heartbeat storm, and the endpoint's `fabric=` is
/// derived from exactly these reports; see `aterm-gui/src/fabric.rs`'s
/// `link_report`).
///
/// The rules, as [`LinkReport::ack_wants_report`] and
/// [`LinkReport::down_wants_report`] apply them: a `down` is sent when the link
/// was up or the reason changed; an `up` is sent when the link was down, when
/// the last report is older than [`LINK_REFRESH`], or when the round trip moved
/// by more than 2x and the last report is older than [`LINK_MOVE_GAP`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkReport {
    up: bool,
    /// The reason last reported, one of [`link_reason`]'s tokens. Empty while
    /// up or before the first report.
    reason: &'static str,
    /// The round trip last reported, ms, rounded UP so a real ack never reads 0.
    rtt_ms: Option<u64>,
    reported_at: Option<Instant>,
}

impl LinkReport {
    const fn new() -> Self {
        Self {
            up: false,
            reason: "",
            rtt_ms: None,
            reported_at: None,
        }
    }

    /// Whether an ack at `rtt_ms`, observed at `now`, is worth a `link up`.
    fn ack_wants_report(&self, rtt_ms: u64, now: Instant) -> bool {
        if !self.up {
            return true;
        }
        let since = self
            .reported_at
            .map_or(Duration::MAX, |t| now.saturating_duration_since(t));
        if since >= LINK_REFRESH {
            return true;
        }
        let moved = self
            .rtt_ms
            .is_none_or(|last| rtt_ms > last.saturating_mul(2) || last > rtt_ms.saturating_mul(2));
        moved && since >= LINK_MOVE_GAP
    }

    /// Whether a `down` for `reason` is news.
    fn down_wants_report(&self, reason: &str) -> bool {
        self.up || self.reason != reason || self.reported_at.is_none()
    }
}

/// The one-token `reason=` a broker-side failure is reported under, from the
/// error's kind. `other` is the token for [`io::ErrorKind::Other`], which is
/// how the broker client surfaces the broker's OWN `Error` reply — a refused
/// attach, a denied read — so the caller says what it was asking.
fn link_reason(e: &io::Error, other: &'static str) -> &'static str {
    use io::ErrorKind as K;
    match e.kind() {
        K::NotFound => "no-socket",
        K::ConnectionRefused => "refused",
        K::PermissionDenied => "denied",
        K::TimedOut | K::WouldBlock => "no-ack",
        K::UnexpectedEof
        | K::BrokenPipe
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::NotConnected => "closed",
        K::Other => other,
        _ => "error",
    }
}

/// A round trip as the `rtt=` token: whole milliseconds, rounded UP, so the
/// sub-millisecond ack a local socket gives reads `1` and never `0` — a `0`
/// beside `fabric=connected` would read as "no ack".
fn rtt_ms_of(rtt: Duration) -> u64 {
    u64::try_from(rtt.as_micros().div_ceil(1_000)).unwrap_or(u64::MAX)
}

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
    /// `--sock <path>`: hand-started observer mode.
    pub sock: Option<String>,
    /// The instance token, for observer mode only.
    pub token: Option<String>,
    /// `--presence meta|minimal` / `[fabric] presence`: whether a session's
    /// presence row carries `role= detail= phase= context= title=` (the
    /// default) or `attention=` alone with no screen ever read
    /// ([`crate::presence::Mode`]).
    pub presence: Mode,
    /// `--receipts` / `[fabric] receipts`: whether an `inbox seen <id>
    /// handled|refused|deferred` on an `ask`/`task` row publishes `kind=ack
    /// re=<off> verdict=<v>` onto the SENDER's inbox lane (R8). ON by default
    /// since round 21, whichever way the bridge was set up — it used to be off
    /// here and on for a node `aterm fabric on` had configured, and with it off
    /// nothing can release a `post --wait-ack`. `--no-receipts` and `[fabric]
    /// receipts = false` turn it off.
    pub receipts: bool,
}

/// The bridge's live state.
pub struct Bridge {
    cfg: Config,
    node: String,
    producer_id: u64,
    inc: u64,
    /// The one incarnation of THIS node, other than its own, whose session rows
    /// this bridge may retire: the one whose WILL it found on the node face when
    /// it attached, which is proof that process is gone.
    ///
    /// `None` for "none but my own", and RE-SET on every attach rather than
    /// filled in once — a reconnect that finds a live node row must clear an
    /// adoption an earlier attach granted. See [`adoptable`].
    witnessed_dead: Option<u64>,
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
    /// `sid -> what the local instance last said about it`. Since round 21 its
    /// whole job is gating the screen read on a moved `status revision=`; the
    /// two §6.6 rows it was built for went with the drive face.
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
    /// When the roster, the local observations and the halt backstop are next
    /// re-read. See [`ROSTER_REFRESH`]: a deadline rather than a count of idle
    /// ticks, because a busy bridge has none.
    roster_due: Instant,
    /// When the retained presence rows under this node are next swept for
    /// ghosts. Its own deadline for the reason every other periodic duty has
    /// one — see [`GHOST_SWEEP`].
    ghost_due: Instant,
    /// `sid -> since when this bridge has seen a `state=live` row for it under
    /// its OWN node with no local session hosting it`.
    ///
    /// The clock is this bridge's own observation, not the row's `t=`
    /// ([`GHOST_AFTER`]), and it is dropped the moment the sid appears in the
    /// roster — so a session that is merely slow to be admitted never
    /// accumulates time here.
    ghosts: BTreeMap<String, Instant>,
    /// `sid -> topic -> the next offset that topic may deliver from` — the
    /// BROADCAST OPT-INS of every local session, with each one's cursor.
    ///
    /// The set is aterm's (`topic add`/`topic drop`); this is the bridge's copy
    /// of it, sampled on the roster round, plus the one thing aterm cannot
    /// know: where on the bus each topic resumes. A `since=head` entry is
    /// resolved ONCE, against the head at the moment this bridge first learns
    /// it; a `since=@<off>` entry starts at that offset. Every delivery moves
    /// the cursor past the record, and the whole map is mirrored into
    /// [`StateDir::set_topics`] so a restart resumes each topic where it was
    /// instead of silently swallowing everything published while the bridge was
    /// down.
    topics: BTreeMap<String, BTreeMap<String, TopicCursor>>,
    /// THE DRAIN POSITION of the broadcast face: the next offset the live
    /// subscription is expected to hand over.
    ///
    /// Everything at or above it is covered by the subscription; everything
    /// below a topic's cursor down to here is a GAP [`Bridge::catch_up_topics`]
    /// owns. It starts at the subscription's `from` — which after a restart is
    /// the lowest cursor any topic still owes, well below the bus head — and
    /// climbs as records arrive.
    say_drained_to: u64,
    /// THE BUS HEAD of the broadcast face, as last known: the head read at
    /// attach, then one past the highest say record taken.
    ///
    /// Not the drain position: after a restart with a gap the two differ by
    /// the whole backlog, and `since=head` must mean the head of the bus.
    say_bus_head: u64,
    /// `sid -> the meaning fields of its presence row` — `attention=`, `role=`,
    /// `detail=`, `phase=`, `context=`, `title=` — as last sampled, the
    /// `status revision=` the screen was last read at, and what is on the bus
    /// ([`Slot`]). A row is republished when a field moved and the row on the
    /// bus is at least [`presence::REPUBLISH_MIN`] old, and only then: presence
    /// is a last-value face on an append-forever log, so a periodic republish
    /// would be one record per session per period forever for no new
    /// information.
    presence: BTreeMap<String, Slot>,
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
    /// `ask offset -> its deadline`, for every `ask`/`task` this node published
    /// with `dl=` and has not seen an [`ANSWER_KINDS`] reply to. Keyed by the
    /// OFFSET rather than the sid because a reply names the offset and nothing
    /// else; pruned to the roster by the sid inside ([`Bridge::refresh_sessions`]),
    /// bounded at [`DEADLINES_KEEP`], and durable ([`StateDir::deadlines`]) so a
    /// relaunched bridge still owes the verdict — which it checks against the
    /// BUS before publishing, so a table that outlived an answer cannot record
    /// a false `expired` (see [`Bridge::expire_deadlines`]).
    deadlines: BTreeMap<u64, Deadline>,
    /// `ask offset -> the principal whose reply it is`, for every `ask`/`task`
    /// this node published ([`Asked`]): a reply of an [`ANSWER_KINDS`] kind
    /// naming that offset counts — settles a deadline, is a receipt — only
    /// when its cap-forced `<src>` is that principal ([`Bridge::reply_from`]).
    /// Newest [`ASKED_KEEP`], durable ([`StateDir::asked`]).
    asked: BTreeMap<u64, String>,
    /// `ask offset -> the asking sid` for the asks this bridge has recorded
    /// `expired` for, newest [`EXPIRED_KEEP`], so a reply arriving afterwards
    /// ON THE ASKER'S LANE is delivered `late=1`.
    expired: BTreeMap<u64, String>,
    /// When the deadline table is next swept. See [`DEADLINE_TICK`].
    deadline_due: Instant,
    /// The live subscriptions' closers. On a reconnect every one is closed
    /// FIRST: two group subscriptions on one cursor would deliver the same
    /// record twice and could walk the commit backwards, which is the one way
    /// this design's exactly-once could actually break.
    ///
    /// Built at CONNECT time rather than from the `Subscription`, because
    /// `Subscription::closer` exists only for the concrete `UnixStream` case and
    /// this bridge also speaks sealed TCP — see [`crate::transport`].
    closers: Vec<Closer>,
    /// What the endpoint has been told about the broker link. See
    /// [`LinkReport`] and [`Bridge::observe`].
    link: LinkReport,
    /// Whether [`Bridge::attach_broker`] is in progress. While it is, an ack
    /// is NOT reported as `link up` — the round trip is kept in
    /// `attach_rtt` and reported once the attach has COMPLETED — because a
    /// link is not up until the bridge can do its job on it. Measured
    /// 2026-09-14 (the round-13 review): a cap missing `ro:/f/<F>/fleet/>`
    /// acked the presence publish (`link up`), was refused the halt read
    /// (`link down reason=read`), redialed, and did that every back-off tick
    /// for ever — `fabric=` flapping and twenty presence records in 8 s for
    /// no new information (§7). Now the halt is read FIRST and nothing is
    /// published until it answered, so a bridge that cannot finish its attach
    /// reads a steady `stalled` and writes no record at all.
    attaching: bool,
    /// The last ack's round trip observed during an attach, reported as the
    /// first `link up` when the attach completes.
    attach_rtt: Option<Duration>,
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
            witnessed_dead: None,
            self_acked: state.self_acked(),
            deadlines: state.deadlines().into_iter().map(|d| (d.off, d)).collect(),
            asked: state.asked().into_iter().map(|a| (a.off, a.to)).collect(),
            expired: BTreeMap::new(),
            deadline_due: Instant::now() + DEADLINE_TICK,
            state,
            caps,
            attachment,
            fault,
            ctl,
            conn: None,
            locals: BTreeMap::new(),
            epochs: BTreeMap::new(),
            local: BTreeMap::new(),
            pending_admit: BTreeSet::new(),
            roster_due: Instant::now() + ROSTER_REFRESH,
            ghost_due: Instant::now() + GHOST_SWEEP,
            ghosts: BTreeMap::new(),
            topics: BTreeMap::new(),
            say_drained_to: 0,
            say_bus_head: 0,
            presence: BTreeMap::new(),
            halts: BTreeMap::new(),
            halt_applied: false,
            halt_reason: String::new(),
            committed: 0,
            closers: Vec::new(),
            link: LinkReport::new(),
            attaching: false,
            attach_rtt: None,
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
    ///
    /// UNDER THE ACK DEADLINE from the first frame: [`ACK_DEADLINE`] is set on
    /// the socket before the `attach`, so a broker that accepts and never
    /// answers fails HERE with `TimedOut`/`WouldBlock` rather than parking the
    /// bridge in its first handshake for ever. The publisher keeps the bound
    /// for its life (every exchange on it is request/reply); a subscription
    /// lifts it once it is open.
    fn connect(&self) -> io::Result<(Conn, Closer)> {
        let (mut c, closer) = transport::connect(&self.cfg.transport, &self.cfg.broker)?;
        closer.set_read_timeout(Some(ACK_DEADLINE))?;
        for cap in &self.caps {
            c.attach(&cap.grant, &cap.tag)?;
        }
        Ok((c, closer))
    }

    // -----------------------------------------------------------------------
    // the broker link, as told to the endpoint
    // -----------------------------------------------------------------------

    /// EVERY REQUEST/REPLY ON THE PUBLISHER CONNECTION PASSES ITS OUTCOME
    /// THROUGH HERE. `started` is when the request was written; `r` is what came
    /// back. An `Ok` is an ack and the round trip is noted; an
    /// [`io::ErrorKind::Other`] is the broker's own `Error` reply — a completed
    /// round trip too, so it is an ack when `other` is `None` (a refused
    /// `Publish` is the broker working: a subject budget, a cap that does not
    /// cover the subject) and a `link down` under that token otherwise (a read
    /// the bridge cannot do its job without); anything else is the transport
    /// failing — the link is reported DOWN under [`link_reason`]'s token and the
    /// connection is DROPPED, because after a timeout the stream is not framed
    /// and after an EOF it is not there. The loop reconnects with back-off.
    fn observe<T>(
        &mut self,
        started: Instant,
        r: io::Result<T>,
        other: Option<&'static str>,
    ) -> io::Result<T> {
        match &r {
            Ok(_) => self.note_ack(started.elapsed()),
            Err(e) if e.kind() == io::ErrorKind::Other && other.is_none() => {
                self.note_ack(started.elapsed());
            }
            Err(e) => {
                let reason = link_reason(e, other.unwrap_or("error"));
                self.link_down(reason);
                self.conn = None;
            }
        }
        r
    }

    /// An ack came back in `rtt`: tell the endpoint if that is news — see
    /// [`LinkReport::ack_wants_report`].
    fn note_ack(&mut self, rtt: Duration) {
        if self.attaching {
            // Not yet: see the `attaching` field. The attach's last ack is
            // what the first `link up` will carry.
            self.attach_rtt = Some(rtt);
            return;
        }
        let rtt_ms = rtt_ms_of(rtt);
        let now = Instant::now();
        if !self.link.ack_wants_report(rtt_ms, now) {
            return;
        }
        self.link = LinkReport {
            up: true,
            reason: "",
            rtt_ms: Some(rtt_ms),
            reported_at: Some(now),
        };
        self.report_link(&format!("link up rtt={rtt_ms}"));
    }

    /// The link is down for `reason`: tell the endpoint if that is news — a
    /// link that was up, or a reason that changed. The retries of one back-off
    /// run all fail the same way and send nothing after the first.
    fn link_down(&mut self, reason: &'static str) {
        if !self.link.down_wants_report(reason) {
            return;
        }
        self.link.up = false;
        self.link.reason = reason;
        self.link.reported_at = Some(Instant::now());
        self.report_link(&format!("link down reason={reason}"));
    }

    /// One `link …` record on the verb lane. Observer mode has no bridge lane
    /// to report on (`link` is bridge-only) and says nothing.
    fn report_link(&mut self, line: &str) {
        if self.attachment != Attachment::Inherited {
            return;
        }
        match self.ctl_request(line) {
            Ok(reply) if reply.ok() => {}
            Ok(reply) => eprintln!("aterm-link: `{line}` refused: {}", reply.header()),
            Err(e) => eprintln!("aterm-link: `{line}` failed: {e}"),
        }
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
        let producer_id = self.producer_id;
        let Some(conn) = self.conn.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the broker is unreachable",
            ));
        };
        let started = Instant::now();
        let answer = conn.publish(producer_id, seq, subject, body);
        self.observe(started, answer, None)
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
    /// named only inside the pct-encoded payload — so the causal pointer §10
    /// asks for belonged to no partition any reader could name, and a reader
    /// scoped to `ro:/f/<F>/pub/<n>/<sid>/>` could not see its own session's
    /// `ev` either. (The consistent-cut auditor that made the first half of
    /// that concrete, `replay.rs`, was cut in round 21: nothing was ever built
    /// that called it. The placement is still right, and
    /// `a_sessions_ev_records_are_attributable_to_that_session` still pins it,
    /// now against `subject::session_face` — the function that mints it.)
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
        // THE SAME READ ANSWERS TWO QUESTIONS. `inc=` gives the incarnation to
        // outrank; `state=` says whether the row is a predecessor's WILL, which
        // is the only local proof this bridge ever gets that another incarnation
        // of this node is gone — a will is published by the broker when the
        // connection behind it drops, and by nothing else. See [`adoptable`].
        let (bus_inc, witnessed_dead) = {
            let conn = self
                .conn
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let started = Instant::now();
            let answer = conn.last(&subject, "", 8);
            let (rows, _) = self.observe(started, answer, Some("attach"))?;
            let newest = rows
                .iter()
                .filter(|(_, s, _)| *s == subject)
                .filter_map(|(_, _, b)| {
                    let (body, _) = Body::decode(b);
                    let inc = body.unknown.get("inc")?.parse::<u64>().ok()?;
                    Some((inc, body.unknown.get("state").cloned()))
                })
                .max_by_key(|(inc, _)| *inc);
            match newest {
                // RE-SET ON EVERY ATTACH, never merely filled in: a reconnect
                // that finds a LIVE node row must clear an adoption an earlier
                // attach granted, or a sibling that came up in between inherits
                // a verdict taken before it existed.
                Some((inc, state)) => (inc, (state.as_deref() == Some("gone")).then_some(inc)),
                None => (0, None),
            }
        };
        self.witnessed_dead = witnessed_dead;
        self.inc = self.state.incarnation().max(bus_inc) + 1;
        self.state.set_incarnation(self.inc)?;
        let gone = format!(
            "v=1 t={} state=gone inc={} fabric=disconnected",
            crate::now_ms(),
            self.inc
        );
        let will_seq = (self.inc << 32) | WILL_SEQ_LOW;
        {
            let producer_id = self.producer_id;
            let conn = self
                .conn
                .as_mut()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no broker"))?;
            let started = Instant::now();
            let answer = conn.will(producer_id, will_seq, &subject, gone.as_bytes());
            self.observe(started, answer, Some("attach"))?;
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
    /// ## AND SO DO `role=`, `detail=`, `phase=`, `context=` AND `title=` (round 13)
    ///
    /// §4.2 names `role=` and `detail=` too, and `ls` printed `-` for both
    /// because nothing wrote them. They are written now, with the two a
    /// manager actually needs — `phase=` (busy | idle | prompt | question |
    /// limited | survey, from the same reader `aterm drive phase` prints) and
    /// `context=<n>%` when Claude Code shows its indicator — from the fields
    /// [`Bridge::sample_presence`] keeps per session ([`crate::presence`]).
    /// `presence = "minimal"` writes `attention=` alone. NEVER TRANSCRIPT TEXT:
    /// every token is a word this bridge chose, a number, or a `meta` value.
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
        // THE FIRST ROW ALREADY MEANS SOMETHING: a live row is sampled before
        // it is first published, whichever path publishes it (admission, an
        // observer's, a reconnect's), so a session is never on the bus as
        // `phase=-` for a round only to be rewritten two seconds later. An
        // `exited` row is not sampled: the session is gone, and the fields it
        // last had are the honest ones.
        if state == "live" && !self.presence.get(sid).is_some_and(|slot| slot.sampled) {
            self.sample_presence(sid);
        }
        let subject = subject::session_face(&self.cfg.fleet, &self.node, sid, "presence");
        let epoch = self.epochs.get(sid).cloned().unwrap_or_else(|| "-".into());
        let hold = u8::from(self.halt_applied);
        let gen = self.live_gen(sid).unwrap_or_else(|| "-".to_string());
        let fields = self.presence.get(sid).map_or_else(
            || Fields::default().tokens(self.cfg.presence),
            |slot| slot.fields.tokens(self.cfg.presence),
        );
        let mut body = format!(
            "v=1 t={} inc={} epoch={epoch} gen={gen} state={state} hold={hold}{fields}",
            crate::now_ms(),
            self.inc
        );
        if self.attachment == Attachment::Observer {
            body.push_str(" observer=1");
        }
        let mode = self.cfg.presence;
        match self.publish(&subject, body.as_bytes()) {
            Ok(_) => {
                if let Some(slot) = self.presence.get_mut(sid) {
                    slot.note_published(mode, Instant::now());
                }
            }
            Err(e) => eprintln!("aterm-link: could not publish presence for {sid}: {e}"),
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
        // below then emptied every per-sid map — `epochs`, which is the set of
        // sids this node hosts, so the bridge forgot its whole roster until the
        // next broker reconnect, and a fleet halt arriving in that window was
        // recorded as applied over zero sessions. Every other reply-consumer in
        // this file already checks.
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
        // PRUNED TO THE ROSTER. A3 only ever ADDED to these maps:
        // `session-exited` removed an entry when its line arrived, and nothing
        // reconciled any of them with the roster.
        //
        // EVERY PER-SID MAP, and the queue — four of them now, six before round
        // 21 took `holders` and `pending` with the drive face. The first fix
        // pruned three and left `local`, `presence` (then `attention`) and
        // `screen_gen`: declared in the same struct, written on the same roster
        // round, keyed by the same sids, removed by nothing anywhere. Sids are
        // 128-bit and never reused and the bridge is resident for the life of
        // the instance, so those grew with every tab ever opened. Each is a
        // pure CACHE of an observation about a live session — the last `status`
        // sample, the last published `attention=`, the last published screen
        // generation — so dropping a departed session's entry can lose nothing:
        // the next holder of that sid does not exist.
        //
        // `every_per_sid_map_is_pruned_to_the_roster` reads the struct rather
        // than this list, so the next map added is covered the day it is added.
        let live: BTreeSet<String> = self.locals.values().cloned().collect();
        // AND THE ROW OF A SESSION THAT LEFT IS WITHDRAWN HERE, not only by the
        // `session-exited` line.
        //
        // THE PRUNE AND THE WITHDRAWAL ARE THE SAME OBSERVATION, and splitting
        // them left this bridge holding two disagreeing pictures of which
        // sessions exist: `epochs` (re-derived from the endpoint, right here)
        // and the `state=` on its own presence rows (moved ONLY by the push
        // lane's `session-exited`). [`Bridge::advertisers`] reads the second one
        // to answer "where does this sid live", and its doc says in as many
        // words that a session cannot be simultaneously not-live and the single
        // routing candidate — which is exactly what the split produced for every
        // departure the push lane had not delivered yet, and permanently for one
        // it never delivers at all: a `GAP` (§4.2 says the line "may simply not
        // exist any more"), a bridge that attached after the exit, an endpoint
        // whose digest coalesced it away. A post addressed to such a sid is
        // routed to a dead `in` face and acked to its sender as landed.
        //
        // BEFORE THE PRUNE, because the row carries `epoch=` and the epoch is
        // about to be dropped. It is exactly once per departure: the
        // `session-exited` arm removes the sid from `epochs` BEFORE it calls
        // this, so a delivered line and this sweep cannot both publish.
        let departed: Vec<String> = self
            .epochs
            .keys()
            .filter(|sid| !live.contains(*sid))
            .cloned()
            .collect();
        for sid in departed {
            self.publish_session_presence(&sid, "exited");
        }
        self.epochs.retain(|sid, _| live.contains(sid));
        self.local.retain(|sid, _| live.contains(sid));
        self.presence.retain(|sid, _| live.contains(sid));
        // AND THE BROADCAST CURSORS, in memory AND on disk: a session that has
        // exited will never take delivery of a topic again, and a cursor file
        // per session ever opened is the same unbounded growth the maps above
        // are pruned for.
        for sid in self.topics.keys() {
            if !live.contains(sid) {
                self.state.forget_topics(sid);
            }
        }
        self.topics.retain(|sid, _| live.contains(sid));
        self.pending_admit.retain(|sid| live.contains(sid));
        // AND THE DEADLINES OF SESSIONS THAT ARE GONE. A verdict for an ask
        // whose asker has exited would land on a lane nobody drains; dropping
        // it loses nothing, and the table is durable so the drop is persisted.
        let before = self.deadlines.len();
        self.deadlines.retain(|_, d| live.contains(&d.sid));
        if self.deadlines.len() != before {
            self.persist_deadlines();
        }
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
                let started = Instant::now();
                let answer = conn.fetch(0, &filter, 0);
                let Ok((_, (_, head))) = self.observe(started, answer, Some("read")) else {
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
                let started = Instant::now();
                let answer = conn.fetch(cursor, &filter, 256);
                match self.observe(started, answer, Some("read")) {
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

    /// ONE BROADCAST RECORD, FANNED IN to the local sessions that asked for its
    /// topic.
    ///
    /// The fan-in is the whole point of the design: ONE record on the log,
    /// however many sessions read it. Each recipient gets the SAME delivery an
    /// addressed message gets — the same ring, the same per-sender quota, the
    /// same `task`/`control` demotion from a principal it does not accept —
    /// plus `topic=<t>`, and nothing about being a broadcast relaxes any of
    /// them: a shout from a stranger is a `note` in an agent's mailbox, not a
    /// task.
    ///
    /// NOBODY IS A RECIPIENT BY DEFAULT, including the sender: delivery is
    /// keyed on this bridge's copy of each session's `topic add` set, so a
    /// session that never asked — the publishing one included — is simply not
    /// in the loop below.
    ///
    /// A SESSION THAT IS BEHIND IS NOT A RECIPIENT EITHER: a cursor below the
    /// drain position owns a backlog [`Bridge::catch_up_topics`] has not walked,
    /// and delivering this record to it would move the cursor past that
    /// backlog. The catch-up delivers this record too, in offset order.
    fn on_say_record(&mut self, rec: &BrokerRecord) {
        let (off, subject, raw) = rec;
        // WHERE THE SUBSCRIPTION HAD REACHED BEFORE THIS RECORD. Every cursor
        // at or above it is AT THE FRONTIER — no gap can exist below it — and
        // every cursor under it is behind, with a backlog somebody else owns.
        let frontier = self.say_drained_to;
        self.say_drained_to = self.say_drained_to.max(off.saturating_add(1));
        self.say_bus_head = self.say_bus_head.max(off.saturating_add(1));
        // OBSERVER MODE DELIVERS NOTHING (§11.2) — and, unlike the addressed
        // path, records nothing either: an `ev undeliverable` per broadcast per
        // observer is a bus write for a message nobody on this node asked for.
        if self.attachment == Attachment::Observer {
            return;
        }
        let Some(say) = subject::parse_say(&self.cfg.fleet, subject) else {
            // The filter is a WILDCARD; the parse is left-anchored and counts
            // segments, so this is the arm an eight-segment forgery lands in.
            // Not reported: a malformed subject names no local session, so
            // there is nobody whose lane the notice would belong on.
            return;
        };
        let (body, _raw_tail) = Body::decode(raw);
        // The relay bound, on the writer, exactly as the addressed path applies
        // it — see [`crate::body::VIA_MAX_HOPS`].
        if !body.via.as_deref().is_none_or(via_ok) {
            return;
        }
        let recipients: Vec<(String, u64)> = self
            .topics
            .iter()
            .filter_map(|(sid, by_topic)| {
                let cursor = by_topic.get(&say.topic)?;
                (cursor.next >= frontier && cursor.next <= *off).then(|| (sid.clone(), cursor.next))
            })
            .collect();
        let mut lane_lost = false;
        for (sid, expect) in recipients {
            if self.deliver_broadcast(&sid, &say, &body, *off, expect) == Delivery::Unaccounted {
                lane_lost = true;
            }
        }
        if lane_lost {
            // The aterm lane is gone: nothing is written down, so the
            // replacement bridge re-reads this record. The frontier sweep would
            // otherwise move the cursor `deliver_broadcast` just declined to.
            return;
        }
        self.advance_frontier(frontier, off.saturating_add(1));
    }

    /// Move every topic cursor AT THE FRONTIER past a record the subscription
    /// has just handed over — the ones it matched and the ones it did not.
    ///
    /// The cursor is "read up to", not "last received": without this a
    /// session on `a` falls behind every time anybody broadcasts on `b`, and
    /// every roster round runs a catch-up over a stretch that holds nothing.
    ///
    /// ONLY the frontier. A cursor BELOW `frontier` belongs to a topic whose
    /// backlog has not been walked yet, and moving it here would answer a
    /// `since=@<off>` with silence.
    fn advance_frontier(&mut self, frontier: u64, next: u64) {
        let mut moved: Vec<String> = Vec::new();
        for (sid, by_topic) in &mut self.topics {
            let mut changed = false;
            for cursor in by_topic.values_mut() {
                if cursor.next >= frontier && cursor.next < next {
                    cursor.next = next;
                    changed = true;
                }
            }
            if changed {
                moved.push(sid.clone());
            }
        }
        for sid in moved {
            self.persist_topics(&sid);
        }
    }

    /// One broadcast record to ONE subscribed session, from the cursor value
    /// `expect` the caller read.
    fn deliver_broadcast(
        &mut self,
        sid: &str,
        say: &subject::SayAddr,
        body: &Body,
        off: u64,
        expect: u64,
    ) -> Delivery {
        // THE KIND IS IN THE BODY, not in the subject (round 23): `to=say:<t>`
        // spends its one leaf on the TOPIC, because the topic is what
        // subscribers name and the subject budget is unreclaimed. An absent or
        // unknown `kind=` decodes to `None` and is a `note` — the floor, not a
        // refusal, so an older or newer peer's record still arrives as
        // something.
        let addr = subject::InAddr {
            node: self.node.clone(),
            sid: sid.to_string(),
            src: say.node.clone(),
            kind: body.kind.clone().unwrap_or_else(|| "note".to_string()),
        };
        let (kind, demoted, _stray) = self.classify_delivery(&addr, body);
        let outcome = self.deliver_line(&DeliverLine {
            addr: &addr,
            body,
            off,
            kind: &kind,
            demoted: demoted.as_deref(),
            // A BROADCAST ANSWERS NO ASK. `re=`/`late=` and the deadline settle
            // are asker-lane facts, and a shout carrying `re=` is a stranger's
            // claim about a conversation it was not in.
            late: false,
            topic: Some(&say.topic),
        });
        // AN UNACCOUNTED DELIVERY LEAVES THE CURSOR: aterm is gone, the process
        // is on its way out, and a replacement bridge must re-offer this record.
        if outcome == Delivery::Unaccounted {
            return outcome;
        }
        self.advance_topic(sid, &say.topic, expect, off.saturating_add(1));
        outcome
    }

    /// Move one session's cursor for one topic past a record, and persist it.
    ///
    /// MONOTONE, and durable on every step: the cursor IS the delivery
    /// guarantee across a bridge restart, and one that moved in memory only
    /// would re-offer the record after a crash — which the ring would dedup,
    /// but only while it still holds the offset.
    ///
    /// AND NEVER ACROSS A GAP: `expect` is the cursor the caller read before
    /// it delivered, and a cursor that has moved since covers records this
    /// delivery did not.
    fn advance_topic(&mut self, sid: &str, topic: &str, expect: u64, next: u64) {
        let moved = {
            let Some(by_topic) = self.topics.get_mut(sid) else {
                return;
            };
            let Some(cursor) = by_topic.get_mut(topic) else {
                return;
            };
            if cursor.next != expect || next <= cursor.next {
                return;
            }
            cursor.next = next;
            true
        };
        if moved {
            self.persist_topics(sid);
        }
    }

    /// Write one session's whole cursor set to the state dir.
    fn persist_topics(&mut self, sid: &str) {
        let Some(snapshot) = self.topics.get(sid).cloned() else {
            return;
        };
        if let Err(e) = self.state.set_topics(sid, &snapshot) {
            eprintln!("aterm-link: the broadcast cursors of {sid} could not be persisted: {e}");
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
        let (kind, demoted, stray) = self.classify_delivery(&addr, &body);
        // A REPLY SETTLES THE ASK IT NAMES (R8), before anything can refuse the
        // row: the answer is on the bus whether or not this endpoint takes it,
        // and the deadline sweep re-reads the bus before any verdict anyway.
        // And a reply to an ask this bridge already recorded `expired` for is
        // delivered as what it is — `late=1`.
        //
        // ONLY ON THE ASKER'S OWN LANE. The table is keyed by offset, and every
        // local session's mail passes through here: an `answer re=<X>` a peer
        // sent to ANOTHER local session answered nobody's question to that
        // peer's asker, and settling on it left the asker with neither an
        // answer nor the `expired` its bridge owed — the sweep's own bus check
        // reads the asker's lane only, so the two disagreed about what settles.
        let late = body
            .re
            .filter(|_| !stray && ANSWER_KINDS.contains(&kind.as_str()))
            .is_some_and(|re| {
                self.settle_deadline_for(&addr.sid, re);
                self.expired
                    .get(&re)
                    .is_some_and(|asker| *asker == addr.sid)
            });
        self.deliver_line(&DeliverLine {
            addr: &addr,
            body: &body,
            off,
            kind: &kind,
            demoted: demoted.as_deref(),
            late,
            topic: None,
        })
    }

    /// BUILD AND SEND ONE `deliver` LINE — the half shared by an ADDRESSED
    /// record and a BROADCAST one.
    ///
    /// Everything above it differs (an addressed record is parsed off this
    /// node's own `in` lane, checked for self-lane forgery and committed to a
    /// group cursor; a broadcast is matched against a topic set and cursored per
    /// session), and everything from here down must NOT: the ring, the
    /// per-sender quota, the demotion, the `trust=` word, the line bound and the
    /// truncation notice are the properties an agent's mailbox is defined by,
    /// and a second copy of them is a second set of them. `topic` is the only
    /// field the broadcast path adds.
    fn deliver_line(&mut self, row: &DeliverLine<'_>) -> Delivery {
        let &DeliverLine {
            addr,
            body,
            off,
            kind,
            demoted,
            late,
            topic,
        } = row;
        let trust = trust_of(&addr.src, body.via.is_some());
        let from = self.render_from(addr, body);
        // A `control` RECORD IS NOW AN ORDINARY INBOX ROW (round 21). It used
        // to also MOVE something — §6.6's handover, which took the keyboard for
        // the claimant and decided who a later `term/in` would be fed from —
        // and that is gone with the drive face. `control` stays in
        // `body::KINDS` and in `DEMOTE_UNLESS_ACCEPTED` because an older node
        // on the wire still sends one and it must still arrive, and be demoted
        // from an unlisted principal, like any other addressed kind.
        let mut line = format!(
            "deliver {} off={off} from={from} kind={kind} trust={trust}",
            addr.sid
        );
        // THE ONE FIELD A BROADCAST ADDS. It names WHY this row is in this
        // mailbox — the session asked for the topic — which an addressed row
        // never has to explain, and it is what the agent replies to.
        if let Some(t) = topic {
            line.push_str(&format!(" topic={t}"));
        }
        if let Some(re) = body.re {
            line.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = body.dl {
            line.push_str(&format!(" dl={dl}"));
        }
        if late {
            line.push_str(" late=1");
        }
        if let Some(d) = demoted {
            line.push_str(&format!(" demoted={d}"));
        }
        if let Some(via) = &body.via {
            line.push_str(&format!(" via={via}"));
        }
        // A RECEIPT'S VERDICT rides an `ack` row and nothing else (R8). The
        // decoder already closed the word to the three the endpoint accepts.
        if kind == "ack" {
            if let Some(v) = &body.verdict {
                line.push_str(&format!(" verdict={v}"));
            }
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
            return self.refuse_delivery(addr, body, off, topic, "oversize");
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
            return self.refuse_delivery(addr, body, off, topic, "oversize");
        }
        if self.fault == Fault::LoseAtermBeforeDeliver {
            self.fault = Fault::None;
            Fault::LoseAtermBeforeDeliver.fired(&self.state);
            let _ = self.ctl.get_ref().shutdown(std::net::Shutdown::Both);
        }
        let outcome = match self.ctl_request(&line) {
            Ok(reply) if reply.ok() => Delivery::Accounted,
            Ok(reply) => {
                // A REFUSAL IS DATA. `ERR quota` is the ring telling us one peer
                // has had its say; the design's answer is to tell the SENDER so,
                // on their own lane, rather than to drop the record silently.
                // The reason travels as ONE TOKEN — see [`reason_token`].
                let why = reason_token(reply.header());
                self.refuse_delivery(addr, body, off, topic, &why)
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
                    self.refuse_delivery(addr, body, off, topic, "refused")
                }
            }
        };
        // …AND THE TRUNCATION NOTICE, which is a BUS RECORD. On a broadcast it
        // would be one record per subscriber for one shout, which is exactly the
        // multiplication §6 forbids ("one record per broadcast"); the cut is
        // still auditable from the row itself, whose `len=` exceeds what
        // arrived. See [`Bridge::refuse_delivery`] for the same rule on the
        // refusal.
        if cut && outcome == Delivery::Accounted && topic.is_none() {
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

    /// A REFUSED DELIVERY, told to whoever it is owed to.
    ///
    /// An ADDRESSED record's refusal is owed to the SENDER: they chose this
    /// recipient, `post --wait-ack` is waiting on the answer, and the design's
    /// rule is that `ERR quota` travels back on the sender's own lane rather
    /// than losing the record silently ([`Bridge::refuse_locally`]).
    ///
    /// A BROADCAST's is owed to nobody on the bus: the recipient chose the
    /// topic, not the sender, and one refusal record per subscriber would
    /// multiply a shout by its audience (§6: one record per broadcast). The
    /// endpoint counts it in the recipient's `dropped=`; here it is accounted
    /// and the cursor moves on, so a full ring is not re-offered forever.
    fn refuse_delivery(
        &mut self,
        addr: &subject::InAddr,
        body: &Body,
        off: u64,
        topic: Option<&str>,
        why: &str,
    ) -> Delivery {
        match topic {
            Some(t) => {
                eprintln!(
                    "aterm-link: broadcast off={off} topic={t} not delivered to {}: {why}",
                    addr.sid
                );
                Delivery::Accounted
            }
            None => self.refuse_locally(addr, body, off, why),
        }
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
    /// `--accept-from` is EMPTY by default, so with no carve-out a human's
    /// `task` or `control` arrived DEMOTED on every node whose operator had not
    /// pre-listed them by name — and §8.4's own spelling of the mitigation,
    /// `--accept-from h-*`, is refused at startup by `subject::is_principal`
    /// (`*` is not in `[a-z0-9-]`), so there was no way to express "every
    /// human" either.
    ///
    /// The case that first made this unacceptable is gone: it used to make
    /// §6.6's row 1 — an `h-*` `claim` "granted unconditionally" — literally
    /// unreachable, because the keyboard moved off the CLASSIFIED kind, so a
    /// human's demoted `control` never reached the handoff table and every
    /// following `term/in` under that claim was refused `reason=holder`. Round
    /// 21 cut the drive face, so nothing moves a keyboard now. The rule stands
    /// on the narrower ground it always also had: a demoted `task` is a `note`,
    /// and a manager's human cannot hand a worker work on a node that has not
    /// heard of them.
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

    /// [`Bridge::classify_kind`], after one more rule — and whether it applied:
    /// A REPLY COUNTS ONLY FROM WHERE THE ASK WENT (round 16 review). A node's
    /// ring grants it `rw,p=<N>:/f/<F>/in/*/*/<N>/*` — every lane on the fleet,
    /// as itself — so a third node could publish `ack re=<off>
    /// verdict=handled` onto an asker's lane for a task it was never sent, and
    /// that settled the asker's `post --wait-ack` (`ack=handled` while the
    /// recipient's inbox still held the task) and its deadline. The `<src>`
    /// segment is the broker's word; where the ask went is this bridge's own
    /// record ([`Bridge::reply_from`]). A reply-kind record naming an ask of
    /// ours from anyone else is STRAY: a receipt arrives as what it is —
    /// `kind=note demoted=ack` — and no stray reply settles anything (the
    /// caller's `late`/deadline arm reads the flag). An offset this bridge has
    /// no record of (older than the table, or asked before it existed) is
    /// judged as before. ONE function for the live delivery and the refill,
    /// so a refilled row reads exactly as it did the first time.
    fn classify_delivery(
        &self,
        addr: &subject::InAddr,
        body: &Body,
    ) -> (String, Option<String>, bool) {
        let stray = ANSWER_KINDS.contains(&addr.kind.as_str())
            && body
                .re
                .is_some_and(|re| self.reply_from(re, &addr.src) == Some(false));
        if stray && addr.kind == "ack" {
            return ("note".to_string(), Some(addr.kind.clone()), true);
        }
        let (kind, demoted) = self.classify_kind(addr, body);
        (kind, demoted, stray)
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

    // -----------------------------------------------------------------------
    // receipts (R8): `inbox seen … handled|refused|deferred` acks the sender
    // -----------------------------------------------------------------------

    /// Put one OWED receipt on the bus and retire it at the endpoint — a
    /// `receipt sid= rid= off= verdict= kind= from=` line off the `outbox` peek,
    /// which the endpoint lists from the moment its session ran `inbox seen <id>
    /// handled|refused|deferred` on an `ask`/`task` row until this retires it
    /// (R8). The result is `kind=ack re=<off> verdict=<v>` on the lane of the
    /// principal that sent the ask — the thing a sender's `post --wait-ack` and
    /// `await inbox re=<off>` wait for.
    ///
    /// DRIVEN BY THE ENDPOINT'S QUEUE, NOT BY AN EVENT. This used to publish
    /// straight from the `inbox-seen` event, and an event exists once: a bridge
    /// reconnecting to the broker drops it with the rest of its mailbox, a
    /// failed publish only logged it, and a relaunched bridge never sees the
    /// ones emitted while it was gone. A receipt given in any of those windows
    /// never reached the sender, and the recipient could not resend it (the
    /// endpoint records a word once per row per change). The queue is a PEEK,
    /// drained after every attach and at every start exactly like the posts, so
    /// nothing in those windows is lost; the event is only the prompt trigger.
    ///
    /// EXACTLY ONCE, by the rule posts keep: the producer sequence is reserved
    /// against `(sid, rid)` BEFORE the publish ([`StateDir::receipt_seq`]) and
    /// reused on every retry, so a bridge that dies between the publish and the
    /// retirement republishes the same `(producer_id, producer_seq)` after its
    /// relaunch and the broker answers `deduped` instead of appending a second
    /// `ack`.
    ///
    /// The line is checked again here, because a line off any socket is input:
    /// a `from=` that is not a principal, a verdict that is not one of the three
    /// words, or a kind that does not wait publishes nothing — and is RETIRED
    /// (`off=-`), as is every receipt a bridge with receipts turned off is handed,
    /// so the queue never holds what this bridge will never send.
    ///
    /// # Errors
    ///
    /// A publish the broker did not take, or a sequence that could not be
    /// reserved: the receipt STAYS OWED and the next drain retries it under the
    /// same sequence — the rule a post the broker cannot take already follows
    /// ("the broker will come back, and the whole point of the queue is to
    /// survive that").
    fn send_receipt(&mut self, r: &OwedReceipt) -> io::Result<()> {
        if !self.cfg.receipts
            || !WAITING_KINDS.contains(&r.kind.as_str())
            || !crate::body::is_verdict(&r.verdict)
        {
            self.retire_receipt(r, None);
            return Ok(());
        }
        let Some(prefix) = self.receipt_lane(&r.from) else {
            self.publish_ev_for(
                Some(&r.sid),
                &format!("unacked re={} verdict={} reason=no-lane", r.off, r.verdict),
            );
            self.retire_receipt(r, None);
            return Ok(());
        };
        let subject = format!("{prefix}/ack");
        let seq = match self.state.receipt_seq(&r.sid, r.rid) {
            Some(seq) => seq,
            None => {
                let seq = self.next_seq()?;
                self.state.set_receipt_seq(&r.sid, r.rid, seq)?;
                seq
            }
        };
        let mut receipt = Body::new(crate::now_ms());
        receipt.re = Some(r.off);
        receipt.from = Some(r.sid.clone());
        receipt.verdict = Some(r.verdict.clone());
        receipt.text = format!("ack re={} verdict={}", r.off, r.verdict);
        let encoded = receipt.encode(None);
        let (at, deduped) = self.publish_at(seq, &subject, &encoded)?;
        // OURS BEFORE ANYTHING CAN SEE IT, as a post's landing is: a receipt
        // for a sender on this node lands on our own lane, and the self-lane
        // check reads this set.
        if subject.starts_with(&format!("/f/{}/in/{}/", self.cfg.fleet, self.node)) {
            self.remember_self_ack(at);
        }
        // A DEDUPED publish is a retry of one a predecessor made — its `ev` is
        // that predecessor's to have written.
        if !deduped {
            self.publish_ev_for(
                Some(&r.sid),
                &format!("ack re={} off={at} verdict={}", r.off, r.verdict),
            );
        }
        self.retire_receipt(r, Some(at));
        Ok(())
    }

    /// `deliver <sid> receipt=<rid> off=<n|->`: the receipt leaves the
    /// endpoint's queue. The pinned sequence is forgotten only once the
    /// endpoint has taken the retirement (or its session is gone, when nothing
    /// will ever list the receipt again) — the file must outlive every moment
    /// a retry could still need it.
    fn retire_receipt(&mut self, r: &OwedReceipt, at: Option<u64>) {
        let off = at.map_or_else(|| "-".to_string(), |n| n.to_string());
        match self.ctl_request(&format!("deliver {} receipt={} off={off}", r.sid, r.rid)) {
            Ok(reply) if reply.ok() || reply.header().starts_with("ERR no such session") => {
                self.state.clear_receipt_seq(&r.sid, r.rid);
            }
            Ok(reply) => eprintln!(
                "aterm-link: retiring receipt {} for {} refused: {}",
                r.rid,
                r.sid,
                reply.header()
            ),
            Err(e) => eprintln!(
                "aterm-link: retiring receipt {} for {} failed: {e}",
                r.rid, r.sid
            ),
        }
    }

    /// The six-segment `in` prefix a receipt for the sender an endpoint row
    /// names as `from=` goes to, or `None` when there is no lane a bridge
    /// drains. The three renderings [`Bridge::render_from`] produces, read
    /// back: `s-<sid>@n-<node>` is that session's lane on that node;
    /// `h-*`/`a-*` hold their own `p/` lane (§6.3); a bare `s-*` is routed
    /// like any address; a bare `n-*` spoke for no session and has no lane.
    fn receipt_lane(&mut self, from: &str) -> Option<String> {
        let fleet = self.cfg.fleet.clone();
        if let Some((sid, node)) = from.split_once('@') {
            if !(sid.starts_with("s-")
                && subject::is_principal(sid)
                && node.starts_with("n-")
                && subject::is_principal(node))
            {
                return None;
            }
            return Some(format!("/f/{fleet}/in/{node}/{sid}/{}", self.node));
        }
        if !subject::is_principal(from) {
            return None;
        }
        if from.starts_with("s-") {
            return match self.resolve_to(&format!("@{from}"), None) {
                Route::To(prefix) => Some(prefix),
                _ => None,
            };
        }
        if from.starts_with("n-") {
            return None;
        }
        Some(format!("/f/{fleet}/in/p/{from}/{}", self.node))
    }

    // -----------------------------------------------------------------------
    // nothing lost (E3): `inbox get @<off>` reads the log through the bridge
    // -----------------------------------------------------------------------

    /// Answer one `fetch sid=<sid> off=<n>` line off the `outbox` peek: the
    /// endpoint has a session parked in `inbox get @<off>` for a record its
    /// ring evicted (or its delivery cut, or its listing skipped), and this is
    /// the one process with bus access. `Fetch{from: off, max: 1}` on THAT
    /// SESSION's own lane — the filter is the authority: a record at `off` that
    /// is not on this session's lane (another session's, another face's, or
    /// nothing at all) answers `err=no-record`, and the endpoint never learns
    /// which. The record is classified by the same code that classified it at
    /// delivery and handed over WHOLE as `deliver <sid> fetched=<off> …` — in
    /// consecutive chunks when it does not fit one control line
    /// ([`fetched_lines`]) — which fills the parked wait and touches no ring.
    ///
    /// A REFUSED ANSWER IS FOLLOWED BY `err=refused`, so the parked read ends
    /// now. It used to be logged and nothing else: the slot stayed pending, the
    /// next drain listed it again, and a record the endpoint would not take
    /// (one over its body bound) was fetched and refused every drain — 35 bus
    /// reads for one read — until the read timed out.
    fn answer_fetch(&mut self, sid: &str, off: u64) {
        let lines = match self
            .fetch_record(sid, off)
            .and_then(|(fields, text)| fetched_lines(sid, off, &fields, &text))
        {
            Ok(lines) => lines,
            Err(why) => vec![format!("deliver {sid} fetched={off} err={why}")],
        };
        for line in lines {
            match self.ctl_request(&line) {
                Ok(reply) if reply.ok() => {}
                Ok(reply) => {
                    eprintln!(
                        "aterm-link: fetched {off} for {sid} refused: {}",
                        reply.header()
                    );
                    if !line.contains(" err=") {
                        let _ =
                            self.ctl_request(&format!("deliver {sid} fetched={off} err=refused"));
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("aterm-link: fetched {off} for {sid} failed: {e}");
                    return;
                }
            }
        }
    }

    /// The record at `off` on `sid`'s lane as the `from= kind= trust= …` fields
    /// of a `fetched=` line and its whole decoded body, or the one-token reason
    /// there is none.
    fn fetch_record(&mut self, sid: &str, off: u64) -> Result<(String, String), &'static str> {
        if self.attachment == Attachment::Observer {
            return Err("observer");
        }
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let Some(conn) = self.conn.as_mut() else {
            return Err("unreadable");
        };
        let started = Instant::now();
        let answer = conn.fetch(off, &filter, 1);
        let (rows, _) = self
            .observe(started, answer, Some("read"))
            .map_err(|_| "unreadable")?;
        let Some((at, subj, raw)) = rows.into_iter().next() else {
            return Err("no-record");
        };
        if at != off {
            return Err("no-record");
        }
        let addr =
            subject::parse_in(&self.cfg.fleet, &self.node, &subj).map_err(|_| "malformed")?;
        if addr.sid != sid {
            return Err("no-record");
        }
        if addr.src == self.node && self.is_forged_self(off) {
            return Err("forged-self");
        }
        let (body, _) = Body::decode(&raw);
        if !body.via.as_deref().is_none_or(via_ok) {
            return Err("via");
        }
        let (kind, demoted, _) = self.classify_delivery(&addr, &body);
        let trust = trust_of(&addr.src, body.via.is_some());
        let from = self.render_from(&addr, &body);
        let mut fields = format!("from={from} kind={kind} trust={trust}");
        if let Some(re) = body.re {
            fields.push_str(&format!(" re={re}"));
        }
        if let Some(dl) = body.dl {
            fields.push_str(&format!(" dl={dl}"));
        }
        if let Some(d) = demoted {
            fields.push_str(&format!(" demoted={d}"));
        }
        if let Some(via) = &body.via {
            fields.push_str(&format!(" via={via}"));
        }
        if kind == "ack" {
            if let Some(v) = &body.verdict {
                fields.push_str(&format!(" verdict={v}"));
            }
        }
        Ok((fields, body.text))
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
        if let Err(e) = self.publish_own_off(subject, body) {
            eprintln!("aterm-link: could not publish {subject}: {e}");
        }
    }

    /// [`Bridge::publish_own`] answering the offset — for the one caller that
    /// must NOT retire its own bookkeeping on a publish that never happened
    /// ([`Bridge::expire_deadlines`]).
    ///
    /// # Errors
    ///
    /// The publish's.
    fn publish_own_off(&mut self, subject: &str, body: &[u8]) -> io::Result<u64> {
        let mine = format!("/f/{}/in/{}/", self.cfg.fleet, self.node);
        let off = self.publish(subject, body)?;
        if subject.starts_with(&mine) {
            self.remember_self_ack(off);
        }
        Ok(off)
    }

    // -----------------------------------------------------------------------
    // deadlines (R8): the asker's own bridge records `expired`
    // -----------------------------------------------------------------------

    /// Remember that the ask at `off` expires `dl` ms from now, unless it is
    /// already remembered (a deduped re-post does not restart the clock), and
    /// whose reply settles it.
    fn note_deadline(&mut self, sid: &str, off: u64, dl: u64, to: Option<String>) {
        if self.deadlines.contains_key(&off) {
            return;
        }
        self.deadlines.insert(
            off,
            Deadline {
                off,
                sid: sid.to_string(),
                at: crate::now_ms().saturating_add(dl),
                dl,
                to,
            },
        );
        while self.deadlines.len() > DEADLINES_KEEP {
            self.deadlines.pop_first();
        }
        self.persist_deadlines();
    }

    /// Remember whose reply the ask at `off` is ([`Bridge::asked`]),
    /// durably, keeping the newest [`ASKED_KEEP`]. A failed write is said and
    /// survived: the in-memory table still judges this process's replies.
    fn note_asked(&mut self, off: u64, to: &str) {
        self.asked.insert(off, to.to_string());
        while self.asked.len() > ASKED_KEEP {
            self.asked.pop_first();
        }
        let list: Vec<Asked> = self
            .asked
            .iter()
            .map(|(off, to)| Asked {
                off: *off,
                to: to.clone(),
            })
            .collect();
        if let Err(e) = self.state.set_asked(&list) {
            eprintln!("aterm-link: could not persist the asked table: {e}");
        }
    }

    /// Whether a reply from `src` naming the ask at `off` is from the
    /// principal that ask went to: `Some(true)`/`Some(false)` when this
    /// bridge knows where it went (the asked table, else the deadline's own
    /// record), `None` when it does not — which is judged as before.
    fn reply_from(&self, off: u64, src: &str) -> Option<bool> {
        self.asked
            .get(&off)
            .cloned()
            .or_else(|| self.deadlines.get(&off).and_then(|d| d.to.clone()))
            .map(|to| to == src)
    }

    /// Forget the deadline of the ask at `off` — when `sid` ASKED it, and only
    /// then: a reply settles the question it answers on the asker's own lane,
    /// never one delivered to another session that happens to name the same
    /// offset.
    fn settle_deadline_for(&mut self, sid: &str, off: u64) {
        if self.deadlines.get(&off).is_some_and(|d| d.sid == sid) {
            self.deadlines.remove(&off);
            self.persist_deadlines();
        }
    }

    /// Write the table down. A failure is said and survived: the in-memory
    /// table still drives this process, and the bus check below is what keeps
    /// a stale durable copy from ever recording a false verdict.
    fn persist_deadlines(&mut self) {
        let list: Vec<Deadline> = self.deadlines.values().cloned().collect();
        if let Err(e) = self.state.set_deadlines(&list) {
            eprintln!("aterm-link: could not persist the deadline table: {e}");
        }
    }

    /// Remember an ask this bridge recorded `expired` for, so a reply that
    /// arrives afterwards is delivered `late=1`. Bounded at [`EXPIRED_KEEP`].
    fn remember_expired(&mut self, off: u64, sid: &str) {
        self.expired.insert(off, sid.to_string());
        while self.expired.len() > EXPIRED_KEEP {
            self.expired.pop_first();
        }
    }

    /// THE SWEEP. For every deadline that has passed: ask the BUS whether the
    /// asker's lane already holds a reply (or a verdict) naming that offset,
    /// and only when it holds none publish `expired re=<off> dl=<ms>` onto the
    /// asker's own inbox lane — under this node's own `<src>`, which is what
    /// makes it a verdict the asker's endpoint can trust (`POSTABLE` refuses a
    /// sender the kind).
    ///
    /// THE BUS IS READ FIRST, EVERY TIME, and that is the whole correctness
    /// argument for a durable table: the table can outlive an answer (a crash
    /// between the delivery that settled it and the write that recorded that),
    /// and a verdict published from the table alone would then tell an agent
    /// its answered question went unanswered. The read is bounded — one lane,
    /// from the ask's offset to the head, in pages — and a read that FAILS
    /// publishes nothing this tick: an unknown is not a `no`.
    ///
    /// EXACTLY ONE `expired` PER ASK, by the same read: a verdict this bridge
    /// (or a dead predecessor) already put on the lane is found there and the
    /// entry is retired without a second one.
    fn expire_deadlines(&mut self) {
        if self.conn.is_none() {
            return;
        }
        let now = crate::now_ms();
        let due: Vec<Deadline> = self
            .deadlines
            .values()
            .filter(|d| d.at <= now)
            .take(DEADLINE_SWEEP_MAX)
            .cloned()
            .collect();
        if due.is_empty() {
            return;
        }
        let mut changed = false;
        for d in due {
            // A session that is gone gets no verdict: nobody drains its lane.
            if !self.epochs.contains_key(&d.sid) {
                self.deadlines.remove(&d.off);
                changed = true;
                continue;
            }
            match self.settled_on_bus(&d.sid, d.off) {
                Err(e) => {
                    eprintln!("aterm-link: deadline sweep stopped at {}: {e}", d.off);
                    break;
                }
                Ok(Some(kind)) => {
                    if kind == "expired" {
                        self.remember_expired(d.off, &d.sid);
                    }
                    self.deadlines.remove(&d.off);
                    changed = true;
                }
                Ok(None) => {
                    let subject = format!(
                        "/f/{}/in/{}/{}/{}/expired",
                        self.cfg.fleet, self.node, d.sid, self.node
                    );
                    let mut verdict = Body::new(now);
                    verdict.re = Some(d.off);
                    verdict.dl = Some(d.dl);
                    verdict.text = format!("expired re={} dl={}", d.off, d.dl);
                    let encoded = verdict.encode(None);
                    match self.publish_own_off(&subject, &encoded) {
                        Ok(at) => {
                            self.remember_expired(d.off, &d.sid);
                            self.publish_ev_for(
                                Some(&d.sid),
                                &format!("expired re={} off={at} dl={}", d.off, d.dl),
                            );
                            self.deadlines.remove(&d.off);
                            changed = true;
                        }
                        Err(e) => {
                            // The entry stays: the next sweep asks the bus
                            // again, and a publish that DID land is found there.
                            eprintln!("aterm-link: could not record expired for {}: {e}", d.off);
                            break;
                        }
                    }
                }
            }
        }
        if changed {
            self.persist_deadlines();
        }
    }

    /// Whether the asker's lane already holds a record that settles the ask at
    /// `off` — one of [`ANSWER_KINDS`], or an `expired` verdict — and which.
    ///
    /// # Errors
    ///
    /// A read the broker refused or the link dropped: "unknown", never "no".
    fn settled_on_bus(&mut self, sid: &str, off: u64) -> io::Result<Option<&'static str>> {
        let filter = format!("/f/{}/in/{}/{sid}/>", self.cfg.fleet, self.node);
        let mut cursor = off.saturating_add(1);
        loop {
            let Some(conn) = self.conn.as_mut() else {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "the broker is unreachable",
                ));
            };
            let started = Instant::now();
            let answer = conn.fetch(cursor, &filter, 256);
            let (rows, (next, head)) = self.observe(started, answer, Some("read"))?;
            for (_, subj, raw) in rows {
                let kind = subj.rsplit('/').next().unwrap_or("");
                let settles = ANSWER_KINDS
                    .iter()
                    .chain(std::iter::once(&"expired"))
                    .find(|k| **k == kind)
                    .copied();
                // `/f/<F>/in/<node>/<sid>/<src>/<kind>`: the record SETTLES
                // only from the recipient (a reply) or from this node (its
                // own `expired`) — the rule delivery applies, read off the
                // bus the same way, so the sweep and the delivery cannot
                // disagree about a stray `ack re=<off>`.
                let src = subj.split('/').nth(6).unwrap_or("");
                let from_right = if kind == "expired" {
                    src == self.node
                } else {
                    self.reply_from(off, src) != Some(false)
                };
                if let Some(k) = settles.filter(|_| from_right) {
                    if Body::decode(&raw).0.re == Some(off) {
                        return Ok(Some(k));
                    }
                }
            }
            if next >= head || next <= cursor {
                return Ok(None);
            }
            cursor = next;
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
        let started = Instant::now();
        let answer = conn.commit(&group, off);
        match self.observe(started, answer, Some("read")) {
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
        // and it is written here because the design file still describes the
        // per-barrier shape. (`tui.rs`'s `/barrier` text did too, until round 21
        // deleted the module.)
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
    /// after a crash ([`Bridge::read_fleet_halts`]), where the endpoint is holding
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
    ///
    /// ## Two halves, on purpose
    ///
    /// [`Bridge::read_fleet_halts`] is the READ and [`Bridge::apply_fleet_halts`]
    /// the APPLY, and the attach runs them either side of
    /// [`Bridge::bring_presence_up`]: the read FIRST, because it is the one
    /// exchange the bridge cannot attach without and a cap the broker refuses
    /// it under must fail before a single record is written (the `attaching`
    /// field has the measurement); the apply AFTER, because the observer's
    /// `hold-skipped` ev is a publish, and a publish before presence is up
    /// would run in the previous incarnation's sequence space. A halt
    /// published between the two arrives on the fleet subscription, which
    /// resumes at the mark THIS read returns — gap-free across the seam, so
    /// the window between the snapshot and the subscribe is covered without
    /// replaying the fleet's whole history to cover it (round 21).
    fn read_fleet_halts(&mut self) -> Option<(Vec<BrokerRecord>, u64)> {
        let filter = format!("/f/{}/fleet/*/halt", self.cfg.fleet);
        let conn = self.conn.as_mut()?;
        // A WALK THAT DID NOT FINISH IS NOT AN ANSWER, and this is the one
        // caller where that distinction lifts a fleet halt. The old loop stopped
        // on an empty page, which the broker's own API says is not the end —
        // `/f/<F>/fleet/` shares an index with a subtree that grows one entry per
        // (member, barrier) and per session ever spawned, so a scan-bound cut is
        // reachable — and the "no halt row exists" that follows an incomplete
        // walk issues `hold off` to every session this bridge hosts.
        let started = Instant::now();
        let answer = last_all(conn, &filter);
        self.observe(started, answer, Some("read")).ok()
    }

    /// WHERE THE BROADCAST FACE SUBSCRIBES FROM: the lowest cursor any local
    /// session's topic still owes, or the broker's HEAD when none does.
    ///
    /// The head is read with `fetch(.., max = 0)` — the broker's head query, one
    /// round trip that returns no records — because `Last` would answer the
    /// newest record PER SUBJECT, and a broadcast topic reuses one subject per
    /// (sender, topic): its last-value answer is not a backlog and its mark is
    /// not this face's head.
    ///
    /// `None` only when the head cannot be read, which is an attach failure like
    /// any other: subscribing from 0 instead would replay the fleet's entire
    /// broadcast history into every session that had asked for a topic.
    ///
    /// Answers BOTH numbers — `(from, head)` — because they are different after
    /// a restart with a gap and the caller needs each for its own thing: the
    /// subscription starts at `from`, and `since=head` resolves to `head`.
    fn broadcast_floor(&mut self, filter: &str) -> Option<(u64, u64)> {
        let owed = self
            .topics
            .values()
            .flat_map(|by_topic| by_topic.values().map(|c| c.next))
            .min();
        let conn = self.conn.as_mut()?;
        let started = Instant::now();
        let head = conn.fetch(0, filter, 0).map(|(_, (_, head))| head);
        let head = self.observe(started, head, Some("read")).ok()?;
        Some((owed.map_or(head, |owed| owed.min(head)), head))
    }

    /// The second half of [`Bridge::read_fleet_halts`]: the standing halts
    /// REBUILT FROM THE BUS, not merged into what this process remembered —
    /// the retained rows ARE the standing state, and a human whose row is gone
    /// is a human who is not halting — and the endpoint's holds written to
    /// agree.
    fn apply_fleet_halts(&mut self, rows: Vec<(u64, String, Vec<u8>)>) {
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
    }

    /// ONE SAMPLE OF WHAT THE INSTANCE SAYS ABOUT EVERY SESSION IT HOSTS, on
    /// the roster tick.
    ///
    /// What it reads, per session: the `status` reply's `revision=` (which
    /// gates the screen read that produces `phase=` and `context=`), its
    /// `hold=` (reconciled against this bridge's own view, the one thing that
    /// catches a drop guard landing after a reconcile), its `detail=`, and the
    /// session's `meta attention=`. A10's notifier and A8's glance read
    /// `attention=` off the presence row and nothing on the bus announces a
    /// local `meta set attention`, so it is sampled here, on a round that is
    /// already paying for a `status`.
    ///
    /// IT WAS THE LOCAL HALF OF §6.6. Two of that table's rows were local
    /// observations rather than bus events — row 4, the conservative pause that
    /// parked a session at `human?` when something moved it that the bridge had
    /// not caused, and row 5, the mirror of a local socket driver's `lease
    /// acquire` onto the bus as `holder=owner-cli:<h>` — and both existed only
    /// to decide who a `term/in` would be accepted from. Round 21 cut the drive
    /// face, and `watch_held_control`, `baseline_local`, `DRIVEN_KEEP`,
    /// `SETTLE_QUIET` and the `driven`/`settled`/`unaccounted` accounting went
    /// with it. What is left is a sampler, not a policy.
    fn sample_local_control(&mut self) {
        if self.attachment == Attachment::Observer {
            return;
        }
        let sids: Vec<String> = self.locals.values().cloned().collect();
        for sid in sids {
            self.observe_local_control(&sid);
            // AND THE BROADCAST OPT-INS. The set lives in aterm (`topic add`),
            // nothing on the bus announces a change to it, and it is a LEVEL
            // like `attention=` — still true when it is read late — so the
            // roster round is where it is sampled, on a tick that is already
            // paying for a round trip per session. The cost of the latency is
            // stated where the verb is documented: a topic added between two
            // rounds takes effect on the next one, within [`ROSTER_REFRESH`].
            self.sample_topics(&sid);

            // THE ESCALATION IS A ROSTER OBSERVATION TOO — and so is the rest
            // of the row's meaning. A10's notifier and A8's glance read
            // `attention=` off the presence row, and a session sets it locally
            // with `meta set attention …` — nothing on the bus announces that.
            // It is sampled here, on the round that already pays for a
            // `status`, together with `role=`, `title=` (the same `meta`),
            // `detail=` (that `status`) and — only when the `status revision=`
            // moved since the last read — `phase=` and `context=` off the
            // screen's tail. The row is republished ONLY when a field moved
            // and the row on the bus is a window old ([`Slot::due`]): a
            // retained face rewritten on a timer is an unbounded write for no
            // new information.
            //
            // The roster deadline is the right one for all of these: every
            // one is a LEVEL. It is still true when it is read late, so a
            // slower sampler sees it late rather than not at all.
            //
            // A SESSION STILL QUEUED FOR ADMISSION IS LEFT TO THE ADMISSION,
            // which runs right after this on the same round and publishes its
            // first row (sampled) itself: sampling and publishing it here too
            // put two identical `live` records on the log for every session
            // that existed when the bridge attached.
            if self.pending_admit.contains(&sid) {
                continue;
            }
            self.sample_presence(&sid);
            let mode = self.cfg.presence;
            if self
                .presence
                .get(&sid)
                .is_some_and(|slot| slot.due(mode, Instant::now()).now())
            {
                self.publish_session_presence(&sid, "live");
            }
        }
    }

    /// ONE session's BROADCAST OPT-INS, off one `topic ls` — and the cursor
    /// each one resumes from.
    ///
    /// THE RESOLUTION HAPPENS ONCE PER ADD, HERE. aterm answers an INTENT
    /// (`since=head` or `since=@<off>`); a cursor is a position, and only this
    /// side knows where the bus is. An entry whose `serial=` this bridge already
    /// holds keeps its cursor whatever the session still says, because by then
    /// records have been delivered against it — re-reading `since=@0` off a
    /// standing `ls` would replay the whole backlog on every roster round.
    ///
    /// PER ADD, NOT PER NAME: `serial=` is the endpoint's serial for the add, so
    /// `drop` then `add` under one name inside one roster round is a different
    /// entry here, and its `since=` is read.
    ///
    /// AN ENDPOINT THAT DOES NOT KNOW THE VERB ANSWERS `ERR`, and that is not a
    /// reason to forget what this bridge is holding: a downgrade mid-flight
    /// would otherwise drop every cursor and every opt-in with it. Only an `OK`
    /// — the session's own answer — replaces the set. An endpoint too old to
    /// print `serial=` reads as generation 0, which is stable, so it keeps the
    /// pre-`serial` behaviour rather than re-resolving every round.
    fn sample_topics(&mut self, sid: &str) {
        let Ok(reply) = self.ctl_request(&format!("@{sid} topic ls")) else {
            return;
        };
        if !reply.ok() {
            return;
        }
        let have = self.topics.get(sid).cloned().unwrap_or_default();
        let head = self.say_bus_head;
        let mut wanted: BTreeMap<String, TopicCursor> = BTreeMap::new();
        for row in reply.rows() {
            let mut words = row.split_whitespace();
            if words.next() != Some("topic") {
                continue;
            }
            let Some(topic) = words.next().filter(|t| subject::is_topic(t)) else {
                continue;
            };
            let mut since = "head";
            let mut serial = 0u64;
            for word in words {
                if let Some(v) = word.strip_prefix("since=") {
                    since = v;
                } else if let Some(v) = word.strip_prefix("serial=") {
                    serial = v.parse().unwrap_or(0);
                }
            }
            let next = have
                .get(topic)
                .filter(|held| held.serial == serial)
                .map_or_else(
                    || {
                        since
                            .strip_prefix('@')
                            .and_then(|n| n.parse::<u64>().ok())
                            .unwrap_or(head)
                    },
                    |held| held.next,
                );
            wanted.insert(topic.to_string(), TopicCursor { next, serial });
        }
        if wanted != have {
            if wanted.is_empty() {
                self.topics.remove(sid);
                self.state.forget_topics(sid);
            } else {
                self.topics.insert(sid.to_string(), wanted);
                self.persist_topics(sid);
            }
        }
        self.catch_up_topics(sid);
    }

    /// THE LATE SUBSCRIBER'S BACKLOG: everything between a topic's cursor and
    /// the live face's drain position, read off the log and delivered.
    ///
    /// The live subscription covers `[say_drained_to, ∞)`; a `topic add …
    /// since=@<n>` with `n` below that names records the subscription has
    /// already passed, and a subscription re-anchored to reach them would
    /// re-deliver the whole interval to every OTHER session as well. So the gap
    /// is read directly, with the same bounded `Fetch` the refill uses.
    ///
    /// ONE WALK FOR ALL OF THIS SESSION'S OWED TOPICS, over the same filter
    /// the live face uses, so a replay arrives in offset order exactly as the
    /// live path does.
    ///
    /// BOUNDED, AND RESUMABLE: at most [`SAY_REPLAY_PAGES`] pages per session
    /// per roster round, and every cursor is persisted as it moves, so a deep
    /// backlog is caught up over several rounds — and across a restart —
    /// instead of parking the scheduling loop, which would delay the halt this
    /// whole mailbox is ordered around.
    fn catch_up_topics(&mut self, sid: &str) {
        let filter = subject::say_filter(&self.cfg.fleet);
        for _ in 0..SAY_REPLAY_PAGES {
            let owed: BTreeMap<String, u64> = self
                .topics
                .get(sid)
                .map(|by_topic| {
                    by_topic
                        .iter()
                        .filter(|(_, c)| c.next < self.say_drained_to)
                        .map(|(t, c)| (t.clone(), c.next))
                        .collect()
                })
                .unwrap_or_default();
            let Some(from) = owed.values().copied().min() else {
                return;
            };
            let Some(conn) = self.conn.as_mut() else {
                return;
            };
            let started = Instant::now();
            let page = conn.fetch(from, &filter, 256);
            let Ok((rows, (next, _))) = self.observe(started, page, Some("read")) else {
                return;
            };
            for (off, subject, raw) in rows {
                // STRICTLY BELOW THE DRAIN POSITION. A record at or past it is
                // one the subscription will deliver (or already has), and
                // delivering it here too would spend a `deliver` the ring is
                // only going to dedup.
                if off >= self.say_drained_to {
                    break;
                }
                let Some(say) = subject::parse_say(&self.cfg.fleet, &subject) else {
                    continue;
                };
                let Some(expect) = owed.get(&say.topic).copied().filter(|c| *c <= off) else {
                    continue;
                };
                let (body, _raw_tail) = Body::decode(&raw);
                if !body.via.as_deref().is_none_or(via_ok) {
                    continue;
                }
                if self.deliver_broadcast(sid, &say, &body, off, expect) == Delivery::Unaccounted {
                    return;
                }
            }
            // `next` is one past the last offset the broker SCANNED, so the
            // walk advances even through a stretch that matched nothing — and
            // every owed topic has now been read up to there, whether or not
            // this page held anything for it.
            let reached = next.max(from.saturating_add(1)).min(self.say_drained_to);
            for (topic, expect) in owed {
                let now = self
                    .topics
                    .get(sid)
                    .and_then(|m| m.get(&topic))
                    .map_or(expect, |c| c.next);
                self.advance_topic(sid, &topic, now, reached);
            }
        }
    }

    /// ONE session's local observation, off one `status`: the `detail=` its
    /// presence row carries, the `revision` the screen reader is gated on, and
    /// the endpoint's own `hold=` reconciled against this bridge's view.
    ///
    /// A STATUS THAT CANNOT BE READ RECONCILES NOTHING — an unread state is not
    /// a disagreement, and the sample is taken FIRST so a `None` returns before
    /// any state moves: `seen` and `revision` are left exactly as the last real
    /// observation left them ([`Bridge::status_sample`]).
    ///
    /// Round 21 cut the rest. This used to be §6.6's rows 4 and 5 — the
    /// conservative pause that parked a driven row at `human?`, and the mirror
    /// of aterm's own local lease onto the bus — and both existed only to
    /// decide who was allowed to send a `term/in`. With the drive face gone
    /// there is no such decision to make, and `moved_at`, `driven_at`,
    /// `DRIVEN_KEEP`, `SETTLE_QUIET` and the whole `match holder` went with it.
    fn observe_local_control(&mut self, sid: &str) {
        let Some(sample) = self.status_sample(sid) else {
            return;
        };
        let (revision, held) = (sample.revision, sample.hold);
        // `detail=` for the presence row, off the read already paid for.
        self.presence
            .entry(sid.to_string())
            .or_default()
            .fields
            .set_detail(Some(&sample.detail));
        self.converge_hold(sid, held);
        let entry = self.local.entry(sid.to_string()).or_default();
        entry.seen = true;
        if revision > entry.revision {
            entry.revision = revision;
        }
    }

    /// RETIRE THE PRESENCE OF A SESSION NOTHING HOSTS — the ghost row, and the
    /// permanent WARNING it produces.
    ///
    /// ## The hole this fills
    ///
    /// Every ordinary exit publishes `state=exited`: the push lane's
    /// `session-exited` line does it ([`Bridge::on_event`]), and
    /// [`Bridge::refresh_sessions`] does it by LOOKING, for the line that never
    /// arrives. Neither runs when the whole instance dies. The only `Will` this
    /// crate registers is on the NODE face ([`Bridge::bring_presence_up`]), so
    /// the node row flips to `state=gone` and every SESSION row under it stays
    /// retained at `state=live` — forever, because presence is a last-value
    /// face and nothing supersedes a row whose writer is dead.
    ///
    /// Measured on the owner's machine 2026-09-20: `aterm fabric status` had
    /// been reporting, with no way to ever stop,
    ///
    /// ```text
    /// ! the bus advertises @s-d5a5de326f33873b6167 live on n-1b631315bf5cae35 —
    ///   this machine's node — but no aterm instance here hosts it: mail to it
    ///   is undeliverable
    /// ```
    ///
    /// and the warning is RIGHT — mail to that sid is undeliverable. What was
    /// missing is anyone to fix it.
    ///
    /// ## Why it is safe to write someone else's row
    ///
    /// A session face names a node, and this bridge only ever publishes under
    /// its own ([`Bridge::publish_session_presence`]), so the subject is one it
    /// already owns. What it must not do is retire a row belonging to a LIVE
    /// sibling — two bridges CAN share a state dir, and hence a node id, and
    /// `fabric.rs` records a measured case of exactly that. Three conditions
    /// together rule it out:
    ///
    /// * the sid is absent from this bridge's own roster, and
    /// * it has been absent across [`GHOST_AFTER`] of THIS bridge's
    ///   observations — not merely old on the bus, which a quietly busy
    ///   session's row also is, and
    /// * the row's `inc=` is at or below this bridge's own incarnation. A
    ///   bridge that attached after this one took `max(local, bus) + 1` and so
    ///   carries a STRICTLY HIGHER `inc`; leaving those alone means a newer
    ///   sibling's live sessions are never touched, and the worst this sweep
    ///   can do to a node it has been superseded on is nothing.
    ///
    /// A retirement is one record per ghost, once — the sid is dropped from
    /// [`Bridge::ghosts`] after it is published, and the row it wrote is no
    /// longer `state=live` so the next sweep does not see it. That is the same
    /// bound presence keeps everywhere else: a last-value face on an
    /// append-forever log may not be republished on a clock.
    fn retire_ghost_presence(&mut self) {
        let filter = format!("/f/{}/pub/{}/*/presence", self.cfg.fleet, self.node);
        let rows = {
            let Some(conn) = self.conn.as_mut() else {
                return;
            };
            let started = Instant::now();
            let answer = last_all(conn, &filter).map(|(rows, _)| rows);
            match self.observe(started, answer, Some("read")) {
                Ok(rows) => rows,
                Err(e) => {
                    eprintln!("aterm-link: could not sweep the presence roster: {e}");
                    return;
                }
            }
        };
        let hosted: BTreeSet<String> = self.locals.values().cloned().collect();
        let now = Instant::now();
        // [`Fault::SweepGhostsAtOnceWhileMarked`] collapses the WAIT and
        // nothing else.
        let patience = if self.fault == Fault::SweepGhostsAtOnceWhileMarked
            && self.state.root().join("sweep-ghosts-now").exists()
        {
            Duration::ZERO
        } else {
            GHOST_AFTER
        };
        let mut unhosted: BTreeSet<String> = BTreeSet::new();
        let mut retire: Vec<String> = Vec::new();
        for (_, subject, raw) in &rows {
            // `["", "f", "<F>", "pub", "<node>", "<sid>", "presence"]` — the
            // node face is `<sid>` = `node` and is not a session.
            let segs: Vec<&str> = subject.split('/').collect();
            if segs.len() != 7 || segs[3] != "pub" || segs[6] != "presence" {
                continue;
            }
            let sid = segs[5];
            if sid == "node" || hosted.contains(sid) {
                continue;
            }
            let (body, _) = Body::decode(raw);
            if body.unknown.get("state").map(String::as_str) != Some("live") {
                continue;
            }
            // ONLY WHAT THIS INCARNATION CAN PROVE UNHOSTED.
            if !adoptable(
                body.unknown.get("inc").and_then(|v| v.parse::<u64>().ok()),
                self.inc,
                self.witnessed_dead,
            ) {
                continue;
            }
            unhosted.insert(sid.to_string());
            let since = *self.ghosts.entry(sid.to_string()).or_insert(now);
            if now.saturating_duration_since(since) >= patience {
                retire.push(sid.to_string());
            }
        }
        // A SID THAT CAME BACK STOPS BEING A GHOST. The clock is "continuously
        // unhosted", so anything not seen unhosted this round loses its date.
        self.ghosts.retain(|sid, _| unhosted.contains(sid));
        for sid in retire {
            self.publish_session_presence(&sid, "exited");
            self.publish_ev(&format!("presence-retired sid={sid} reason=unhosted"));
            self.ghosts.remove(&sid);
        }
    }

    /// THE PERIODIC BACKSTOP: the local observations, the roster re-read, the
    /// halt reassert for anything new, and the outbox drain.
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
    /// sample first — it is where a stale `fabric-lost` hold is corrected — and
    /// the roster next, so a session admitted this round is admitted against a
    /// hold this round has already reconciled.
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

    /// The session's `status revision=`, `hold=` and `detail=`, or `None`
    /// when the verb could not be read at all.
    ///
    /// `None` IS NOT A ZERO, and no caller may turn it into one: `revision` is
    /// compared against the last one seen, so a failed read folded into `0`
    /// makes the next successful read an advance. See
    /// [`Bridge::observe_local_control`].
    ///
    /// `detail=` rides the same reply — it is the sanitized running command
    /// `ls` prints (`control_session.rs`'s F5 column, one value for both
    /// verbs) — and [`Bridge::observe_local_control`] hands it to the presence
    /// slot, so the row's `detail=` costs no read of its own.
    fn status_sample(&mut self, sid: &str) -> Option<StatusSample> {
        if self.fault == Fault::FailStatusWhileMarked
            && self.state.root().join("fail-status").exists()
        {
            return None;
        }
        let reply = self.ctl_request(&format!("@{sid} status")).ok()?;
        if !reply.ok() {
            return None;
        }
        let mut sample = StatusSample::default();
        for tok in reply.header().split_whitespace() {
            if let Some(v) = tok.strip_prefix("revision=") {
                sample.revision = v.parse().unwrap_or(0);
            } else if let Some(v) = tok.strip_prefix("hold=") {
                sample.hold = v == "1";
            } else if let Some(v) = tok.strip_prefix("detail=") {
                sample.detail = v.to_string();
            }
        }
        Some(sample)
    }

    /// The session's `meta` header, when it answers.
    fn meta_header(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} meta")).ok()?;
        reply.ok().then(|| reply.header().to_string())
    }

    /// The last [`presence::TAIL_ROWS`] rows of the session's screen, when it
    /// answers — the rows the phase reader is shown, and NOTHING of them
    /// leaves [`Fields::read_screen`] but a word and a number.
    fn screen_tail(&mut self, sid: &str) -> Option<Vec<String>> {
        let reply = self
            .ctl_request(&format!("@{sid} text tail={}", presence::TAIL_ROWS))
            .ok()?;
        reply.ok().then(|| reply.rows().to_vec())
    }

    /// Sample the meaning fields of one session's presence row into its
    /// [`Slot`]: `attention=`, `role=` and `title=` from `meta`; `detail=` is
    /// already there from the round's `status` ([`Bridge::status_sample`]);
    /// `phase=` and `context=` from the screen's tail — read ONLY when the
    /// `status revision=` the last read was taken at has moved
    /// ([`Slot::needs_screen`]), so an idle session costs no screen read, and
    /// never in `minimal` mode. A read that fails leaves the field as it was.
    ///
    /// The revision is the one [`Bridge::observe_local_control`] recorded this
    /// round: aterm's classifier bumps it on every phase or detail change it
    /// publishes — output starting is `running` at once, output stopping is
    /// `quiet` after its 5 s `quiet_after` — so a Claude Code turn that ends
    /// moves it within 5 s, which is the edge that makes the next round
    /// re-read (measured in `tests/r13_presence.rs`: `phase=idle` on the bus
    /// 8 s after the screen showed it).
    fn sample_presence(&mut self, sid: &str) {
        let mode = self.cfg.presence;
        let revision = self.local.get(sid).filter(|s| s.seen).map(|s| s.revision);
        let meta = self.meta_header(sid);
        let screen = if mode == Mode::Meta
            && self
                .presence
                .get(sid)
                .is_none_or(|slot| slot.needs_screen(revision))
        {
            self.screen_tail(sid)
        } else {
            None
        };
        let slot = self.presence.entry(sid.to_string()).or_default();
        slot.sampled = true;
        if let Some(header) = meta {
            slot.fields.read_meta(&header);
        }
        if let Some(rows) = screen {
            slot.fields.read_screen(&rows);
            slot.text_rev = revision;
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
    /// say the opposite, which is an invitation to build a `serial=`-fenced
    /// approval out of a `hash=` a driver already has and watch every record it
    /// fences be refused `reason=gen` for a units mismatch two doc comments
    /// asserted away.
    ///
    /// `None` when the screen cannot be read, and that REFUSES a `serial=`-bearing
    /// record rather than admitting it — an unreadable screen is not a matching
    /// one.
    fn live_gen(&mut self, sid: &str) -> Option<String> {
        let reply = self.ctl_request(&format!("@{sid} text --json")).ok()?;
        gen_of_frame(reply.rows().first()?)
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
        let drain = parse_drain(&drained);
        // THE FETCHES FIRST: each is a session parked in `inbox get @<off>`
        // with a bounded wait, and answering one is a single bounded read.
        // A fetch resolves no address — it reads the parked session's OWN lane
        // on this node, and the endpoint refuses an answer for a session that
        // has gone — so it neither needs nor waits on the roster read below.
        for (sid, off) in drain.fetches {
            self.answer_fetch(&sid, off);
        }
        if drain.receipts.is_empty() && drain.posts.is_empty() {
            return;
        }
        // THE ROSTER IS RE-READ BEFORE A SINGLE ADDRESS IS RESOLVED, and that is
        // the difference between asking the endpoint and remembering it.
        //
        // [`Bridge::resolve_to`] answers `@s-<sid>` from `epochs`, and `epochs`
        // is a CACHE the push lane moves. Its "ASK BEFORE GIVING UP" refresh is
        // one-sided: it re-reads only when the sid is ABSENT, so a stale
        // presence in the map — a session that exited while this loop was busy,
        // the window between aterm dropping it and the `session-exited` item
        // being taken off the mailbox — routed the post to the node's own `in`
        // face and the endpoint was told `off=<n>`: LANDED, for a session that
        // no longer exists and will never fetch it. Measured 2026-09-14 on a
        // loaded machine: 2 of 16 concurrent runs, the drain resolving
        // `hosted=true` 67 ms before the bridge took `session-exited` off its
        // own queue. A door that gives up must ask; a door that COMMITS must ask
        // too, and this is the only side that can lose a message.
        //
        // BEFORE THE RECEIPTS AS WELL AS THE POSTS. A receipt to a bare `s-*`
        // sender is routed by the same `resolve_to` ([`Bridge::receipt_lane`]),
        // and a keyed post is resolved BEFORE its key chooses a sequence: an
        // address the live roster no longer lists is retired `unroutable` and
        // publishes nothing, and its key keeps naming whatever record it named
        // — the broker's dedup, not the route, is what makes a re-post under it
        // one record, so a later re-post that does route still answers the
        // first record's offset `dup=1`. (A receipt to `s-<sid>@n-<node>` names
        // its lane outright and reads no roster: the sender's own node accounts
        // for a sender that has gone, as `undeliverable reason=not-hosted`.)
        // The same read prunes the deadline table to the askers still here.
        //
        // ONE READ PER DRAIN, not one per post, and only when there is something
        // to resolve: an idle bridge pays nothing and a draining one pays a
        // bounded local round trip it is already making three of. A roster that
        // cannot be read at all ends the drain rather than resolving against the
        // last one — the endpoint is unreachable, so the retirement this drain
        // would have to write could not be taken either, and the next drain is
        // 250 ms away. The receipts and posts it held stay owed and queued at
        // the endpoint, exactly as a publish the broker refused leaves them.
        if let Err(e) = self.refresh_sessions() {
            eprintln!("aterm-link: the roster could not be re-read before a drain: {e}");
            return;
        }
        // THEN THE OWED RECEIPTS (R8), before the posts, so a post the broker
        // keeps refusing cannot starve a decision a session already made. One
        // that could not be published stays owed; the rest wait for the next
        // drain with it, in order.
        for receipt in drain.receipts {
            if let Err(e) = self.send_receipt(&receipt) {
                eprintln!(
                    "aterm-link: could not publish the receipt for {} ({}): {e}",
                    receipt.off, receipt.sid
                );
                break;
            }
        }
        for post in drain.posts {
            let routed = self.resolve_to(&post.to, Some(&post.sid));
            match routed {
                Route::To(_) | Route::Exact(_) => {
                    // A directed message spends its last segment on the kind; a
                    // broadcast spends it on the TOPIC and carries the kind in
                    // the body.
                    let (subject, in_body) = match routed {
                        Route::To(prefix) => (format!("{prefix}/{}", post.kind), None),
                        Route::Exact(subject) => (subject, Some(post.kind.clone())),
                        _ => unreachable!("matched above"),
                    };
                    let mut body = Body::new(crate::now_ms());
                    body.kind = in_body;
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
                    //
                    // AND TO THE CALLER'S KEY BEFORE THE POST (R7, §6.5): a
                    // `post key=` whose key an earlier post already reserved
                    // publishes at THAT sequence, so the broker's dedup — the
                    // same dedup that collapses a crash retry — answers the
                    // original offset and appends nothing. `via_key` is what
                    // turns the retirement into `dup=1`, and it is persisted
                    // in the post's pin so a crash between the publish and the
                    // retirement answers the same thing after the relaunch.
                    let (seq, via_key) = match self.state.post_seq(&post.sid, post.id) {
                        Some(pinned) => pinned,
                        None => {
                            let keyed = post
                                .key
                                .as_deref()
                                .and_then(|k| self.state.key_seq(&post.sid, k));
                            let (seq, via_key) = match keyed {
                                Some(seq) => (seq, true),
                                None => {
                                    let Ok(seq) = self.next_seq() else { return };
                                    // THE KEY FIRST, THEN THE PIN, THEN THE
                                    // PUBLISH. A crash after the key is written
                                    // and before the pin re-reads the same post,
                                    // finds the key, and lands on the same
                                    // sequence; a crash before either burns the
                                    // number, which costs nothing.
                                    if let Some(k) = post.key.as_deref() {
                                        if self.state.set_key_seq(&post.sid, k, seq).is_err() {
                                            return;
                                        }
                                    }
                                    (seq, false)
                                }
                            };
                            if self
                                .state
                                .set_post_seq(&post.sid, post.id, seq, via_key)
                                .is_err()
                            {
                                return;
                            }
                            (seq, via_key)
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
                        Ok((off, deduped)) => {
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
                            // THE ASKER'S OWN BRIDGE OWES THE VERDICT (R8): an
                            // ask with a deadline is remembered from the moment
                            // it lands, at the absolute time it expires. Keyed
                            // by the offset, so a deduped re-post under the same
                            // key does not restart the clock.
                            //
                            // AND ONLY FOR A RECORD THIS POST PUT THERE. A post
                            // whose `key=` an EARLIER post reserved, deduped by
                            // the broker, appended nothing: `off` is the earlier
                            // record's, and its kind and `dl=` are that record's
                            // own — an `ask dl=` re-posted under a `note`'s key
                            // started a clock for a note that never waited, and
                            // got an `expired` for it. The original post noted
                            // its own deadline when it landed (and a crash retry
                            // of THAT post is `via_key == false`, so it still
                            // does).
                            let appended = !(via_key && deduped);
                            // WHOSE REPLY IT IS, remembered with the ask: the
                            // owner of the lane it went to.
                            let to = replier_of(&subject);
                            if appended && WAITING_KINDS.contains(&post.kind.as_str()) {
                                if let Some(to) = &to {
                                    self.note_asked(off, to);
                                }
                            }
                            if let Some(dl) = post
                                .dl
                                .filter(|_| appended && WAITING_KINDS.contains(&post.kind.as_str()))
                            {
                                self.note_deadline(&post.sid, off, dl, to);
                            }
                            // `dup=1` IS THE BROKER'S WORD, NOT A GUESS: only a
                            // sequence a `key=` entry chose AND the broker
                            // deduped is a duplicate the caller made. A crash
                            // retry of the same post id is deduped too, and it
                            // is not one — the caller posted once.
                            let dup = if via_key && deduped { " dup=1" } else { "" };
                            let retired = self.ctl_request(&format!(
                                "outbox sent {} {} off={off}{dup}",
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
        // `say` and `say:<topic>` — the BROADCAST face. The subject's last
        // segment is the topic (`say` alone means the topic "say", so every
        // address that worked before still does), and the kind moves into the
        // body: round 23 spends that segment on the thing subscribers name.
        if to == "say" || to.starts_with("say:") {
            let topic = to.strip_prefix("say:").unwrap_or("say");
            if !subject::is_topic(topic) {
                return Route::Unroutable;
            }
            return match from_sid.filter(|s| subject::is_principal(s)) {
                Some(sid) => {
                    Route::Exact(format!("/f/{fleet}/pub/{}/{sid}/say/{topic}", self.node))
                }
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
            let started = Instant::now();
            let answer = last_all(conn, &filter).map(|(rows, _)| rows);
            let page = match self.observe(started, answer, Some("read")) {
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
                // `EVENT <local> inbox-seen <id> off=<n> [verdict=<v> kind=<k>
                // from=<p>]` — the digest names the LOCAL id; `seen_off` is
                // keyed by sid, so the map is the join. The optional tokens are
                // there exactly when the session DECIDED a row (R8).
                let Some(sid) = target.parse::<u64>().ok().and_then(|l| self.locals.get(&l)) else {
                    return;
                };
                let sid = sid.clone();
                let mut off: Option<u64> = None;
                let mut decided = false;
                for t in toks {
                    if let Some(n) = t.strip_prefix("off=") {
                        off = n.parse().ok();
                    } else if t.starts_with("verdict=") {
                        decided = true;
                    }
                }
                let Some(off) = off else { return };
                let _ = self.state.set_seen_off(&sid, off);
                // A DECIDED row may owe a receipt, and the endpoint HOLDS it —
                // listed on the `outbox` peek until retired — so this event is
                // only the prompt trigger. The drain is what publishes it: the
                // same drain that runs after every attach and at every start,
                // which is why a receipt given while the broker was away or
                // while no bridge was running is not lost (see
                // [`Bridge::send_receipt`]).
                if decided {
                    self.drain_outbox();
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
                // THE LINE THAT NEVER ARRIVES — see
                // [`Fault::DropSessionExitedWhileMarked`]. Everything that
                // follows the drop is the shipped path: the departure is then
                // discovered by LOOKING, in `refresh_sessions`.
                if self.fault == Fault::DropSessionExitedWhileMarked
                    && self.state.root().join("drop-session-exited").exists()
                {
                    return;
                }
                let sid = toks.next().unwrap_or(target).to_string();
                self.publish_session_presence(&sid, "exited");
                self.epochs.remove(&sid);
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
        self.attaching = true;
        self.attach_rtt = None;
        let attached = self.attach_broker_inner();
        self.attaching = false;
        if attached {
            // THE FIRST `link up`, now that the link is one the bridge can
            // do its job on: the halt read, the presence publish and every
            // subscription answered. The round trip is the attach's last
            // ack — a real exchange with this broker, never a guess.
            if let Some(rtt) = self.attach_rtt.take() {
                self.note_ack(rtt);
            }
        } else {
            self.attach_rtt = None;
        }
        attached
    }

    /// [`Bridge::attach_broker`]'s body, under its `attaching` guard.
    fn attach_broker_inner(&mut self) -> bool {
        self.close_subscriptions();
        // Forget the old connection's queued inputs and closure notices before
        // opening a new one: see `Mailbox::reset_broker_sources`.
        self.mailbox.reset_broker_sources();
        // The PUBLISHER connection first, before the drain connection: a
        // post-restart herd can be refused by `MAX_CONNS`, and it is the
        // publisher's `live inc+1` that suppresses a fenced will (§7).
        let (conn, _pub_closer) = match self.connect() {
            Ok(c) => c,
            Err(e) => {
                // THE DIAL FAILED, and the endpoint hears which way: no socket
                // file, nobody listening, a permission, a broker that took the
                // connection and never answered the attach, or one that refused
                // the cap.
                self.link_down(link_reason(&e, "attach"));
                return false;
            }
        };
        self.conn = Some(conn);
        // THE STANDING HALT IS READ BEFORE ANY RECORD IS WRITTEN. It is the one
        // exchange the bridge cannot attach without, so a cap the broker
        // refuses it under fails HERE, having published nothing, and the redial
        // that follows costs the bus nothing (the `attaching` field has the
        // measurement). It is applied below, once presence is up — see
        // [`Bridge::read_fleet_halts`] for why the two halves sit where they do.
        let Some((halts, fleet_from)) = self.read_fleet_halts() else {
            eprintln!("aterm-link: the standing fleet halt could not be read; not attaching");
            self.conn = None;
            return false;
        };
        if let Err(e) = self.bring_presence_up() {
            eprintln!("aterm-link: presence could not come up: {e}");
            // A transport failure was already reported by `observe`; a
            // `Publish` the broker refused was acked there, so this is the
            // one that has to say the attach failed.
            if e.kind() == io::ErrorKind::Other {
                self.link_down("attach");
            }
            self.conn = None;
            return false;
        }
        // BEFORE ANY RECORD IS TAKEN. The endpoint may be holding `fabric-lost`
        // from the last incarnation's death, and until that is reconciled with
        // the fleet's standing halt no session on this instance can be driven at
        // all — including by the replay of a feed the crash interrupted.
        self.apply_fleet_halts(halts);
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
        let group = subject::inbox_group(&self.cfg.fleet, &self.node);
        let inbox_filter = subject::inbox_filter(&self.cfg.fleet, &self.node);
        // THE FLEET FACE RESUMES FROM ITS OWN LAST RECORD, NOT FROM ZERO.
        //
        // It is a last-value face — `on_fleet_record` keeps only
        // `fleet/h-*/halt`, and the standing verdict is the newest row per
        // human — so the `Last` read whose answer `apply_fleet_halts` REBUILDS
        // `self.halts` from is already the whole state. Subscribing from zero
        // after it re-delivered every halt record ever published to re-derive
        // the state that read had just derived, and `state.rs`'s `halt-acked`
        // exists only to stop the node re-answering each one: "the fleet face
        // resubscribes from offset 0 on every reconnect, so without this the
        // node re-answers every halt in the fleet's history at every
        // reconnect."
        //
        // AND THE REPLAY WAS NOT MERELY REDUNDANT. `on_fleet_record` APPLIES
        // each halt as it arrives, so the walk re-applied every barrier the
        // fleet ever carried: the first `state=on` in the log held every
        // session on this instance until the walk reached its matching `off`,
        // a hold nothing on the bus justified and one this attach had already
        // computed the correct answer to moments earlier.
        //
        // The mark closes the window the old comment worried about instead of
        // paying for it — `Request::Last`'s own doc says a subscribe from a
        // page's `next` tails on gap-free from that page's snapshot, so a halt
        // published between the read and this subscribe still arrives, exactly
        // once.
        // THE FLEET FACE, ON ITS OWN CONNECTION. This was a loop over two faces
        // until round 21 took the drive face's; one subscription does not need
        // one, and a loop over a single element reads as though a second is
        // expected.
        {
            let (client, closer) = match self.connect() {
                Ok(c) => c,
                Err(e) => {
                    self.link_down(link_reason(&e, "attach"));
                    return false;
                }
            };
            let Ok(sub) = client.subscribe(fleet_from, &fleet_filter) else {
                self.link_down("subscribe");
                return false;
            };
            // THE DEADLINE COMES OFF once the subscription is open: parking in
            // `recv` with nothing to deliver is what a subscription does.
            let _ = closer.set_read_timeout(None);
            self.closers.push(closer);
            spawn_reader(sub, Source::Fleet, self.mailbox.clone());
        }
        let (client, closer) = match self.connect() {
            Ok(c) => c,
            Err(e) => {
                self.link_down(link_reason(&e, "attach"));
                return false;
            }
        };
        let Ok(sub) = client.subscribe_group(&group, &inbox_filter) else {
            self.link_down("subscribe");
            return false;
        };
        let _ = closer.set_read_timeout(None);
        self.closers.push(closer);
        spawn_reader(sub, Source::Inbox, self.mailbox.clone());
        // THE BROADCAST FACE — ONE SUBSCRIPTION FOR THE WHOLE FLEET.
        //
        // Not a group: a group subscription's cursor is shared with every other
        // member of it, and a broadcast has no exactly-once obligation to
        // anybody — the obligation is per RECIPIENT, and that is exactly what
        // `self.topics`' per-(session, topic) cursors hold, durably. So the
        // subscription starts wherever the earliest un-delivered topic sits, and
        // each record is filtered per session on the way in.
        //
        // NO ROW HAS BEEN SAMPLED YET on the first attach of a process, so the
        // cursors come off disk here, before the first roster round. That is the
        // whole restart guarantee: a bridge that resubscribed from the head
        // would skip every broadcast published while it was down, and no session
        // would ever learn it had missed one.
        for sid in self.locals.values().cloned().collect::<Vec<_>>() {
            if self.topics.contains_key(&sid) {
                // A RECONNECT, not a restart: what this process holds is at
                // least as new as what is on disk (every step of a cursor is
                // persisted as it is taken), and re-reading the file would
                // rewind a cursor whose persist had failed.
                continue;
            }
            let persisted = self.state.topics(&sid);
            if !persisted.is_empty() {
                self.topics.insert(sid, persisted);
            }
        }
        let say_filter = subject::say_filter(&self.cfg.fleet);
        let Some((say_from, say_head)) = self.broadcast_floor(&say_filter) else {
            self.link_down("attach");
            return false;
        };
        // THE TWO ARE NOT THE SAME NUMBER after a restart with a gap: the
        // subscription starts at the lowest cursor still owed, which may be
        // thousands of records back, while `head` is where the bus actually is.
        // `since=head` must mean the latter.
        self.say_drained_to = say_from;
        self.say_bus_head = say_head;
        let (client, closer) = match self.connect() {
            Ok(c) => c,
            Err(e) => {
                self.link_down(link_reason(&e, "attach"));
                return false;
            }
        };
        let Ok(sub) = client.subscribe(say_from, &say_filter) else {
            self.link_down("subscribe");
            return false;
        };
        let _ = closer.set_read_timeout(None);
        self.closers.push(closer);
        spawn_reader(sub, Source::Say, self.mailbox.clone());
        // THE GHOST CANDIDATES ARE DISCOVERED HERE, ONCE PER ATTACH, so the
        // loop's sweep arm can be gated on the set being non-empty and a fleet
        // with no ghost pays nothing at all. This first call retires nothing —
        // [`GHOST_AFTER`]'s patience is measured from this bridge's own first
        // sight — it only seeds [`Bridge::ghosts`] and arms the deadline.
        //
        // ONCE PER ATTACH IS THE HONEST BOUND, and it is worth writing down:
        // a ghost that appears WHILE this bridge is attached (another instance
        // sharing this node id dying) is not noticed until the next attach. The
        // alternative is the minute timer this gating exists to remove.
        self.retire_ghost_presence();
        self.ghost_due = Instant::now() + GHOST_SWEEP;
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
            // AND THE LOCAL OBSERVATIONS, FOR THE SAME REASON. The roster, the
            // `attention=` escalation and the per-session `detail=`/`revision`
            // sample are discovered by LOOKING, not by anything arriving, so a
            // bridge that only looks when nothing is arriving does not do them
            // on a busy node. See [`ROSTER_REFRESH`].
            if Instant::now() >= self.roster_due {
                self.roster_backstop();
                self.roster_due = Instant::now() + ROSTER_REFRESH;
            }
            // AND THE GHOSTS — ONLY WHILE THERE IS ONE, which is what keeps
            // this from being a heartbeat.
            //
            // The sweep is a `Last` walk of a broker subtree, and `observe`
            // turns any answered broker request into an ack, which
            // `ack_wants_report` turns into a `link up` once it is a
            // `LINK_REFRESH` since the last one. On a free-running minute timer
            // that is one report per minute for the life of every bridge on the
            // fleet, forever, for nothing — and three docs in this tree promise
            // there is no such thing (`LINK_REFRESH` here, `status`'s catalog
            // entry, and the manual's fabric page). The candidates are
            // discovered once per attach, in the attach's own burst of reads,
            // and this arm runs only until the set it seeded drains to empty.
            //
            // After the roster round, so a sid admitted this very tick is
            // already in `locals` and is never counted unhosted for the round
            // that admitted it.
            let sweep_now = self.fault == Fault::SweepGhostsAtOnceWhileMarked
                && self.state.root().join("sweep-ghosts-now").exists();
            if sweep_now || (!self.ghosts.is_empty() && Instant::now() >= self.ghost_due) {
                self.retire_ghost_presence();
                self.ghost_due = Instant::now() + GHOST_SWEEP;
            }
            // AND THE DEADLINES (R8). The broker holds no timers; this clock is
            // the only one that can say an ask went unanswered.
            if Instant::now() >= self.deadline_due {
                self.expire_deadlines();
                self.deadline_due = Instant::now() + DEADLINE_TICK;
            }
            // EACH OF THE DUTIES ABOVE CARRIES ITS OWN DEADLINE, and none of
            // them lives on the idle branch, because a busy bridge never idles
            // and every one of them is discovered by LOOKING rather than by
            // something arriving.
            match self.mailbox.take(IDLE_TICK) {
                Some(Item::Fleet(r)) => self.on_fleet_record(&r),
                Some(Item::Event(line)) => self.on_event(&line),
                Some(Item::Inbox(r)) => self.on_inbox_record(&r),
                Some(Item::Say(r)) => self.on_say_record(&r),
                Some(Item::Closed(Source::Aterm)) => {
                    eprintln!("aterm-link: aterm closed the bridge connection; exiting");
                    return Ok(());
                }
                Some(Item::Closed(_)) => {
                    // A broker reader ended: the connection is gone. Drop the
                    // publisher too and reconnect the whole set, because a
                    // half-attached bridge is the state nothing is defined for.
                    // AND SAY SO FIRST: this is the moment a killed broker is
                    // noticed (its peer's EOF), and the endpoint's `fabric=`
                    // goes to `stalled` here — before the redial that will
                    // refine the reason to `refused` or `no-socket`.
                    eprintln!("aterm-link: a broker subscription ended; reconnecting");
                    self.link_down("closed");
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
/// evidence of absence — and one of them ([`Bridge::read_fleet_halts`]) LIFTS A
/// STANDING FLEET HALT on that evidence. `glance::read` already does it
/// correctly and says why; this is that loop, in one place, for the readers that
/// did not.
///
/// `Err` is returned for a walk that could not be COMPLETED, page bound
/// included, because a caller that cannot tell "no rows" from "I stopped
/// looking" is the caller that lifts the halt.
///
/// AND THE SPLICE POINT, which is the FIRST page's `next` and not the last's.
/// `Request::Last`'s own doc states the rule: each page of a paged query is
/// read at its OWN, later head, so a reader that tails from the last page's
/// mark skips whatever superseded a value an earlier page reported. A caller
/// that wants to go on watching the face it just snapshotted passes this to
/// `subscribe`, and the two are gap-free and dup-free across the seam.
fn last_all(conn: &mut Conn, filter: &str) -> io::Result<(Vec<BrokerRecord>, u64)> {
    let mut rows = Vec::new();
    let mut after = String::new();
    let mut splice = None;
    for _ in 0..LAST_PAGES_MAX {
        let (page, (next, _), resume) = conn.last_page(filter, &after, 256)?;
        splice.get_or_insert(next);
        rows.extend(page);
        if resume.is_empty() {
            return Ok((rows, splice.unwrap_or(next)));
        }
        after = resume;
    }
    Err(io::Error::other(format!(
        "the last-value walk of {filter} did not finish within {LAST_PAGES_MAX} pages"
    )))
}

/// ONE `deliver` LINE'S FIELDS, as [`Bridge::deliver_record`] or
/// [`Bridge::deliver_broadcast`] has already classified them.
struct DeliverLine<'a> {
    /// The lane the record is being delivered ON — synthesized for a broadcast
    /// from the publishing node and the receiving session.
    addr: &'a subject::InAddr,
    /// The decoded record.
    body: &'a Body,
    /// The broker offset, which is the idempotency key the ring dedups on.
    off: u64,
    /// The kind the endpoint is told, after [`Bridge::classify_delivery`].
    kind: &'a str,
    /// The kind it CLAIMED, when that classification demoted it.
    demoted: Option<&'a str>,
    /// An answer that arrived after its ask was recorded expired. Never true
    /// for a broadcast: a shout answers no ask.
    late: bool,
    /// The broadcast topic, when this is one. The only field the broadcast
    /// path adds, and what turns off the two bus writes an addressed refusal
    /// or truncation makes (see [`Bridge::refuse_delivery`]).
    topic: Option<&'a str>,
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

/// The pct-encoding of the longest prefix of `text` that fits `budget` encoded
/// bytes, cut at a character boundary, and how many DECODED bytes it covers.
fn encode_prefix(text: &str, budget: usize) -> (String, usize) {
    let whole = crate::pct::encode(text);
    if whole.len() <= budget {
        return (whole, text.len());
    }
    let mut out = String::with_capacity(budget);
    let mut used = 0;
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        let piece = crate::pct::encode(ch.encode_utf8(&mut buf));
        if out.len() + piece.len() > budget {
            break;
        }
        out.push_str(&piece);
        used += ch.len_utf8();
    }
    (out, used)
}

/// The `deliver <sid> fetched=<off> <fields> …` lines that carry one fetched
/// record to the endpoint: ONE when the body fits a control line, else
/// consecutive chunks, each a whole line with the same fields — `at=<n>` naming
/// the byte offset in the decoded body where its `text=` starts (absent for 0)
/// and `more=1` on every line but the last. That is what makes a body the
/// DELIVERY had to cut (`truncated=1`) come back whole: the answer travels on
/// the very line that cut it, so one line could only ever carry it cut again.
///
/// The body is first cut at [`FETCH_BODY_MAX`], the endpoint's own bound, and
/// `len=` then names the record's true size — the endpoint's `truncated=1`.
/// `Err("oversize")` only when the fields alone leave no room for text.
///
/// # Errors
///
/// `oversize`, as above.
pub fn fetched_lines(
    sid: &str,
    off: u64,
    fields: &str,
    text: &str,
) -> Result<Vec<String>, &'static str> {
    let mut end = text.len().min(FETCH_BODY_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let carried = &text[..end];
    let mut head = format!("deliver {sid} fetched={off} {fields}");
    if carried.len() < text.len() {
        head.push_str(&format!(" len={}", text.len()));
    }
    // THE SAME BOUND `deliver_record` KEEPS: the line [`Ctl::request`] itself
    // refuses to write past, less this line's own fixed spend.
    let budget = REQUEST_LINE_MAX
        .checked_sub(head.len() + " at= more=1 text=".len() + 20)
        .filter(|b| *b >= 12)
        .ok_or("oversize")?;
    let mut lines = Vec::new();
    let mut at = 0;
    loop {
        let (piece, used) = encode_prefix(&carried[at..], budget);
        let next = at + used;
        let mut line = head.clone();
        if at > 0 {
            line.push_str(&format!(" at={at}"));
        }
        if next < carried.len() {
            line.push_str(" more=1");
        }
        line.push_str(&format!(" text={piece}"));
        lines.push(line);
        if next >= carried.len() {
            return Ok(lines);
        }
        at = next;
    }
}

/// Where one outbound post is going — or why it is going nowhere (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The six-segment owner+src prefix of the `in` subject to publish under.
    /// The kind is appended as the last segment.
    To(String),
    /// THE WHOLE SUBJECT, kind and all already decided — a broadcast's
    /// `/f/<F>/pub/<node>/<sid>/say/<topic>`, where the last segment is the
    /// TOPIC and the kind rides in the body ([`crate::body::Body::kind`]).
    Exact(String),
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
    /// The `reason=` token this verdict retires a post with. `To` and `Exact`
    /// have none — they are not verdicts — and answer `unroutable` only so the
    /// type is total.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Route::Ambiguous => "ambiguous",
            Route::To(_) | Route::Exact(_) | Route::Unroutable => "unroutable",
        }
    }
}

/// `<content_seq>:<fp16>` out of one `text --json` frame — §6.6's `serial=`.
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
///
/// THE PRIME IS THE ONE FNV PUBLISHES, and until round 22 it was not: this read
/// `0x1000_0000_01b3`, which is 2^44 + 0x1b3, where FNV-64's multiplier is
/// 2^40 + 0x1b3. The doc above already claimed to be "the same HASH" as
/// `turn_ledger`'s and was not, and because the multiply only carries low bits
/// upward the two agreed in their bottom forty bits and diverged above them —
/// which is exactly how it was found, by a `--report-to` that hashed a screen
/// here and compared it with the `hash=` `status` stamps. Nothing persisted a
/// value: [`gen_of_frame`]'s token is only ever compared with another token
/// from this same function, so the repair changes no recorded state.
/// [`the_prime_is_fnvs_own`] pins it against the published vectors.
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

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

/// WHETHER A RETAINED ROW AT `inc` IS THIS BRIDGE'S TO RETIRE.
///
/// The predicate the ghost sweep turns on, split out so it can be tested
/// without two bridges and a broker. Three answers, and the default is no:
///
/// * `Some(inc) == self_inc` — this incarnation published the row itself and
///   does not host the session, so it is a ghost of our own making.
/// * `Some(inc) == witnessed_dead` — an incarnation whose WILL this bridge saw
///   on the node face when it attached. A will fires only on disconnect, so
///   that record is proof the process behind it is gone.
/// * anything else, `None` included — refuse. A row with no `inc=` cannot be
///   attributed at all, and a row from an incarnation whose death nobody
///   witnessed may belong to a sibling that is alive right now.
///
/// ROUND 21 SHIPPED THIS AS `inc > self_inc => skip`, WHICH IS THE WRONG WAY
/// ROUND. `self.inc` is `max(local, bus) + 1`, so a bridge attaching SECOND on
/// a shared state dir always outranks the first: the older sibling's rows are
/// ALL below it, and all of them were retired after five minutes, alive or not.
/// The guard read as though it protected a sibling and in fact protected only a
/// FUTURE one, which cannot exist while this process is the one sweeping.
///
/// AND THE OBVIOUS REPAIR IS ALSO WRONG. `inc <= witnessed_dead` looks like the
/// generous reading of "and everything older" — but a surviving OLDER sibling
/// does not rewrite the node face when a younger one's will fires, so a node
/// face reading `state=gone inc=K` proves K dead and says nothing whatever
/// about `K-1`. The equality is the whole of what is proven, so the equality is
/// what is used.
fn adoptable(inc: Option<u64>, self_inc: u64, witnessed_dead: Option<u64>) -> bool {
    match inc {
        Some(inc) => inc == self_inc || witnessed_dead == Some(inc),
        None => false,
    }
}

/// The principal whose REPLY a record published at `subject` is — the owner
/// of the `in` lane it went to: `<node>` of `/f/<F>/in/<node>/<sid>/<src>/<kind>`,
/// `<principal>` of `/f/<F>/in/p/<principal>/<src>/<kind>`. `None` for any
/// other subject (a `say` face has no recipient to answer it).
fn replier_of(subject: &str) -> Option<String> {
    let seg: Vec<&str> = subject.split('/').collect();
    if seg.len() != 8 || seg[1] != "f" || seg[3] != "in" {
        return None;
    }
    let owner = if seg[4] == "p" { seg[5] } else { seg[4] };
    subject::is_principal(owner).then(|| owner.to_string())
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
    /// The caller's idempotency key (`post key=`), carried by the endpoint on
    /// every drain so a relaunched bridge reads the same key from the same
    /// queued row. See [`StateDir::key_seq`].
    pub key: Option<String>,
    pub body: Vec<u8>,
}

/// Everything one `outbox` peek hands the bridge: the queued posts, the
/// receipts sessions owe, and the `inbox get @<off>` reads parked for a record
/// the ring let go.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Drain {
    pub posts: Vec<QueuedPost>,
    /// One per `receipt sid= rid= off= verdict= kind= from=` line.
    pub receipts: Vec<OwedReceipt>,
    /// `(sid, off)` per `fetch sid=<sid> off=<n>` line.
    pub fetches: Vec<(String, u64)>,
}

/// One receipt a session owes (R8): the endpoint's `receipt …` line off the
/// `outbox` peek, listed from the session's `inbox seen <id>
/// handled|refused|deferred` until `deliver <sid> receipt=<rid>` retires it.
/// See [`Bridge::send_receipt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedReceipt {
    /// The deciding session — the recipient of the ask.
    pub sid: String,
    /// The endpoint's per-session receipt id: the retirement's name and the
    /// key the producer sequence is pinned to.
    pub rid: u64,
    /// The decided row's offset: the receipt's `re=`.
    pub off: u64,
    pub verdict: String,
    /// The row's kind as the endpoint classified it.
    pub kind: String,
    /// The row's `from=`: whom the receipt goes to.
    pub from: String,
}

/// One `receipt …` line's tokens, or `None` for a line that does not parse —
/// dropped, never guessed at: the sid is spliced into a subject and a state
/// file name, so it must be a principal.
fn parse_receipt_line(line: &str) -> Option<OwedReceipt> {
    let (mut sid, mut rid, mut off, mut verdict, mut kind, mut from) =
        (None, None, None, None, None, None);
    for tok in line.split_whitespace() {
        match tok.split_once('=') {
            Some(("sid", v)) => sid = Some(v),
            Some(("rid", v)) => rid = v.parse::<u64>().ok(),
            Some(("off", v)) => off = v.parse::<u64>().ok(),
            Some(("verdict", v)) => verdict = Some(v),
            Some(("kind", v)) => kind = Some(v),
            Some(("from", v)) => from = Some(v),
            _ => {}
        }
    }
    let sid = sid.filter(|s| subject::is_principal(s))?;
    Some(OwedReceipt {
        sid: sid.to_string(),
        rid: rid.filter(|n| *n > 0)?,
        off: off?,
        verdict: verdict?.to_string(),
        kind: kind?.to_string(),
        from: from?.to_string(),
    })
}

/// Parse an `outbox` frame: `post sid=… id=… to=… kind=… [re=] [dl=] [via=]
/// [key=] len=<n>` then exactly `n` body bytes, repeated. See [`parse_drain`]
/// for the whole frame; this is its posts alone.
///
/// TOTAL: a truncated or malformed frame yields the posts that parsed and stops.
/// The alternative — refusing the whole frame — would wedge the outbound queue on
/// one bad row forever.
#[must_use]
pub fn parse_outbox(frame: &[u8]) -> Vec<QueuedPost> {
    parse_drain(frame).posts
}

/// Parse an `outbox` frame whole: the `post …` lines with their bodies, and
/// the bodiless `fetch sid=<sid> off=<n>` lines the endpoint appends AFTER
/// every post — after, so a bridge from before this rung, whose parser stops
/// at the first line without a `len=`, still drains every post.
#[must_use]
pub fn parse_drain(frame: &[u8]) -> Drain {
    let mut out = Drain::default();
    let mut rest = frame;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
            break;
        };
        let line = String::from_utf8_lossy(&rest[..nl]).into_owned();
        let tail = &rest[nl + 1..];
        if let Some(receipt) = line.strip_prefix("receipt ") {
            if let Some(r) = parse_receipt_line(receipt) {
                out.receipts.push(r);
            }
            rest = tail;
            continue;
        }
        if let Some(fetch) = line.strip_prefix("fetch ") {
            let mut sid: Option<&str> = None;
            let mut off: Option<u64> = None;
            for tok in fetch.split_whitespace() {
                if let Some(s) = tok.strip_prefix("sid=") {
                    sid = Some(s);
                } else if let Some(n) = tok.strip_prefix("off=") {
                    off = n.parse().ok();
                }
            }
            if let (Some(sid), Some(off)) = (sid, off) {
                if subject::is_principal(sid) {
                    out.fetches.push((sid.to_string(), off));
                }
            }
            rest = tail;
            continue;
        }
        let mut post = QueuedPost {
            sid: String::new(),
            id: 0,
            to: String::new(),
            kind: String::new(),
            re: None,
            dl: None,
            via: None,
            key: None,
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
                "key" => post.key = Some(v.to_string()),
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
        out.posts.push(post);
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
                    Source::Inbox => mailbox.push_inbox(rec, generation),
                    Source::Say => mailbox.push_say(rec, generation),
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

    /// **THE PRIME IS FNV'S OWN.** Pinned against the vectors FNV publishes,
    /// because the value this returns is compared with a hash another crate
    /// computes (`aterm-gui`'s `turn_ledger::fnv1a_64`, which `status` stamps a
    /// screen with and `aterm-link hook --report-to` checks its rows against).
    /// A multiplier of `0x1000_0000_01b3` — 2^44 + 0x1b3, which this was —
    /// agrees with the real thing in the bottom forty bits and nowhere above,
    /// so nothing but a cross-crate comparison or a published vector catches it.
    #[test]
    fn the_prime_is_fnvs_own() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

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

    /// **AN OLDER LIVE SIBLING'S ROWS ARE NEVER RETIRED — the two-bridges,
    /// one-node case, at unit level.**
    ///
    /// Two bridges CAN share a state dir and therefore a node id; `fabric.rs`
    /// records the measured 2026-09-14 case where one did. The pair is
    /// pathological but it is reachable, and the sweep publishes `state=exited`
    /// over somebody's presence row, so the guard is the difference between
    /// retiring a ghost and taking a live agent off the fleet roster.
    ///
    /// ROUND 21'S GUARD GOT THIS BACKWARDS and the shape of the mistake is worth
    /// keeping: it skipped `inc > self.inc`, which reads as "do not touch a
    /// sibling" and in fact means "do not touch a FUTURE incarnation" — one that
    /// cannot exist while this process is the one sweeping. Because `self.inc`
    /// is `max(local, bus) + 1`, the bridge that attaches SECOND always
    /// outranks the first, so every row the older LIVE sibling had published was
    /// below the guard and was retired after five minutes.
    ///
    /// Driven through [`adoptable`] rather than through two real bridges: what
    /// is being asserted is the predicate, and a second broker, a second aterm
    /// and a shared state dir would make a slow test of a fast question.
    #[test]
    fn only_this_incarnation_and_a_witnessed_dead_one_are_adoptable() {
        // B attached second on a shared state dir: A is inc 1 and ALIVE, B is
        // inc 2, and B saw a LIVE node row at attach, so it witnessed no death.
        let (a_live, b) = (1, 2);
        assert!(
            !adoptable(Some(a_live), b, None),
            "an older sibling that is still running had its sessions retired"
        );
        // B's own rows for sessions it does not host are B's to retire.
        assert!(adoptable(Some(b), b, None));
        // A DEATH IT WITNESSED. B attaches, finds the node face carrying A's
        // will (`state=gone inc=1`), and may retire exactly A's rows.
        assert!(adoptable(Some(a_live), b, Some(a_live)));
        // AND ONLY THAT ONE. A will proves the incarnation it names is gone and
        // says nothing about any other, because a surviving older sibling does
        // not rewrite the node face when a younger one's will fires.
        assert!(
            !adoptable(Some(1), 3, Some(2)),
            "`inc <= witnessed` is the generous reading, and it is not proven"
        );
        // A FUTURE incarnation is still refused, which the old guard did get right.
        assert!(!adoptable(Some(9), b, None));
        assert!(!adoptable(Some(9), b, Some(a_live)));
        // A row with no `inc=` cannot be attributed to anyone.
        assert!(!adoptable(None, b, Some(a_live)));
        assert!(!adoptable(None, b, None));
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
    /// This reads the STRUCT rather than a list, so the next map is covered the
    /// day it is added — which is the shape of the mistake, not the instance of
    /// it. Round 21 proved both halves in one round: it ADDED `ghosts` and this
    /// test caught it on the first run (it is the exception that proves the
    /// rule, named in the loop with the reason its entries must outlive their
    /// session), and it REMOVED three — `holders` and `pending` with the drive
    /// face, `screen_gen` with the screen face — which is why the closing
    /// assertion no longer counts them.
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
        let mut checked: Vec<&str> = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            // `<name>: BTreeMap<String, …>` — a map keyed by a sid. The two
            // maps keyed by something else (`halts` is per HUMAN, `locals` is
            // aterm's own local id) are named in the exception below, and so is
            // the one map whose entries EXIST because their session is not in
            // the roster.
            let Some((name, _)) = line.split_once(": BTreeMap<String, ") else {
                continue;
            };
            // `ghosts` IS reconciled, and deliberately not against `live`: it
            // dates sids the bridge has seen advertised on the bus with NO
            // local session hosting them ([`Bridge::retire_ghost_presence`]),
            // so a retain against the roster would empty it on every round and
            // the five-minute patience it exists to measure could never
            // elapse. Its own reconciliation is against the set still seen
            // unhosted this sweep — `self.ghosts.retain(|sid, _|
            // unhosted.contains(sid))` — which is the same bound this test is
            // about, taken against the right set.
            if matches!(name, "halts" | "ghosts") {
                continue;
            }
            checked.push(name);
            assert!(
                src.contains(&format!("self.{name}.retain(|sid, _| live.contains(sid))")),
                "`{name}` is keyed by a sid and nothing reconciles it with the roster: \
                 add it to `refresh_sessions`'s retain block, or say in its doc why a \
                 departed session's entry must outlive the session"
            );
        }
        // THE SCAN ITSELF STILL WORKS — asserted by NAME, not by a count.
        //
        // This line used to be `checked >= 6`, then `>= 4` after round 21's
        // drive-face cut took `holders` and `pending`, and it would have gone
        // to `>= 3` an hour later when the `--screen` cut took `screen_gen`. A
        // number that has to be edited by every cut is not a guard, it is a
        // chore that eventually gets edited without being thought about. What
        // the floor was ever FOR is catching a loop that silently stopped
        // matching the struct — a field reformatted onto two lines, a type
        // alias, a rename — and `epochs` answers that: it is the sid set this
        // node hosts, the one map here that cannot go without the bridge losing
        // its roster.
        assert!(
            checked.contains(&"epochs"),
            "the field scan matched no `epochs` — this loop has stopped reading \
             the struct and is checking nothing: {checked:?}"
        );
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
        // §6.6's `serial=` fingerprint is deliberately NOT aterm's `turn` `hash=`,
        // and the design says the two "must never be compared". Both doc
        // comments used to say the opposite — an invitation to fence a keystroke
        // with a `hash=` a driver already has and have every one refused
        // `reason=gen` for a units mismatch.
        assert!(
            src.contains("IT IS NOT aterm's `turn` `hash=`, AND THE TWO MUST NEVER BE COMPARED."),
            "`live_gen` must say what the design says about `serial=` versus `hash=`"
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
        // THE ROW-4/ROW-5 PIN WENT WITH ROWS 4 AND 5. It asserted the local
        // sweep's header did not describe a pre-`LOCAL_OBSERVE` shape, and named
        // `watch_held_control` as where row 4 had moved to. Round 21 cut the
        // drive face, and both rows and that function with it, so the negative
        // pin now guards prose that cannot come back and names a function that
        // does not exist — the same "green run over an empty set" this test
        // removed for `SCREEN_PERIOD` a few lines above.
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

    /// THE `receipt` AND `fetch` LINES RIDE THE SAME PEEK, AFTER EVERY POST,
    /// AND A PARSER FROM BEFORE THEM STILL GETS EVERY POST. `parse_outbox` is
    /// the old reader; it must answer the posts of a frame that carries
    /// receipts and fetches, and `parse_drain` must answer all three — dropping
    /// a line whose sid is not a principal (it is spliced into a subject and a
    /// state file name) or whose fields are incomplete, never guessing.
    #[test]
    fn a_drain_frame_carries_receipts_and_fetches_after_the_posts_and_an_old_reader_keeps_the_posts(
    ) {
        let frame = b"post sid=s-a id=1 to=%40s-b kind=note len=2\nhi\
                      receipt sid=s-a rid=3 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=bad rid=4 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=s-a rid=0 off=40 verdict=handled kind=ask from=s-z@n-q\n\
                      receipt sid=s-a rid=5 off=40 kind=ask from=s-z@n-q\n\
                      fetch sid=s-a off=41\nfetch sid=s-b off=7\nfetch sid=bad off=9\n";
        let drain = parse_drain(frame);
        assert_eq!(drain.posts.len(), 1);
        assert_eq!(drain.posts[0].body, b"hi");
        assert_eq!(
            drain.receipts,
            vec![OwedReceipt {
                sid: "s-a".to_string(),
                rid: 3,
                off: 40,
                verdict: "handled".to_string(),
                kind: "ask".to_string(),
                from: "s-z@n-q".to_string(),
            }],
            "a bad sid, a zero rid and a missing verdict are each dropped"
        );
        assert_eq!(
            drain.fetches,
            vec![("s-a".to_string(), 41), ("s-b".to_string(), 7)],
            "a sid that is not a principal is dropped, never spliced into a filter"
        );
        assert_eq!(parse_outbox(frame).len(), 1);
        // A fetch AHEAD of a post is parsed too — the endpoint never emits
        // that order, but a parser must not depend on it.
        let ahead = b"fetch sid=s-a off=41\npost sid=s-a id=1 to=%40s-b kind=note len=0\n";
        let drain = parse_drain(ahead);
        assert_eq!((drain.posts.len(), drain.fetches.len()), (1, 1));
    }

    /// **A FETCHED BODY GOES BACK WHOLE: ONE LINE WHEN IT FITS, CONSECUTIVE
    /// CHUNKS WHEN IT DOES NOT, AND CUT AT THE ENDPOINT'S BOUND WITH `len=` WHEN
    /// IT IS LARGER THAN THAT.** Reviewed defect: the answer reused the very
    /// line budget that cut the body at delivery, so `inbox get @<off>` of a
    /// `truncated=1` row came back cut again (39,999 bytes on the bus, 32,688
    /// answered); and a record over 256 KiB was answered with a `len=` the
    /// endpoint refused, so the read was re-fetched every drain until it timed
    /// out. Every line must fit [`REQUEST_LINE_MAX`], the chunks must decode
    /// back to the body byte for byte at the offsets `at=` names, and only the
    /// last may lack `more=1`.
    #[test]
    fn fetched_lines_carry_a_body_whole_in_chunks_and_cut_one_over_the_bound() {
        let fields = "from=s-a@n-b kind=note trust=agent";
        let decode = |lines: &[String]| -> String {
            let mut body = String::new();
            for (i, line) in lines.iter().enumerate() {
                let at: usize = line
                    .split_whitespace()
                    .find_map(|t| t.strip_prefix("at="))
                    .map_or(0, |n| n.parse().expect("at="));
                assert_eq!(at, body.len(), "chunk {i} starts where the last ended");
                assert_eq!(
                    line.contains(" more=1 "),
                    i + 1 < lines.len(),
                    "more=1 on every line but the last"
                );
                assert!(line.len() <= REQUEST_LINE_MAX, "chunk {i} fits one line");
                let text = line.split(" text=").nth(1).expect("text=");
                body.push_str(&crate::pct::decode(text));
            }
            body
        };
        // Fits: one line, no `at=`, no `more=1`, no `len=`.
        let small = fetched_lines("s-x", 7, fields, "hello world").expect("small");
        assert_eq!(
            small,
            vec!["deliver s-x fetched=7 from=s-a@n-b kind=note trust=agent text=hello%20world"]
        );
        // The reviewer's shape: 39,999 bytes of `x x x …` — every space
        // pct-encodes to three bytes, so it cannot fit one 64 KiB line.
        let mut text = "x ".repeat(19_999);
        text.push('x');
        let lines = fetched_lines("s-x", 4, fields, &text).expect("chunked");
        assert!(lines.len() > 1, "{} line(s)", lines.len());
        assert!(
            !lines.iter().any(|l| l.contains(" len=")),
            "not cut, so no len="
        );
        assert_eq!(decode(&lines), text, "whole, byte for byte");
        // Multi-byte characters are never split across chunks.
        let wide = "é🙂".repeat(20_000);
        let lines = fetched_lines("s-x", 5, fields, &wide).expect("wide");
        assert!(lines.len() > 1);
        assert_eq!(decode(&lines), wide);
        // Over the endpoint's bound: cut there, at a character boundary, and
        // `len=` names the record's true size on every chunk.
        let big = format!("{}é", "z".repeat(FETCH_BODY_MAX - 1));
        let lines = fetched_lines("s-x", 6, fields, &big).expect("big");
        let got = decode(&lines);
        assert_eq!(
            got.len(),
            FETCH_BODY_MAX - 1,
            "cut before the split character"
        );
        assert!(lines
            .iter()
            .all(|l| l.contains(&format!(" len={} ", big.len()))));
        // Fields that leave no room for text are a verdict, not a loop.
        let huge = "x".repeat(REQUEST_LINE_MAX);
        assert_eq!(fetched_lines("s-x", 8, &huge, "hi"), Err("oversize"));
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
        let cited = concat!("no_bus_record_ever", "_reaches_a_pty");
        let header: String = src
            .lines()
            .take_while(|l| l.starts_with("//") || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            header.contains(cited),
            "the module header must cite the test that holds the claim that NO bus \
             record reaches a PTY"
        );
        assert!(
            include_str!("../tests/bridge_e2e.rs").contains(&format!("fn {cited}")),
            "the cited guard must exist under exactly that name"
        );
        // AND NOTHING FEEDS A PTY ANY MORE, which is a claim this test can check
        // over the source rather than take on trust. Round 21 cut `feed`,
        // `on_term_record` and `resolve_pending_feed`; the two assertions that
        // used to pin the replay's retry budget went with them, because a bound
        // on a thing that no longer exists is the "green run over an empty set"
        // this test was written against. Split so this prose is not the
        // counterexample.
        for gone in [
            concat!("fn ", "feed", "("),
            concat!("fn ", "on_term_record"),
            concat!("fn ", "resolve_pending_feed"),
        ] {
            assert!(
                !src.contains(gone),
                "`{gone}` is back: the module header claims no bus record reaches a \
                 PTY by any path, and that claim is now the absence of these"
            );
        }
        // THE SCREEN FACE'S RATE PIN WENT WITH THE SCREEN FACE. It asserted
        // that the header did NOT restate §3.3's per-subject "≤ 4/s" as a
        // per-node aggregate — a real defect while `--screen all` could publish
        // 4N/s of screen CONTENT onto an append-forever log. Round 21 deleted
        // `SCREEN_PERIOD`, `publish_screens` and the flag, so the assertion
        // became a negative pin over prose that cannot come back: a green run
        // over an empty set, which is the exact thing the paragraph twelve lines
        // above says this test removed. It is removed here rather than left to
        // read as coverage.
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

    /// THE LINK RECORD GOES OUT ON CHANGE, NOT ON A CLOCK. A first ack, a
    /// down-to-up, a reason that changed, a 2x move past the move gap, and a
    /// report older than the refresh window each earn one record; a same-reason
    /// retry, a jitter inside 2x, and a 2x move inside the gap earn none. The
    /// decision is pure over `(state, rtt, now)` so it is pinned without a
    /// broker.
    #[test]
    fn a_link_record_is_sent_on_change_on_a_2x_move_and_on_a_stale_report() {
        let t0 = Instant::now();
        let mut link = LinkReport::new();
        assert!(link.down_wants_report("refused"), "the first down is news");
        link.up = false;
        link.reason = "refused";
        link.reported_at = Some(t0);
        assert!(
            !link.down_wants_report("refused"),
            "a retry that fails the same way is not"
        );
        assert!(link.down_wants_report("no-socket"), "a different reason is");
        assert!(link.ack_wants_report(1, t0), "down to up is");

        let up = LinkReport {
            up: true,
            reason: "",
            rtt_ms: Some(2),
            reported_at: Some(t0),
        };
        assert!(up.down_wants_report("closed"), "up to down is");
        assert!(
            !up.ack_wants_report(3, t0 + Duration::from_millis(300)),
            "within 2x: quiet"
        );
        assert!(
            !up.ack_wants_report(4, t0 + Duration::from_millis(300)),
            "exactly 2x: quiet"
        );
        assert!(
            up.ack_wants_report(5, t0 + Duration::from_millis(300)),
            "past 2x, past the gap"
        );
        assert!(
            !up.ack_wants_report(5, t0 + Duration::from_millis(100)),
            "past 2x, inside the gap"
        );
        assert!(
            !up.ack_wants_report(1, t0 + LINK_MOVE_GAP),
            "a drop to exactly half is not MORE than 2x"
        );
        let slow = LinkReport {
            rtt_ms: Some(3),
            ..up.clone()
        };
        assert!(
            slow.ack_wants_report(1, t0 + LINK_MOVE_GAP),
            "a drop past 2x counts, past the gap"
        );
        assert!(
            !slow.ack_wants_report(1, t0 + Duration::from_millis(100)),
            "...and not inside it"
        );
        assert!(
            !up.ack_wants_report(2, t0 + Duration::from_millis(1_999)),
            "same rtt, inside the refresh"
        );
        assert!(
            up.ack_wants_report(2, t0 + LINK_REFRESH),
            "the same rtt after the refresh window"
        );
        assert!(
            LINK_MOVE_GAP < LINK_REFRESH,
            "the move gap is the finer bound"
        );
        assert!(
            ACK_DEADLINE >= RECONNECT_MAX,
            "the ack deadline is not shorter than a back-off tick"
        );
    }

    /// EVERY TRANSPORT FAILURE HAS ONE TOKEN, and the broker's own refusal is the
    /// caller's to name: `Other` is how the client surfaces a broker `Error`
    /// reply, so a refused attach and a refused read must not share a word with
    /// a dead socket. And a real ack never reads `rtt=0`.
    #[test]
    fn a_link_reason_is_one_token_per_failure_and_an_ack_never_reads_zero() {
        use io::ErrorKind as K;
        for (kind, token) in [
            (K::NotFound, "no-socket"),
            (K::ConnectionRefused, "refused"),
            (K::PermissionDenied, "denied"),
            (K::TimedOut, "no-ack"),
            (K::WouldBlock, "no-ack"),
            (K::UnexpectedEof, "closed"),
            (K::BrokenPipe, "closed"),
            (K::ConnectionReset, "closed"),
            (K::NotConnected, "closed"),
            (K::InvalidData, "error"),
        ] {
            assert_eq!(
                link_reason(&io::Error::new(kind, "x"), "attach"),
                token,
                "{kind:?}"
            );
        }
        assert_eq!(
            link_reason(&io::Error::other("denied by cap"), "attach"),
            "attach"
        );
        assert_eq!(
            link_reason(&io::Error::other("denied by cap"), "read"),
            "read"
        );
        assert_eq!(rtt_ms_of(Duration::from_micros(1)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(999)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(1_000)), 1);
        assert_eq!(rtt_ms_of(Duration::from_micros(1_001)), 2);
        assert_eq!(
            rtt_ms_of(Duration::ZERO),
            0,
            "only a zero-length round trip reads 0, and none is"
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

    /// A minted node id is `n-` plus sixteen lowercase hex digits — a principal
    /// by §3.2's grammar, so it can be a subject segment and a cap principal
    /// without any further escaping.
    #[test]
    fn a_minted_node_id_is_a_principal() {
        let id = mint_node_id();
        assert!(subject::is_principal(&id), "{id}");
        assert_eq!(id.len(), 18);
        assert!(id.starts_with("n-"));
        assert_ne!(id, mint_node_id(), "the CSPRNG is not a constant");
    }
}
