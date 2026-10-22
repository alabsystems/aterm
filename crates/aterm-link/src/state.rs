// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The state dir: the facts a bridge must not lose across a `kill -9`.
//!
//! Every file is replaced whole: a temp of the writer's own (`create_temp_beside`),
//! then a `rename` over the target, so a reader sees the old value or the new one,
//! never a truncation. A half-written producer sequence is worse than none (the
//! broker would dedup live records away), so nothing is updated in place — but
//! `acked`, a set only ever grown, which is APPENDED to, one offset per
//! `write`, and whose reader takes no unterminated tail as an offset
//! ([`AckedLog`]).
//!
//! ## Atomic and durable writes
//!
//! The failure model is a `kill -9` of the bridge (or a crash of aterm), and the
//! atomic replace alone survives it: the bytes and the rename are in the kernel once
//! the calls return. The exactly-once rungs test it with a real `SIGKILL` at named
//! points.
//!
//! A power loss or an OS crash is survived only by a durable write: the temp flushed
//! before the rename, the directory after it (a flushed file whose entry is not may
//! be gone after the boot). On macOS each flush is `F_FULLFSYNC`, a whole-drive flush
//! of several milliseconds, so it is spent only where a power loss would outlive
//! what the file pairs with:
//!
//! - Durable: `node`, `inc`, `pins`, `halts`, `rewound-log`, `logkey`, `logfloor`.
//!   The node id is the mail
//!   address and what its caps were minted for; the incarnation lease's top fences
//!   every incarnation and sequence the next boot uses (written once per lease of
//!   [`INC_LEASE`], not once per attach: see the next section); a pin is the T15
//!   first sight of a remote sid, which outlives this host's reboot; a standing halt
//!   is a human's, and the bus row it stands in for may be gone; a rewound identity
//!   stays refused even if its head grows again, and releasing it first flushes the
//!   replacement log's witness; the bus mirror's alias key is every edge's alias for
//!   the node's life (ruling Q6), and the floor of its lifetime total is what a
//!   total resumes at when its pin cannot say (ruling Q13, raised once per
//!   [`crate::mirror::FLOOR_LEASE_BYTES`] of bodies). Each is written once, or when
//!   its set changes, never per message.
//! - Atomic only: everything else (`logpin`, the bus mirror's one batch in
//!   flight, written only by a bridge with `[fabric] log = "turns"`, among it). A power loss ends what those files pair with
//!   anyway: the outbox, the owed receipts and the inbox ring live in aterm's memory
//!   (a seamless update carries them, a reboot does not), and aterm mints fresh sids
//!   at every launch, so every file serving a session (`seen/`, `sent/`, `acks/`,
//!   `keys/`, `topics/`, `unhandled/`, `asked`, `deadlines`, `acked`, `inflight/`)
//!   is orphaned by a reboot, flushed or not. The not-hosted gate runs before the
//!   self-lane check, so a stale `acked` judges no record of a session that is gone.
//!   `seq` is fenced by the durable `inc`: the first attach after the boot takes
//!   `inc + 1`, and every sequence of it is above anything a rolled-back `seq` could
//!   name. An attach trusts `inc-lease` only in the boot that wrote it, so no
//!   rolled-back one decides an incarnation. The doctor's `publish_exited`, which
//!   does not roll the incarnation, reads it from any boot ([`StateDir::incarnation`]):
//!   a rolled-back or lost lease names an incarnation at or below the one used, never
//!   above it. It meets a rolled-back `seq` as a deduped publish and refuses
//!   (fail-closed) until a bridge has attached once. A rolled-back `halt-acked`
//!   costs one re-ack, and a rolled-back `bus-log` is a lower high-water, which the
//!   halts do not rest on.
//!
//! ## The incarnation lease
//!
//! An attach takes a fresh incarnation, and the incarnation is the fence on
//! everything the node did before it: every sequence of incarnation n+1 is above
//! incarnation n's reserved top (the will's number), so a rolled-back `seq` reuses
//! nothing, and a predecessor's will that fires late, from a connection still half
//! open, is suppressed by the broker's fence (`Bridge::bring_presence_up`). The
//! fence asks one thing of every attach: that the incarnation it takes is above
//! every incarnation this node ever PRESENTED to a broker — registered a will
//! under, or published under.
//!
//! The local disk has to answer that on its own. The bus's node row is a second
//! opinion (`max(local, bus) + 1`), not a sufficient one: a registered will is a
//! hidden `/a/will` record that no `Last` read returns, and a remote broker keeps it
//! through this host's power loss, on a connection that may still be half open
//! when the host is back. An incarnation whose will is registered and whose `live`
//! row has not landed is invisible on the bus; reusing it lets the old will fire,
//! unfenced, over the new incarnation's `live`, and makes the new one's own will a
//! duplicate that never fires.
//!
//! So `inc` was flushed — temp and directory, two `F_FULLFSYNC`s, 8–12 ms measured
//! in place — before every will, on the attach's critical path. It is still
//! flushed before any incarnation it covers is presented, but as a LEASE: `inc`
//! holds the TOP of a block of [`INC_LEASE`] incarnations, and `inc-lease` (atomic,
//! never flushed) names the one the last attach took, the top it took it under, the
//! very `inc` file that top was flushed into ([`IncFile`]), and the BOOT it was
//! written in ([`crate::durability::this_boot`]). An attach in that same boot, on that
//! same file, trusts `inc-lease` exactly, because the page cache holding it has run
//! throughout (a `kill -9`, a crash of aterm, an update's handoff and a sleep all
//! keep it), and takes the next incarnation of the lease with no flush. An attach
//! in any other boot — or one that cannot name its boot, or whose state dir is off
//! the boot volume, where a disk can vanish while the kernel runs (astream's weak
//! tiers draw the same line: `store.rs`, `Volume`), or a copy of the dir, whose
//! `inc` nobody flushed — trusts only `inc`, takes the incarnation above the whole
//! lease, and leases afresh, flushed, before it is presented.
//! [`StateDir::take_incarnation`] is the one writer.
//!
//! WHY IT HOLDS — every presented incarnation is at or below the `inc` the media
//! holds, and that only grows:
//!
//! - `inc-lease` names a top only after the flushed write of that top returned, and
//!   an attach that takes an incarnation without a flush takes one at or below the
//!   top a lease of THIS boot named.
//! - `inc-lease` is trusted only while the top it names is the top `inc` holds. A
//!   flushed write a `kill -9` cut between its rename and its directory flush (a top
//!   in the page cache that may not be on the media), or an older build that took
//!   `inc + 1` and wrote that, leaves the two apart, and the attach leases afresh.
//! - `inc-lease` is trusted only while the `inc` file and the directory it names are
//!   the ones there now: a copy of the dir (`cp -R`, a Finder duplicate, `rsync`, a
//!   backup restored) carries this boot's lease and its top beside an `inc`, in a
//!   directory, that nobody flushed, and leases afresh at its first attach.
//! - `seq`'s incarnation counts as taken: an older build's doctor publishes under
//!   whatever `inc` holds, a lease's unused top included. This build's doctor never
//!   does ([`StateDir::incarnation`]).
//!
//! A POWER LOSS at any step: the next boot trusts only `inc`, whose media value is
//! at least every top a lease ever named, so it takes an incarnation above
//! everything presented, whichever versions of `inc-lease` and `seq` the media kept.
//! A KILL at any step of one boot: until `inc-lease` names the new incarnation,
//! nothing of it was presented, and the next attach takes it again or (a top already
//! renamed in) leases past it; once it does, the next attach takes the one above.
//! The tests walk every step against a simulated power loss
//! (`the_incarnation_lease_refines_its_model`), bound to the model
//! `aterm-models-session` states (`incarnation_lease_model`); the copy, the doctor
//! and the shipped trust decision have their own
//! (`a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease`,
//! `the_doctor_never_answers_a_lost_leases_unused_top`,
//! `the_shipped_trust_decision_leases_a_dir_on_the_boot_volume`).
//!
//! NOT an overlap of the flush with the will's and the `live` row's round trips,
//! joined before the first non-presence sequence. That lets the will reach the
//! broker before `inc` is on the media, and a power loss in that window leaves a
//! registered `/a/will` (on a remote broker, with its connection half open; on a
//! local one, fired at the broker's next open, but only enqueued, so the attach's
//! `Last` read can be served first) beside an `inc` that never moved: the next
//! attach takes the same incarnation. The model's `Buggy = 4` is that overlap.
//!
//! ## Whose row it is
//!
//! A session whose instance went with nobody to say so (a tab closed while no
//! bridge ran, an instance gone in a power cut) leaves its presence row `live`,
//! and the bridge's ghost sweep retires it once five minutes of its own sight
//! find nothing hosting it (`Bridge::retire_ghost_presence`). Which rows a bridge
//! may retire is the question: a SIBLING under a shared node id (a copy of this
//! dir, run by another aterm) may be alive, and its rows are not this bridge's.
//! An incarnation number does not answer it. A copy takes its numbers as this
//! dir does, `max(local, Last) + 1`, so it can take one this dir took too: one an
//! attach here took and never presented (its will refused, or the attach killed
//! before it), or one whose rows a log that lost power no longer shows. Finding
//! F4 of the simulation, and its review, are those two halves: retire only by
//! this incarnation and a witnessed will, and a ghost whose writer's successors
//! all died inside the five minutes stands until the operator retires it; retire
//! by every incarnation this dir took, and a copy that took one of the same
//! numbers has its live sessions retired.
//!
//! So the ROW says whose it is. Every session row this dir's bridges write
//! carries the dir's mark ([`StateDir::mark`], under [`PRESENCE_MARK`]): a hash
//! of the kernel boot identity, node and the dir's own device and inode. Device
//! and inode alone are local names: a cloned disk on another machine can repeat
//! both. A different boot therefore never grants adoption by the mark, even
//! when every directory byte and both local names match.
//! One state dir is one bridge ([`StateDir::lock_bridge`]), so while a bridge
//! holds the lock, every other bridge that wrote a row with its mark is dead,
//! and such a row at or below this incarnation in THIS BOOT is the bridge's to
//! retire, under the rules its own are. The mark is never stored and costs no
//! write. After a reboot, or without a kernel boot identity, only the own
//! exact writer-marked will rules apply; an incarnation number alone grants
//! no retirement, and unmarked legacy ghosts may need the operator.
//!
//! ## A torn file after a power cut
//!
//! "Atomic" is a promise against a `kill -9`, not against a power loss: POSIX
//! keeps nothing of an unflushed file's DATA across one, so a rename that
//! reached the media may name a file whose bytes did not — empty, or cut
//! anywhere. A reader of a number in this dir reads a cut one as a prefix, a
//! LOWER number, which every such file tolerates (the table), and an empty one
//! as absent. But the readers of the in-flight journal, the reservations and
//! the refill floors ([`TornKind`]) REFUSE damage, fail-closed, because within
//! one boot damage is a bug: refused at every attach of every later boot, a torn
//! one wedged the bridge until a person repaired the dir (the simulation's
//! finding F1) — while every one of them serves only sessions, which a reboot
//! ends, so the table calls losing it, after one, harmless (the rows marked
//! TORN). And a cut can READ: an in-flight entry cut just before its `log=`
//! line, or inside its hex body at an even length, is an entry with no witness
//! (a reply's, an unwitnessed reference the attach refuses for good).
//!
//! So the first bridge after an UNCLEAN STOP IN AN EARLIER BOOT — the durable
//! `run` record says the last bridge did not stop cleanly, in a boot the kernel
//! no longer runs ([`StateDir::torn_files_possible`], astream's own rule for its
//! weak tiers) — moves every file of those kinds that is not provably whole into
//! `quarantine/`, with its exact bytes, durably, before it unlinks it, and goes
//! on as if it were lost ([`StateDir::set_aside_torn`]). Provably whole: it ends
//! at the newline its writer ends it with, its reader takes it, and, in flight,
//! it names the log it was decided on (a cut anywhere short of the last byte
//! fails one of the three). Never deleted: what was moved is the owner's to read
//! or put back, and the bridge says where each went. Within one boot, on any
//! volume, and after a clean stop (whose every such file was flushed,
//! [`StateDir::end_run_clean`]), damage is refused as it always was.
//!
//! The other files cannot wedge torn: `node`, `inc`, `pins`, `halts`,
//! `rewound-log`, `logkey`, `logfloor`, `run` and `quarantine/` are durable (on
//! the media before anything rests on them), and every other reader takes an
//! empty file as absent and a cut number as a lower one — `inc-lease` as no
//! lease (the attach leases afresh), `seq` as a lower one the durable `inc`
//! fences, `acked`'s torn tail as no offset ([`AckedLog`]) — save `logpin`,
//! the mirror's pin, which holds what no reboot ends: the node's alias key
//! (ruling Q6) and its lifetime mirrored total (ruling Q13). Each has a DURABLE
//! COPY a bridge of this build writes before it is needed: `logkey` before the
//! first take that names the key, `logfloor` — at or above every byte the
//! mirror sent — before any byte of a batch is sent. The copies hold only while
//! no build that keeps neither writes the pin, so this build's pins are of a
//! version an older build refuses (`v=2`, [`crate::mirror::Pin`]), the first
//! one written durably. So the same first bridge RECONCILES the pin with them
//! ([`StateDir::reconcile_logpin`]): a torn one is moved into the quarantine
//! like the rest, a whole one written from the copies in its place, and the
//! mirror resumes with the same key and the floor as its total; a whole one a
//! cut may have rolled back to a lower total is raised to the floor. A pin
//! that does not read where the copies cannot be shown to cover it (no floor
//! or no key on the media, or an older build's pin), within one boot or after
//! a clean stop (which flushed it) stops the mirror and stays where it is, and
//! the bridge says what a person does (`Bridge::mirror_hello_answered`).
//!
//! ## An in-flight entry a power cut brought back
//!
//! A power cut does not only tear: it can roll back an unlink nothing flushed,
//! and the entry of an own-lane record that LANDED comes back
//! ([`StateDir::clear_in_flight`]). Republished by the next attach, it is
//! deduped where the log still holds the record — but where the log, on
//! another volume or after a lossy flush, lost the record on its own (the law
//! lets it lose an unescalated one) and the sender's later posts landed after,
//! it lands again BEHIND them: the sender's posts out of order (the
//! simulation's finding F2). Nothing in the dir or on the log tells that entry
//! from one whose record never landed (a read carries no producer sequence;
//! the same cut rolls the later post's reservation back), and nothing needs
//! either: the sessions they serve ended with the boot. So the same first
//! bridge, after the torn sweep, RETIRES every whole in-flight entry into the
//! quarantine as `retired.inflight.<seq>`, never publishing it again
//! ([`StateDir::retire_in_flight_of_an_earlier_boot`]). Within one boot, and
//! after a clean stop, the republish keeps the order, and stays — except
//! where the state dir's volume alone lost power and came back within the
//! boot, which nothing tells from a kill and which is not covered. No attach
//! goes on while `run` still names an earlier boot's unclean stop (the disk
//! refused this boot's [`StateDir::begin_run`]), so a boot's own entries are
//! never retired.
//!
//! Offsets name records in one log. `seen/`, `topics/`, `unhandled/`, `acked`,
//! `halt-acked`, `inbox-owed`, `deadlines`, `asked` and the `original-off` of `keys/`
//! and `sent/` hold bus offsets, each meaningless on another log, as is an `inflight/` record's
//! witnessed reference into one. The attach that finds another log ([`LogWitness`])
//! forgets those offsets before using them ([`StateDir::forget_log_offsets`]); a
//! reference witnessed on an unknown log stays uncertain. The rest of the dir is the
//! node's own and outlives any log.
//!
//! One state dir is one bridge. The dir is the node (its id, producer id and
//! sequence), so a second bridge on it would publish as the same producer from a
//! second counter: the two would dedup each other's records away and refuse each
//! other's own-lane mail as forged. [`StateDir::lock_bridge`] is the exclusive hold a
//! bridge keeps for its whole life.
//!
//! | file | write | what it holds | why losing it is not fatal |
//! |---|---|---|---|
//! | `lock` | — | nothing: the bridge's `flock` ([`StateDir::lock_bridge`]) | it is recreated empty; only the lock means anything |
//! | `pid` | — | the pid of the lock's holder | it names the holder in a refusal, nothing else |
//! | `node` | durable | this node's `n-<16 hex>` id | load-bearing: a new id is a new node, without its mail lane |
//! | `inc` | durable | the top of the incarnation LEASE: at or above every incarnation this node presented, flushed before the first of them is ([`StateDir::take_incarnation`]) | the bus's last presence row counts too: `max(local, Last) + 1` |
//! | `inc-lease` | atomic | `used=<inc> top=<inc> boot=<id> file=<dev>:<dir>:<ino>`: the incarnation the last attach took, the `inc` it took it under, the boot that wrote it, and the `inc` file that top was flushed into ([`IncLease`]) | the next attach leases afresh above `inc`: one flush, and the lease's unused rest burned; the doctor answers `seq`'s incarnation |
//! | `seq` | atomic | the last producer sequence of the newest reserved BLOCK, written before any number of the block is used ([`StateDir::reserve_sequence_block`]) | a `kill -9` skips the block's unused rest and reuses none; after a power loss, no dup either: the incarnation moved, and every sequence of `inc+1` is above `inc`'s reserved top |
//! | `seen/<sid>` | atomic | where the session's addressed refill starts after: the endpoint's reported floor minus one ([`StateDir::seen_off`]) | the refill starts lower and the ring dedups on `off=` |
//! | `acked` | append | the offsets with a `PublishAck` on this node's own lanes, each ended by its comma, one appended per own-lane publish, in one `write`, and compacted to the newest `keep` at twice that ([`AckedLog`]) | the self-lane check gets more conservative, never less |
//! | `inflight/<seq>` | atomic | one record bound for this node's own lanes and the log it was decided on, from before its publish until `acked` holds its offset ([`StateDir::in_flight`]) | the attach cannot re-learn an offset a kill left out of `acked`, and the self-lane check accuses that one record of ours; after a reboot, nothing: the record's session is gone, and the not-hosted gate runs before the self-lane check. TORN, set aside; whole after an unclean stop in an earlier boot, RETIRED unpublished ([`StateDir::retire_in_flight_of_an_earlier_boot`]) |
//! | `pins` | durable | the TOFU `sid → node` map, newest [`PINS_KEEP`] | a re-pin on next sight, the TOFU rule anyway |
//! | `halt-acked` | atomic | the highest fleet-halt offset this node has answered | a re-ack of an answered barrier: one extra retained row |
//! | `inbox-owed` | atomic | the inbox-group commit a bridge owed when it exited with no broker connection to make it on, every record at or below it accounted for ([`StateDir::inbox_owed`]); removed once the next attach commits it | the group offers those records again, and an endpoint that no longer remembers their offsets (an update's successor) lists them as new |
//! | `bus-log` | atomic | the identity of the log this node reads and the highest broker head seen there ([`LogWitness`]) | a replaced log is judged by its head alone until the next attach records the identity; the halts do not rest on it |
//! | `rewound-log` | durable | one known identity whose head this node saw go backwards | regrowth cannot make its reused offsets trustworthy; only a different known log releases the refusal |
//! | `halts` | durable | the fleet halts this node holds in force ([`StandingHalt`]) | the attach re-reads them from the bus; only one whose row the bus lost goes with it |
//! | `sent/<sid>.<id>` | atomic | the sequence reserved for one queued post, its publication identity, and whether a `key=` entry chose it, one line an older bridge reads too | load-bearing: a fresh sequence is a second copy — within one boot; after a reboot aterm's outbox is gone with its sessions, and nothing reads it. TORN, set aside |
//! | `keys/<sid>` | atomic | `post key=` → its reserved sequence, newest [`KEYS_KEEP`] per session | a re-post under a lost key is a second record — within one boot; after a reboot its session is gone. TORN, set aside |
//! | `acks/<sid>.<rid>` | atomic | the sequence reserved for one owed receipt (R8), removed at retirement | a fresh sequence is a second `ack` after a crash between publish and retirement — within one boot; after a reboot the receipt is gone with aterm's memory. TORN, set aside |
//! | `asked` | atomic | the recipient of each `ask`/`task` this node published, newest [`ASKED_KEEP`] | a reply to a forgotten ask is judged without its recipient (a deadline's `to` still counts) |
//! | `deadlines` | atomic | each `ask`/`task` published with `dl=` and not yet answered, newest [`DEADLINES_KEEP`] | no `expired` for those asks; the bus is checked before any verdict, so never a false one |
//! | `topics/<sid>` | atomic | `<topic> <next>` per broadcast opt-in ([`StateDir::topics`]) | the topic restarts from the session's `since=`, and `head` skips what was published meanwhile, so it is written every step |
//! | `unhandled/<sid>` | atomic | `<topic> <off>`: per topic, the lowest offset of a broadcast the endpoint took as new and `inbox seen` has not passed, written before the cursor moves past it ([`StateDir::topic_floors`]) | a successor that keeps the sid (a seamless update, a PTY keeper's recovery) is not offered those broadcasts again, though they are gone from its inbox; no successor keeps a sid across a reboot. TORN, set aside |
//! | `logpin` | atomic, flushed at a clean stop; durable where it replaces none of this build's | the bus mirror's one batch in flight, the node's total of mirrored bytes and its alias key ([`StateDir::logpin`]), `v=2`, which an older build refuses ([`crate::mirror::Pin`]); only with `[fabric] log = "turns"` | an older whole version: one batch taken again at fresh sequences, a second copy each reader dedupes; after an unclean stop in an earlier boot, its total raised to `logfloor` ([`StateDir::reconcile_logpin`]), which counts the floor's lease as mirrored. TORN there, with `logfloor` and `logkey` on the media: set aside, a whole pin written from them in its place, and the mirror resumes (rulings Q6, Q13). Torn with no floor or no key (an older build's pin, the only copy), torn as an older build's `v=1`, within one boot or after a clean stop: the mirror stops, keeps it as it is, and says what a person does |
//! | `logkey` | durable | the bus mirror's alias key (ruling Q6), 32 hex digits, written once, before the first take that names it ([`StateDir::logkey`]); only with `[fabric] log = "turns"` | load-bearing: a fresh key is a second alias for every edge on the bus, so a torn pin is never set aside without it. Missing in a dir only an older build mirrored in, whose pin's `key=` is copied here before the next take |
//! | `logfloor` | durable | the floor of the bus mirror's lifetime total (ruling Q13): at or above every body byte it sent, raised by [`crate::mirror::FLOOR_LEASE_BYTES`] past a batch before any byte of it is sent ([`StateDir::logfloor`]); only with `[fabric] log = "turns"` | the total a mirror resumes at when its pin is gone or may be rolled back: the lease's unused rest and the batch in flight are counted as mirrored, never a byte less. Missing: no bridge of this build sent a batch, and a torn pin stops the mirror |
//! | `run` | durable | the boot the last bridge ran in and whether it stopped cleanly, every file of a [`TornKind`] it wrote flushed first ([`LastRun`]): written once per boot by the first bridge, after its sweep, and at a clean stop (a quit: a seamless update's handoff leaves it open for the successor that goes on with the run) | a dir without one is judged by its lease's boot, as a build before it was; a torn file found after a stop it cannot call unclean is refused where it is read |
//! | `quarantine/` | durable | the exact bytes of each file found torn after an unclean stop in an earlier boot, moved aside by the first bridge after it ([`StateDir::set_aside_torn`], [`QUARANTINE`]) — the mirror's pin among them, as `logpin`, once its copies are on the media ([`StateDir::reconcile_logpin`]) — and of each whole in-flight entry it retired then, as `retired.inflight.<seq>` ([`StateDir::retire_in_flight_of_an_earlier_boot`]) | nothing reads it: it is the owner's, to read or remove. Only its count is read: how many torn files it still holds ([`StateDir::quarantined_torn`]), which at none ends aterm's entry for the owner's look |

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// One outstanding deadline: an `ask` or `task` this node published with `dl=` and
/// has not yet seen an `answer`, `report` or `ack` for.
///
/// The asker's own bridge records `expired re=<off> dl=<ms>` when it passes (§6.4,
/// R8; the broker holds no timers). `at` is absolute (the publish clock plus `dl`),
/// so a relaunched bridge does not restart it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadline {
    /// The bus offset of the ask (what a reply carries back as `re=`).
    pub off: u64,
    /// The asking session.
    pub sid: String,
    /// When it expires, in `now_ms()`.
    pub at: u64,
    /// The advisory `dl=` the ask carried, echoed onto the verdict.
    pub dl: u64,
    /// The principal whose reply settles it: the node (or `p/` principal) owning the
    /// lane the ask went to ([`Asked`]). `None` for a line without `to=`, which any
    /// reply settles.
    pub to: Option<String>,
}

impl Deadline {
    /// The one line this deadline is stored as.
    #[must_use]
    pub fn render(&self) -> String {
        let mut line = format!(
            "off={} sid={} at={} dl={}",
            self.off, self.sid, self.at, self.dl
        );
        if let Some(to) = &self.to {
            line.push_str(&format!(" to={to}"));
        }
        line
    }

    /// Parse one back; a partial line is `None`, never a deadline to publish a verdict on.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let (mut off, mut sid, mut at, mut dl, mut to) = (None, None, None, None, None);
        for tok in line.split_whitespace() {
            match tok.split_once('=') {
                Some(("off", v)) => off = v.parse().ok(),
                Some(("sid", v)) => sid = Some(v.to_string()),
                Some(("at", v)) => at = v.parse().ok(),
                Some(("dl", v)) => dl = v.parse().ok(),
                Some(("to", v)) => to = Some(v.to_string()),
                _ => {}
            }
        }
        let sid: String = sid?;
        if sid.is_empty() {
            return None;
        }
        // A `to=` that is present and empty is a damaged line, not an old one.
        if to.as_deref() == Some("") {
            return None;
        }
        Some(Self {
            off: off?,
            sid,
            at: at?,
            dl: dl?,
            to,
        })
    }
}

/// One `ask`/`task` this node published, and the principal whose reply it is: the
/// node owning the lane it went to (`/f/<F>/in/<node>/<sid>/…`) or the principal of a
/// `p/` lane (`/f/<F>/in/p/<principal>/…`). The broker binds a reply's `<src>`
/// segment to its publisher, so this tells the recipient's receipt from a third
/// node's record naming the same offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The bus offset of the ask.
    pub off: u64,
    /// The principal whose reply it is.
    pub to: String,
}

impl Asked {
    /// The one line this entry is stored as: `<off> <to>`.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} {}", self.off, self.to)
    }

    /// Parse one back; a partial line is `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut toks = line.split_whitespace();
        let off = toks.next()?.parse().ok()?;
        let to = toks.next()?.to_string();
        toks.next().is_none().then_some(Self { off, to })
    }
}

/// How many published asks the node remembers the recipient of, newest last.
/// A `post --wait-ack` parks for at most 600 s, and a deadline carries its
/// own [`Deadline::to`]: this only has to outlive the waits, with room.
pub const ASKED_KEEP: usize = 1024;

/// How many `post key=` reservations one session keeps, newest last: R7's bound on
/// the broker's dedup window. A re-post under an evicted key is a new record.
pub const KEYS_KEEP: usize = 4096;

/// How many outstanding deadlines the node keeps, newest offset last.
pub const DEADLINES_KEEP: usize = 4096;

/// How many TOFU pins the table keeps, oldest first sight out.
///
/// `pins` gains a line per remote sid ever addressed by a bare `@s-<sid>`, sids are
/// never reused, and `Bridge::pinned_node_for` re-reads the file on every such post.
/// Eviction is by first sight, not use: recency would cost a durable rewrite per
/// post, and a re-pin on next sight is the TOFU rule. An evicted pin costs the T15
/// protection for a sid not among the last [`PINS_KEEP`] fresh ones. Deleting
/// `<state>/pins` is safe.
pub const PINS_KEEP: usize = 4096;

/// One session's position on one broadcast topic: the offset it resumes from after a
/// bridge restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopicCursor {
    /// The next offset this topic may deliver from.
    pub next: u64,
}

impl TopicCursor {
    /// Parse the state file's `<topic> <next>` line; `None` for anything else, which
    /// the caller reports. A third number (a 0.90.x bridge's add serial) is read past.
    ///
    /// The topic is held to the grammar ([`crate::subject::is_topic`]) like every
    /// other way a topic enters the bridge: the catch-up builds a `Fetch` filter from
    /// it, and no broadcast under any other name can be delivered anyway.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, TopicCursor)> {
        let mut words = line.split_whitespace();
        let topic = words.next().filter(|t| crate::subject::is_topic(t))?;
        // Digits only: `u64::from_str` would accept `+5`.
        let number = |w: &str| {
            (!w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
                .then(|| w.parse::<u64>().ok())?
        };
        let next = number(words.next()?)?;
        if let Some(extra) = words.next() {
            number(extra)?;
        }
        if words.next().is_some() {
            return None;
        }
        Some((topic.to_string(), TopicCursor { next }))
    }
}

/// What this node knows of the bus log it reads: the `bus-log` file.
///
/// Every offset in this dir is an offset on one log and means nothing on another:
/// a log deleted and recreated, truncated by a repair, restored from a backup, or
/// another host's broker gives the same numbers to different records.
/// [`LogWitness::replaced_by`] is the one judgement of "this is not the log those
/// offsets index", from two kinds of evidence:
///
/// * The log's identity, which astream answers every `Hello` with
///   ([`astream_broker::LogIdentity`]: a name for the log and the generation of its
///   offsets). Any difference is another log, whatever its head. A broker that
///   predates identity answers none, which says nothing either way; the identity
///   seen last stays the witness.
/// * The high-water: offsets on one log only grow, so a head below the highest this
///   node saw there is another log (or the same one rolled back under its own
///   identity, which reuses offsets just the same). It is the only evidence against
///   a broker without identity, and it catches only a shorter log.
///
/// Every offset in the dir rests on the verdict ([`StateDir::forget_log_offsets`]),
/// and so does the reporting; the halts do not ([`StandingHalt`]). Unknown tokens,
/// and an `id=` that does not parse, are read past, so a newer bridge's line still
/// parses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogWitness {
    /// The highest broker head (one past the newest offset) this node has seen
    /// on the log it reads.
    pub high: u64,
    /// The identity of that log, once a broker that answers one was read.
    pub id: Option<astream_broker::LogIdentity>,
}

impl LogWitness {
    /// The one line this is stored as: `[id=<identity> ]high=<n>`.
    #[must_use]
    pub fn render(&self) -> String {
        match self.id {
            Some(id) => format!("id={id} high={}", self.high),
            None => format!("high={}", self.high),
        }
    }

    /// Parse one back: `None` without a readable `high=`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let high = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("high="))
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let id = line
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("id="))
            .and_then(astream_broker::LogIdentity::parse);
        Some(Self { high, id })
    }

    /// Whether a broker answering `head` under `id` is provably not the log this
    /// witness saw: another identity, or a head below one this node already read
    /// there.
    #[must_use]
    pub fn replaced_by(&self, head: u64, id: Option<astream_broker::LogIdentity>) -> bool {
        head < self.high || matches!((self.id, id), (Some(was), Some(now)) if was != now)
    }

    /// WHERE THE LOG THIS WITNESS SAW CONTINUES on the log a broker answers as
    /// `id`, with the `lineage` its same `Hello` told
    /// ([`astream_broker::Client::log_lineage`]): `Some(k)` — every offset below
    /// `k` names the same record on both, so an offset this dir holds resumes
    /// at the lower of itself and `k` ([`StateDir::forget_log_offsets`]'s
    /// `keep_below`) instead of being forgotten; `None` — another log as far as
    /// anything can tell, and every offset is forgotten, as before lineage.
    ///
    /// A moved GENERATION of the same log continues when astream recorded
    /// where the generations agree ([`astream_broker::LogIdentity::continues`]):
    /// after a power loss under a weak tier, a torn batch cut, a repair's cut.
    /// It never continues across another `id`, without a witnessed identity
    /// (a witness written before identity, a broker before it), without
    /// lineage (a broker before it, or a log found shorter, re-based or foreign,
    /// which astream records none for), or past the newest
    /// [`astream_broker::Lineage::MAX`] steps. The same identity is
    /// `Some(u64::MAX)` — nothing to clamp — and a head below the high-water
    /// under it is the rewound log a bridge refuses, never one it continues.
    #[must_use]
    pub fn continues(
        &self,
        id: Option<astream_broker::LogIdentity>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> Option<u64> {
        let (was, now) = (self.id?, id?);
        if was == now {
            return Some(u64::MAX);
        }
        now.continues(was, lineage?)
    }
}

/// One fleet halt this node holds in force ([`StateDir::standing_halts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingHalt {
    /// The `reason=` the sessions are held under: the human's own words, the
    /// pct token the bridge passed to `hold` (it holds no space).
    pub reason: String,
    /// `true` when the face this node reads carries no row for it — it was
    /// read on a log since replaced — so it stands on this file alone, and
    /// only its human's own row on this log lifts it.
    pub kept: bool,
}

impl StandingHalt {
    /// `<fleet> <human> <reason> witnessed|kept`.
    #[must_use]
    pub fn render(&self, fleet: &str, human: &str) -> String {
        let how = if self.kept { "kept" } else { "witnessed" };
        format!("{fleet} {human} {} {how}", self.reason)
    }

    /// Parse one back as `(fleet, human, halt)`; a partial line, or a "human" that is
    /// not an `h-` principal, is `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<(String, String, Self)> {
        let mut toks = line.split_whitespace();
        let (fleet, human, reason, how) = (toks.next()?, toks.next()?, toks.next()?, toks.next()?);
        if toks.next().is_some() || !human.starts_with("h-") {
            return None;
        }
        let kept = match how {
            "kept" => true,
            "witnessed" => false,
            _ => return None,
        };
        Some((
            fleet.to_string(),
            human.to_string(),
            Self {
                reason: reason.to_string(),
                kept,
            },
        ))
    }
}

/// One state-file line, bounded and stripped of control characters for a
/// report: the file is on disk and anything that can write the state directory
/// can put an escape sequence in it, and this line goes to a terminal.
fn safe_line(line: &str) -> String {
    line.chars().filter(|c| !c.is_control()).take(120).collect()
}

/// Whether a durable write really flushes (`F_FULLFSYNC` on macOS): always, but in
/// this crate's own unit tests. No test can see a flush — only its count
/// ([`FileOps::fsyncs`], counted either way) and the kill points around it
/// ([`IncSeam`]) — and the flushes, serialised across the drive at about 4 ms each,
/// were most of those tests' wall. Every other build flushes, another crate's tests
/// of this one included.
pub(crate) const fn flushes_media() -> bool {
    !cfg!(test)
}

/// Flush a directory, so the entries a rename put in it survive a power loss.
///
/// Tolerates what astream's log does (`store.rs`'s `sync_parent_dir`): a filesystem
/// that cannot flush a directory handle (`Unsupported`, `InvalidInput`), a
/// directory this process may write but not open (`PermissionDenied`), one already
/// gone (`NotFound`). The file's bytes are flushed already, and refusing here would
/// refuse a state dir that works.
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    sync_dir_as(dir, aterm_durable::Flush::Durable)
}

/// [`sync_dir`] with `flush` (`aterm_durable`'s two): the full flush, or the
/// barrier that orders the directory's entry changes ahead of every later
/// write on its device ([`StateDir::end_run_clean`]'s). The same errors are
/// tolerated; a directory so tolerated was not flushed, and orders nothing.
pub(crate) fn sync_dir_as(dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    // The open, and what its error is taken for, run in every build: only the
    // flush is skipped in this crate's tests ([`flushes_media`]).
    match std::fs::File::open(dir).and_then(|d| {
        if flushes_media() {
            flush.file(&d)
        } else {
            Ok(())
        }
    }) {
        Err(e)
            if !matches!(
                e.kind(),
                io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ) =>
        {
            Err(e)
        }
        _ => Ok(()),
    }
}

/// THE INCARNATION LAYOUT IS ASTREAM'S: incarnation `inc` owns the producer
/// sequences [`astream_broker::incarnation_base`]`(inc)` (`inc << 32`) through
/// [`astream_broker::will_seq`]`(inc)` (`(inc << 32) | WILL_SEQ_LOW`), its ordinary
/// publishes strictly between the two and its will at the top, so no ordinary
/// publish of an incarnation collides with its will and every sequence of the next
/// is above it (§7). astream exports the layout beside the will fence it serves, and
/// states there the producer obligation the fence rests on (`Request::Will`): a new
/// incarnation's sequences above every one ever presented, and the incarnation
/// flushed before its will is registered. The layout meets the first clause by
/// construction; the second, and the counter that hands incarnations out, stay this
/// state dir's (the module doc's "The incarnation lease",
/// [`StateDir::take_incarnation`], [`StateDir::reserve_sequence_block`]).
///
/// The values were this file's before they were astream's, and they are a
/// PERSISTENCE CONTRACT: `seq` and `inc` hold numbers in this layout, so a changed
/// value would let an upgraded bridge present a sequence its older build already
/// presented. [`WILL_SEQ_LOW`] is the will's low word; [`INC_MAX`] the highest
/// incarnation a node may run under, the last whose will sits below astream's
/// reserved ack half ([`astream_broker::ACK_SEQ_BASE`]), where a will is refused
/// (`SeqReserved`) and an ordinary publish is the caller's to keep out
/// (`Client::publish`'s contract). Nothing above it is adopted, persisted or reserved
/// under, and astream's `incarnation_base` and `will_seq` answer `None` there.
pub use astream_broker::{INC_MAX, WILL_SEQ_LOW};

/// The incarnation `seq`, a number this state dir stored as its own, belongs to
/// ([`astream_broker::incarnation_of`]). A number in astream's reserved ack half is
/// no incarnation's: it is a corrupt `seq`, never incarnation 0, so it answers
/// `u64::MAX` — past [`INC_MAX`], where every take and reservation refuses, by name,
/// rather than hand out sequences the node already presented.
fn incarnation_of_seq(seq: u64) -> u64 {
    astream_broker::incarnation_of(seq).unwrap_or(u64::MAX)
}

/// How many incarnations one flushed write of `inc` covers (the module doc's "The
/// incarnation lease"): the attaches of one boot pay one flush per lease instead of
/// one each, and a boot that cannot trust the last lease burns at most this many of
/// [`INC_MAX`]'s 2^31 − 1 — a whole lease burned at every boot would last 33 million
/// boots. The bridge's sequence block (`SEQ_BLOCK`) makes the same trade one level
/// down, and for the same reason: a number nobody reads back until the next start
/// is written once per block, and a crash burns the block's unused rest.
pub const INC_LEASE: u64 = 64;
const _: () = assert!(INC_LEASE >= 1 && INC_LEASE <= INC_MAX);

/// The token a session's presence row carries this state dir's mark under
/// ([`StateDir::mark`], the module doc's "Whose row it is"): `statedir=<16 hex>`.
pub(crate) const PRESENCE_MARK: &str = "statedir";

/// What `inc-lease` holds: the incarnation the last attach took (`used`), the top of
/// the lease it took it under — what `inc` held, FLUSHED, when this was written —
/// the boot that wrote it, `None` where the kernel names none
/// ([`crate::durability::this_boot`]) or the state dir is off the boot volume, and the
/// very `inc` file that flush went into ([`IncFile`]). The module doc's "The
/// incarnation lease" is the argument; [`StateDir::take_incarnation`] is the one
/// writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncLease {
    /// The incarnation the last attach took.
    pub used: u64,
    /// The lease's top, flushed into `inc` before `used` was presented.
    pub top: u64,
    /// The boot that wrote it, the one boot whose page cache vouches for it.
    pub boot: Option<[u8; 16]>,
    /// The `inc` the top was flushed into, `None` where the platform names no inode.
    pub file: Option<IncFile>,
}

/// WHICH `inc` A LEASE VOUCHES FOR: its device, the state dir's inode and `inc`'s own
/// inode, as the flushed write left them ([`StateDir::inc_file`]).
///
/// A lease's top was flushed into ONE file, renamed into ONE directory whose entry was
/// then flushed. A copy of the state dir in the same boot — `cp -R`, a Finder
/// duplicate, `rsync`, a backup restored, or `cp -al`'s hard links — carries the same
/// lease and the same top in a directory, and files, that nobody flushed: trusted on
/// boot and top alone, the copy would present a whole lease of incarnations with no
/// flushed `inc` under them, and after a power loss take one of them again. A copy's
/// directory is another inode, and a restored `inc` is another file, so the lease
/// names both, and is trusted only where both are still the ones it names
/// ([`plan_incarnation`]). A flushed write renames a NEW file in, so the lease it
/// writes names the new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncFile {
    /// The device both live on.
    pub dev: u64,
    /// The state dir's inode.
    pub dir: u64,
    /// `inc`'s inode.
    pub ino: u64,
}

impl IncLease {
    /// The one line it is stored as.
    pub(crate) fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "-".to_string(), |b| aterm_codec::hex::encode_lower(&b));
        let file = self.file.map_or_else(
            || "-".to_string(),
            |f| format!("{}:{}:{}", f.dev, f.dir, f.ino),
        );
        format!(
            "used={} top={} boot={boot} file={file}",
            self.used, self.top
        )
    }

    /// Parse one back: all four fields once each, `used` at or below `top`, a boot that
    /// is `-` or a boot id, and a file that is `-` or `<dev>:<dir>:<ino>`. Anything
    /// else — a torn or hand-edited file — is `None`, which the next attach reads as no
    /// lease at all: the direction that flushes, never the one that trusts.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (mut used, mut top, mut boot, mut file) = (None, None, None, None);
        for tok in text.split_whitespace() {
            match tok.split_once('=')? {
                ("used", v) if used.is_none() => used = Some(v.parse().ok()?),
                ("top", v) if top.is_none() => top = Some(v.parse().ok()?),
                ("boot", "-") if boot.is_none() => boot = Some(None),
                ("boot", v) if boot.is_none() => {
                    boot = Some(Some(astream_sys::parse_boot_id(v.as_bytes())?));
                }
                ("file", "-") if file.is_none() => file = Some(None),
                ("file", v) if file.is_none() => {
                    let mut ids = v.split(':').map(|n| n.parse::<u64>().ok());
                    let (dev, dir, ino) = (ids.next()??, ids.next()??, ids.next()??);
                    if ids.next().is_some() {
                        return None;
                    }
                    file = Some(Some(IncFile { dev, dir, ino }));
                }
                _ => return None,
            }
        }
        let (used, top, boot, file) = (used?, top?, boot?, file?);
        (used <= top).then_some(Self {
            used,
            top,
            boot,
            file,
        })
    }
}

/// What one attach does about its incarnation ([`plan_incarnation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IncPlan {
    /// The incarnation it takes.
    pub next: u64,
    /// The lease top `inc-lease` will name, flushed into `inc` first when `flush`.
    pub top: u64,
    /// Whether `inc` is flushed before `next` may be presented.
    pub flush: bool,
}

/// One past both the state dir's and the bus's incarnation, or `None` past
/// [`INC_MAX`] — checked rather than wrapped, so a corrupt `inc` file is a named
/// refusal and not an incarnation that collides with an old one.
pub(crate) fn next_incarnation(local: u64, bus: u64) -> Option<u64> {
    local.max(bus).checked_add(1).filter(|inc| *inc <= INC_MAX)
}

/// THE ATTACH'S DECISION, from what the disk and the bus say: `top` is what `inc`
/// holds and `file` which file holds it ([`IncFile`]), `lease` what `inc-lease`
/// holds, `seq_inc` the incarnation `seq`'s number belongs to, `boot` this boot when
/// the page cache can vouch for the state dir (the kernel names the boot and the dir
/// is on the boot volume), and `bus` the newest `inc=` on the node's own presence
/// face. `None` past [`INC_MAX`].
///
/// The lease is TRUSTED only when this boot wrote it, the top it names is the top
/// `inc` holds, and the `inc` it names is this very file in this very directory;
/// then what it says was taken is exactly what was taken. Otherwise everything up to
/// `inc` counts as taken. `seq`'s incarnation counts either way. The incarnation
/// taken is one past all of that and the bus; it is covered with no flush when a
/// trusted lease's top is at or above it, and leased afresh — one flush of `inc`
/// covering `span` incarnations ([`INC_LEASE`]) — when not. A boot the page cache
/// cannot vouch for gets a lease of one (`inc` is then exactly the incarnation taken,
/// as the file always was), since no later attach could trust the rest of a longer
/// one.
pub(crate) fn plan_incarnation(
    top: u64,
    file: Option<IncFile>,
    lease: Option<IncLease>,
    seq_inc: u64,
    boot: Option<[u8; 16]>,
    bus: u64,
    span: u64,
) -> Option<IncPlan> {
    let trusted = lease.filter(|l| {
        boot.is_some() && l.boot == boot && l.top == top && file.is_some() && l.file == file
    });
    let taken = trusted.map_or(top, |l| l.used).max(seq_inc);
    let next = next_incarnation(taken, bus)?;
    if trusted.is_some() && next <= top {
        return Some(IncPlan {
            next,
            top,
            flush: false,
        });
    }
    let span = if boot.is_some() { span.max(1) } else { 1 };
    Some(IncPlan {
        next,
        top: next.saturating_add(span - 1).min(INC_MAX),
        flush: true,
    })
}

/// Whether `dir` shares a device with the boot volume: the device that loses power
/// only together with the kernel, so a page cache that has run throughout this boot
/// still holds every write made to it. ASTREAM'S PREDICATE
/// ([`astream_broker::on_boot_volume`]), the one its weak tiers judge their log by,
/// asked of the directory opened as a file (which answers for the files written in
/// it), so a lease is trusted exactly where astream trusts a weak log's page cache:
/// the device of what was opened, following links, and `false` wherever that cannot
/// be read and off Unix, the safe direction (a wrong `false` costs a flush, a wrong
/// `true` would trust a write the disk may have lost). Off the boot volume, a
/// removable or network disk can vanish while the kernel runs and take an unflushed
/// write with it, so no lease written there is trusted.
fn on_boot_volume(dir: &Path) -> bool {
    std::fs::File::open(dir).is_ok_and(|opened| astream_broker::on_boot_volume(&opened))
}

/// The points of [`StateDir::take_incarnation`] a test can kill it at
/// ([`IncSeam::kill_at`]). A kill anywhere else is a kill before the take, or one
/// after it returned and before the will went out, which the walk stands in
/// without help.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncStep {
    /// `inc`'s flushed write has renamed its temp over `inc` and not yet flushed the
    /// directory: the new top is in the page cache, and maybe not on the media.
    Renamed,
    /// `inc`'s flushed write returned; `inc-lease` is not yet written.
    Flushed,
}

/// TEST-ONLY: what [`StateDir::take_incarnation`] is told instead of asking, where it
/// is killed, and which defect it carries — the incarnation lease's conformance walk
/// (`the_incarnation_lease_refines_its_model`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IncSeam {
    /// This boot, as the walk stands it in: `Some(None)` is a boot the kernel cannot
    /// name; `None` asks the kernel.
    pub boot: Option<Option<[u8; 16]>>,
    /// The dir stands OFF THE BOOT VOLUME: the kernel names `boot`, but the page
    /// cache cannot vouch for the dir ([`StateDir::trusted_boot`] is `None`).
    pub off_volume: bool,
    /// The step the next take dies at.
    pub kill_at: Option<IncStep>,
    /// One defect, the walk's negative controls (the model's `Buggy`): 0 none; 1 a
    /// lease trusted across boots; 2 a lease trusted whose top is not `inc`'s; 3 the
    /// lease written before its top is flushed (naming the `inc` file it found, the
    /// one it can name before the rename).
    pub mutant: u8,
    /// The lease a flush covers, short enough for the walk to run one out (the
    /// model's `Span`); `None` is [`INC_LEASE`].
    pub span: Option<u64>,
}

/// The bridge's state directory.
pub struct StateDir {
    root: PathBuf,
    /// THE DOOR every write of this dir goes through ([`crate::fx::Disk`]): the
    /// filesystem as it always was, or a simulated device that can lose power.
    disk: crate::fx::Disk,
    /// The files of a [`TornKind`] this process wrote without a flush — and,
    /// when the last bridge of this boot did not stop cleanly, every one there
    /// ([`StateDir::begin_run`]) — by path inside the dir: what a clean stop
    /// flushes before it says it stopped cleanly ([`StateDir::end_run_clean`]).
    unflushed: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// Retain the current own-written pin once this open established its
    /// v=2 fence durably, preventing removal/recreation from reusing its
    /// identity. Atomic successors preserve that durable-version baseline;
    /// a new open, error or replacement does not.
    logpin_fence: std::sync::Mutex<Option<std::fs::File>>,
    /// TEST-ONLY: what this dir's writers did to the filesystem ([`FileOps`]).
    #[cfg(test)]
    ops: std::sync::Mutex<FileOps>,
    /// TEST-ONLY: the incarnation take's boot, kill point and defect ([`IncSeam`]).
    #[cfg(test)]
    inc_seam: std::sync::Mutex<IncSeam>,
    /// TEST-ONLY: the one file whose replaces fail as a full disk fails them
    /// ([`StateDir::refuse_writes_of`]).
    #[cfg(test)]
    refused: std::sync::Mutex<Option<String>>,
}

/// The filesystem operations one [`StateDir`] made, by kind (counted only in a
/// test build, [`StateDir::ops`]), so a test states what a post COSTS as a count
/// — deterministic for a given build — rather than as a time this shared machine
/// would make into noise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileOps {
    /// Reservation and journal reads ([`StateDir::read_reservation`]), hits and
    /// misses alike.
    pub reads: u64,
    /// Atomic replaces: a fresh temp written, then renamed over the target.
    pub replaces: u64,
    /// Renames, the replaces' own included.
    pub renames: u64,
    /// `fsync`s (`F_FULLFSYNC` on macOS): a durable replace flushes its file
    /// and its directory.
    pub fsyncs: u64,
    /// Barriers (`F_BARRIERFSYNC` on macOS, `aterm_durable::ordered`): a
    /// file flushed where it is, an ordered replace's temp, and an ordered
    /// flush of a directory (a clean stop's).
    pub barriers: u64,
    /// Unlinks of a journal entry or a reservation.
    pub unlinks: u64,
    /// Appends to an append-only file.
    pub appends: u64,
}

#[cfg(test)]
impl std::ops::Sub for FileOps {
    type Output = Self;

    fn sub(self, before: Self) -> Self {
        Self {
            reads: self.reads - before.reads,
            replaces: self.replaces - before.replaces,
            renames: self.renames - before.renames,
            fsyncs: self.fsyncs - before.fsyncs,
            barriers: self.barriers - before.barriers,
            unlinks: self.unlinks - before.unlinks,
            appends: self.appends - before.appends,
        }
    }
}

/// The original publication's face and semantic digest, persisted before send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishIdentity {
    pub subject: String,
    pub digest: String,
}

/// A sequence reservation. A file without an identity needs independent evidence
/// before a deduped offset is accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishPin {
    pub seq: u64,
    pub via_key: bool,
    pub identity: Option<PublishIdentity>,
    /// A key's previous winner; the attempt itself may carry different bytes.
    pub original: Option<PublishIdentity>,
    /// The key winner's landed offset on the witnessed log, distinct from a
    /// later attempt's offset. Forgotten when that log is replaced.
    pub original_off: Option<u64>,
    /// The original post within this session's key file, before any publish.
    pub owner: Option<u64>,
}

impl PublishPin {
    pub(crate) fn parse(text: &str) -> io::Result<Self> {
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid publication reservation",
            )
        };
        let mut words = text.split_whitespace();
        let seq = words.next().and_then(|s| s.parse().ok()).ok_or_else(bad)?;
        let mut tail: Vec<&str> = words.collect();
        let via_key = tail.first() == Some(&"key");
        if via_key {
            tail.remove(0);
        }
        let (mut identity, mut original, mut original_off, mut owner) = (None, None, None, None);
        let mut parts = tail.as_slice();
        while !parts.is_empty() {
            if parts[0] == "owner" && owner.is_none() {
                owner = Some(
                    parts
                        .get(1)
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|id| *id > 0)
                        .ok_or_else(bad)?,
                );
                parts = &parts[2..];
                continue;
            }
            if parts[0] == "original-off" && original_off.is_none() {
                original_off = Some(parts.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?);
                parts = &parts[2..];
                continue;
            }
            if parts.len() < 3 {
                return Err(bad());
            }
            let (label, subject, digest) = (parts[0], parts[1], parts[2]);
            if !is_face(subject) || !aterm_codec::hex::is_lower(digest, 64) {
                return Err(bad());
            }
            let slot = match label {
                "identity" if identity.is_none() => &mut identity,
                "original" if original.is_none() && via_key => &mut original,
                _ => return Err(bad()),
            };
            *slot = Some(PublishIdentity {
                subject: subject.to_string(),
                digest: digest.to_string(),
            });
            parts = &parts[3..];
        }
        if (original.is_some() || original_off.is_some()) && identity.is_none() {
            return Err(bad());
        }
        Ok(Self {
            seq,
            via_key,
            identity,
            original,
            original_off,
            owner,
        })
    }

    fn render(&self) -> String {
        let mut line = self.seq.to_string();
        if self.via_key {
            line.push_str(" key");
        }
        if let Some(identity) = &self.identity {
            line.push_str(&format!(
                " identity {} {}",
                identity.subject, identity.digest
            ));
        }
        if let Some(original) = &self.original {
            line.push_str(&format!(
                " original {} {}",
                original.subject, original.digest
            ));
        }
        if let Some(off) = self.original_off {
            line.push_str(&format!(" original-off {off}"));
        }
        if let Some(owner) = self.owner {
            line.push_str(&format!(" owner {owner}"));
        }
        line
    }
}

/// One non-blank line of a `keys/<sid>` file: the key, then its reservation.
fn key_line(line: &str) -> io::Result<(&str, PublishPin)> {
    let (key, reservation) = line
        .split_once(char::is_whitespace)
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid publication key"))?;
    Ok((key, PublishPin::parse(reservation)?))
}

/// One session's refill floors file ([`StateDir::topic_floors`]) read back from
/// `body`, the file at `path`: empty, or any line that is not one unique topic and
/// offset, is damage.
fn parse_topic_floors(path: &Path, body: &str) -> io::Result<BTreeMap<String, u64>> {
    if body.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty broadcast refill floor file",
        ));
    }
    let mut out = BTreeMap::new();
    for line in body.lines() {
        let mut words = line.split_whitespace();
        let topic = words.next().filter(|t| crate::subject::is_topic(t));
        let off = words
            .next()
            .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|w| w.parse::<u64>().ok());
        match (topic, off, words.next()) {
            (Some(topic), Some(off), None) => {
                if out.insert(topic.to_string(), off).is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "duplicate broadcast refill topic",
                    ));
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: `{}` is not a broadcast refill floor",
                        path.display(),
                        safe_line(line)
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// A face a reservation may publish on: absolute, printable, no wildcard.
fn is_face(subject: &str) -> bool {
    subject.starts_with('/')
        && subject
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'*' | b'>'))
}

/// One record in flight onto this node's own lanes ([`StateDir::in_flight`]): its
/// reservation, face, exact bytes and the log it was decided on. A relaunched
/// bridge publishes it again at the same sequence, learning its offset from the
/// broker's dedup before the self-lane check judges it; the log says when it must
/// not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InFlight {
    pub pin: PublishPin,
    pub subject: String,
    pub body: Vec<u8>,
    /// The log this body was prepared on, the only one where a `re=` in it points;
    /// `None` for an entry written without a witness.
    pub log: Option<astream_broker::LogIdentity>,
}

impl InFlight {
    /// The pin, face, body in lower hex, then an optional `log=<identity>`.
    fn render(&self) -> String {
        let mut text = format!(
            "{}\n{}\n{}",
            self.pin.render(),
            self.subject,
            aterm_codec::hex::encode_lower(&self.body)
        );
        if let Some(log) = self.log {
            text.push_str(&format!("\nlog={log}"));
        }
        text
    }

    /// Parse the three-line form, or the four with `log=`; a `log=` line that does
    /// not parse is damage, never a missing witness.
    pub(crate) fn parse(seq: u64, text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let pin = PublishPin::parse(lines.next()?).ok()?;
        let subject = lines.next()?;
        let body = aterm_codec::hex::decode_vec(lines.next()?.as_bytes())?;
        let log = match lines.next() {
            Some(line) => Some(astream_broker::LogIdentity::parse(
                line.strip_prefix("log=")?,
            )?),
            None => None,
        };
        (lines.next().is_none() && pin.seq == seq && is_face(subject)).then(|| Self {
            pin,
            subject: subject.to_string(),
            body,
            log,
        })
    }
}

/// The acked self-lane log as read back ([`StateDir::self_acked_log`]).
///
/// AN APPEND LOG, NOT A SET REWRITTEN PER PUBLISH. The file used to be the
/// whole set, replaced for every own-lane publish: at the bridge's steady
/// state that is thousands of offsets, tens of KiB written and renamed for
/// one record, between its durable ack and its delivery. Now each offset ends
/// at its comma, the newest appended last in a `write` of its own, and the
/// bridge compacts the log to the newest set when it reaches twice that size
/// ([`StateDir::set_self_acked`]). Repeats are harmless: it is read as a set.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AckedLog {
    /// Every offset the file holds whole.
    pub offsets: std::collections::BTreeSet<u64>,
    /// How many entries the file holds, repeats included: the size an append
    /// grows and a compaction resets.
    pub entries: usize,
    /// Whether the file ends at a terminator (or is absent), so an append may
    /// go on after its last byte. `false` for a torn tail and for the
    /// newline-ended line an older bridge wrote: the next write replaces the
    /// file whole.
    pub appendable: bool,
}

impl AckedLog {
    /// Read the log. Only a number ended by its comma is an offset — and,
    /// for a file an older bridge wrote, the last number of its one line,
    /// ended by the newline. Anything after the last terminator is a torn
    /// append and is NOT an offset of ours: a prefix of a number names some
    /// other record, and vouching for it would let a forgery through the
    /// self-lane check.
    fn parse(text: &str) -> Self {
        let mut log = Self {
            appendable: text.is_empty() || text.ends_with(','),
            ..Self::default()
        };
        let mut pieces: Vec<&str> = text.split(',').collect();
        let tail = pieces.pop().unwrap_or_default();
        for piece in pieces {
            log.entries += 1;
            if let Ok(off) = piece.trim().parse() {
                log.offsets.insert(off);
            }
        }
        if tail.ends_with('\n') {
            if let Ok(off) = tail.trim().parse() {
                log.entries += 1;
                log.offsets.insert(off);
            }
        }
        log
    }

    /// `offsets`, each ended by its comma: what an append writes, and what a
    /// compaction writes whole.
    fn render(offsets: impl Iterator<Item = u64>) -> String {
        let mut text = String::new();
        for off in offsets {
            text.push_str(&off.to_string());
            text.push(',');
        }
        text
    }
}

/// A KIND OF FILE A POWER CUT MAY TEAR AND THE NEXT BOOT MAY SET ASIDE
/// ([`StateDir::set_aside_torn`]): written atomically and never flushed (but
/// at a clean stop, [`StateDir::end_run_clean`]), read back by a reader that
/// REFUSES damage rather than read it as nothing, and serving only sessions,
/// which a reboot ends: every sid is minted afresh at aterm's launch, and the
/// outbox, the owed receipts and the rings live in aterm's memory. The module
/// doc's table says what losing each costs; after a reboot, nothing a later
/// boot reads. (`logpin` is not one: what it holds outlives a reboot, so it is
/// set aside only once its durable copies are on the media,
/// [`StateDir::reconcile_logpin`]; a clean stop flushes it as it flushes these.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TornKind {
    /// `inflight/<seq>` ([`StateDir::in_flight`]).
    InFlight,
    /// `sent/<sid>.<id>` ([`StateDir::post_pin`]).
    Sent,
    /// `acks/<sid>.<rid>` ([`StateDir::receipt_pin`]).
    Acks,
    /// `keys/<sid>` ([`StateDir::key_pin`]).
    Keys,
    /// `unhandled/<sid>` ([`StateDir::topic_floors`]).
    Unhandled,
}

impl TornKind {
    /// Every kind, in the order a sweep visits them.
    pub(crate) const ALL: [Self; 5] = [
        Self::InFlight,
        Self::Sent,
        Self::Acks,
        Self::Keys,
        Self::Unhandled,
    ];

    /// The directory its files are entries of.
    const fn place(self) -> &'static str {
        match self {
            Self::InFlight => "inflight",
            Self::Sent => "sent",
            Self::Acks => "acks",
            Self::Keys => "keys",
            Self::Unhandled => "unhandled",
        }
    }

    /// The kind whose file `rel` (a path inside the dir) is, if it is one.
    fn of(rel: &str) -> Option<Self> {
        let (place, name) = rel.split_once('/')?;
        Self::ALL
            .into_iter()
            .find(|kind| kind.place() == place && kind.names(name))
    }

    /// Whether `text`, the file `rel` of this kind, is PROVABLY WHOLE: it ends
    /// at the newline every writer of the kind ends it with
    /// ([`StateDir::replace`]), its own reader takes it — the reader's parse,
    /// so nothing it would refuse is ever whole — and, in flight, it names the
    /// log it was decided on. A cut anywhere short of the last byte fails one
    /// of the three: the newline is the last byte, a single-line file cut
    /// short has none, and the cuts of an in-flight entry that end at a
    /// newline (just before its `log=` line, or after its face) or that still
    /// decode (inside the hex body at an even length) leave no witness. A
    /// whole entry with no witness is written only under a broker that names
    /// no log, and serves nothing after a reboot either. A multi-line
    /// `keys/` or `unhandled/` file cut at a line's end reads whole, with
    /// fewer lines: what a reboot ended anyway.
    fn whole(self, rel: &Path, name: &str, text: &str) -> bool {
        text.ends_with('\n')
            && match self {
                Self::InFlight => Self::entry(name, text).is_some_and(|entry| entry.log.is_some()),
                _ => self.reads(rel, name, text),
            }
    }

    /// Whether its own reader takes `text`, the file `rel` of this kind: the
    /// reader's parse and nothing more — what a sweep that trusted the reader
    /// alone called whole, a cut that still reads included (the simulation's
    /// negative control, `Mutant::SweepByTheReaderAlone`).
    fn reads(self, rel: &Path, name: &str, text: &str) -> bool {
        match self {
            Self::InFlight => Self::entry(name, text).is_some(),
            Self::Sent | Self::Acks => PublishPin::parse(text).is_ok(),
            Self::Keys => text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .all(|line| key_line(line).is_ok()),
            Self::Unhandled => parse_topic_floors(rel, text).is_ok(),
        }
    }

    /// The in-flight entry `name` holds, read as [`StateDir::in_flight`] reads it.
    fn entry(name: &str, text: &str) -> Option<InFlight> {
        name.parse().ok().and_then(|seq| InFlight::parse(seq, text))
    }

    /// Whether `name`, an entry of this kind's directory, is one of its files: a
    /// writer's hidden temp (`.<name>.tmp.…`) never is, and an `inflight/` entry
    /// is a sequence number ([`StateDir::in_flight`]'s own rule).
    fn names(self, name: &str) -> bool {
        !name.is_empty()
            && !name.contains('/')
            && !name.starts_with('.')
            && (self != Self::InFlight || name.bytes().all(|b| b.is_ascii_digit()))
    }
}

/// WHAT A RUN RECORD'S WRITE FLUSHED ([`StateDir::begin_run`],
/// [`StateDir::end_run_clean`]): the files flushed, the directories flushed,
/// and the flushes asked for of each of `aterm_durable`'s two — the barriers
/// (`F_BARRIERFSYNC` on macOS) and the full flushes (`F_FULLFSYNC`), the
/// record's own included — what the bridge says a stop or a start cost
/// (`Bridge::end_run`, `Bridge::begin_run`). Counted as asked, in every
/// build: this crate's own tests skip the flush itself ([`flushes_media`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flushed {
    /// Files of a [`TornKind`], and the mirror's pin, flushed where they are.
    pub files: u64,
    /// Directories whose entries moved, flushed before the record (the
    /// root's full flush for the record not counted).
    pub dirs: u64,
    /// Barriers: a file's, a directory's, the record's bytes'.
    pub ordered: u64,
    /// Full flushes: a file's or a directory's on another device than the
    /// root, the record's (two at a run's start, temp and directory).
    pub full: u64,
}

impl Flushed {
    /// One write finished by `flush`: a barrier, or `full` full flushes.
    fn note(&mut self, flush: Option<aterm_durable::Flush>, full: u64) {
        match flush {
            Some(aterm_durable::Flush::Ordered) => self.ordered += 1,
            Some(aterm_durable::Flush::Durable) => self.full += full,
            None => {}
        }
    }
}

/// The rules [`StateDir::end_run_clean`] keeps; the clean-stop law's negative
/// controls switch one off ([`StateDir::end_run_clean_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StopRules {
    /// Each file the run left unflushed is flushed where it is.
    pub flush_files: bool,
    /// Every directory whose entries moved is flushed before the record.
    pub flush_dirs: bool,
    /// The record is written after what it vouches for.
    pub record_last: bool,
}

impl StopRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        flush_files: true,
        flush_dirs: true,
        record_last: true,
    };
}

/// HOW THE LAST BRIDGE OF THIS DIR STOPPED, AND IN WHICH BOOT (`run`, durable):
/// what tells the first bridge of a boot whether a power cut can have torn a
/// file of a [`TornKind`] ([`StateDir::torn_files_possible`]) — astream's own
/// unclean-stop rule for its weak tiers (`astream_broker::Durability::Relaxed`:
/// a run that did not close cleanly, judged in another boot).
///
/// `open boot=<id>` from the first start in a boot (written after the sweep,
/// before any file of those kinds), `clean boot=<id>` once a bridge has
/// flushed every such file it or a predecessor of this boot wrote and stops
/// ([`StateDir::end_run_clean`]); `-` for a boot the kernel does not name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastRun {
    /// The boot it ran in, as the kernel names it.
    pub boot: Option<[u8; 16]>,
    /// Whether it stopped cleanly, every file of a [`TornKind`] on the media.
    pub clean: bool,
}

impl LastRun {
    /// The one line it is stored as.
    fn render(&self) -> String {
        let boot = self
            .boot
            .map_or_else(|| "-".to_string(), |b| aterm_codec::hex::encode_lower(&b));
        let state = if self.clean { "clean" } else { "open" };
        format!("{state} boot={boot}")
    }

    /// Parse one back: `open` or `clean`, then `boot=` and a boot id or `-`.
    /// Anything else — a hand-edited file — is `None`; the reader refuses
    /// such a record rather than treating it as an older build's missing one.
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let clean = match words.next()? {
            "open" => false,
            "clean" => true,
            _ => return None,
        };
        let boot = match words.next()?.strip_prefix("boot=")? {
            "-" => None,
            id => Some(astream_sys::parse_boot_id(id.as_bytes())?),
        };
        words.next().is_none().then_some(Self { boot, clean })
    }
}

/// One damaged file [`StateDir::set_aside_torn`] moved aside: where it was and
/// where its bytes are now, both relative to the state dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetAside {
    pub from: String,
    pub to: String,
}

/// Where [`StateDir::set_aside_torn`] keeps what it moves aside, inside the dir.
pub const QUARANTINE: &str = "quarantine";

/// The bus mirror's pin ([`StateDir::logpin`]).
pub(crate) const LOGPIN: &str = "logpin";
/// The bus mirror's alias key, its durable copy ([`StateDir::logkey`]).
pub(crate) const LOGKEY: &str = "logkey";
/// The floor of the bus mirror's lifetime total ([`StateDir::logfloor`]).
pub(crate) const LOGFLOOR: &str = "logfloor";

/// What [`StateDir::reconcile_logpin`] did with the mirror's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reconciled {
    /// Nothing: no unclean stop in an earlier boot, no floor, a whole pin at
    /// or above the floor, or a torn one the copies cannot be shown to cover.
    Kept,
    /// A whole pin's total, below the floor, rewritten at it.
    Raised { from: u64, to: u64 },
    /// A torn pin moved into the quarantine, and a whole one written from
    /// the copies in its place.
    SetAside(SetAside),
    /// No pin: a whole one written from the copies.
    Restored,
}

/// The rules [`StateDir::reconcile_logpin`] applies; the simulation's negative
/// controls switch one off ([`StateDir::reconcile_logpin_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PinRules {
    /// A torn pin is moved aside.
    pub set_aside_torn: bool,
    /// Where the pin cannot say, the total is the floor's: a whole pin's
    /// below it raised to it, a pin written from the copies at it.
    pub total_from_floor: bool,
    /// A torn pin is moved aside only where `logkey` holds a key.
    pub need_key: bool,
}

impl PinRules {
    /// Every rule on: what ships.
    pub(crate) const ALL: Self = Self {
        set_aside_torn: true,
        total_from_floor: true,
        need_key: true,
    };
}

/// What a whole in-flight entry retired after an unclean stop in an earlier
/// boot ([`StateDir::retire_in_flight_of_an_earlier_boot`], finding F2) is
/// named with in [`QUARANTINE`], before its path: `retired.inflight.<seq>`.
/// A torn file's name there starts with its kind's directory, never this.
pub const RETIRED_PREFIX: &str = "retired.";

/// [`StateDir::lock_bridge`]'s hold, released by `LOCK_UN` before the close
/// (`clippy.toml`'s flock release rule): a child forked while it is held keeps the
/// descriptor until it execs, so a release by the close alone would leave the lock
/// taken on that copy.
pub struct BridgeLock(std::fs::File);

impl Drop for BridgeLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl StateDir {
    /// Open (creating) a state directory.
    ///
    /// # Errors
    ///
    /// If the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        Self::open_on(root, crate::fx::Disk::Os)
    }

    /// [`StateDir::open`], writing through `disk` (the bridge's door,
    /// [`crate::fx::Fx::disk`]).
    ///
    /// # Errors
    ///
    /// If the directory cannot be created.
    pub(crate) fn open_on(root: impl Into<PathBuf>, disk: crate::fx::Disk) -> io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(root.join("seen"))?;
        std::fs::create_dir_all(root.join("sent"))?;
        Ok(Self::at_on(root, disk))
    }

    fn at(root: PathBuf) -> Self {
        Self::at_on(root, crate::fx::Disk::Os)
    }

    fn at_on(root: PathBuf, disk: crate::fx::Disk) -> Self {
        Self {
            root,
            disk,
            unflushed: std::sync::Mutex::default(),
            logpin_fence: std::sync::Mutex::default(),
            #[cfg(test)]
            ops: std::sync::Mutex::default(),
            #[cfg(test)]
            inc_seam: std::sync::Mutex::default(),
            #[cfg(test)]
            refused: std::sync::Mutex::default(),
        }
    }

    /// TEST-ONLY: every filesystem operation this dir has counted so far.
    #[cfg(test)]
    pub(crate) fn ops(&self) -> FileOps {
        *self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// TEST-ONLY: stand in the incarnation take's boot, kill point and defect.
    #[cfg(test)]
    pub(crate) fn set_inc_seam(&self, seam: IncSeam) {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner()) = seam;
    }

    /// TEST-ONLY: every replace of `name` (a path inside the dir) fails from
    /// now on as a full disk fails it, before anything is written; `None`
    /// lifts it. The simulated device refuses only a dead process's writes
    /// ([`crate::fx::Disk::before`]); this is one the disk itself refuses.
    #[cfg(test)]
    pub(crate) fn refuse_writes_of(&self, name: Option<&str>) {
        *self.refused.lock().unwrap_or_else(|e| e.into_inner()) = name.map(str::to_string);
    }

    /// TEST-ONLY: the full disk's error for a replace of `name` a test
    /// refuses ([`StateDir::refuse_writes_of`]); `Ok` for every other.
    #[cfg(test)]
    fn refused_by_a_test(&self, name: &str) -> io::Result<()> {
        let refused = self.refused.lock().unwrap_or_else(|e| e.into_inner());
        if refused.as_deref() == Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!("{name}: no space left on device (a test's refusal)"),
            ));
        }
        Ok(())
    }

    /// TEST-ONLY: the incarnation take's stand-ins as they are now.
    #[cfg(test)]
    fn inc_seam(&self) -> IncSeam {
        *self.inc_seam.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Die here if a test armed this step ([`IncSeam::kill_at`]): the take stops with
    /// every file as the kernel has it, which is what a `kill -9` leaves. Nothing at
    /// all outside a test build.
    fn kill_point(&self, _step: IncStep) -> io::Result<()> {
        #[cfg(test)]
        if self.inc_seam().kill_at == Some(_step) {
            return Err(io::Error::other(format!("killed at {_step:?}")));
        }
        Ok(())
    }

    /// Count one operation ([`FileOps`]); nothing at all outside a test build.
    fn count(&self, _note: impl FnOnce(&mut FileOps)) {
        #[cfg(test)]
        _note(&mut self.ops.lock().unwrap_or_else(|e| e.into_inner()));
    }

    /// Unlink `name`, counted ([`FileOps::unlinks`]).
    fn unlink(&self, name: &str) -> io::Result<()> {
        self.count(|ops| ops.unlinks += 1);
        self.remove_path(&self.path(name))
    }

    /// THE ONE UNLINK every file of this dir is removed by, through the dir's
    /// door ([`crate::fx::Disk`]): `std::fs::remove_file`, its error as it is.
    fn remove_path(&self, path: &Path) -> io::Result<()> {
        let w = crate::fx::Write::Remove { path };
        self.disk.before(w)?;
        std::fs::remove_file(path)?;
        self.disk.after(w);
        Ok(())
    }

    /// A state directory to read, creating nothing: `aterm fabric`'s report must not
    /// make a dir that a command names and that does not exist. A missing file reads
    /// as "nothing known".
    #[must_use]
    pub fn peek(root: impl Into<PathBuf>) -> Self {
        Self::at(root.into())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Read one file's trimmed contents.
    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn read_reservation(&self, name: &str) -> io::Result<Option<String>> {
        self.count(|ops| ops.reads += 1);
        match std::fs::read_to_string(self.path(name)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(self.path(name)) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// THE BUS MIRROR'S PIN (`logpin`, session log phase 4,
    /// [`crate::mirror::Pin`]): the one batch in flight and the node's total
    /// of mirrored bytes. Absent on a node that never mirrored; a bridge with
    /// `[fabric] log = "off"` never reads or writes it (save the first bridge
    /// after an unclean stop in an earlier boot, [`StateDir::reconcile_logpin`]).
    /// Read with the missing-versus-damaged rule every reservation is.
    ///
    /// # Errors
    ///
    /// A pin that exists and cannot be read.
    pub(crate) fn logpin(&self) -> io::Result<Option<String>> {
        self.read_reservation(LOGPIN)
    }

    /// Replace `logpin` whole, atomically, 0600 (it holds record bytes, as
    /// `inflight/` does). Not flushed, but at a clean stop
    /// ([`StateDir::end_run_clean`]): a power loss ends what its batch pairs
    /// with (the endpoint's rings), and losing it costs at most one batch taken
    /// again at fresh sequences — a second copy each reader dedupes. What it
    /// holds that no reboot ends has its durable copies, `logkey` and
    /// `logfloor`.
    ///
    /// SAVE THE FIRST OF THIS OPEN: a pin this open has not established as
    /// this build's (`v=2`, [`crate::mirror::Pin::by_this_build`]) durably —
    /// or one removed, replaced or left uncertain by an error — is written
    /// DURABLY, so no power cut can bring back a pin an older build reads (it refuses a `v=2`
    /// one): an older build that ran next would mirror under its key past a
    /// floor it never raises, or mint a second key where it found none. Once
    /// per open and after uncertainty; later own replacements stay atomic.
    ///
    /// # Errors
    ///
    /// The write's.
    pub(crate) fn set_logpin(&self, text: &str) -> io::Result<()> {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        let durable = !self.logpin_fence_known(&mut known);
        self.replace_logpin(text, &mut known, durable)
    }

    /// Establish the current pin's v=2 fence before a copy, take or send.
    /// A visible v=2, even across a reopen, may be a failed post-rename
    /// flush. Render/write only when this open lacks successful admission.
    pub(crate) fn ensure_logpin_durable(&self, text: impl FnOnce() -> String) -> io::Result<()> {
        let mut known = self.logpin_fence.lock().unwrap_or_else(|e| e.into_inner());
        if self.logpin_fence_known(&mut known) {
            return Ok(());
        }
        self.replace_logpin(&text(), &mut known, true)
    }

    fn logpin_fence_known(&self, known: &mut Option<std::fs::File>) -> bool {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};
        let matches = known.as_ref().is_some_and(|held| {
            let Ok(meta) = held.metadata() else {
                return false;
            };
            let mut head = [0u8; crate::mirror::PIN_HEADER.len() + 1];
            meta.is_file()
                && Some((meta.dev(), meta.ino())) == self.logpin_identity()
                && held
                    .read_at(&mut head, 0)
                    .ok()
                    .is_some_and(|n| crate::mirror::Pin::by_this_build(&head[..n]))
        });
        if matches {
            return true;
        }
        *known = None;
        false
    }

    fn logpin_identity(&self) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(self.path(LOGPIN)).ok()?;
        meta.is_file().then(|| (meta.dev(), meta.ino()))
    }

    fn retained_logpin(&self) -> Option<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.path(LOGPIN))
            .ok()?;
        file.metadata().ok()?.is_file().then_some(file)
    }

    fn replace_logpin(
        &self,
        text: &str,
        known: &mut Option<std::fs::File>,
        durable: bool,
    ) -> io::Result<()> {
        // Keep the predecessor's inode alive through the whole replace and
        // successor admission. Every error still leaves the cache empty.
        let previous = known.take();
        self.replace_bytes(LOGPIN, text.as_bytes(), durable, 0o600)?;
        if crate::mirror::Pin::by_this_build(text.as_bytes()) {
            *known = self.retained_logpin();
        }
        drop(previous);
        Ok(())
    }

    /// Whether `logpin`, whole or torn, begins as an OLDER BUILD'S pin
    /// ([`crate::mirror::Pin::by_an_older_build`]), which the durable copies
    /// need not cover; a file that is not there or does not read is not one.
    pub(crate) fn logpin_by_an_older_build(&self) -> bool {
        self.logpin_head()
            .is_some_and(|head| crate::mirror::Pin::by_an_older_build(&head))
    }

    /// The first bytes of `logpin`, enough to name its version; `None` where
    /// it is not a regular file or does not read.
    fn logpin_head(&self) -> Option<Vec<u8>> {
        use std::io::Read as _;
        let path = self.path(LOGPIN);
        if !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            return None;
        }
        let file = std::fs::File::open(&path).ok()?;
        let mut head = Vec::new();
        file.take(crate::mirror::PIN_HEADER.len() as u64 + 1)
            .read_to_end(&mut head)
            .ok()?;
        Some(head)
    }

    /// THE BUS MIRROR'S ALIAS KEY, its durable copy (`logkey`, ruling Q6):
    /// 32 hex digits and a newline, written once ([`StateDir::set_logkey`])
    /// before the first take that names the key, so no alias derived from it
    /// is on the bus before the key is on the media. `None` where no bridge
    /// of this build has named one yet (a pin an older build wrote may hold
    /// it, `key=`).
    ///
    /// # Errors
    ///
    /// One that exists and cannot be read, or does not read as one key: it is
    /// written durably, so no power cut tears it, and damage is a bug's or a
    /// hand's — the mirror stops on it rather than mint a second key.
    pub(crate) fn logkey(&self) -> io::Result<Option<[u8; 16]>> {
        let Some(text) = self.read_reservation(LOGKEY)? else {
            return Ok(None);
        };
        let mut key = [0u8; 16];
        match text.strip_suffix('\n') {
            Some(hex)
                if hex.len() == 32 && aterm_codec::hex::decode_into(hex.as_bytes(), &mut key) =>
            {
                Ok(Some(key))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} does not read as one alias key",
                    self.path(LOGKEY).display()
                ),
            )),
        }
    }

    /// Write `logkey` DURABLY, 0600 (it is the secret the edges' aliases are
    /// keyed by): the temp flushed, then the directory. Once per node, before
    /// the first take that names the key ([`StateDir::logkey`]).
    ///
    /// # Errors
    ///
    /// The write's or a flush's.
    pub(crate) fn set_logkey(&self, key: &[u8; 16]) -> io::Result<()> {
        self.replace(LOGKEY, &aterm_codec::hex::encode_lower(key), true, 0o600)
    }

    /// THE FLOOR OF THE MIRROR'S LIFETIME TOTAL (`logfloor`, ruling Q13): at
    /// or above every body byte this node's mirror ever sent, because it is
    /// raised past a batch's bytes before any byte of the batch is sent
    /// ([`StateDir::raise_logfloor`]). The total a bridge resumes at when the
    /// pin cannot say (it is gone, or a power cut may have rolled it back):
    /// never below what was counted. `None` where no bridge of this build
    /// has sent a batch.
    ///
    /// # Errors
    ///
    /// One that exists and cannot be read, or does not read as one number:
    /// durable, so damage is a bug's or a hand's.
    pub(crate) fn logfloor(&self) -> io::Result<Option<u64>> {
        let Some(text) = self.read_reservation(LOGFLOOR)? else {
            return Ok(None);
        };
        text.strip_suffix('\n')
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse().ok())
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} does not read as one floor",
                        self.path(LOGFLOOR).display()
                    ),
                )
            })
    }

    /// RAISE `logfloor` to `top`, DURABLY — never lower it: a floor at or
    /// above `top` already is kept as it is, and nothing is written. Answers
    /// the floor the media now holds.
    ///
    /// # Errors
    ///
    /// The floor's read ([`StateDir::logfloor`]: one that does not read is
    /// never written over), the write's or a flush's.
    pub(crate) fn raise_logfloor(&self, top: u64) -> io::Result<u64> {
        match self.logfloor()? {
            Some(floor) if floor >= top => Ok(floor),
            _ => {
                self.replace(LOGFLOOR, &top.to_string(), true, 0o666)?;
                Ok(top)
            }
        }
    }

    /// RULING Q6 AND Q13 ACROSS A POWER CUT: AFTER AN UNCLEAN STOP IN AN
    /// EARLIER BOOT ([`StateDir::torn_files_possible`]), THE MIRROR'S PIN IS
    /// RECONCILED WITH ITS DURABLE COPIES — its floor on the media
    /// (`logfloor`) and its key (`logkey`), which a bridge of this build
    /// writes before any take names the key and before any byte of a batch is
    /// sent.
    ///
    /// The pin is atomic only, so the cut may have left it ROLLED BACK (an
    /// older whole version, whose total is below what was counted since) or
    /// TORN (not provably whole: [`crate::mirror::Pin::parse`] refuses
    /// exactly that). So, where the dir holds a floor:
    ///
    /// - a whole pin whose total is below the floor is rewritten at the floor,
    ///   its batch and key kept (the batch is republished at its pinned
    ///   sequences, as after a kill). Nothing tells the newest pin from an
    ///   older one, so this is every such reboot's, and the floor's lease
    ///   ([`crate::mirror::FLOOR_LEASE_BYTES`]) is counted as mirrored;
    /// - a torn one, where `logkey` holds the key, is MOVED ASIDE, never
    ///   deleted — its exact bytes into [`QUARANTINE`] as `quarantine/logpin`,
    ///   durably, before its name is unlinked
    ///   ([`StateDir::move_into_quarantine`]) — and a WHOLE PIN IS WRITTEN FROM
    ///   THE COPIES IN ITS PLACE (no batch, `logkey`'s key, the floor as its
    ///   total), durably ([`StateDir::set_logpin`]): the mirror resumes from it,
    ///   its lost batch taken again where its sessions still are, a copy each
    ///   reader dedupes (what an older whole version always cost), and an
    ///   older build that runs here before this one pins again refuses it
    ///   rather than find no pin and mint a second key;
    /// - an absent one has that whole pin written in its place too (a bridge
    ///   killed between the move and the write, or a pin whose first write
    ///   the cut took).
    ///
    /// NOTHING is moved where the copies cannot be shown to cover the pin: no
    /// floor (no bridge of this build sent a batch here, so the pin an older
    /// build wrote holds the node's only key and total); a torn pin with no
    /// key in `logkey` (it may hold the only copy of one); and a torn pin that
    /// begins as an OLDER BUILD'S ([`crate::mirror::Pin::by_an_older_build`]:
    /// it wrote past a floor it does not raise). There the torn pin stays
    /// where the mirror refuses it and stops, naming what a person does.
    /// Within one boot, after a clean stop (which flushed the pin,
    /// [`StateDir::end_run_clean`]) and where no boot can be told from
    /// another, nothing is changed either: damage there is a bug's, and the
    /// mirror stops on it.
    ///
    /// WHATEVER THE `[fabric] log` MODE: a pin and a floor exist only where a
    /// mirror ran, and the next bridge of this boot to turn it on must find
    /// the pin reconciled, because no later start of the boot can tell the
    /// cut. Run by the first bridge of the boot, after
    /// [`StateDir::set_aside_torn`] and before [`StateDir::begin_run`] ends
    /// the verdict, so a bridge killed part-way leaves the next in the same
    /// one (idempotent: a raised pin is at the floor, a moved one is gone and
    /// its place holds the whole pin, or none, which the next writes).
    ///
    /// # Errors
    ///
    /// The verdict's, the floor's or the key's read, the pin's read, its
    /// rewrite, its move or its replacement's write: what was not done is
    /// the mirror's to refuse, as before.
    pub(crate) fn reconcile_logpin(&self) -> io::Result<Reconciled> {
        self.reconcile_logpin_by(PinRules::ALL)
    }

    /// TEST-ONLY: [`StateDir::reconcile_logpin`] with one of its rules off —
    /// the simulation's negative controls (`Mutant::KeepTornPin`,
    /// `Mutant::KeyInThePinAlone`, `Mutant::TotalInThePinAlone`).
    #[cfg(test)]
    pub(crate) fn reconcile_logpin_with(&self, rules: PinRules) -> io::Result<Reconciled> {
        self.reconcile_logpin_by(rules)
    }

    fn reconcile_logpin_by(&self, rules: PinRules) -> io::Result<Reconciled> {
        if !self.torn_files_possible()? {
            return Ok(Reconciled::Kept);
        }
        let Some(floor) = self.logfloor()? else {
            return Ok(Reconciled::Kept);
        };
        let key = self.logkey()?;
        // The pin written from the copies: no batch, the key, the floor.
        let from_the_copies = || crate::mirror::Pin {
            key,
            total: if rules.total_from_floor { floor } else { 0 },
            ..crate::mirror::Pin::default()
        };
        let path = self.path(LOGPIN);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.set_logpin(&from_the_copies().render())?;
                return Ok(Reconciled::Restored);
            }
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => return Ok(Reconciled::Kept),
            Ok(_) => {}
        }
        let bytes = std::fs::read(&path)?;
        match std::str::from_utf8(&bytes)
            .ok()
            .map(crate::mirror::Pin::parse)
        {
            Some(Ok(mut pin)) => {
                if !rules.total_from_floor || pin.total >= floor {
                    return Ok(Reconciled::Kept);
                }
                let from = pin.total;
                pin.total = floor;
                self.set_logpin(&pin.render())?;
                Ok(Reconciled::Raised { from, to: floor })
            }
            _ if !rules.set_aside_torn
                || (rules.need_key && key.is_none())
                || crate::mirror::Pin::by_an_older_build(&bytes) =>
            {
                Ok(Reconciled::Kept)
            }
            _ => {
                let moved = self.move_into_quarantine(&[(LOGPIN.to_string(), bytes)], "")?;
                let Some(moved) = moved.into_iter().next() else {
                    return Ok(Reconciled::Kept);
                };
                self.set_logpin(&from_the_copies().render())?;
                Ok(Reconciled::SetAside(moved))
            }
        }
    }

    pub(crate) fn post_pin(&self, sid: &str, id: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("sent/{sid}.{id}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_post_pin(&self, sid: &str, id: u64, pin: &PublishPin) -> io::Result<()> {
        self.write(&format!("sent/{sid}.{id}"), &pin.render())
    }

    pub(crate) fn receipt_pin(&self, sid: &str, rid: u64) -> io::Result<Option<PublishPin>> {
        self.read_reservation(&format!("acks/{sid}.{rid}"))?
            .map(|s| PublishPin::parse(&s))
            .transpose()
    }

    pub(crate) fn set_receipt_pin(&self, sid: &str, rid: u64, pin: &PublishPin) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &pin.render())
    }

    pub(crate) fn key_pin(&self, sid: &str, key: &str) -> io::Result<Option<PublishPin>> {
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        text.lines()
            .find(|line| line.split_whitespace().next() == Some(key))
            .map(|line| {
                let rest = line.strip_prefix(key).unwrap_or_default().trim();
                PublishPin::parse(rest)
            })
            .transpose()
    }

    pub(crate) fn set_key_pin(
        &self,
        sid: &str,
        key: &str,
        pin: &PublishPin,
    ) -> io::Result<PublishPin> {
        if let Some(have) = self.key_pin(sid, key)? {
            return Ok(have);
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines.push(format!("{key} {}", pin.render()));
        let over = lines.len().saturating_sub(KEYS_KEEP);
        self.write(&format!("keys/{sid}"), &lines[over..].join("\n"))?;
        Ok(pin.clone())
    }

    /// Reconcile a retained key to the bytes actually found/appended, before
    /// retiring the post. Another reservation or an evicted key is left alone.
    pub(crate) fn reconcile_key(
        &self,
        sid: &str,
        key: &str,
        seq: u64,
        identity: &PublishIdentity,
        off: u64,
        owner: Option<u64>,
    ) -> io::Result<()> {
        let have = match self.key_pin(sid, key)? {
            Some(have) if have.seq == seq => have,
            _ => return Ok(()),
        };
        let owner = owner.or(have.owner);
        if have.identity.as_ref() == Some(identity)
            && have.original_off == Some(off)
            && have.owner == owner
        {
            return Ok(());
        }
        let next = PublishPin {
            seq,
            via_key: false,
            identity: Some(identity.clone()),
            original: None,
            original_off: Some(off),
            owner,
        };
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let lines: Vec<String> = text
            .lines()
            .map(|line| {
                if line.split_whitespace().next() == Some(key) {
                    format!("{key} {}", next.render())
                } else {
                    line.to_string()
                }
            })
            .collect();
        self.write(&format!("keys/{sid}"), &lines.join("\n"))
    }

    /// The `keys/` and `sent/` rewrites a log switch needs: each reservation keeps
    /// its ownership and identities and loses only `original-off`, an offset on the
    /// old log — on a log that continues the old one below `keep_below`
    /// ([`StateDir::forget_log_offsets`]), only one at or above it. Reads every
    /// reservation and writes none; the caller writes them before the witness
    /// advances, and a partial rewrite is safe to repeat.
    fn publication_offset_resets(
        &self,
        keep_below: Option<u64>,
    ) -> io::Result<Vec<(String, String)>> {
        // Take `original-off` when it names a record this log does not keep.
        let reset = |pin: &mut PublishPin| -> bool {
            let stale = pin
                .original_off
                .is_some_and(|off| keep_below.is_none_or(|f| off >= f));
            stale && pin.original_off.take().is_some()
        };
        let mut rewrites = Vec::new();
        for directory in ["keys", "sent"] {
            let path = self.path(directory);
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        && std::fs::symlink_metadata(&path)
                            .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid publication filename")
                })?;
                // A crash can leave an atomic write's hidden temp; it is not
                // the committed reservation at this name.
                if name.starts_with('.') {
                    continue;
                }
                if !entry.file_type()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "publication reservation is not a regular file",
                    ));
                }
                let text = std::fs::read_to_string(entry.path())?;
                let next = if directory == "keys" {
                    let mut changed = false;
                    let mut lines = Vec::new();
                    for line in text.lines() {
                        if line.trim().is_empty() {
                            lines.push(line.to_string());
                            continue;
                        }
                        let (key, mut pin) = key_line(line)?;
                        if reset(&mut pin) {
                            changed = true;
                            lines.push(format!("{key} {}", pin.render()));
                        } else {
                            lines.push(line.to_string());
                        }
                    }
                    changed.then(|| lines.join("\n"))
                } else {
                    let mut pin = PublishPin::parse(&text)?;
                    reset(&mut pin).then(|| pin.render())
                };
                if let Some(next) = next {
                    rewrites.push((format!("{directory}/{name}"), next));
                }
            }
        }
        Ok(rewrites)
    }

    /// Replace one file atomically: temp, rename, so a reader sees the old value or
    /// the new one, never a truncation, and a `kill -9` at any point leaves one of
    /// the two. Nothing is flushed; the module doc says which files must survive a
    /// power loss ([`StateDir::write_durable`]) and why the rest need not.
    ///
    /// The temp is this write's own fresh `O_EXCL` name (`create_temp_beside`), never
    /// a shared `<name>.tmp` that a second writer (a CLI beside the bridge, `fabric
    /// on`) could truncate mid-write and rename over the target. Nothing planted at
    /// the name is written through.
    ///
    /// # Errors
    ///
    /// Any I/O failure; a bridge that cannot persist a sequence reservation must not
    /// publish.
    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, false, 0o666)
    }

    /// [`StateDir::write`], made durable across a power loss or an OS crash: the temp
    /// is flushed before the rename and the directory after it, as astream's log does
    /// (`store.rs`'s `sync_parent_dir`; a flushed file whose entry is not may be gone
    /// after the boot). Several milliseconds on macOS (`F_FULLFSYNC`), so only for
    /// the files the module doc lists as durable, never one written per message.
    ///
    /// # Errors
    ///
    /// [`StateDir::write`]'s, and a directory flush failure [`sync_dir`] does not
    /// tolerate.
    fn write_durable(&self, name: &str, value: &str) -> io::Result<()> {
        self.replace(name, value, true, 0o666)
    }

    /// The one replace behind [`StateDir::write`] and [`StateDir::write_durable`]:
    /// `value` and its newline. `mode` is subject to the umask: 0o666
    /// (`File::create`'s) for every file but the ones holding a record's bytes,
    /// 0o600 ([`StateDir::set_in_flight`]).
    fn replace(&self, name: &str, value: &str, durable: bool, mode: u32) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(value.len() + 1);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(b'\n');
        self.replace_bytes(name, &bytes, durable, mode)
    }

    /// [`StateDir::replace`] of exactly `bytes`, in ONE `write`: the temp is this
    /// write's own, so nothing can read it half written, and a second `write`
    /// for the newline was one more system call on every replace.
    fn replace_bytes(&self, name: &str, bytes: &[u8], durable: bool, mode: u32) -> io::Result<()> {
        let flush = durable.then_some(aterm_durable::Flush::Durable);
        self.replace_bytes_as(name, bytes, flush, mode)
    }

    /// [`StateDir::replace_bytes`] finished by `flush` (`aterm_durable`'s two):
    /// none; [`aterm_durable::Flush::Durable`], the temp flushed before the
    /// rename and the directory after it, on the media on return; or
    /// [`aterm_durable::Flush::Ordered`], the temp's data flushed by the
    /// barrier before the rename — so the name never reaches the media before
    /// its bytes, whole — and the rename left to a later flush of the
    /// directory, the caller's: the clean stop's record
    /// ([`StateDir::end_run_clean`]), whose root is flushed in full next.
    fn replace_bytes_as(
        &self,
        name: &str,
        bytes: &[u8],
        flush: Option<aterm_durable::Flush>,
        mode: u32,
    ) -> io::Result<()> {
        #[cfg(test)]
        self.refused_by_a_test(name)?;
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        let w = crate::fx::Write::Replace {
            path: &target,
            bytes,
            flush,
            mode,
        };
        self.disk.before(w)?;
        std::fs::create_dir_all(parent)?;
        let (tmp, mut f) = crate::enable::create_temp_beside(&target, mode)?;
        let durable = flush == Some(aterm_durable::Flush::Durable);
        self.count(|ops| {
            ops.replaces += 1;
            ops.renames += 1;
            ops.fsyncs += 2 * u64::from(durable);
            ops.barriers += u64::from(flush == Some(aterm_durable::Flush::Ordered));
        });
        let staged = (|| {
            f.write_all(bytes)?;
            if let Some(flush) = flush.filter(|_| flushes_media()) {
                flush.file(&f)?;
            }
            drop(f);
            std::fs::rename(&tmp, &target)
        })();
        if staged.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return staged;
        }
        if durable {
            self.kill_point(IncStep::Renamed)?;
            if flushes_media() {
                sync_dir(parent)?;
            }
        }
        self.disk.after(w);
        // A file of a [`TornKind`] left unflushed is one a clean stop flushes
        // ([`StateDir::end_run_clean`]), and so is the mirror's pin, so a pin
        // found damaged after a clean stop is a bug's, never a power cut's; a
        // durable replace has flushed it. An ordered one (none of these
        // kinds is written so: only the clean stop's record) is the media's
        // only once a later flush is, so it settles nothing.
        if TornKind::of(name).is_some() || name == LOGPIN {
            let mut unflushed = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
            if durable {
                unflushed.remove(name);
            } else {
                unflushed.insert(name.to_string());
            }
        }
        Ok(())
    }

    /// Take this state dir for one bridge's life: an exclusive `flock` on
    /// `<root>/lock`, released when the [`BridgeLock`] drops or the process dies.
    ///
    /// [`StateDir::open`] does not take it: `fabric on` (provisioning the node id)
    /// and `aterm fabric` (reading) run beside a bridge and never publish as the
    /// node. The lock orders the writers that do: a bridge and the doctor's
    /// `publish_exited`.
    ///
    /// Opened with `enable::LOCK_OPEN_FLAGS`, so a link planted at the name is
    /// refused and a FIFO is not waited on. A filesystem without advisory locking
    /// (`Unsupported`) runs unguarded rather than not at all, as astream's own log
    /// lock does.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::WouldBlock`] when another process holds it (the message names
    /// its pid when the `pid` file does); otherwise the open's or the lock's error.
    pub fn lock_bridge(&self) -> io::Result<BridgeLock> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = self.path("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the bridge lock {} is not a regular file", path.display()),
            ));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: BridgeLock's Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(BridgeLock(file)),
            Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
                Ok(BridgeLock(file))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
            Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "the state dir {} is held by another bridge{}",
                    self.root.display(),
                    self.holder()
                        .map_or_else(String::new, |pid| format!(" (pid {pid})"))
                ),
            )),
        }
    }

    /// The pid the `pid` file names: the holder of [`StateDir::lock_bridge`], which
    /// writes it once it holds the lock. For messages only; the lock is the fact.
    #[must_use]
    pub fn holder(&self) -> Option<u32> {
        self.read("pid").and_then(|s| s.parse().ok())
    }

    /// This node's id, minted once and kept: the id is the mail address, so a new one
    /// would orphan the node's inbox lane.
    ///
    /// Durable, and so is the state dir's entry in its parent: the mint is usually the
    /// dir's first write, and an id flushed into a directory a power loss takes back
    /// leaves the node with no mail lane and no caps.
    ///
    /// # Errors
    ///
    /// If a freshly minted id cannot be persisted: the bridge refuses to start rather
    /// than run under an id that will not survive a restart.
    pub fn node_id(&self, mint: impl FnOnce() -> String) -> io::Result<String> {
        if let Some(id) = self.node() {
            return Ok(id);
        }
        let id = mint();
        self.write_durable("node", &id)?;
        sync_dir(self.root.parent().unwrap_or(Path::new("")))?;
        Ok(id)
    }

    /// The node id this dir records, if it records one — read, never minted.
    #[must_use]
    pub fn node(&self) -> Option<String> {
        self.read("node")
    }

    /// The incarnation this node last used, as far as the local disk knows — for the
    /// doctor's `publish_exited`, which publishes INSIDE it rather than rolling a new
    /// one: the one `inc-lease` says the last attach took, or `seq`'s incarnation where
    /// that is higher. A bridge reserves its first block under its incarnation after
    /// the take, so `seq`'s is never above the one taken, and it is what answers for an
    /// older build's dir, which keeps no lease (its attach reserved under the
    /// incarnation `inc` held); 0 where neither file says anything.
    ///
    /// NEVER WHAT `inc` HOLDS, which is the top of a lease: a doctor that published
    /// under it would raise the node's high water past the will of the incarnation
    /// really in use, and fence it — the dead node's row would stay `live`, and the next
    /// attach would never witness that incarnation dead. Not even with no whole lease
    /// left: a power loss can take the dir's FIRST lease, which has no older version on
    /// the media to fall back on, and `inc` alone cannot tell a lease's top from an
    /// older build's exact incarnation. So every answer is at or below the incarnation
    /// used, read from any boot's files: a stale lease, a rolled-back `seq` or nothing
    /// at all names one at or below it, where the doctor's sequences either dedup (it
    /// refuses, fail-closed, as after any rolled-back `seq`) or are fresh numbers below
    /// that incarnation's will, which fence nothing.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        let used = self.lease().map_or(0, |lease| lease.used);
        used.max(incarnation_of_seq(self.sequence()))
    }

    /// What `inc` holds: the top of the incarnation lease, at or above every
    /// incarnation this node has presented to a broker (the module doc's "The
    /// incarnation lease").
    fn incarnation_top(&self) -> u64 {
        self.read("inc").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// What `inc-lease` holds, if it holds a whole one ([`IncLease::parse`]).
    pub(crate) fn lease(&self) -> Option<IncLease> {
        self.read("inc-lease").and_then(|s| IncLease::parse(&s))
    }

    /// Which `inc` this dir holds now ([`IncFile`]): `None` where it holds none, where
    /// `inc` is not a regular file of the dir's own device (a link planted at the name
    /// is another file), or where the platform names no inode — and then no lease is
    /// trusted.
    #[cfg(unix)]
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        use std::os::unix::fs::MetadataExt;
        let dir = std::fs::metadata(&self.root).ok()?;
        let inc = std::fs::symlink_metadata(self.path("inc")).ok()?;
        (inc.is_file() && inc.dev() == dir.dev()).then(|| IncFile {
            dev: dir.dev(),
            dir: dir.ino(),
            ino: inc.ino(),
        })
    }

    /// No inode to name: never a trusted lease (nor a boot to trust it in:
    /// [`on_boot_volume`]).
    #[cfg(not(unix))]
    pub(crate) fn inc_file(&self) -> Option<IncFile> {
        None
    }

    /// THE MARK THIS STATE DIR'S BRIDGES PUT ON EVERY PRESENCE ROW OF A SESSION
    /// THEY WRITE ([`PRESENCE_MARK`], the module doc's "Whose row it is"):
    /// sixteen hex digits of FNV-1a/64 over the kernel boot identity, `node`
    /// and the dir's own device and inode. `None` without either identity —
    /// and then no row is ever the dir's by its mark. Read, never stored,
    /// so a copy of the dir (which keeps the bytes and gets a directory of its
    /// own) has a mark of its own, and a dir renamed within its volume keeps
    /// its mark IN THE SAME BOOT. A cloned machine can repeat device and inode,
    /// so a different boot must differ even for those same local names. A fixed
    /// versioned hash lets an update within a boot read its predecessor's rows;
    /// the previous, unqualified hash is deliberately not adopted. A witnessed
    /// will carries its writer's mark too: the incarnation alone can alias.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn mark(&self, node: &str) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let boot = self.kernel_boot()?;
        let dir = std::fs::metadata(&self.root).ok()?;
        let named = format!(
            "aterm-link boot state dir {} {node} {}:{}",
            aterm_codec::hex::encode_lower(&boot),
            dir.dev(),
            dir.ino()
        );
        Some(format!("{:016x}", aterm_hash::fnv1a_64(named.as_bytes())))
    }

    /// No inode to name: no mark, and no row is ever the dir's by one.
    #[cfg(not(unix))]
    #[must_use]
    pub(crate) fn mark(&self, _node: &str) -> Option<String> {
        None
    }

    /// This boot, when the page cache can vouch for this state dir: the kernel names
    /// it and the dir is on the boot volume ([`on_boot_volume`]). `None` otherwise,
    /// which trusts no lease. The boot is the broker's own
    /// ([`crate::durability::this_boot`]), so the lease and the log's unclean verdict
    /// never judge the page cache by different boots.
    pub(crate) fn trusted_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        {
            let seam = self.inc_seam();
            if let Some(boot) = seam.boot {
                return boot.filter(|_| !seam.off_volume);
            }
        }
        crate::durability::this_boot().filter(|_| on_boot_volume(&self.root))
    }

    /// This boot as the kernel names it, ON ANY VOLUME — the broker's own
    /// ([`crate::durability::this_boot`]): what the `run` record names
    /// ([`LastRun`]). Unlike [`StateDir::trusted_boot`] it vouches for no page
    /// cache; it only tells this boot from another.
    fn kernel_boot(&self) -> Option<[u8; 16]> {
        #[cfg(test)]
        if let Some(boot) = self.inc_seam().boot {
            return boot;
        }
        crate::durability::this_boot()
    }

    /// TAKE THE INCARNATION AN ATTACH RUNS UNDER, before its will is registered: one
    /// past what the disk says this node has taken and past `bus`, the newest `inc=`
    /// on the node's own presence face, and covered by a FLUSHED `inc` before it is
    /// answered (the module doc's "The incarnation lease": [`plan_incarnation`]
    /// decides, and this writes).
    ///
    /// THE ORDER IS THE ARGUMENT. `inc`, when the lease must grow, is flushed first;
    /// `inc-lease`, which names the new top, the file it was flushed into and the
    /// incarnation taken, is written only after that flush returned; the incarnation
    /// is answered only after both. So `inc-lease` never names a top the media may not
    /// hold, and nothing of an incarnation reaches a broker before a flushed `inc`
    /// covers it. In the boot that wrote it, a lease with room answers with one
    /// unflushed write (the attach's steady state); in any other, or in a copy of the
    /// dir, `inc` alone is trusted and the lease starts again above it.
    ///
    /// A read-plan-write of two files, so only the holder of
    /// [`StateDir::lock_bridge`] calls it.
    ///
    /// # Errors
    ///
    /// The incarnation space is exhausted (a corrupt `inc` or `seq`, or a bus row at
    /// [`INC_MAX`]: a named refusal, never a wrapped incarnation), or a write failed —
    /// and then the attach must not register a will.
    pub fn take_incarnation(&self, bus: u64) -> io::Result<u64> {
        let (top, file) = (self.incarnation_top(), self.inc_file());
        let boot = self.trusted_boot();
        let (lease, seq_inc, span) = (self.lease(), incarnation_of_seq(self.sequence()), INC_LEASE);
        #[cfg(test)]
        let (lease, span) = {
            let seam = self.inc_seam();
            let lease = match seam.mutant {
                1 => lease.map(|l| IncLease { boot, ..l }),
                2 => lease.map(|l| IncLease { top, ..l }),
                _ => lease,
            };
            (lease, seam.span.unwrap_or(span))
        };
        let plan =
            plan_incarnation(top, file, lease, seq_inc, boot, bus, span).ok_or_else(|| {
                io::Error::other(format!(
                    "this node's incarnation space is exhausted (inc {} of {INC_MAX})",
                    top.max(seq_inc).max(bus)
                ))
            })?;
        let leased = |file| IncLease {
            used: plan.next,
            top: plan.top,
            boot,
            file,
        };
        #[cfg(test)]
        if plan.flush && self.inc_seam().mutant == 3 {
            self.write("inc-lease", &leased(file).render())?;
        }
        // A flushed write renames a NEW file in: the lease names that one, read back
        // after the flush returned, so a copy of it is never the file a lease vouches
        // for. A lease with room keeps the file it was trusted on, which is `file`.
        let file = if plan.flush {
            self.write_durable("inc", &plan.top.to_string())?;
            self.kill_point(IncStep::Flushed)?;
            self.inc_file()
        } else {
            file
        };
        self.write("inc-lease", &leased(file).render())?;
        Ok(plan.next)
    }

    /// The highest producer sequence this bridge has reserved (not necessarily
    /// published) — the LAST number of the newest block
    /// ([`StateDir::reserve_sequence_block`]). It is written before any number of the
    /// block is used, so a crash burns the block's unused rest rather than risk a
    /// duplicate (the trade `asb pub --seq-file` makes).
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.read("seq").and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// Reserve the next producer sequence of incarnation `inc` and persist it before
    /// it is used: a block of one ([`StateDir::reserve_sequence_block`]), for the
    /// doctor's `publish_exited`, which publishes a handful of rows and holds the
    /// bridge's lock while it does.
    ///
    /// # Errors
    ///
    /// The incarnation's space is exhausted or the write failed; the caller must not
    /// publish either way.
    pub fn reserve_next_sequence(&self, inc: u64) -> io::Result<u64> {
        self.reserve_sequence_block(inc, 1)
            .map(|block| *block.start())
    }

    /// Reserve a BLOCK of up to `want` producer sequences of incarnation `inc` —
    /// the numbers just above both the file and `inc << 32` — and persist the
    /// block's LAST number before answering it: the one reservation, for the
    /// bridge (which hands the block out from memory, `Bridge::next_seq`) and the
    /// doctor's `publish_exited` alike.
    ///
    /// NEVER A NUMBER TWICE, and the argument is the file alone. The file only
    /// ever grows — every call writes a number above what it read — and it holds
    /// the last number of every block handed out, written (temp + rename) before
    /// the block's first number reaches anybody. So every number ever handed out
    /// is at or below the file, and the next call, in this process or the next,
    /// starts above it. A `kill -9` part-way through a block skips the block's
    /// unused rest — a burned number, which costs nothing — and reuses none. A
    /// power loss that rolls the file back is fenced by the DURABLE `inc`, as it
    /// always was (the module doc): the next attach takes `inc + 1`, whose every
    /// number is above anything a rolled-back file could name.
    ///
    /// Clamped below the incarnation's reserved top (astream's
    /// [`astream_broker::will_seq`], the will's own number), so a block near the
    /// top is shorter rather than refused; one that would start at the top is
    /// refused, as is an incarnation past [`INC_MAX`], where astream's layout has
    /// no space. A read-add-write of one file, so only the holder of
    /// [`StateDir::lock_bridge`] calls it.
    ///
    /// # Errors
    ///
    /// The incarnation's space is exhausted or the write failed; the caller must not
    /// publish either way.
    pub fn reserve_sequence_block(
        &self,
        inc: u64,
        want: u64,
    ) -> io::Result<std::ops::RangeInclusive<u64>> {
        // astream's layout, checked rather than shifted: `inc << 32` drops the
        // high bits, so an unchecked incarnation 2^32 would be incarnation 0's
        // space again.
        let (Some(base), Some(top)) = (
            astream_broker::incarnation_base(inc),
            astream_broker::will_seq(inc),
        ) else {
            return Err(io::Error::other(format!(
                "incarnation {inc} is past this node's incarnation space ({INC_MAX})"
            )));
        };
        let first = self
            .sequence()
            .max(base)
            .checked_add(1)
            .filter(|n| *n < top)
            .ok_or_else(|| io::Error::other("this incarnation's sequence space is exhausted"))?;
        let last = first.saturating_add(want.max(1) - 1).min(top - 1);
        self.write("seq", &last.to_string())?;
        Ok(first..=last)
    }

    /// Where one session's addressed refill starts after: the `floor=` of the
    /// `EVENT <local> inbox-seen` digest, minus one. The floor is the lowest offset of
    /// an addressed row the agent has not handled, else one past the highest
    /// delivered, so an acknowledged broadcast never moves it. The refill (§6.2)
    /// starts at `+1`.
    ///
    /// One file per session ever delivered to, and no sweep removes them: "departed"
    /// is not knowable here (a seamless update's sid is off aterm's roster between
    /// the re-exec and adoption), and a sweep in that window would refill a live
    /// session from `REFILL_FLOOR_SPAN`, re-offering thousands of old offsets.
    #[must_use]
    pub fn seen_off(&self, sid: &str) -> Option<u64> {
        self.read(&format!("seen/{sid}"))
            .and_then(|s| s.parse().ok())
    }

    /// Record a session's refill watermark ([`Self::seen_off`]). Monotone: moving it
    /// back would re-deliver rows the agent already handled.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_seen_off(&self, sid: &str, off: u64) -> io::Result<()> {
        if self.seen_off(sid).is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write(&format!("seen/{sid}"), &off.to_string())
    }

    /// Every offset this bridge holds a `PublishAck` for on its own lanes.
    ///
    /// For the self-lane check (§6.2): a record on this node's lane, under its
    /// `<src>`, at an offset not here is a forgery by a co-holder of the node cap. A
    /// set, not a high-water mark: other producers' offsets interleave with ours, and
    /// a watermark would vouch for every record below it.
    ///
    /// Every offset the file holds, older ones included: the bridge keeps the
    /// newest `keep` of them ([`AckedLog`]).
    #[must_use]
    pub fn self_acked(&self) -> std::collections::BTreeSet<u64> {
        self.self_acked_log().offsets
    }

    /// The `acked` file as it stands: its offsets, how many entries it holds, and
    /// whether an append may go on after its last byte ([`AckedLog`]).
    ///
    /// A missing file is the empty log, appendable. A file this process cannot
    /// read is the empty set too — the self-lane check gets more conservative,
    /// never less — and NOT appendable, so the next write replaces it whole.
    pub(crate) fn self_acked_log(&self) -> AckedLog {
        match std::fs::read_to_string(self.path("acked")) {
            Ok(text) => AckedLog::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            Err(_) => AckedLog::default(),
        }
    }

    /// Replace the acked self-lane set with the newest `keep` of `set`: the
    /// COMPACTION of the append log ([`StateDir::append_self_acked`]), and the
    /// write that follows any failed or torn one. The bridge's in-memory copy is
    /// the set, so it is written without reading the file back.
    ///
    /// Each offset ends at its comma and nothing follows the last: an append then
    /// goes on after the last byte, and an older bridge's reader (`split(',')`
    /// after a trim) reads the same set.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_self_acked(
        &self,
        set: &std::collections::BTreeSet<u64>,
        keep: usize,
    ) -> io::Result<()> {
        let over = set.len().saturating_sub(keep);
        self.replace_bytes(
            "acked",
            AckedLog::render(set.iter().skip(over).copied()).as_bytes(),
            false,
            0o666,
        )
    }

    /// Append `off` to the acked self-lane log, ended by its comma, in ONE
    /// `write` to a file opened `O_APPEND`: a `kill -9` lands all of the write
    /// or none of it, so the file always ends at a terminator. What a power
    /// loss or a full disk can tear is the tail, and a number without its comma
    /// is never read as an offset ([`AckedLog::parse`]); a failed append is
    /// followed by a whole replace ([`StateDir::set_self_acked`]), never by
    /// another append onto the fragment, which could join it to a new number.
    ///
    /// Opened with `enable::LOCK_OPEN_FLAGS`: a link planted at the name is
    /// refused, not written through, and a FIFO is not waited on. The opened
    /// handle must name a regular file owned by this uid with one link before
    /// any append; refused occupants are neither written nor removed.
    ///
    /// # Errors
    ///
    /// Any I/O failure: the caller keeps the offset's in-flight entry and
    /// writes the set whole next time.
    pub(crate) fn append_self_acked(&self, off: u64) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        self.count(|ops| ops.appends += 1);
        let path = self.path("acked");
        let line = AckedLog::render(std::iter::once(off));
        let w = crate::fx::Write::Append {
            path: &path,
            bytes: line.as_bytes(),
        };
        self.disk.before(w)?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o666)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != aterm_uds::peer::our_uid()
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the acked log is not an owned regular single-link file",
            ));
        }
        file.write_all(line.as_bytes())?;
        self.disk.after(w);
        Ok(())
    }

    /// Every record in flight onto this node's own lanes, by sequence: written before
    /// its publish ([`StateDir::set_in_flight`]) and cleared once `acked` holds its
    /// offset, so its offset may be unknown (a kill between the `PublishAck` and the
    /// `acked` write, or before any answer). The attach publishes each again at its
    /// sequence before anything is judged, and the broker's dedup hands back the
    /// offset.
    ///
    /// A missing dir is nothing pending. Any listing, read or parse failure leaves a
    /// publication's fate unknown and stops the attach before any own row is judged;
    /// nothing is discarded, so a repaired journal is retried at the same sequence.
    ///
    /// # Errors
    ///
    /// Any directory or entry I/O error, or a malformed numeric entry.
    pub(crate) fn in_flight(&self) -> io::Result<Vec<(u64, InFlight)>> {
        let dir = self.path("inflight");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && std::fs::symlink_metadata(&dir)
                        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            // Digits only: a writer's temp (`.<seq>.tmp.…`) is not an entry.
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let bad = || {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: malformed in-flight entry", entry.path().display()),
                )
            };
            let seq = name.parse().map_err(|_| bad())?;
            let text = self
                .read_reservation(&format!("inflight/{name}"))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "{}: in-flight entry disappeared during recovery",
                            entry.path().display()
                        ),
                    )
                })?;
            let record = InFlight::parse(seq, &text).ok_or_else(bad)?;
            out.push((seq, record));
        }
        out.sort_by_key(|(seq, _)| *seq);
        Ok(out)
    }

    /// Record one own-lane record as in flight, before it is published.
    ///
    /// Mode 0o600: this is the one file here that holds a record's body (the
    /// reservations hold only its identity), from the publish to the `acked` write,
    /// or until the next attach when the publish's fate is unknown.
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub(crate) fn set_in_flight(&self, entry: &InFlight) -> io::Result<()> {
        self.replace(
            &format!("inflight/{}", entry.pin.seq),
            &entry.render(),
            false,
            0o600,
        )
    }

    /// Forget the in-flight record at `seq`: it landed and `acked` holds its offset.
    ///
    /// Atomic only, as the entry itself is, so no flush is spent per message: an
    /// unlink a power cut rolls back brings the entry back, and the first bridge
    /// of the next boot retires it unpublished
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]) — published again, it
    /// landed behind its sender's later posts wherever the log had lost the
    /// record on its own (finding F2). A record that did NOT land is dropped
    /// durably ([`StateDir::drop_in_flight`]).
    pub(crate) fn clear_in_flight(&self, seq: u64) {
        let _ = self.unlink(&format!("inflight/{seq}"));
    }

    /// DROP the in-flight record at `seq` FOR GOOD: it provably did not land — the
    /// broker refused it, another writer holds its sequence, it is not provably ours
    /// — or it was decided on another log and is not published on this one. The
    /// unlink is flushed (the `inflight/` directory), because what the bridge does
    /// next says so to somebody: a post retired `off=-`, a receipt `unacked`, a
    /// sender told its post did not go. Brought back by a power cut, the entry was
    /// published by the next attach after all — after the sender's later posts,
    /// and on the lane of a session that may have left. (The next boot's first
    /// bridge now retires whatever a cut brings back,
    /// [`StateDir::retire_in_flight_of_an_earlier_boot`]; the flush still keeps
    /// the drop where nothing marks the stop unclean.) One directory flush, spent
    /// only on a refusal or a move of the log, never per message.
    ///
    /// # Errors
    ///
    /// The unlink's (other than a missing entry) or the flush's.
    pub(crate) fn drop_in_flight(&self, seq: u64) -> io::Result<()> {
        match self.unlink(&format!("inflight/{seq}")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.flush_dir(&self.path("inflight"))
    }

    /// Flush the directory `dir` through the dir's door ([`crate::fx::Disk`]), so
    /// every entry change in it (an unlink, a rename) survives a power loss.
    fn flush_dir(&self, dir: &Path) -> io::Result<()> {
        self.flush_dir_as(dir, aterm_durable::Flush::Durable)
    }

    /// FLUSH THE FILE `rel` WHERE IT IS, its bytes as they stand, by `flush`
    /// ([`StateDir::end_run_clean`]'s): no copy is written and nothing is
    /// renamed, so its mode, its bytes and its name are the ones the run left,
    /// and only what the page cache still holds of it is written. Its name is
    /// its directory's flush's. `false` when there is no regular file there:
    /// gone since, or a directory or FIFO planted at the name (opened without
    /// waiting, never flushed through). A link or a socket planted at the name
    /// is refused by the open itself (`O_NOFOLLOW`: `ELOOP`; a socket does not
    /// open) and is the error, so the stop that met it leaves `run` open and
    /// claims no clean stop. The caller looks first and passes over what is
    /// not a regular file then ([`StateDir::end_run_clean`]), so only an
    /// occupant planted between that look and this open is refused here.
    fn flush_in_place(&self, rel: &str, flush: aterm_durable::Flush) -> io::Result<bool> {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(test)]
        self.refused_by_a_test(rel)?;
        let path = self.path(rel);
        let w = crate::fx::Write::Flush { path: &path, flush };
        self.disk.before(w)?;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if !file.metadata()?.is_file() {
            return Ok(false);
        }
        self.count(|ops| match flush {
            aterm_durable::Flush::Durable => ops.fsyncs += 1,
            aterm_durable::Flush::Ordered => ops.barriers += 1,
        });
        if flushes_media() {
            flush.file(&file)?;
        }
        self.disk.after(w);
        Ok(true)
    }

    /// [`StateDir::flush_dir`] with `flush`: on the media on return, or
    /// ordered ahead of every later write on the device (the barrier, a clean
    /// stop's, [`StateDir::end_run_clean`]).
    fn flush_dir_as(&self, dir: &Path, flush: aterm_durable::Flush) -> io::Result<()> {
        let w = crate::fx::Write::SyncDir { path: dir, flush };
        self.disk.before(w)?;
        self.count(|ops| match flush {
            aterm_durable::Flush::Durable => ops.fsyncs += 1,
            aterm_durable::Flush::Ordered => ops.barriers += 1,
        });
        sync_dir_as(dir, flush)?;
        self.disk.after(w);
        Ok(())
    }

    /// How the last bridge of this dir stopped, and in which boot (`run`,
    /// [`LastRun`]); `None` only for a genuinely missing record (a build
    /// before it). An unreadable, damaged or nonregular occupant is refused.
    pub(crate) fn last_run(&self) -> io::Result<Option<LastRun>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = self.path("run");
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return match std::fs::symlink_metadata(&path) {
                    Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(other) => Err(other),
                    Ok(_) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "invalid bridge run record");
        if !file.metadata()?.is_file() {
            return Err(bad());
        }
        let mut text = String::new();
        file.take(128).read_to_string(&mut text)?;
        if text.len() >= 128 {
            return Err(bad());
        }
        LastRun::parse(&text).map(Some).ok_or_else(bad)
    }

    /// WHETHER A POWER CUT CAN HAVE TORN A FILE OF A [`TornKind`] since the
    /// last bridge wrote one: that bridge did NOT STOP CLEANLY, and it ran in a
    /// boot the kernel no longer runs — astream's own unclean-stop rule for its
    /// weak tiers (a run that did not close cleanly, judged in another boot).
    /// Read off the durable `run` record ([`LastRun`]); a dir without one (a
    /// build before it) by its incarnation lease's boot, which names one only
    /// for a dir on the boot volume ([`StateDir::trusted_boot`]).
    ///
    /// `false` within one boot ON ANY VOLUME — a kill, a crash, a disk that
    /// dropped out and came back: the sessions those files serve are alive,
    /// and damage there is a bug, refused where it is read (a reservation set
    /// aside would publish its post again, a second copy). `false` after a
    /// CLEAN STOP, which flushed every such file before it said so
    /// ([`StateDir::end_run_clean`]): nothing a reboot keeps is torn, and
    /// damage is a bug's. And `false` where the kernel names no boot, now or
    /// then: no evidence of a reboot is no sweep — fail-closed, the safe
    /// direction here as a flush is astream's.
    pub(crate) fn torn_files_possible(&self) -> io::Result<bool> {
        let last = self.last_run()?;
        let Some(now) = self.kernel_boot() else {
            return Ok(false);
        };
        Ok(match last {
            Some(run) => !run.clean && run.boot.is_some_and(|then| then != now),
            None => self
                .lease()
                .and_then(|lease| lease.boot)
                .is_some_and(|then| then != now),
        })
    }

    /// WHETHER AN ATTACH RAN IN THIS BOOT, as far as the incarnation lease
    /// says: every attach writes it, naming the boot it was taken in where the
    /// page cache vouches for the dir ([`StateDir::take_incarnation`],
    /// [`StateDir::trusted_boot`]). `false` off the boot volume, where it
    /// names none, and before the first attach of a boot, whose lease an
    /// earlier boot wrote.
    fn attached_in_this_boot(&self) -> bool {
        self.trusted_boot()
            .is_some_and(|now| self.lease().and_then(|lease| lease.boot) == Some(now))
    }

    /// A swept-kind parent must be a real directory. In particular a planted
    /// directory link must never lend external files to the sweep or clean close.
    fn torn_kind_dir(&self, kind: TornKind) -> io::Result<Option<PathBuf>> {
        let path = self.path(kind.place());
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
            Ok(meta) if meta.is_dir() => Ok(Some(path)),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a directory", path.display()),
            )),
        }
    }

    /// Every regular file of a [`TornKind`] in the dir, as `(kind, name, path
    /// inside the dir)`, kind by kind in [`TornKind::ALL`]'s order and names
    /// sorted. A link, a directory or anything else at an entry's name is
    /// none (a power cut tears data; it makes no link and no directory).
    fn torn_kind_files(&self) -> io::Result<Vec<(TornKind, String, String)>> {
        let mut out = Vec::new();
        for kind in TornKind::ALL {
            for (name, rel) in self.kind_files(kind)? {
                out.push((kind, name, rel));
            }
        }
        Ok(out)
    }

    /// Every regular file of `kind` in the dir, as `(name, path inside the
    /// dir)`, names sorted ([`StateDir::torn_kind_files`]'s rule for one kind).
    fn kind_files(&self, kind: TornKind) -> io::Result<Vec<(String, String)>> {
        let Some(dir) = self.torn_kind_dir(kind)? else {
            return Ok(Vec::new());
        };
        let entries = std::fs::read_dir(dir)?;
        let mut names = Vec::new();
        for entry in entries {
            if let Some(name) = entry?.file_name().to_str().filter(|n| kind.names(n)) {
                names.push(name.to_string());
            }
        }
        names.sort_unstable();
        let mut out = Vec::new();
        for name in names {
            let rel = format!("{}/{name}", kind.place());
            match std::fs::symlink_metadata(self.path(&rel)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
                Ok(meta) if !meta.is_file() => {}
                Ok(_) => out.push((name, rel)),
            }
        }
        Ok(out)
    }

    /// AFTER AN UNCLEAN STOP IN AN EARLIER BOOT ([`StateDir::torn_files_possible`]),
    /// SET EVERY TORN FILE ASIDE — and the readers then find it gone, which is
    /// what losing it costs, rather than refuse it at every attach of every later
    /// boot until a person repairs the dir.
    ///
    /// POSIX keeps nothing of an unflushed file's DATA across a power loss, so a
    /// rename that reached the media may name a file whose bytes did not: empty,
    /// or cut anywhere. Every file of a [`TornKind`] is written atomically and
    /// flushed only at a clean stop, so a power cut may tear it, and its reader
    /// refuses damage (fail-closed: within one boot damage is a bug, never a
    /// cut) — or, cut at the wrong byte, reads it as something it never was (an
    /// in-flight reply with no witness, which the attach refuses for good).
    /// Each kind serves only sessions, which a reboot ends, so a torn one is
    /// harmless to lose — but kept, it wedged the bridge for good: the
    /// in-flight journal stopped every attach, a torn reservation stopped every
    /// attach that had to forget a moved log's offsets.
    ///
    /// SO A FILE THAT IS NOT PROVABLY WHOLE ([`TornKind`]'s rule: its last
    /// newline, its reader's parse and, in flight, its witness) IS MOVED ASIDE,
    /// NEVER DELETED: its exact bytes go into [`QUARANTINE`] inside the dir
    /// (`quarantine/<path with / as .>`, a `.<n>` added past a name taken),
    /// DURABLY, before its own name is unlinked and that directory flushed — so
    /// a power cut at any step leaves it in one place or both, never in none,
    /// and moving it back is a person's `mv`. A whole file stays where it is,
    /// as does anything that is not a regular file.
    ///
    /// Within one boot, after a clean stop, and where no boot can be told from
    /// another, NOTHING is moved: damage there is not a power cut's, and its
    /// reader refuses it, as it always did. Run before anything reads the dir's
    /// files of these kinds and before [`StateDir::begin_run`] renews the
    /// record that says whether one can be torn, so a bridge killed part-way
    /// leaves the next one in the same verdict, to sweep again (idempotent:
    /// what was moved is gone, what is whole stays). Only the holder of
    /// [`StateDir::lock_bridge`] calls it.
    ///
    /// # Errors
    ///
    /// The first I/O failure listing, reading or moving: what was moved before it
    /// stays moved, and the reader of what was not refuses it as before.
    pub(crate) fn set_aside_torn(&self) -> io::Result<Vec<SetAside>> {
        self.sweep_torn(TornKind::whole)
    }

    /// TEST-ONLY: [`StateDir::set_aside_torn`] with each file judged by its
    /// reader alone ([`TornKind::reads`]), as finding F1's first fix judged it:
    /// a cut that still reads stays. The simulation's negative control
    /// (`Mutant::SweepByTheReaderAlone`).
    #[cfg(test)]
    pub(crate) fn set_aside_torn_by_the_reader_alone(&self) -> io::Result<Vec<SetAside>> {
        self.sweep_torn(TornKind::reads)
    }

    /// The sweep [`StateDir::set_aside_torn`] is, with `whole` the judge of a
    /// file of a kind.
    fn sweep_torn(
        &self,
        whole: fn(TornKind, &Path, &str, &str) -> bool,
    ) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? {
            return Ok(Vec::new());
        }
        let mut torn: Vec<(String, Vec<u8>)> = Vec::new();
        for (kind, name, rel) in self.torn_kind_files()? {
            let path = self.path(&rel);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let whole =
                std::str::from_utf8(&bytes).is_ok_and(|text| whole(kind, &path, &name, text));
            if !whole {
                torn.push((rel, bytes));
            }
        }
        self.move_into_quarantine(&torn, "")
    }

    /// FINDING F2, DECIDED: AFTER AN UNCLEAN STOP IN AN EARLIER BOOT
    /// ([`StateDir::torn_files_possible`]), EVERY WHOLE IN-FLIGHT ENTRY IS
    /// RETIRED — moved aside, never published again.
    ///
    /// An entry's unlink is atomic only ([`StateDir::clear_in_flight`]), so a
    /// power cut can bring back the entry of a record that LANDED and was
    /// answered. When the log on its own lost that record (a log on another
    /// volume, a lossy flush: the law lets it lose an unescalated one) and the
    /// sender's later posts landed after it, the next attach's republish
    /// ([`crate::bridge::Bridge::settle_in_flight`]) appended it again BEHIND
    /// them: the sender's posts out of order, the one thing the outbox promises
    /// per sender. Nothing the bridge can read tells that entry from one whose
    /// record never landed: a read of the log carries no producer sequence, the
    /// sender's later post may be on any lane of any node, and the same cut
    /// rolls back the later post's reservation (`sent/`) and entry with it.
    ///
    /// AND NOTHING NEEDS IT REPUBLISHED. An entry exists so the self-lane check
    /// can re-learn the offset of a record of ours before the refill judges it,
    /// and so a record the bridge decided to publish lands once. Both serve the
    /// sessions on this node's own lanes — the record's sender and its
    /// recipient — and a reboot ended them: aterm mints every sid afresh at
    /// launch, the outbox lives in its memory, and the not-hosted gate runs
    /// before the self-lane check, so no record of a session that is gone is
    /// ever judged. The table calls a lost entry harmless after a reboot; this
    /// makes the entry lost on purpose, rather than let a cut decide.
    ///
    /// WITHIN ONE BOOT the republish stays, and keeps the order: an entry
    /// outlives its record's answer only where `acked` could not be written,
    /// one post per sender is in flight (`Bridge::settle_sender`: a sender's
    /// next post goes out only once its last is answered), the log keeps only
    /// a prefix of what it held, and every attach to it republishes before any
    /// post — so a republish that lands a record lands it on a log that lost
    /// every later post of its sender too. So it does after a CLEAN STOP: its
    /// flush kept every unlink ([`StateDir::end_run_clean`]), so an entry it
    /// leaves was never cleared, and the same argument carries it across the
    /// reboot. Both need the state dir's page cache to have outlived what the
    /// log lost. A state dir on a volume of its own that loses power alone
    /// and comes back within the boot (`ATERM_STATE_HOME` on an external
    /// disk) can roll an unlink back as a power cut does, and nothing here
    /// tells that from a kill — the case [`StateDir::torn_files_possible`]
    /// counts as one boot too. It is NOT covered: an entry it brings back is
    /// published again, behind its sender's later posts where the log, on
    /// another volume, lost its record on its own; retiring on it would take
    /// live sessions' entries.
    ///
    /// AND A BOOT'S OWN ENTRIES ARE NOT RETIRED, though `run` can still name
    /// an earlier boot's unclean stop after this boot's first bridge: the
    /// disk can refuse its [`StateDir::begin_run`] (full, an I/O error), and
    /// that is said and survived. No attach of this build goes on while it
    /// does (`Bridge::run_names_this_boot` writes `run` again first), so no
    /// entry is written then. And where an incarnation lease names this boot
    /// — an attach ran in it, an older build's, which did not wait — the
    /// entries may be live sessions', and nothing is moved
    /// ([`StateDir::attached_in_this_boot`]). Only on the boot volume does
    /// the lease name a boot: off it, an older build's attach past a refused
    /// `run`, followed in the same boot by this build's start, is the one
    /// case where a live entry is still retired.
    ///
    /// Each entry's exact bytes go into [`QUARANTINE`] as
    /// `retired.inflight.<seq>` ([`RETIRED_PREFIX`]), durably, before its
    /// own name is unlinked and `inflight/` flushed, as [`StateDir::set_aside_torn`]
    /// moves a torn file — so the bytes stay the owner's, and a person's `mv`
    /// back publishes one. Only a PROVABLY WHOLE entry ([`TornKind`]'s rule) is
    /// retired: one that is not is the torn sweep's, or, where that sweep did
    /// not run, refused where it is read as before. Within one boot, after a
    /// clean stop (which flushed every unlink, so an entry there is one still
    /// in flight), once an attach of this boot is on record, and where no boot
    /// can be told from another, NOTHING is moved. Run after
    /// [`StateDir::set_aside_torn`] and before
    /// [`StateDir::begin_run`], so a bridge killed part-way leaves the next one
    /// the same verdict (idempotent: what was moved is gone).
    ///
    /// # Errors
    ///
    /// A `run` record that does not read ([`StateDir::torn_files_possible`]),
    /// or the first I/O failure listing, reading or moving: what was moved
    /// before it stays moved, and what was not is published again by the
    /// attach, as before.
    pub(crate) fn retire_in_flight_of_an_earlier_boot(&self) -> io::Result<Vec<SetAside>> {
        if !self.torn_files_possible()? || self.attached_in_this_boot() {
            return Ok(Vec::new());
        }
        let mut whole: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, rel) in self.kind_files(TornKind::InFlight)? {
            let path = self.path(&rel);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if std::str::from_utf8(&bytes)
                .is_ok_and(|text| TornKind::InFlight.whole(&path, &name, text))
            {
                whole.push((rel, bytes));
            }
        }
        self.move_into_quarantine(&whole, RETIRED_PREFIX)
    }

    /// HOW MANY TORN FILES THE QUARANTINE STILL HOLDS: every regular file in
    /// [`QUARANTINE`] a sweep set aside ([`StateDir::set_aside_torn`], and the
    /// mirror's pin, [`StateDir::reconcile_logpin`]) and the owner has not yet
    /// removed or moved back — never a whole in-flight entry an earlier boot
    /// left ([`RETIRED_PREFIX`], told to nobody), nor a hidden temp a move
    /// stages. `None` where there is no quarantine: nothing was ever set aside
    /// in this dir. It is the owner's to empty, and what tells aterm that the
    /// fact it filed for the owner's look has ended (`Bridge::tell_quarantine`).
    ///
    /// # Errors
    ///
    /// The quarantine's name holds something that is not a real directory (a
    /// link or a file planted there), or it cannot be listed.
    pub(crate) fn quarantined_torn(&self) -> io::Result<Option<u64>> {
        let quarantine = self.path(QUARANTINE);
        match std::fs::symlink_metadata(&quarantine) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a directory", quarantine.display()),
                ));
            }
            Ok(_) => {}
        }
        let mut torn = 0u64;
        for entry in std::fs::read_dir(&quarantine)? {
            let entry = entry?;
            let named = entry
                .file_name()
                .to_str()
                .is_some_and(|n| !n.starts_with('.') && !n.starts_with(RETIRED_PREFIX));
            // `DirEntry::file_type` does not follow a link: a link is none.
            if named && entry.file_type()?.is_file() {
                torn += 1;
            }
        }
        Ok(Some(torn))
    }

    /// MOVE EACH `(path inside the dir, its bytes)` INTO [`QUARANTINE`], NEVER
    /// DELETED: its exact bytes at `quarantine/<prefix><path with / as .>` (a
    /// `.<n>` added past a name taken, never over another), DURABLY, mode 0600;
    /// then its own name unlinked and each directory it left flushed — so a
    /// power cut at any step leaves it in one place or both, never in none. The
    /// quarantine's own entry is flushed when it is made, and one that is not a
    /// real directory (a link or file planted at the name) refuses the move,
    /// moving nothing. Answers `(from, to)` per file, in the order given.
    fn move_into_quarantine(
        &self,
        files: &[(String, Vec<u8>)],
        prefix: &str,
    ) -> io::Result<Vec<SetAside>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        // The quarantine's own entry in the dir, on the media before anything
        // is moved into it. Only a real directory is written into: a link or a
        // file planted at the name is never written through.
        let quarantine = self.path(QUARANTINE);
        match std::fs::symlink_metadata(&quarantine) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not a directory", quarantine.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&quarantine)?;
                self.flush_dir(&self.root)?;
            }
            Err(e) => return Err(e),
        }
        let mut moved = Vec::new();
        for (from, bytes) in files {
            let base = format!("{QUARANTINE}/{prefix}{}", from.replace('/', "."));
            let mut to = base.clone();
            let mut n = 0u32;
            loop {
                match self.publish_quarantine_file(&to, bytes) {
                    Ok(()) => break,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        n = n
                            .checked_add(1)
                            .ok_or_else(|| io::Error::other("quarantine suffix space exhausted"))?;
                        to = format!("{base}.{n}");
                    }
                    Err(e) => return Err(e),
                }
            }
            moved.push(SetAside {
                from: from.clone(),
                to,
            });
        }
        let mut dirs = std::collections::BTreeSet::new();
        for SetAside { from, .. } in &moved {
            let path = self.path(from);
            match self.remove_path(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            dirs.insert(path.parent().unwrap_or(&self.root).to_path_buf());
        }
        for dir in dirs {
            self.flush_dir(&dir)?;
        }
        Ok(moved)
    }

    /// Publish exact evidence bytes without replacing any occupant: stage and
    /// flush an owned temp, then link it exclusively at `name`. A collision is
    /// the link's `AlreadyExists`, even if it appeared after staging. Every other
    /// error is returned before the sweep can unlink any source.
    fn publish_quarantine_file(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let target = self.path(name);
        let parent = target.parent().unwrap_or(&self.root);
        let w = crate::fx::Write::Replace {
            path: &target,
            bytes,
            flush: Some(aterm_durable::Flush::Durable),
            mode: 0o600,
        };
        self.disk.before(w)?;
        let (tmp, mut file) = crate::enable::create_temp_beside(&target, 0o600)?;
        let staged = (|| {
            file.write_all(bytes)?;
            if flushes_media() {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "quarantine evidence is flushed before its source may be unlinked"
                )]
                file.sync_all()?;
            }
            drop(file);
            std::fs::hard_link(&tmp, &target)
        })();
        let cleaned = std::fs::remove_file(&tmp);
        cleaned?;
        staged?;
        self.count(|ops| {
            ops.replaces += 1;
            ops.fsyncs += 1;
        });
        sync_dir(parent)?;
        self.count(|ops| ops.fsyncs += 1);
        self.disk.after(w);
        Ok(())
    }

    /// A BRIDGE'S RUN BEGINS, after its sweep ([`StateDir::set_aside_torn`])
    /// and before it writes any file of a [`TornKind`]: `run` names this boot,
    /// OPEN, durably — so a power cut from here on is judged unclean by the
    /// first bridge of the next boot ([`StateDir::torn_files_possible`]). One
    /// durable write at the first start in a boot and at the first after a
    /// clean stop; none at a start after a kill or a handoff in the same boot
    /// (`Bridge::end_run`: a seamless update's successor goes on with the run),
    /// whose record already says so.
    ///
    /// And what this run must flush before it may say it stopped cleanly
    /// ([`StateDir::end_run_clean`]): what it writes itself, and — unless the
    /// last run stopped cleanly or ran in another boot, so the media holds every
    /// such file there now — every one already here, which a predecessor killed
    /// in this boot may have left unflushed; the mirror's pin among them.
    ///
    /// # Errors
    ///
    /// Reading the existing record, listing those files, or writing the record.
    /// Invalid or unreadable marker evidence is not replaced, and no marker is
    /// removed to force a legacy fallback. A write may already have progressed
    /// before an I/O error; uncertainty must remain fail-closed.
    pub(crate) fn begin_run(&self) -> io::Result<Flushed> {
        let now = self.kernel_boot();
        let last = self.last_run()?;
        let on_the_media = last.is_some_and(|run| {
            run.clean || (now.is_some() && run.boot.is_some() && run.boot != now)
        });
        let open = LastRun {
            boot: now,
            clean: false,
        };
        if !on_the_media {
            let here = self.torn_kind_files()?;
            let pin = std::fs::symlink_metadata(self.path(LOGPIN))
                .is_ok_and(|meta| meta.is_file())
                .then(|| LOGPIN.to_string());
            self.unflushed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(here.into_iter().map(|(_, _, rel)| rel).chain(pin));
        }
        let mut flushed = Flushed::default();
        if last != Some(open) {
            self.write_durable("run", &open.render())?;
            flushed.full += 2;
        }
        Ok(flushed)
    }

    /// A CLEAN STOP: every file of a [`TornKind`] this run must flush
    /// ([`StateDir::begin_run`]), and the mirror's pin, flushed where it is,
    /// every such directory flushed (an unlink made since is kept), and only
    /// then `run` says the run stopped cleanly — on the media, with all of it,
    /// when this returns. A power cut from then until the next bridge's
    /// [`StateDir::begin_run`] tears none of them, so the first bridge of the
    /// next boot sets nothing aside: damage it finds is a bug's, refused where
    /// it is read — the promise astream's clean close makes of its log
    /// (`BrokerLog::close_clean`).
    ///
    /// ORDERED BY THE BARRIER, ENDED BY ONE FULL FLUSH (sim item S6). The law
    /// needs one order only: `run`=clean never on the media before what it
    /// vouches for. That is what aterm-durable's barrier
    /// (`aterm_durable::ordered`, `F_BARRIERFSYNC` on macOS) promises: "A site
    /// may use this for 'A persists before B' on one device only after
    /// flushing every descriptor that phase A depends on, including directory
    /// publications". Phase A is each file flushed where it is by the barrier
    /// ([`StateDir::flush_in_place`]: its bytes as they stand, nothing copied
    /// or renamed), then the barrier on every directory whose entries moved —
    /// the renames that put those bytes at their names, and every unlink this
    /// run made; the root's too where the mirror's pin was flushed. Phase B is
    /// the record: its temp flushed by the barrier before its rename (its name
    /// never on the media ahead of its bytes), and the root flushed in full,
    /// which puts every write flushed before it on the device on the media too
    /// ("a successful full flush also persists the earlier, already-flushed
    /// I/O on its device") — so on return the stop is as durable as it was
    /// with a full flush at every step. Its recovery, which a barrier's caller
    /// must name: a power cut before the return leaves either `run` open, and
    /// the next boot's sweep sets aside whatever it finds torn, or `run` clean
    /// with every file and unlink under it whole (`sim::tests`'
    /// `a_clean_stop_keeps_the_runs_files_exactly_under_its_record`: a kill at every
    /// write and a tearing power cut after it, with the record first, the
    /// directories unflushed and the files unflushed as its controls). A
    /// directory on another device than the root, where `run` lives, is the
    /// barrier's blind spot ("it promises nothing about another volume"): it
    /// and its files are flushed in full
    /// ([`aterm_durable::Flush::ordered_if_same_device`]).
    ///
    /// The last act of a bridge that stops because aterm went away for good —
    /// a quit, or aterm gone without a word; never a handoff, whose successor
    /// goes on with the run (`Bridge::end_run`) — after its last write of
    /// those files (`Bridge::wind_down`): a barrier a file this run wrote or
    /// inherited (an entry still in flight, a reservation, a session's keys
    /// or floors, the mirror's pin) and a directory, one the record's bytes,
    /// and one full flush. Measured on fabric_world's `handoff_cost` (sixteen
    /// files, four directories): 38 full flushes in about 147 ms when each
    /// file was rewritten durably; the CHANGELOG has the numbers since. A
    /// bridge that dies instead leaves `run` open, and the next boot's sweep
    /// runs.
    ///
    /// # Errors
    ///
    /// The first record read, directory admission, file flush or directory
    /// flush that fails.
    /// Invalid marker evidence is left untouched, and paths still needing a
    /// flush remain pending for another attempt. No marker is removed to force
    /// a legacy fallback.
    pub(crate) fn end_run_clean(&self) -> io::Result<Flushed> {
        self.end_run_clean_by(StopRules::ALL)
    }

    /// TEST-ONLY: [`StateDir::end_run_clean`] with one of its rules off — the
    /// negative controls of the clean-stop law on the simulated device
    /// (`sim::tests`' `a_clean_stop_keeps_the_runs_files_exactly_under_its_record`).
    #[cfg(test)]
    pub(crate) fn end_run_clean_with(&self, rules: StopRules) -> io::Result<Flushed> {
        self.end_run_clean_by(rules)
    }

    fn end_run_clean_by(&self, rules: StopRules) -> io::Result<Flushed> {
        use aterm_durable::Flush;
        let _ = self.last_run()?;
        let dirs: Vec<PathBuf> = TornKind::ALL
            .into_iter()
            .filter_map(|kind| self.torn_kind_dir(kind).transpose())
            .collect::<io::Result<_>>()?;
        let paths = self
            .unflushed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // A barrier orders only its own device's I/O: a directory elsewhere
        // than the root, where `run` lives, is flushed in full, and so are
        // its files.
        let tier = |dir: &Path| Flush::ordered_if_same_device(dir, &self.root);
        let mut flushed = Flushed::default();
        if !rules.record_last {
            self.write_clean_record(&mut flushed)?;
        }
        let mut root_moved = false;
        for rel in &paths {
            let path = self.path(rel);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
                Ok(meta) if !meta.is_file() => continue,
                Ok(_) => {}
            }
            let parent = path.parent().unwrap_or(&self.root);
            let flush = tier(parent);
            if rules.flush_files && self.flush_in_place(rel, flush)? {
                flushed.files += 1;
                flushed.note(Some(flush), 1);
            }
            root_moved |= parent == self.root;
        }
        // Every directory whose entries moved — the renames above, and every
        // unlink this run made — flushed after them and before the record.
        if rules.flush_dirs {
            let root = self.root.clone();
            for dir in dirs.iter().chain(root_moved.then_some(&root)) {
                let flush = tier(dir);
                self.flush_dir_as(dir, flush)?;
                flushed.dirs += 1;
                flushed.note(Some(flush), 1);
            }
        }
        if rules.record_last {
            self.write_clean_record(&mut flushed)?;
        }
        self.unflushed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|rel| !paths.contains(rel));
        Ok(flushed)
    }

    /// `run` says this boot's run stopped cleanly ([`StateDir::end_run_clean`]'s
    /// last write): its bytes flushed by the barrier before its rename, then
    /// the root flushed in full — the one full flush of a clean stop, which
    /// puts the record, and every write flushed before it on the device, on
    /// the media before it returns.
    fn write_clean_record(&self, flushed: &mut Flushed) -> io::Result<()> {
        use aterm_durable::Flush;
        let clean = LastRun {
            boot: self.kernel_boot(),
            clean: true,
        };
        let mut record = clean.render().into_bytes();
        record.push(b'\n');
        self.replace_bytes_as("run", &record, Some(Flush::Ordered), 0o666)?;
        flushed.note(Some(Flush::Ordered), 1);
        self.flush_dir_as(&self.root, Flush::Durable)?;
        flushed.note(Some(Flush::Durable), 1);
        Ok(())
    }

    /// The highest fleet-halt offset this node has published an `ack` for, so a
    /// reconnect does not re-answer the halt row its `Last` read returns again, nor a
    /// row that arrives both on a `Last` page and on the fleet tail. `None` is
    /// "answered nothing": it re-acks rather than skip a barrier.
    #[must_use]
    pub fn halt_acked(&self) -> Option<u64> {
        self.read("halt-acked").and_then(|s| s.parse().ok())
    }

    /// Record the halt offset this node has answered. Monotone, like `seen`: an
    /// out-of-order replay must not make the node re-answer the history.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_halt_acked(&self, off: u64) -> io::Result<()> {
        if self.halt_acked().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("halt-acked", &off.to_string())
    }

    /// The inbox-group commit a bridge OWED when it exited with no broker
    /// connection to make it on (`Bridge::hand_over_inbox_commit`): every
    /// record at or below it was accounted for — delivered, or refused with
    /// its verdict answered on the bus — so the next bridge commits it before
    /// its group subscription reads the cursor, rather than have the group offer
    /// a burst again to an endpoint that may no longer remember taking it.
    /// `None`: nothing was handed over.
    #[must_use]
    pub fn inbox_owed(&self) -> Option<u64> {
        self.read("inbox-owed").and_then(|s| s.parse().ok())
    }

    /// Hand over the inbox-group commit through `off` ([`Self::inbox_owed`]).
    /// Monotone: the group commit itself never moves back, and a lower offset
    /// handed over after a higher one names records already covered.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_inbox_owed(&self, off: u64) -> io::Result<()> {
        if self.inbox_owed().is_some_and(|have| have >= off) {
            return Ok(());
        }
        self.write("inbox-owed", &off.to_string())
    }

    /// The inbox commit handed over has been made: forget it.
    ///
    /// # Errors
    ///
    /// Any I/O failure other than the file already being gone.
    pub fn clear_inbox_owed(&self) -> io::Result<()> {
        match self.remove_path(&self.path("inbox-owed")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget the halt watermark: it names an offset on a log this node no
    /// longer reads ([`LogWitness::replaced_by`]), and on the log it does, a
    /// halt at or below that number would never be answered.
    ///
    /// # Errors
    ///
    /// Any I/O failure other than the file already being gone.
    pub fn clear_halt_acked(&self) -> io::Result<()> {
        match self.remove_path(&self.path("halt-acked")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Forget every offset this dir holds that the log this node reads now does
    /// not vouch for ([`LogWitness::replaced_by`]), and put each back where a
    /// reader of this log, whose head is `head`, resumes.
    ///
    /// `keep_below` is where the old log CONTINUES on this one
    /// ([`LogWitness::continues`]): `Some(f)` — a moved generation of the same
    /// log, every offset below `f` naming the same record on both — CLAMPS
    /// instead of forgetting: what names a record below `f` is kept, what names
    /// one at or above it (a record the loss took, whose offset this log now
    /// gives to another) goes, and every cursor resumes at the lower of itself
    /// and `f`, so nothing posted after the fork is skipped and nothing below it
    /// re-read. `None` — another log — forgets every offset:
    ///
    /// * `seen/*`: forgotten, so a refill starts at its bounded floor below this
    ///   log's head, not past it, where the old watermark would answer "nothing to
    ///   refill" over rows the endpoint lost. Clamped: one at or above `f` falls
    ///   to `f - 1`, so the refill starts at the fork — the new records there are
    ///   delivered, the ones below it are not offered again;
    /// * every `topics/*` cursor resumes at `head`, as a `since=head` opt-in made
    ///   now does, not at an old offset that would skip this log's broadcasts or
    ///   replay a longer log's history as new mail. Clamped: at `min(next, f)`;
    /// * `unhandled/*`: forgotten: a refill floor names a record of the old log,
    ///   and rewinding a cursor to it would offer this log's history as new mail.
    ///   Clamped: only the floors at or above `f` go;
    /// * `halt-acked` goes ALWAYS, clamped or not, so a halt here is answered,
    ///   never skipped. It stores the HALT ROW's offset, which can sit below `f`
    ///   while the ack it stands for sat above it and was lost: a remote bridge
    ///   would never re-ack. Clearing costs one redundant ack row;
    /// * `inbox-owed`: forgotten: committed on this log, it would carry the group
    ///   past records nobody delivered. Clamped: kept below `f`, where the records
    ///   it covers are the same ones;
    /// * `acked` keeps one member, `head - 1`, clamped or not: the self-lane
    ///   window then starts at this log's head, so a record of this node's lane
    ///   already on this log reads "cannot tell" (this node never held an ack for
    ///   it here), not a forgery, and every later record is judged as before.
    ///   Vouching for the old acks below `f` would buy nothing — nothing below the
    ///   fork is offered again — and would put a record a crash left between
    ///   `f` and `head` inside the window, as an accusation;
    /// * `deadlines` and `asked`: forgotten: no reply to an ask made on the old
    ///   log can arrive on this one, and an `expired` here would name another
    ///   record. Clamped: an ask below `f` is still that ask, and kept;
    /// * `keys/` and `sent/` lose only `original-off`: a previous winner's
    ///   position is no evidence about a publication on this log. Clamped: only
    ///   one at or above `f`;
    /// * an `inflight/*` record carrying a `re=` and witnessed on a different known
    ///   log goes — clamped, only one whose `re=` is at or above ITS OWN fork, and
    ///   one below it is re-witnessed onto `log`, durably, where the attach then
    ///   replays it — returned as `(sequence, face)` for the attach to report, and
    ///   dropped durably ([`StateDir::drop_in_flight`]'s reason: the attach says
    ///   so, and a receipt goes `unacked`). An entry's own fork is where `log`
    ///   continues the log IT was witnessed on, read off `lineage`
    ///   (`LogIdentity::continues`), and never above `f`: an entry a power cut
    ///   rolled back to an older generation than the one `f` is measured from
    ///   parts from this log at the LOWEST fork of the generations between, and
    ///   `f` alone would keep a `re=` that names another record here. With no
    ///   `lineage`, `f` is every entry's. One with an unknown witness or no `re=`
    ///   stays: absence is not proof that a pending body belongs to another log.
    ///   That is a body this node DECIDED to publish and holds no proof of, so it
    ///   is published on this log — which is why an entry ends in the step that
    ///   writes its record's offset to `acked`, and never outlives the record's
    ///   delivery: one kept past it would publish a delivered record here a second
    ///   time.
    ///
    /// Node-scoped facts stay (`node`, `inc`, `seq`, `pins`, `acks/`, and the
    /// sequences, identities and key ownership in `sent/` and `keys/`), and so do
    /// the halts this node holds in force (`halts`): only their human's own row
    /// lifts them, never the absence of one.
    ///
    /// Idempotent — a clamp applied twice clamps to the same `f` — and run
    /// before the witness is re-based ([`StateDir::set_log_witness`]): a crash in
    /// between re-judges the log at the next attach and forgets (or clamps)
    /// again.
    ///
    /// # Errors
    ///
    /// The first I/O failure, including an in-flight journal that cannot be read
    /// whole. Everything is read before the first change; what was forgotten before
    /// a later write failure stays forgotten. The caller must have the endpoint's
    /// acknowledgement of the new log (`Bridge::tell_endpoint_log`) before this
    /// reset may drop an old referenced publication.
    pub fn forget_log_offsets(
        &self,
        head: u64,
        log: Option<astream_broker::LogIdentity>,
        keep_below: Option<u64>,
        lineage: Option<&astream_broker::Lineage>,
    ) -> io::Result<Vec<(u64, String)>> {
        // A fork above the head cannot be: the log grew from it. Held to the
        // head, so a cursor never resumes past what this log holds.
        let keep_below = keep_below.map(|f| f.min(head));
        // Whether an offset of the old log still names its record here.
        let kept = |off: u64| keep_below.is_some_and(|f| off < f);
        // Enumerate and read everything before the first removal. An absent
        // directory is empty; a failed listing, entry or topic read is
        // uncertainty and must not let the witness move onto the new log.
        let entries = |name: &str| -> io::Result<Vec<std::fs::DirEntry>> {
            let path = self.path(name);
            match std::fs::read_dir(&path) {
                Ok(dir) => dir.collect(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(&path) {
                        Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                        Err(other) => Err(other),
                        Ok(_) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        };
        // `seen/*`: removed, or (clamped) rewritten at the fork. A watermark
        // that does not parse names nothing anyone can trust, and goes.
        let mut seen_removed = Vec::new();
        let mut seen_clamped = Vec::new();
        for entry in entries("seen")? {
            let name = entry.file_name();
            let sid = name.to_str().filter(|n| !n.contains('.'));
            let off = sid.and_then(|sid| self.seen_off(sid));
            match (sid, off, keep_below) {
                (Some(_), Some(off), Some(_)) if kept(off) => {}
                (Some(sid), Some(_), Some(f)) if f > 0 => {
                    seen_clamped.push((sid.to_string(), f - 1));
                }
                _ => seen_removed.push(entry.path()),
            }
        }
        // `unhandled/*`: removed, or (clamped) the floors below the fork kept.
        let mut unhandled_removed = Vec::new();
        let mut unhandled_kept = Vec::new();
        for entry in entries("unhandled")? {
            let name = entry.file_name();
            let floors = match name.to_str().filter(|n| !n.contains('.')) {
                Some(sid) => {
                    // Uncertainty is not permission to drop a retained refill
                    // floor, including when the caller must forget this log.
                    let floors = self.topic_floors(sid)?;
                    keep_below.map(|_| (sid.to_string(), floors))
                }
                None => None,
            };
            match floors {
                Some((sid, floors)) => {
                    let below: BTreeMap<String, u64> =
                        floors.into_iter().filter(|(_, off)| kept(*off)).collect();
                    unhandled_kept.push((sid, below));
                }
                None => unhandled_removed.push(entry.path()),
            }
        }
        let mut topics = Vec::new();
        for entry in entries("topics")? {
            let name = entry.file_name();
            let Some(sid) = name.to_str().filter(|n| !n.contains('.')) else {
                continue;
            };
            let body = std::fs::read_to_string(entry.path())?;
            let rebased: BTreeMap<String, TopicCursor> = self
                .parse_topics(sid, &body)
                .into_iter()
                .map(|(topic, cursor)| {
                    let next = match keep_below {
                        Some(f) => cursor.next.min(f),
                        None => head,
                    };
                    (topic, TopicCursor { next })
                })
                .collect();
            topics.push((sid.to_string(), rebased));
        }
        let publication_resets = self.publication_offset_resets(keep_below)?;
        let in_flight = self.in_flight()?;
        let owed = self.inbox_owed();
        let deadlines: Vec<Deadline> = self
            .deadlines()
            .into_iter()
            .filter(|d| kept(d.off))
            .collect();
        let asked: Vec<Asked> = self.asked().into_iter().filter(|a| kept(a.off)).collect();
        for path in seen_removed.into_iter().chain(unhandled_removed) {
            match self.remove_path(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        for (sid, off) in seen_clamped {
            // Not `set_seen_off`, which only rises: the clamp moves it down.
            self.write(&format!("seen/{sid}"), &off.to_string())?;
        }
        for (sid, floors) in unhandled_kept {
            self.set_topic_floors(&sid, &floors)?;
        }
        for (sid, rebased) in topics {
            self.set_topics(&sid, &rebased)?;
        }
        for (name, text) in publication_resets {
            self.write(&name, &text)?;
        }
        self.set_self_acked(&head.checked_sub(1).into_iter().collect(), 1)?;
        self.clear_halt_acked()?;
        if !owed.is_some_and(kept) {
            self.clear_inbox_owed()?;
        }
        self.set_deadlines(&deadlines)?;
        self.set_asked(&asked)?;
        let mut dropped = Vec::new();
        let mut moved = false;
        for (seq, entry) in in_flight {
            let (Some(was), Some(now)) = (entry.log, log) else {
                continue;
            };
            let re = crate::body::Body::decode(&entry.body).0.re;
            if was == now || re.is_none() {
                continue;
            }
            // THE ENTRY'S OWN FORK: where this log continues the one the entry
            // was witnessed on. `f` is measured from the log the bridge last
            // read; an entry from an older generation — a drop or a re-witness
            // that a power cut rolled back — parts from this log at the lowest
            // fork of every generation between, which `f` alone overstates.
            let fork = match lineage {
                Some(lineage) => keep_below.and_then(|f| Some(f.min(now.continues(was, lineage)?))),
                None => keep_below,
            };
            // A REPLY BELOW THE FORK IS RE-WITNESSED ONTO THIS LOG: its `re=`
            // names the same record here, and the journal's witness is what
            // the attach's replay goes by ([`crate::bridge`]'s
            // `settle_in_flight`, which replays a reference only on the log it
            // names). Kept under the old witness it would survive this clamp
            // only to be dropped there, unpublished. Rewritten durably before
            // the bridge's own witness moves, so neither a crash nor a power
            // cut in between finds it on the old log once the witness says the
            // new one: the clamp stays idempotent.
            if re.is_some_and(|off| fork.is_some_and(|f| off < f)) {
                self.replace(
                    &format!("inflight/{seq}"),
                    &InFlight { log, ..entry }.render(),
                    true,
                    0o600,
                )?;
                continue;
            }
            match self.remove_path(&self.path(&format!("inflight/{seq}"))) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {
                    moved = true;
                    dropped.push((seq, entry.subject));
                }
            }
        }
        if moved {
            self.flush_dir(&self.path("inflight"))?;
        }
        Ok(dropped)
    }

    /// What this node knows of the bus log it reads ([`LogWitness`]), or
    /// `None` before its first attach recorded anything.
    #[must_use]
    pub fn log_witness(&self) -> Option<LogWitness> {
        self.read("bus-log").and_then(|s| LogWitness::parse(&s))
    }

    /// Record what this node knows of the bus log it reads. Not monotone: the attach
    /// that finds the log replaced re-bases it onto the new log.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_log_witness(&self, witness: &LogWitness) -> io::Result<()> {
        self.write("bus-log", &witness.render())
    }

    /// The known log identity this node saw rewind (reuse its offsets), which stays
    /// refused. Only a missing file is no refusal: an unreadable or damaged one is
    /// an error, never read as absent.
    pub(crate) fn rewound_log(&self) -> io::Result<Option<astream_broker::LogIdentity>> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;

        let path = self.path("rewound-log");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the rewound log fact is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::enable::LOCK_OPEN_FLAGS)
            .open(path)?;
        let mut text = String::new();
        file.take(128).read_to_string(&mut text)?;
        if text.len() < 128 {
            if let Some(log) = astream_broker::LogIdentity::parse(text.trim()) {
                return Ok(Some(log));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the rewound log fact has no valid identity",
        ))
    }

    /// Record a rewound identity durably, so its refusal outlives a bridge or host
    /// restart.
    pub(crate) fn set_rewound_log(&self, log: astream_broker::LogIdentity) -> io::Result<()> {
        self.write_durable("rewound-log", &log.to_string())
    }

    /// Release the refusal, once the caller has fenced the endpoint on a different
    /// known log. The new witness is flushed before the refusal is removed, so a
    /// restart cannot find the old witness without its refusal.
    pub(crate) fn clear_rewound_log(&self, witness: &LogWitness) -> io::Result<()> {
        self.write_durable("bus-log", &witness.render())?;
        match self.remove_path(&self.path("rewound-log")) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        self.flush_dir(&self.root)
    }

    /// The fleet halts of `fleet` this node holds in force: `human → StandingHalt`.
    /// The bus is the authority (every attach re-reads `Last{/f/<F>/fleet/*/halt}`);
    /// this stands in for a row the bus has lost: a human whose halt this node held
    /// and whose row is not on the face it reads now is still halting, until that
    /// human's own row on this log says otherwise.
    ///
    /// A line for another fleet is skipped; a damaged one is reported and read as
    /// nothing, leaving the halts to the bus alone.
    #[must_use]
    pub fn standing_halts(&self, fleet: &str) -> BTreeMap<String, StandingHalt> {
        let Some(body) = self.read("halts") else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for line in body.lines() {
            match StandingHalt::parse(line) {
                Some((f, human, halt)) if f == fleet => {
                    out.insert(human, halt);
                }
                Some(_) => {}
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a standing halt; it is not kept",
                    self.path("halts").display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Replace `fleet`'s standing halts ([`StateDir::standing_halts`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_standing_halts(
        &self,
        fleet: &str,
        halts: &BTreeMap<String, StandingHalt>,
    ) -> io::Result<()> {
        let lines: Vec<String> = halts
            .iter()
            .map(|(human, halt)| halt.render(fleet, human))
            .collect();
        // Durable: the halt is a human's, and the row this file stands in for
        // may be on no log this node can still read.
        self.write_durable("halts", &lines.join("\n"))
    }

    /// The TOFU `sid → node` pins (§6.1): the first node seen advertising a sid owns
    /// it, and a second node claiming it is a conflict, never a route.
    #[must_use]
    pub fn pins(&self) -> BTreeMap<String, String> {
        self.pin_lines()
            .into_iter()
            .filter_map(|l| {
                l.split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            })
            .collect()
    }

    /// The pin file's lines in first-sight order, the order [`PINS_KEEP`] evicts in.
    fn pin_lines(&self) -> Vec<String> {
        self.read("pins")
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pin `sid` to `node` unless it is pinned already. Answers the node it is pinned
    /// to, which is not necessarily `node`: a caller that assumed it was would turn a
    /// conflict into a route.
    ///
    /// Bounded at [`PINS_KEEP`], oldest first sight out; a new pin is appended, so the
    /// file stays in first-sight order.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn pin(&self, sid: &str, node: &str) -> io::Result<String> {
        let mut lines = self.pin_lines();
        if let Some(have) = lines
            .iter()
            .filter_map(|l| l.split_once(' '))
            .find(|(s, _)| *s == sid)
            .map(|(_, n)| n.to_string())
        {
            return Ok(have);
        }
        lines.push(format!("{sid} {node}"));
        let over = lines.len().saturating_sub(PINS_KEEP);
        // Durable: a remote sid outlives this host's reboot, and a first sight
        // the boot took back is one a second claimant may then win.
        self.write_durable("pins", &lines[over..].join("\n"))?;
        Ok(node.to_string())
    }

    /// One session's broadcast cursors: `topic → the next offset it may deliver from`.
    ///
    /// The opt-in lives in aterm, the cursor here. The session owns the set (`topic
    /// add`/`topic drop`) and answers `since=head|@<off>`, an intent, not a position;
    /// the bridge resolves it once, against the broker's head when it first learns
    /// the entry, and writes the offset here, so a restarted bridge resumes each topic
    /// instead of re-reading `head` and losing what was published while it was down.
    /// One file per session ([`StateDir::forget_topics`]).
    #[must_use]
    pub fn topics(&self, sid: &str) -> BTreeMap<String, TopicCursor> {
        self.read(&format!("topics/{sid}"))
            .map_or_else(BTreeMap::new, |body| self.parse_topics(sid, &body))
    }

    /// The cursor parse shared by [`Self::topics`] and [`Self::forget_log_offsets`].
    fn parse_topics(&self, sid: &str, body: &str) -> BTreeMap<String, TopicCursor> {
        let mut out = BTreeMap::new();
        for line in body.trim().lines() {
            match TopicCursor::parse(line) {
                Some((topic, cursor)) => {
                    out.insert(topic, cursor);
                }
                // Reported: a dropped cursor is a dropped delivery guarantee,
                // and the session has no other way to learn it.
                None => eprintln!(
                    "aterm-link: {}: `{}` is not a broadcast cursor; that topic \
                     resumes from the bus head; what it missed is lost",
                    self.path(&format!("topics/{sid}")).display(),
                    safe_line(line)
                ),
            }
        }
        out
    }

    /// Replace one session's broadcast cursors.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_topics(&self, sid: &str, cursors: &BTreeMap<String, TopicCursor>) -> io::Result<()> {
        let body: Vec<String> = cursors
            .iter()
            .map(|(t, c)| format!("{t} {}", c.next))
            .collect();
        self.write(&format!("topics/{sid}"), &body.join("\n"))
    }

    /// Forget the broadcast cursors of a session gone from the roster, and the
    /// refill floors that rest on them ([`Self::topic_floors`]).
    pub fn forget_topics(&self, sid: &str) {
        let _ = self.remove_path(&self.path(&format!("topics/{sid}")));
        let _ = self.remove_path(&self.path(&format!("unhandled/{sid}")));
    }

    /// One session's broadcast refill floors: per topic, the lowest offset of a
    /// broadcast its endpoint took as a new row (`deliver` answered `OK <id>`) that
    /// `inbox seen` has not yet passed. A topic with nothing unhandled has no line.
    ///
    /// The topic cursor ([`Self::topics`]) moves past a broadcast on the endpoint's
    /// `OK`, since a cursor that waited for the agent would re-offer every unread
    /// broadcast on every bridge restart. But the ring holding the row lives in
    /// aterm's memory, and a relaunch that keeps the sid (a seamless update, a PTY
    /// keeper's recovery) starts an empty one. A bridge that loads a session's
    /// cursors from disk rewinds each to its floor here, and the catch-up offers the
    /// stretch again: a fresh ring takes the rows as new, and a ring that still holds
    /// them (a bridge-only restart) dedups them on `off=`.
    ///
    /// Written before the cursor moves past the row, so a kill between the two
    /// leaves a floor at or above the cursor, which rewinds nothing. A missing file
    /// is empty; an unreadable or damaged one is uncertainty and never authorizes
    /// advancing past an unread row.
    ///
    /// # Errors
    ///
    /// An unreadable directory or entry, a non-regular entry (a dangling symlink
    /// included), an empty file, or any line that is not one unique topic and offset.
    pub fn topic_floors(&self, sid: &str) -> io::Result<BTreeMap<String, u64>> {
        let dir = self.path("unhandled");
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor parent is not a directory",
                ));
            }
            Ok(_) => {}
        }
        let path = dir.join(sid);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e),
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "refill floor is not a regular file",
                ));
            }
            Ok(_) => {}
        }
        parse_topic_floors(&path, &std::fs::read_to_string(&path)?)
    }

    /// Replace one session's broadcast refill floors ([`Self::topic_floors`]);
    /// an empty set removes the file.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_topic_floors(&self, sid: &str, floors: &BTreeMap<String, u64>) -> io::Result<()> {
        let name = format!("unhandled/{sid}");
        if floors.is_empty() {
            return match self.remove_path(&self.path(&name)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let body: Vec<String> = floors.iter().map(|(t, off)| format!("{t} {off}")).collect();
        self.write(&name, &body.join("\n"))
    }
    /// The producer sequence this bridge reserved for one outbound post, and whether
    /// a `post key=` entry an earlier post reserved chose it ([`StateDir::key_seq`]).
    ///
    /// This makes the outbound half exactly once. `outbox` is a peek, so a bridge that
    /// dies between `Publish` and `outbox sent` re-reads the post on restart, and a
    /// fresh sequence would be a second record (the broker dedups on `(producer_id,
    /// producer_seq)`). At the pinned sequence the retry is byte-identical and the
    /// broker answers `deduped` with the original offset, the one the sender's
    /// `--wait` was promised. The `key` flag makes the retirement say `dup=1`, after a
    /// relaunch too.
    #[must_use]
    pub fn post_seq(&self, sid: &str, id: u64) -> Option<(u64, bool)> {
        self.post_pin(sid, id)
            .ok()
            .flatten()
            .map(|pin| (pin.seq, pin.via_key))
    }

    /// Reserve a sequence for one outbound post before it is published. `via_key`
    /// says a `post key=` entry chose it ([`StateDir::post_seq`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_post_seq(&self, sid: &str, id: u64, seq: u64, via_key: bool) -> io::Result<()> {
        let line = if via_key {
            format!("{seq} key")
        } else {
            seq.to_string()
        };
        self.write(&format!("sent/{sid}.{id}"), &line)
    }

    /// Forget a retired post's sequence; called only once the endpoint has
    /// acknowledged the retirement.
    pub fn clear_post_seq(&self, sid: &str, id: u64) {
        let _ = self.unlink(&format!("sent/{sid}.{id}"));
    }

    /// The producer sequence this bridge reserved for one owed receipt, if any: the
    /// `receipt sid=<sid> rid=<rid> …` line an endpoint's `outbox` peek lists from a
    /// session's `inbox seen <id> handled|refused|deferred` until the bridge retires
    /// it (R8).
    ///
    /// [`StateDir::post_seq`]'s rule for receipts: republished after a crash at this
    /// sequence, the `ack` is `deduped` rather than a second one on the sender's lane.
    #[must_use]
    pub fn receipt_seq(&self, sid: &str, rid: u64) -> Option<u64> {
        self.receipt_pin(sid, rid).ok().flatten().map(|pin| pin.seq)
    }

    /// Reserve a sequence for one owed receipt before it is published
    /// ([`StateDir::receipt_seq`]).
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_receipt_seq(&self, sid: &str, rid: u64, seq: u64) -> io::Result<()> {
        self.write(&format!("acks/{sid}.{rid}"), &seq.to_string())
    }

    /// Forget a retired receipt's sequence. Called only after the endpoint has
    /// taken the retirement.
    pub fn clear_receipt_seq(&self, sid: &str, rid: u64) {
        let _ = self.remove_path(&self.path(&format!("acks/{sid}.{rid}")));
    }

    /// The producer sequence a session's `post key=<key>` reserved, if any post under
    /// that key did.
    ///
    /// This makes `post key=` exactly once (R7, §6.5). [`StateDir::post_seq`] pins a
    /// sequence to a post id, which survives a bridge crash but not a re-post: after
    /// `ERR timeout` the sender cannot tell landed from lost, and posting again mints
    /// a new id. This table pins the sequence to the caller's key, so a re-post
    /// publishes at the same `(producer_id, producer_seq)` and the broker's dedup
    /// (rebuilt from the log on a broker restart) answers `deduped` with the original
    /// offset. The endpoint's `outbox` carries the key on every drain, so a
    /// relaunched bridge reads it again. First wins, like a pin; one file per
    /// session, newest last, bounded at [`KEYS_KEEP`].
    #[must_use]
    pub fn key_seq(&self, sid: &str, key: &str) -> Option<u64> {
        self.key_pin(sid, key).ok().flatten().map(|pin| pin.seq)
    }

    /// The key file's lines in reservation order, the order [`KEYS_KEEP`] evicts in.
    #[cfg(test)]
    fn key_lines(&self, sid: &str) -> Vec<String> {
        self.read(&format!("keys/{sid}"))
            .map(|s| {
                s.lines()
                    .filter(|l| l.contains(' '))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Reserve `seq` for `key` before the publish, unless the key holds one already
    /// (first wins). Answers the sequence the key holds afterwards.
    ///
    /// # Errors
    ///
    /// Any I/O failure; the caller must not publish then.
    pub fn set_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<u64> {
        self.set_key_pin(
            sid,
            key,
            &PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
        )
        .map(|pin| pin.seq)
    }

    /// Forget `key`'s reservation if it is `seq` (that pair only): `seq` turned out
    /// to be another writer's, and a re-post under the key must reserve its own
    /// number, not dedup onto a record that is not its post.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn forget_key_seq(&self, sid: &str, key: &str, seq: u64) -> io::Result<()> {
        if !self.key_pin(sid, key)?.is_some_and(|pin| pin.seq == seq) {
            return Ok(());
        }
        let text = self
            .read_reservation(&format!("keys/{sid}"))?
            .unwrap_or_default();
        let kept: Vec<&str> = text
            .lines()
            .filter(|l| l.split_whitespace().next() != Some(key))
            .collect();
        self.write(&format!("keys/{sid}"), &kept.join("\n"))
    }

    /// Every deadline this node still owes a verdict on, oldest offset first.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.read("deadlines")
            .map(|s| s.lines().filter_map(Deadline::parse).collect())
            .unwrap_or_default()
    }

    /// Every ask this node remembers the recipient of, oldest offset first.
    #[must_use]
    pub fn asked(&self) -> Vec<Asked> {
        self.read("asked")
            .map(|s| s.lines().filter_map(Asked::parse).collect())
            .unwrap_or_default()
    }

    /// Replace the asked table, keeping the newest [`ASKED_KEEP`] by position.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_asked(&self, list: &[Asked]) -> io::Result<()> {
        let over = list.len().saturating_sub(ASKED_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Asked::render).collect();
        self.write("asked", &lines.join("\n"))
    }

    /// Replace the deadline table, keeping the newest [`DEADLINES_KEEP`] by position.
    /// Written whole: it is small and changes once per `dl=` ask and per reply.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn set_deadlines(&self, list: &[Deadline]) -> io::Result<()> {
        let over = list.len().saturating_sub(DEADLINES_KEEP);
        let lines: Vec<String> = list[over..].iter().map(Deadline::render).collect();
        self.write("deadlines", &lines.join("\n"))
    }

    /// The directory itself, for what this type does not model: the `pid` file and
    /// the bridge's fault and test markers.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("atlink-state-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// Incarnation `inc`'s first number, by astream's layout
    /// ([`astream_broker::incarnation_base`]) — never spelled out here, because
    /// the layout is astream's persistence contract and a second transcription
    /// is nothing tied to it.
    fn base(inc: u64) -> u64 {
        astream_broker::incarnation_base(inc).expect("in the layout")
    }

    /// Incarnation `inc`'s will, by astream's layout
    /// ([`astream_broker::will_seq`]).
    fn will(inc: u64) -> u64 {
        astream_broker::will_seq(inc).expect("in the layout")
    }

    /// **EVERY NUMBER THIS DIR HANDS OUT SITS IN ASTREAM'S INCARNATION LAYOUT**
    /// ([`INC_MAX`]'s doc): a block of incarnation `inc` lies strictly between
    /// astream's `incarnation_base(inc)` and `will_seq(inc)` — above everything of
    /// every earlier incarnation, below this one's will — and astream's
    /// `incarnation_of` maps it back to `inc`, which is the incarnation the dir then
    /// answers for, up to the last incarnation the layout holds.
    ///
    /// Negative controls: past [`INC_MAX`], where astream's layout has no space,
    /// nothing is reserved; and a `seq` in astream's reserved ack half, which no
    /// incarnation owns, is a corrupt file that refuses the next take — read as
    /// incarnation 0 (`incarnation_of`'s `None` taken for zero), it would hand out
    /// incarnation 1 again.
    #[test]
    fn every_reserved_sequence_sits_in_astreams_incarnation_layout() {
        let dir = scratch("layout");
        let st = StateDir::open(&dir).expect("open");
        for inc in [0, 1, 7, INC_MAX] {
            let block = st.reserve_sequence_block(inc, 3).expect("a block");
            let base = base(inc);
            let will = will(inc);
            assert!(
                base < *block.start() && *block.end() < will,
                "inc {inc}: {block:?} strictly inside ({base}, {will})"
            );
            assert_eq!(astream_broker::incarnation_of(*block.end()), Some(inc));
            assert_eq!(
                st.incarnation(),
                inc,
                "the dir answers for the block's incarnation"
            );
        }
        assert!(
            st.reserve_sequence_block(INC_MAX + 1, 1).is_err(),
            "past INC_MAX the layout has no space"
        );
        let corrupt = scratch("layout-ack-half");
        let st = StateDir::open(&corrupt).expect("open");
        std::fs::write(
            corrupt.join("seq"),
            (astream_broker::ACK_SEQ_BASE | 5).to_string(),
        )
        .expect("seed a seq in the ack half");
        assert!(
            st.take_incarnation(0).is_err(),
            "a seq no incarnation owns refuses the take; it is never incarnation 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&corrupt);
    }

    /// A BLOCK IS ONE WRITE, AND ITS LAST NUMBER IS ON DISK BEFORE ITS FIRST IS
    /// ANSWERED. The file holds the block's end the moment the call returns, so
    /// a reopen — the next process, after a `kill -9` that used part of the
    /// block — starts above every number the block could have handed out; a
    /// block of one is the old one-number reservation; and a block that would
    /// cross the incarnation's reserved top (the will's own number) is cut
    /// short below it, then refused once nothing is left.
    #[test]
    fn a_sequence_block_is_on_disk_before_use_and_never_reaches_the_wills_number() {
        let dir = scratch("seqblock");
        let st = StateDir::open(&dir).expect("open");
        let block = st.reserve_sequence_block(3, 64).expect("a block");
        assert_eq!(block, base(3) + 1..=base(3) + 64);
        assert_eq!(
            st.sequence(),
            *block.end(),
            "the END is what the file holds"
        );
        let reopened = StateDir::open(&dir).expect("reopen");
        assert_eq!(
            reopened.reserve_next_sequence(3).expect("one more"),
            block.end() + 1,
            "a reopen starts above the whole block, however little of it was used"
        );
        // A later incarnation starts above its own base, not above the file.
        assert_eq!(
            reopened.reserve_sequence_block(4, 2).expect("inc 4"),
            base(4) + 1..=base(4) + 2
        );
        // Near the top: cut short below the will's number, then refused.
        std::fs::write(dir.join("seq"), (will(5) - 3).to_string()).expect("seed near the top");
        let short = reopened
            .reserve_sequence_block(5, 64)
            .expect("a short block");
        assert_eq!(short, will(5) - 2..=will(5) - 1);
        assert!(
            reopened.reserve_sequence_block(5, 64).is_err(),
            "nothing below the will's number is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory flush tolerates a directory already gone and one this process
    /// may write but not open (its file's bytes are flushed), and refuses a path
    /// no directory can be at.
    #[test]
    fn a_directory_flush_tolerates_a_gone_directory_and_refuses_a_broken_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("syncdir");
        let locked = dir.join("locked");
        std::fs::create_dir_all(&locked).expect("scratch");
        assert!(sync_dir(&dir).is_ok());
        assert!(sync_dir(&dir.join("gone")).is_ok());
        let mode = |m| std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(m));
        mode(0o300).expect("write but not read");
        let opened = sync_dir(&locked);
        mode(0o700).expect("readable again");
        assert!(opened.is_ok());
        std::fs::write(dir.join("file"), b"").expect("a file");
        assert_eq!(
            sync_dir(&dir.join("file").join("dir")).map_err(|e| e.kind()),
            Err(io::ErrorKind::NotADirectory)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The node id (the node's mail address) is minted once and read back on reopen.
    #[test]
    fn the_node_id_is_minted_once_and_survives_reopen() {
        let dir = scratch("node");
        let first = StateDir::open(&dir)
            .expect("open")
            .node_id(|| "n-0123456789abcdef".to_string())
            .expect("mint");
        let second = StateDir::open(&dir)
            .expect("reopen")
            .node_id(|| panic!("must not re-mint"))
            .expect("read back");
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cursor line is `<topic> <next>` (a third number is read past); any other line
    /// is dropped and reported while its neighbours survive.
    #[test]
    fn a_topic_cursor_line_parses_or_is_dropped_beside_its_neighbours() {
        let c = |next| TopicCursor { next };
        assert_eq!(
            TopicCursor::parse("build.failed 42"),
            Some(("build.failed".to_string(), c(42)))
        );
        assert_eq!(
            TopicCursor::parse("old.topic 3 7"),
            Some(("old.topic".to_string(), c(3)))
        );
        for bad in [
            "",
            "onlytopic",
            "t x",
            "t 1 y",
            "t 1 2 3",
            "t +5",
            "T 5",
            "a/b 5",
            "a* 5",
            "> 5",
        ] {
            assert_eq!(TopicCursor::parse(bad), None, "{bad:?}");
        }
        let dir = scratch("topics");
        let st = StateDir::open(&dir).expect("open");
        let mut good = BTreeMap::new();
        good.insert("a".to_string(), c(10));
        good.insert("b".to_string(), c(20));
        st.set_topics("s-x", &good).expect("write");
        assert_eq!(st.topics("s-x"), good);
        std::fs::write(
            st.path("topics/s-x"),
            "a 10\nthis is not a cursor \x1b[2J\nb 20\n",
        )
        .expect("corrupt one line");
        assert_eq!(st.topics("s-x"), good, "the neighbours survive");
        st.forget_topics("s-x");
        assert!(st.topics("s-x").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session's broadcast refill floors round-trip, an empty set removes
    /// the file, a damaged line refuses the whole read until repaired, and a
    /// floor goes with its session's cursors and with the log its offsets index.
    #[test]
    fn a_broadcast_refill_floor_round_trips_and_goes_with_its_cursors_and_its_log() {
        let dir = scratch("unhandled");
        let st = StateDir::open(&dir).expect("open");
        let mut floors = BTreeMap::new();
        floors.insert("a".to_string(), 10);
        floors.insert("b.c".to_string(), 7);
        st.set_topic_floors("s-x", &floors).expect("write");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);
        std::fs::write(
            st.path("unhandled/s-x"),
            "a 10\nb.c +7\nB 3\nb.c 7\nd 1 2\n",
        )
        .expect("damage some lines");
        assert!(
            st.topic_floors("s-x").is_err(),
            "damaged state is not an empty floor"
        );
        st.set_topic_floors("s-x", &floors).expect("repair");
        assert_eq!(st.topic_floors("s-x").unwrap(), floors);

        st.set_topic_floors("s-x", &BTreeMap::new())
            .expect("an empty set");
        assert!(
            !st.path("unhandled/s-x").exists(),
            "an empty set is no file"
        );
        assert!(st.set_topic_floors("s-x", &BTreeMap::new()).is_ok());

        st.set_topic_floors("s-x", &floors).expect("write");
        st.forget_topics("s-x");
        assert!(
            st.topic_floors("s-x").unwrap().is_empty(),
            "a departed session's floors go"
        );

        st.set_topic_floors("s-y", &floors).expect("write");
        st.forget_log_offsets(100, None, None, None)
            .expect("re-base");
        assert!(
            st.topic_floors("s-y").unwrap().is_empty(),
            "a floor names a record of the old log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `seen` watermark never moves backwards.
    #[test]
    fn the_seen_watermark_never_goes_backwards() {
        let dir = scratch("seen");
        let st = StateDir::open(&dir).expect("open");
        st.set_seen_off("s-a", 42).expect("write");
        st.set_seen_off("s-a", 7).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(42));
        st.set_seen_off("s-a", 43).expect("write");
        assert_eq!(st.seen_off("s-a"), Some(43));
        assert_eq!(st.seen_off("s-unknown"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The self-lane acked set is a set, not a watermark, and bounded; the halt-ack
    /// watermark never moves backwards.
    #[test]
    fn the_self_acked_set_is_a_set_and_is_bounded() {
        let dir = scratch("acked");
        let st = StateDir::open(&dir).expect("open");
        let mut set = std::collections::BTreeSet::new();
        for off in [10u64, 30, 20] {
            set.insert(off);
            st.set_self_acked(&set, 3).expect("write");
        }
        assert_eq!(st.self_acked(), [10, 20, 30].into_iter().collect());
        assert_eq!(st.halt_acked(), None);
        st.set_halt_acked(91).expect("write");
        st.set_halt_acked(7).expect("write");
        assert_eq!(st.halt_acked(), Some(91));
        st.set_halt_acked(92).expect("write");
        assert_eq!(st.halt_acked(), Some(92));
        set.insert(40);
        st.set_self_acked(&set, 3).expect("write");
        assert_eq!(
            st.self_acked(),
            [20, 30, 40].into_iter().collect(),
            "the oldest is dropped, and 10 is now unknown rather than proven ours"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pin is first-wins and answers the pinned node, not the asked-for one.
    #[test]
    fn a_pin_is_first_wins_and_reports_the_pinned_node() {
        let dir = scratch("pins");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.pin("s-a", "n-one").expect("pin"), "n-one");
        assert_eq!(
            st.pin("s-a", "n-two").expect("pin"),
            "n-one",
            "a second node claiming a pinned sid is a conflict, not a re-pin"
        );
        assert_eq!(
            StateDir::open(&dir).expect("reopen").pins().get("s-a"),
            Some(&"n-one".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A post's sequence is pinned to the post and survives a reopen
    /// ([`StateDir::post_seq`]).
    #[test]
    fn a_posts_sequence_is_pinned_to_the_post_and_survives_a_reopen() {
        let dir = scratch("postseq");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.post_seq("s-a", 1), None);
        st.set_post_seq("s-a", 1, 0x0000_0002_0000_0007, false)
            .expect("reserve");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 1),
            Some((0x0000_0002_0000_0007, false))
        );
        // Another post has its own, and a retirement forgets one.
        assert_eq!(st.post_seq("s-a", 2), None);
        st.clear_post_seq("s-a", 1);
        assert_eq!(st.post_seq("s-a", 1), None);
        // The `key=` marker survives too, so a relaunched bridge's retirement still
        // says `dup=1`.
        st.set_post_seq("s-a", 3, 0x0000_0002_0000_0009, true)
            .expect("reserve via key");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").post_seq("s-a", 3),
            Some((0x0000_0002_0000_0009, true))
        );
        // A file without the marker reads as a plain pin.
        std::fs::write(dir.join("sent/s-a.4"), "17\n").expect("write");
        assert_eq!(st.post_seq("s-a", 4), Some((17, false)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An owed receipt's sequence is pinned to `(sid, rid)`, survives a reopen, and
    /// is forgotten at retirement.
    #[test]
    fn a_receipt_pins_its_sequence_until_it_is_retired() {
        let dir = scratch("acks");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.receipt_seq("s-a", 1), None);
        st.set_receipt_seq("s-a", 1, 77).expect("reserve");
        st.set_receipt_seq("s-a", 2, 78).expect("reserve");
        st.set_receipt_seq("s-b", 1, 79).expect("reserve");
        let again = StateDir::open(&dir).expect("reopen");
        assert_eq!(again.receipt_seq("s-a", 1), Some(77), "survives a reopen");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78));
        assert_eq!(again.receipt_seq("s-b", 1), Some(79), "per session");
        again.clear_receipt_seq("s-a", 1);
        assert_eq!(again.receipt_seq("s-a", 1), None, "retired");
        assert_eq!(again.receipt_seq("s-a", 2), Some(78), "the others stand");
        assert!(
            std::fs::read_to_string(dir.join("acks/s-a.2"))
                .expect("the pin file")
                .trim()
                .parse::<u64>()
                .is_ok(),
            "a pin holds the number and nothing else"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publication_identity_survives_pins_and_malformed_reservations_are_not_absent() {
        let dir = scratch("identities");
        let st = StateDir::open(&dir).unwrap();
        let pin = PublishPin {
            seq: 17,
            via_key: true,
            identity: Some(PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: Some(PublishIdentity {
                subject: "/f/f/in/n-d/s-e/n-c/note".into(),
                digest: "b".repeat(64),
            }),
            original_off: Some(0),
            owner: Some(1),
        };
        st.set_post_pin("s-a", 1, &pin).unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().post_pin("s-a", 1).unwrap(),
            Some(pin.clone())
        );
        for damaged in [
            "",
            "17 identity /f/f/x nope",
            "17 identity /f/f/x",
            "17 garbage",
            "17 owner 1 owner 2",
            "17 original-off nope",
        ] {
            st.write("sent/s-a.1", damaged).unwrap();
            assert_eq!(
                st.post_pin("s-a", 1).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for damaged in [
            "key-only",
            "key-only garbage",
            "key-only 17 identity /f/f/x bad",
        ] {
            st.write("keys/s-a", damaged).unwrap();
            assert_eq!(
                st.key_pin("s-a", "key-only").unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        st.write("keys/s-a", "key-only\t17").unwrap();
        assert_eq!(st.key_pin("s-a", "key-only").unwrap().unwrap().seq, 17);
        st.forget_key_seq("s-a", "key-only", 17).unwrap();
        assert!(st.key_pin("s-a", "key-only").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The in-flight journal round-trips the exact bytes, owner-only, in sequence
    /// order; a damaged entry blocks recovery rather than reading as absent or being
    /// discarded, and a writer's temp beside the entries is not one.
    #[test]
    fn in_flight_entries_round_trip_in_order_and_a_damaged_one_is_not_absent() {
        let dir = scratch("inflight");
        let st = StateDir::open(&dir).unwrap();
        assert!(
            st.in_flight().unwrap().is_empty(),
            "no dir is nothing in flight"
        );
        let entry = |seq: u64, body: &[u8]| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: Some(PublishIdentity {
                    subject: "/f/f/in/n-a/s-b/n-a/note".into(),
                    digest: "c".repeat(64),
                }),
                original: None,
                original_off: None,
                owner: Some(3),
            },
            subject: "/f/f/in/n-a/s-b/n-a/note".into(),
            body: body.to_vec(),
            log: None,
        };
        let late = InFlight {
            log: Some(astream_broker::LogIdentity {
                id: [0xaa; 16],
                generation: 2,
            }),
            ..entry(0x0000_0002_0000_0009, b"v=1 t=7 text=two%0Alines\n\x00\xff")
        };
        let early = entry(0x0000_0002_0000_0002, b"");
        st.set_in_flight(&late).unwrap();
        st.set_in_flight(&early).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(dir.join(format!("inflight/{}", late.pin.seq)))
                .unwrap()
                .permissions(),
        );
        assert_eq!(
            mode & 0o077,
            0,
            "a record's bytes are the owner's: {mode:o}"
        );
        std::fs::write(dir.join("inflight/.9.tmp.1.abcd"), "junk").unwrap();
        assert_eq!(
            StateDir::open(&dir).unwrap().in_flight().unwrap(),
            vec![(early.pin.seq, early.clone()), (late.pin.seq, late.clone())]
        );
        for damaged in [
            "",
            "8589934601",
            "8589934601\n/f/f/x",
            "8589934601\n/f/f/x\nnot-hex",
            "8589934601\n/f/f/*\n00",
            "8589934601\n/f/f/x\n00\nextra",
            "8589934601\n/f/f/x\n00\nlog=",
            "8589934601\n/f/f/x\n00\nlog=AA.0",
            "8589934601\n/f/f/x\n00\nlog=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.0\nextra",
            "8589934600\n/f/f/x\n00",
        ] {
            st.write(&format!("inflight/{}", late.pin.seq), damaged)
                .unwrap();
            assert_eq!(
                st.in_flight().unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{damaged:?} must block recovery"
            );
            assert!(
                dir.join(format!("inflight/{}", late.pin.seq)).exists(),
                "uncertain publication fate must not discard its journal"
            );
        }
        st.clear_in_flight(late.pin.seq);
        st.clear_in_flight(early.pin.seq);
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE ACKED LOG IS APPENDED, READS AN OLDER BRIDGE'S FILE, AND NEVER
    /// TAKES A TORN TAIL FOR AN OFFSET.** Appends land after the last
    /// terminator; the one line an older bridge wrote reads whole, newline and
    /// all, but is not appendable, so the next write replaces it; a number
    /// without its comma is a prefix of some other offset and is dropped, and
    /// the file is then not appendable either; a compaction keeps the newest
    /// set in the format an older bridge's `split(',')` reads the same; and a
    /// link planted at the name is refused, not written through.
    #[test]
    fn the_acked_log_is_appended_reads_an_older_file_and_never_takes_a_torn_tail() {
        let dir = scratch("acked-log");
        let st = StateDir::open(&dir).unwrap();
        let set = |offs: &[u64]| {
            offs.iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                appendable: true,
                ..AckedLog::default()
            },
            "no file is the empty log, appendable"
        );
        for off in [5, 7, 9, 7] {
            st.append_self_acked(off).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("acked")).unwrap(),
            "5,7,9,7,"
        );
        assert_eq!(
            st.self_acked_log(),
            AckedLog {
                offsets: set(&[5, 7, 9]),
                entries: 4,
                appendable: true,
            }
        );
        let older = |text: &str| {
            std::fs::write(dir.join("acked"), text).unwrap();
            st.self_acked_log()
        };
        assert_eq!(
            older("1,2,3\n"),
            AckedLog {
                offsets: set(&[1, 2, 3]),
                entries: 3,
                appendable: false,
            },
            "an older bridge's line"
        );
        assert_eq!(
            older("5,7,45"),
            AckedLog {
                offsets: set(&[5, 7]),
                entries: 2,
                appendable: false,
            },
            "45 may be the torn head of 456: it is no offset of ours"
        );
        assert_eq!(older("").offsets, set(&[]));
        st.set_self_acked(&set(&[20, 30, 40]), 2).unwrap();
        let text = std::fs::read_to_string(dir.join("acked")).unwrap();
        assert_eq!(text, "30,40,", "the newest two, each ended by its comma");
        let older_reader: std::collections::BTreeSet<u64> = text
            .trim()
            .split(',')
            .filter_map(|n| n.parse().ok())
            .collect();
        assert_eq!(
            older_reader,
            set(&[30, 40]),
            "an older bridge reads the same set"
        );
        assert!(st.self_acked_log().appendable);
        std::fs::remove_file(dir.join("acked")).unwrap();
        let elsewhere = dir.join("elsewhere");
        std::fs::write(&elsewhere, "").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join("acked")).unwrap();
        assert!(
            st.append_self_acked(1).is_err(),
            "a planted link is refused"
        );
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An append admits its opened file before it writes: ordinary state is
    /// appendable, but a second name for another owned file is not our log.
    #[test]
    fn an_acked_append_preserves_a_file_with_another_name() {
        let scratch = aterm_tempfile::TempDir::new().expect("exclusive state scratch");
        let root = std::fs::canonicalize(scratch.path()).expect("canonical state scratch");
        let ordinary = StateDir::open(root.join("ordinary")).expect("ordinary state");
        ordinary
            .append_self_acked(5)
            .expect("first ordinary append");
        ordinary
            .append_self_acked(7)
            .expect("second ordinary append");
        assert_eq!(
            std::fs::read(ordinary.path("acked")).expect("ordinary log"),
            b"5,7,",
            "ordinary append controls precede the occupied-file case"
        );

        let linked = StateDir::open(root.join("linked")).expect("linked state");
        let other = root.join("other-owned-file");
        const SENTINEL: &[u8] = b"the other file owns these bytes\n";
        std::fs::write(&other, SENTINEL).expect("owned sentinel");
        std::fs::hard_link(&other, linked.path("acked")).expect("owned hardlink");
        let result = linked.append_self_acked(42);
        let other_bytes = std::fs::read(&other).expect("other file remains readable");
        let state_bytes = std::fs::read(linked.path("acked")).expect("state name remains readable");
        assert!(
            result.is_err() && other_bytes == SENTINEL && state_bytes == SENTINEL,
            "ACKED_APPEND_REQUIRES_OWNED_SINGLE_LINK_FILE: result={result:?}, other={other_bytes:?}, state={state_bytes:?}"
        );
    }

    /// A re-base drops an in-flight record carrying a `re=` only when both log
    /// identities prove it was prepared on another log; an unknown witness or a
    /// record without `re=` stays, and a damaged journal blocks every reset mutation.
    #[test]
    fn a_re_base_drops_proven_old_references_and_keeps_uncertain_or_unrelated_mail() {
        use astream_broker::LogIdentity;
        let (a, b) = (
            LogIdentity {
                id: [0xaa; 16],
                generation: 0,
            },
            LogIdentity {
                id: [0xbb; 16],
                generation: 0,
            },
        );
        let dir = scratch("inflight-rebase");
        let st = StateDir::open(&dir).unwrap();
        let face = "/f/f/in/n-a/s-b/n-a/expired";
        let entry = |seq: u64, log: Option<LogIdentity>| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: face.into(),
            body: format!("v=1 t=1 re={seq}").into_bytes(),
            log,
        };
        st.set_in_flight(&entry(1, Some(a))).unwrap();
        st.set_in_flight(&entry(2, None)).unwrap();
        st.set_in_flight(&entry(3, Some(b))).unwrap();
        let note = InFlight {
            body: b"v=1 t=1 text=still-owed".to_vec(),
            ..entry(4, Some(a))
        };
        st.set_in_flight(&note).unwrap();
        assert_eq!(
            st.forget_log_offsets(100, Some(b), None, None).unwrap(),
            vec![(1, face.to_string())]
        );
        let kept = vec![(2, entry(2, None)), (3, entry(3, Some(b))), (4, note)];
        assert_eq!(st.in_flight().unwrap(), kept);
        assert!(
            st.forget_log_offsets(100, Some(b), None, None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        assert!(
            st.forget_log_offsets(7, None, None, None)
                .unwrap()
                .is_empty(),
            "no identity proves no mismatch"
        );
        assert_eq!(st.in_flight().unwrap(), kept, "uncertainty is retained");
        st.set_seen_off("s-b", 44).unwrap();
        st.write("inflight/5", "damaged").unwrap();
        assert_eq!(
            st.forget_log_offsets(7, Some(b), None, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(dir.join("inflight/5").exists(), "damage is kept for repair");
        assert_eq!(st.seen_off("s-b"), Some(44), "preflight changed nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A MOVED GENERATION CLAMPS INSTEAD OF FORGETTING
    /// ([`StateDir::forget_log_offsets`] with `keep_below = Some(f)`): every
    /// offset below the fork `f` still names its record, so whatever names one
    /// below it stays, whatever names one at or above it goes, and every cursor
    /// resumes at the lower of itself and `f` — `seen/` at `f - 1`, a topic at
    /// `f`; an in-flight reply below it is re-witnessed onto the new
    /// generation, where the attach replays it. `halt-acked` goes either way (it
    /// can name a halt row below the fork whose ack the loss took above it), and
    /// the `acked` window re-bases at the head either way. A clamp applied twice
    /// clamps to the same. NEGATIVE
    /// CONTROL on a twin dir: `None` — another log — forgets every one of them.
    #[test]
    fn a_moved_generation_clamps_the_offsets_it_continues_and_forgets_the_rest() {
        use astream_broker::LogIdentity;
        let old = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let new = LogIdentity {
            generation: 1,
            ..old
        };
        let pin = |original_off: Option<u64>| PublishPin {
            seq: 1,
            via_key: true,
            identity: original_off.map(|_| PublishIdentity {
                subject: "/f/f/in/n-a/s-b/n-c/ask".into(),
                digest: "a".repeat(64),
            }),
            original: None,
            original_off,
            owner: None,
        };
        let in_flight = |seq: u64, re: u64| InFlight {
            pin: PublishPin { seq, ..pin(None) },
            subject: "/f/f/in/n-a/s-b/n-a/answer".into(),
            body: format!("v=1 t=1 re={re}").into_bytes(),
            log: Some(old),
        };
        let deadline = |off| Deadline {
            off,
            sid: "s-x".into(),
            at: 9_000,
            dl: 60,
            to: None,
        };
        let asked = |off| Asked {
            off,
            to: "n-b".into(),
        };
        let cursors = |pairs: &[(&str, u64)]| -> BTreeMap<String, TopicCursor> {
            pairs
                .iter()
                .map(|(t, next)| ((*t).to_string(), TopicCursor { next: *next }))
                .collect()
        };
        let floors = |pairs: &[(&str, u64)]| -> BTreeMap<String, u64> {
            pairs
                .iter()
                .map(|(t, off)| ((*t).to_string(), *off))
                .collect()
        };
        let fill = |st: &StateDir| {
            st.set_seen_off("s-low", 3).unwrap();
            st.set_seen_off("s-high", 9).unwrap();
            st.set_topics("s-x", &cursors(&[("a", 2), ("b", 8)]))
                .unwrap();
            st.set_topic_floors("s-x", &floors(&[("a", 1), ("b", 7)]))
                .unwrap();
            st.set_topic_floors("s-y", &floors(&[("c", 6)])).unwrap();
            st.set_halt_acked(2).unwrap();
            st.set_inbox_owed(4).unwrap();
            st.set_deadlines(&[deadline(3), deadline(6)]).unwrap();
            st.set_asked(&[asked(3), asked(6)]).unwrap();
            st.set_key_pin("s-x", "low", &pin(Some(1))).unwrap();
            st.set_key_pin("s-x", "high", &pin(Some(6))).unwrap();
            st.set_in_flight(&in_flight(1, 2)).unwrap();
            st.set_in_flight(&in_flight(2, 7)).unwrap();
            st.set_self_acked(&[1, 6].into_iter().collect(), 8).unwrap();
        };
        let answer = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/answer".to_string());

        let dir = scratch("clamp");
        let st = StateDir::open(&dir).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), Some(5), None).unwrap(),
            vec![answer(2)],
            "only the in-flight reply to a record the loss took goes"
        );
        let clamped = |st: &StateDir| {
            assert_eq!(st.seen_off("s-low"), Some(3), "below the fork: kept");
            assert_eq!(st.seen_off("s-high"), Some(4), "at the fork");
            assert_eq!(st.topics("s-x"), cursors(&[("a", 2), ("b", 5)]));
            assert_eq!(st.topic_floors("s-x").unwrap(), floors(&[("a", 1)]));
            assert!(st.topic_floors("s-y").unwrap().is_empty());
            assert_eq!(st.halt_acked(), None, "cleared, clamped or not");
            assert_eq!(
                st.inbox_owed(),
                Some(4),
                "the commit owed is below the fork"
            );
            assert_eq!(st.deadlines(), [deadline(3)]);
            assert_eq!(st.asked(), [asked(3)]);
            assert_eq!(
                st.key_pin("s-x", "low").unwrap().unwrap().original_off,
                Some(1)
            );
            assert_eq!(
                st.key_pin("s-x", "high").unwrap().unwrap().original_off,
                None
            );
            assert_eq!(
                st.in_flight().unwrap(),
                vec![(
                    1,
                    InFlight {
                        log: Some(new),
                        ..in_flight(1, 2)
                    }
                )],
                "the reply below the fork is kept, RE-WITNESSED onto the new generation: the \
                 attach replays a reference only on the log its witness names"
            );
            assert_eq!(
                st.self_acked_log().offsets,
                [11].into_iter().collect(),
                "the window re-bases at the head"
            );
        };
        clamped(&st);
        assert!(
            st.forget_log_offsets(12, Some(new), Some(5), None)
                .unwrap()
                .is_empty(),
            "idempotent"
        );
        clamped(&st);
        // A fork above the head cannot be (the log grew from it): held to the head.
        st.set_topics("s-z", &cursors(&[("d", 30)])).unwrap();
        st.forget_log_offsets(12, Some(new), Some(40), None)
            .unwrap();
        assert_eq!(st.topics("s-z"), cursors(&[("d", 12)]));

        // NEGATIVE CONTROL: another log forgets every offset.
        let twin = scratch("clamp-ctl");
        let st = StateDir::open(&twin).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(12, Some(new), None, None).unwrap(),
            vec![answer(1), answer(2)]
        );
        assert_eq!((st.seen_off("s-low"), st.seen_off("s-high")), (None, None));
        assert_eq!(st.topics("s-x"), cursors(&[("a", 12), ("b", 12)]));
        assert!(st.topic_floors("s-x").unwrap().is_empty());
        assert_eq!(st.halt_acked(), None);
        assert_eq!(st.inbox_owed(), None);
        assert!(st.deadlines().is_empty() && st.asked().is_empty());
        assert_eq!(
            st.key_pin("s-x", "low").unwrap().unwrap().original_off,
            None
        );
        assert!(st.in_flight().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&twin);
    }

    /// AN IN-FLIGHT REPLY IS JUDGED AT ITS OWN LOG'S FORK. Generation 0 parted
    /// from generation 1 at 41 and generation 1 from generation 2 at 138, so the
    /// move onto generation 2 clamps at 138 — but a reply witnessed on
    /// generation 0 (a drop or a re-witness at the first move that a power cut
    /// rolled back, `crate::sim` seed 3113) names a record of generation 2 only
    /// below 41. Judged at 138 alone, its `ack re=41` was carried onto
    /// generation 2 and published there, naming whatever generation 1 put at 41.
    #[test]
    fn an_in_flight_reply_is_kept_only_below_the_fork_of_the_log_it_was_witnessed_on() {
        let log = |generation| astream_broker::LogIdentity {
            id: [0x3c; 16],
            generation,
        };
        let lineage = astream_broker::Lineage::from_steps(&[(1, 41), (2, 138)])
            .expect("a lineage astream reads");
        let entry = |seq: u64, re: u64, generation: u64| InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: "/f/f/in/n-a/s-b/n-a/ack".into(),
            body: format!("v=1 t=1 re={re}").into_bytes(),
            log: Some(log(generation)),
        };
        let fill = |st: &StateDir| {
            st.set_in_flight(&entry(1, 40, 0)).unwrap();
            st.set_in_flight(&entry(2, 41, 0)).unwrap();
            st.set_in_flight(&entry(3, 137, 1)).unwrap();
            st.set_in_flight(&entry(4, 138, 1)).unwrap();
        };
        let ack = |seq: u64| (seq, "/f/f/in/n-a/s-b/n-a/ack".to_string());
        let dir = scratch("own-fork");
        let st = StateDir::open(&dir).unwrap();
        fill(&st);
        let keep_below = LogWitness {
            high: 150,
            id: Some(log(1)),
        }
        .continues(Some(log(2)), Some(&lineage));
        assert_eq!(keep_below, Some(138), "the newest move's fork");
        assert_eq!(
            st.forget_log_offsets(150, Some(log(2)), keep_below, Some(&lineage))
                .unwrap(),
            vec![ack(2), ack(4)],
            "generation 0's reply at 41 and generation 1's at 138 go"
        );
        let rewitnessed = |seq, re, generation| {
            (
                seq,
                InFlight {
                    log: Some(log(2)),
                    ..entry(seq, re, generation)
                },
            )
        };
        assert_eq!(
            st.in_flight().unwrap(),
            vec![rewitnessed(1, 40, 0), rewitnessed(3, 137, 1)],
            "each kept below its own fork, re-witnessed onto generation 2"
        );
        // No lineage: the one fork the caller judged is every entry's.
        let twin = scratch("own-fork-none");
        let st = StateDir::open(&twin).unwrap();
        fill(&st);
        assert_eq!(
            st.forget_log_offsets(150, Some(log(2)), keep_below, None)
                .unwrap(),
            vec![ack(4)]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&twin);
    }

    /// A `post key=` pins its sequence to the key, first wins, and the table is
    /// bounded at [`KEYS_KEEP`]: the 4097th key evicts the oldest, and a re-post under
    /// an evicted key is a new record.
    #[test]
    fn a_key_pins_its_sequence_first_wins_and_the_4097th_evicts_the_oldest() {
        let dir = scratch("keys");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.key_seq("s-a", "k-1"), None);
        assert_eq!(st.set_key_seq("s-a", "k-1", 41).expect("reserve"), 41);
        assert_eq!(
            StateDir::open(&dir).expect("reopen").key_seq("s-a", "k-1"),
            Some(41),
            "the reservation survives a reopen"
        );
        // First wins: a second reservation answers the sequence the key holds.
        assert_eq!(st.set_key_seq("s-a", "k-1", 99).expect("re-reserve"), 41);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));
        // Per session: another session's identical key is its own reservation.
        assert_eq!(st.key_seq("s-b", "k-1"), None);
        assert_eq!(st.set_key_seq("s-b", "k-1", 7).expect("reserve"), 7);
        assert_eq!(st.key_seq("s-a", "k-1"), Some(41));

        // The bound: seed a full table as one file (a line at a time is quadratic),
        // then add one more.
        let full: Vec<String> = (0..KEYS_KEEP).map(|n| format!("k-{n} {n}")).collect();
        st.write("keys/s-c", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.key_seq("s-c", "k-0"), Some(0));
        assert_eq!(
            st.key_seq("s-c", &format!("k-{}", KEYS_KEEP - 1)),
            Some(KEYS_KEEP as u64 - 1)
        );
        assert_eq!(
            st.set_key_seq("s-c", "k-newest", 1_000_000)
                .expect("the 4097th"),
            1_000_000
        );
        assert_eq!(st.key_seq("s-c", "k-0"), None, "the oldest key is evicted");
        assert_eq!(st.key_seq("s-c", "k-1"), Some(1), "the next-oldest stands");
        assert_eq!(st.key_seq("s-c", "k-newest"), Some(1_000_000));
        assert_eq!(st.key_lines("s-c").len(), KEYS_KEEP, "the table is capped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The deadline table round-trips and is bounded; a damaged line reads as nothing
    /// (a half-parsed deadline would be a verdict for the wrong ask).
    #[test]
    fn deadlines_round_trip_are_bounded_and_a_damaged_line_reads_as_nothing() {
        let dir = scratch("deadlines");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.deadlines().is_empty());
        let one = Deadline {
            off: 90_312,
            sid: "s-abcdef0123456789".to_string(),
            at: 1_700_000_240_123,
            dl: 240_000,
            to: Some("n-0123456789abcdef".to_string()),
        };
        st.set_deadlines(std::slice::from_ref(&one)).expect("write");
        assert_eq!(
            StateDir::open(&dir).expect("reopen").deadlines(),
            vec![one.clone()]
        );
        st.set_deadlines(&[]).expect("clear");
        assert!(st.deadlines().is_empty());
        for damaged in [
            "",
            "off=1 sid=s-a at=2",          // no dl
            "off=1 sid=s-a dl=3",          // no at
            "sid=s-a at=2 dl=3",           // no offset
            "off=1 at=2 dl=3",             // no sid
            "off=1 sid= at=2 dl=3",        // an empty sid
            "off=x sid=s-a at=2 dl=3",     // an offset that is not one
            "off=1 sid=s-a at=2 dl=3 to=", // an empty recipient
        ] {
            assert_eq!(Deadline::parse(damaged), None, "{damaged:?}");
        }
        assert_eq!(Deadline::parse(&one.render()), Some(one.clone()));
        // A line from before `to=` reads, with no recipient to hold replies to.
        assert_eq!(
            Deadline::parse("off=90312 sid=s-abcdef0123456789 at=1700000240123 dl=240000"),
            Some(Deadline { to: None, ..one })
        );
        // Bounded: the newest by position survive.
        let many: Vec<Deadline> = (0..DEADLINES_KEEP as u64 + 3)
            .map(|n| Deadline {
                off: n,
                sid: "s-a".to_string(),
                at: n,
                dl: 1,
                to: None,
            })
            .collect();
        st.set_deadlines(&many).expect("write many");
        let kept = st.deadlines();
        assert_eq!(kept.len(), DEADLINES_KEEP);
        assert_eq!(kept[0].off, 3, "the three oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The asked table round-trips and is bounded; a damaged line reads as nothing (a
    /// misread recipient would hold a real receipt as a stray).
    #[test]
    fn the_asked_table_round_trips_and_is_bounded() {
        let dir = scratch("asked");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.asked().is_empty());
        let two = vec![
            Asked {
                off: 7,
                to: "n-0123456789abcdef".to_string(),
            },
            Asked {
                off: 9,
                to: "h-andrew".to_string(),
            },
        ];
        st.set_asked(&two).expect("write");
        assert_eq!(StateDir::open(&dir).expect("reopen").asked(), two);
        for damaged in ["", "7", "x n-a", "7 n-a extra"] {
            assert_eq!(Asked::parse(damaged), None, "{damaged:?}");
        }
        let many: Vec<Asked> = (0..ASKED_KEEP as u64 + 2)
            .map(|off| Asked {
                off,
                to: "n-a".to_string(),
            })
            .collect();
        st.set_asked(&many).expect("write many");
        let kept = st.asked();
        assert_eq!(kept.len(), ASKED_KEEP);
        assert_eq!(kept[0].off, 2, "the two oldest are gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pin table is bounded, oldest first sight out, without disturbing what it
    /// keeps.
    #[test]
    fn the_pin_table_is_bounded_and_evicts_the_oldest_first_sight() {
        let dir = scratch("pins-bound");
        let st = StateDir::open(&dir).expect("open");
        // A full table, written as one file: filling it via `pin` would be
        // quadratic.
        let full: Vec<String> = (0..PINS_KEEP)
            .map(|n| format!("s-{n:016x} n-{:016x}", n % 3))
            .collect();
        st.write("pins", &full.join("\n"))
            .expect("seed a full table");
        assert_eq!(st.pins().len(), PINS_KEEP);
        for n in PINS_KEEP..PINS_KEEP + 16 {
            st.pin(&format!("s-{n:016x}"), &format!("n-{:016x}", n % 3))
                .expect("pin");
        }
        let pins = st.pins();
        assert_eq!(pins.len(), PINS_KEEP, "the table is capped");
        // The sixteen oldest first sights are gone; everything newer stands.
        for n in 0..16 {
            assert!(!pins.contains_key(&format!("s-{n:016x}")), "{n} evicted");
        }
        for n in 16..PINS_KEEP + 16 {
            assert_eq!(
                pins.get(&format!("s-{n:016x}")),
                Some(&format!("n-{:016x}", n % 3)),
                "{n} kept, with its node"
            );
        }
        // First-wins holds across the bound: re-pinning a survivor writes nothing,
        // so asking cannot provoke an eviction.
        let survivor = format!("s-{:016x}", PINS_KEEP);
        assert_eq!(
            st.pin(&survivor, "n-rogue").expect("re-pin"),
            format!("n-{:016x}", PINS_KEEP % 3)
        );
        assert_eq!(st.pins().len(), PINS_KEEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second take of the bridge lock is `WouldBlock` naming the `pid` file's pid,
    /// and the dir is free once the hold drops. `flock` conflicts between two open
    /// file descriptions in one process, so this needs no second process.
    #[test]
    fn the_bridge_lock_has_one_holder_until_it_drops() {
        let dir = scratch("lock");
        let st = StateDir::open(&dir).expect("open");
        let held = st.lock_bridge().expect("the first take");
        std::fs::write(dir.join("pid"), "4242\n").expect("pid");
        let refused = st.lock_bridge().err().expect("a second take is refused");
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        assert!(refused.to_string().contains("(pid 4242)"), "{refused}");
        drop(held);
        let _again = st.lock_bridge().expect("free once the holder let go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing directory is empty; a dangling directory entry is unreadable
    /// state and cannot authorize finishing a replacement log's reset.
    #[cfg(unix)]
    #[test]
    fn a_log_switch_refuses_a_dangling_offset_directory() {
        for blocked in ["seen", "topics"] {
            let dir = scratch("dangling-offsets");
            let st = StateDir::open(&dir).unwrap();
            let area = dir.join(blocked);
            if area.is_dir() {
                std::fs::remove_dir(&area).unwrap();
            }
            std::os::unix::fs::symlink(dir.join("missing"), &area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None, None).is_err(),
                "unreadable {blocked} was empty"
            );
            assert!(area.is_symlink(), "the unreadable directory entry stays");
            std::fs::remove_file(&area).unwrap();
            assert!(
                st.forget_log_offsets(20, None, None, None).is_ok(),
                "an absent directory is empty"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The log witness round-trips with and without an identity, reads past a
    /// token (or an `id=`) it cannot parse, and names another log by a different
    /// identity at any head or by a head below its high-water. An identity missing
    /// on either side says nothing.
    #[test]
    fn the_log_witness_round_trips_and_names_another_log_by_identity_or_head() {
        use astream_broker::LogIdentity;
        let a = LogIdentity {
            id: [0xaa; 16],
            generation: 0,
        };
        let a_rewritten = LogIdentity { generation: 1, ..a };
        let b = LogIdentity {
            id: [0xbb; 16],
            generation: 0,
        };
        let dir = scratch("witness");
        let st = StateDir::open(&dir).expect("open");
        assert_eq!(st.log_witness(), None, "nothing seen yet");
        st.set_log_witness(&LogWitness {
            high: 500,
            id: None,
        })
        .expect("write");
        let w = st.log_witness().expect("read back");
        assert_eq!(
            w,
            LogWitness {
                high: 500,
                id: None
            }
        );
        assert!(w.replaced_by(499, None) && w.replaced_by(499, Some(a)));
        assert!(!w.replaced_by(500, None) && !w.replaced_by(10_000, Some(a)));
        let with_id = LogWitness {
            high: 500,
            id: Some(a),
        };
        st.set_log_witness(&with_id).expect("write");
        assert_eq!(st.log_witness(), Some(with_id));
        // Another identity is another log, longer or not, and so is the same
        // log's next generation, whose offsets went to other records.
        assert!(with_id.replaced_by(10_000, Some(b)));
        assert!(with_id.replaced_by(10_000, Some(a_rewritten)));
        assert!(!with_id.replaced_by(10_000, Some(a)));
        assert!(
            !with_id.replaced_by(10_000, None),
            "no identity says nothing"
        );
        assert!(
            with_id.replaced_by(499, Some(a)),
            "a head below is evidence too"
        );
        assert_eq!(
            LogWitness::parse("id=00ff high=7"),
            Some(LogWitness { high: 7, id: None })
        );
        assert_eq!(LogWitness::parse("high=+7"), None);
        assert_eq!(LogWitness::parse("id=00ff"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Standing halts round-trip per fleet with their kept/witnessed flag, a damaged
    /// line is no halt, and clearing the halt watermark is idempotent.
    #[test]
    fn standing_halts_round_trip_per_fleet_and_a_damaged_line_is_nothing() {
        let dir = scratch("halts");
        let st = StateDir::open(&dir).expect("open");
        assert!(st.standing_halts("f").is_empty());
        let halts = BTreeMap::from([
            (
                "h-a".to_string(),
                StandingHalt {
                    reason: "main%20broken".to_string(),
                    kept: false,
                },
            ),
            (
                "h-b".to_string(),
                StandingHalt {
                    reason: "fleet-halt".to_string(),
                    kept: true,
                },
            ),
        ]);
        st.set_standing_halts("f", &halts).expect("write");
        assert_eq!(st.standing_halts("f"), halts);
        assert!(st.standing_halts("g").is_empty(), "another fleet's halts");
        std::fs::write(
            dir.join("halts"),
            "f h-a r witnessed\nf h-b r maybe\nf n-node r kept\nf h-c\n",
        )
        .expect("write");
        assert_eq!(
            st.standing_halts("f").into_keys().collect::<Vec<_>>(),
            ["h-a"]
        );
        st.set_standing_halts("f", &BTreeMap::new()).expect("empty");
        assert!(st.standing_halts("f").is_empty());

        st.set_halt_acked(9).expect("ack");
        st.clear_halt_acked().expect("clear");
        assert_eq!(st.halt_acked(), None);
        st.clear_halt_acked().expect("clearing nothing is no error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blank or garbage file reads as "nothing known", never a panic.
    #[test]
    fn a_corrupt_file_reads_as_unknown() {
        let dir = scratch("corrupt");
        let st = StateDir::open(&dir).expect("open");
        std::fs::write(dir.join("seq"), "   \n").expect("write");
        assert_eq!(st.sequence(), 0);
        std::fs::write(dir.join("inc"), "not-a-number").expect("write");
        assert_eq!(st.incarnation(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // the incarnation lease
    // -----------------------------------------------------------------------

    /// The boot id standing in for boot number `b` (never the all-zero "unknown").
    fn boot_id(b: u8) -> [u8; 16] {
        [b; 16]
    }

    /// A seam that names this boot, kills nowhere and carries no defect.
    fn in_boot(b: u8) -> IncSeam {
        IncSeam {
            boot: Some(Some(boot_id(b))),
            ..IncSeam::default()
        }
    }

    /// The `inc` file standing in for file number `n` of one dir.
    fn inc_file_id(n: u64) -> Option<IncFile> {
        Some(IncFile {
            dev: 1,
            dir: 2,
            ino: n,
        })
    }

    /// `inc-lease` RENDERS AND PARSES BACK, and anything but a whole lease is no lease:
    /// a torn, hand-edited or duplicated field, `used` above `top`, a boot that is not a
    /// boot id (the all-zero one included), or a file that is not `<dev>:<dir>:<ino>`
    /// reads as `None`, which flushes.
    #[test]
    fn an_incarnation_lease_round_trips_and_anything_torn_is_no_lease() {
        for lease in [
            IncLease {
                used: 3,
                top: 66,
                boot: Some(boot_id(7)),
                file: Some(IncFile {
                    dev: u64::MAX,
                    dir: 0,
                    ino: 12_345,
                }),
            },
            IncLease {
                used: 5,
                top: 5,
                boot: None,
                file: None,
            },
        ] {
            assert_eq!(IncLease::parse(&lease.render()), Some(lease), "{lease:?}");
        }
        let id = aterm_codec::hex::encode_lower(&boot_id(7));
        for torn in [
            String::new(),
            "used=3 top=66".to_string(),
            format!("used=3 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id}"),
            format!("used=3 top=66 boot={} file=1:2:3", &id[..31]),
            format!("used=3 top=66 boot={} file=1:2:3", "0".repeat(32)),
            format!("used=67 top=66 boot={id} file=1:2:3"),
            format!("used=3 used=4 top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2:3 extra=1"),
            format!("used=x top=66 boot={id} file=1:2:3"),
            format!("used=3 top=66 boot={id} file=1:2"),
            format!("used=3 top=66 boot={id} file=1:2:3:4"),
            format!("used=3 top=66 boot={id} file=1::3"),
            format!("used=3 top=66 boot={id} file=1:2:3 file=-"),
        ] {
            assert_eq!(IncLease::parse(&torn), None, "{torn:?}");
        }
    }

    /// THE PLAN, case by case: a lease this boot wrote, whose top is `inc`'s and whose
    /// file is the `inc` there now, answers with no flush while it has room and leases
    /// afresh above itself once it runs out; any other boot's lease, a lease whose top is
    /// not `inc`'s, a lease naming another file (a copy's) or none, no lease, or a boot
    /// nobody can name takes the incarnation above `inc` and flushes; `seq`'s incarnation
    /// and the bus's both count; a boot nobody can name leases one; the space ends at
    /// `INC_MAX`.
    #[test]
    fn the_incarnation_plan_trusts_only_this_boots_lease_and_counts_seq_and_the_bus() {
        let (b1, b2) = (Some(boot_id(1)), Some(boot_id(2)));
        let f = inc_file_id(1);
        let lease = |used, top, boot| {
            Some(IncLease {
                used,
                top,
                boot,
                file: f,
            })
        };
        let plan = |next, top, flush| Some(IncPlan { next, top, flush });
        let span = INC_LEASE;
        // This boot's lease with room: the next one, unflushed.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(4, 66, false)
        );
        // Run out: leased afresh above it.
        assert_eq!(
            plan_incarnation(66, f, lease(66, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        // Another boot's lease: everything up to `inc` counts as taken.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b2, 0, span),
            plan(67, 66 + span, true)
        );
        // A top that is not `inc`'s (an interrupted flush renamed in, or an older build's
        // `inc + 1`): not trusted.
        assert_eq!(
            plan_incarnation(130, f, lease(3, 66, b1), 3, b1, 0, span),
            plan(131, 130 + span, true)
        );
        // Another `inc` file than the lease names (a copy of the dir, a restored `inc`),
        // no `inc` file at all, or a lease that names none: not trusted.
        for (file, named) in [(inc_file_id(2), f), (None, f), (f, None), (None, None)] {
            let other = Some(IncLease {
                used: 3,
                top: 66,
                boot: b1,
                file: named,
            });
            assert_eq!(
                plan_incarnation(66, file, other, 3, b1, 0, span),
                plan(67, 66 + span, true),
                "inc {file:?}, lease naming {named:?}"
            );
        }
        // No lease at all (a dir an older build kept): `inc` was the incarnation.
        assert_eq!(
            plan_incarnation(9, f, None, 9, b1, 0, span),
            plan(10, 9 + span, true)
        );
        // `seq`'s incarnation counts: an older build's doctor published under the top.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 66, b1, 0, span),
            plan(67, 66 + span, true)
        );
        // So does the bus's: a copied dir's sibling ran ahead.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, b1), 3, b1, 90, span),
            plan(91, 90 + span, true)
        );
        // A boot nobody can name trusts no lease and leases ONE: `inc` is then exactly
        // the incarnation taken, as the file always was.
        assert_eq!(
            plan_incarnation(66, f, lease(3, 66, None), 3, None, 0, span),
            plan(67, 67, true)
        );
        // The space ends at INC_MAX: the top is clamped, and past it is refused.
        assert_eq!(
            plan_incarnation(INC_MAX - 1, f, None, 0, b1, 0, span),
            plan(INC_MAX, INC_MAX, true)
        );
        assert_eq!(plan_incarnation(INC_MAX, f, None, 0, b1, 0, span), None);
        assert_eq!(plan_incarnation(0, None, None, 0, b1, INC_MAX, span), None);
        assert_eq!(
            plan_incarnation(0, None, None, u64::MAX >> 32, b1, 0, span),
            None
        );
    }

    /// WHAT AN ATTACH COSTS, as a count: in one boot, one lease of `INC_LEASE` attaches
    /// is ONE flushed write — two `fsync`s (`F_FULLFSYNC` on macOS, 8–12 ms measured in
    /// place on this machine) — and every other attach is one atomic replace; the attach
    /// after the lease runs out flushes again. A new boot flushes at its first attach,
    /// however much of the last lease was left, and a boot nobody can name flushes every
    /// time, as every attach did before the lease.
    ///
    /// Negative control: the old `set_incarnation` flushed on every take, `2 * INC_LEASE`
    /// fsyncs where this counts 2.
    #[test]
    fn a_lease_of_attaches_costs_one_flush_and_a_new_boot_flushes_once() {
        let dir = scratch("inclease-cost");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        let take = |n: u64| -> Vec<u64> {
            (0..n)
                .map(|_| st.take_incarnation(0).expect("take"))
                .collect()
        };
        let before = st.ops();
        let mut taken = take(3);
        // MID-LEASE, where what was taken and the lease's top differ (3 and 64): the
        // doctor's answer is the one taken (`the_doctor_never_answers_a_lost_leases_unused_top`).
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor's answer is what was taken, not the top"
        );
        taken.extend(take(INC_LEASE - 3));
        let lease = st.ops() - before;
        assert_eq!(
            taken,
            (1..=INC_LEASE).collect::<Vec<_>>(),
            "one apart, from 1"
        );
        assert_eq!(
            (lease.fsyncs, lease.replaces),
            (2, INC_LEASE + 1),
            "one flushed `inc`, then one `inc-lease` replace per attach: {lease:?}"
        );
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), INC_LEASE + 1);
        assert_eq!(
            (st.ops() - before).fsyncs,
            2,
            "the lease ran out: flushed again"
        );

        // A NEW BOOT: the lease has 63 incarnations left, and none of them is trusted.
        st.set_inc_seam(in_boot(2));
        let before = st.ops();
        assert_eq!(
            st.take_incarnation(0).expect("take"),
            2 * INC_LEASE + 1,
            "above the whole lease"
        );
        assert_eq!((st.ops() - before).fsyncs, 2);
        let before = st.ops();
        assert_eq!(st.take_incarnation(0).expect("take"), 2 * INC_LEASE + 2);
        assert_eq!((st.ops() - before).fsyncs, 0, "and then trusted again");

        // A BOOT NOBODY CAN NAME: every take flushes, and leases one.
        st.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        for _ in 0..3 {
            let before = st.ops();
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!((st.ops() - before).fsyncs, 2);
            assert_eq!(st.lease().map(|l| (l.used, l.top)), Some((inc, inc)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE DOCTOR NEVER PUBLISHES UNDER A LEASE'S UNUSED TOP**, not even once a power
    /// loss took the lease that named what was used. `inc` holds the top of a block of
    /// `INC_LEASE` and was flushed; `inc-lease`, which says which incarnation of the
    /// block the last attach took, was not, and the dir's FIRST lease has no older
    /// version for the media to fall back on. A doctor that answered `inc` there would
    /// reserve `incarnation_base(64) + 1`, raise the producer's high water above
    /// incarnation 3's will (astream's `will_seq(3)`) and fence it: the dead node's row
    /// would stay `live`, and the next attach would never witness incarnation 3 dead.
    ///
    /// What remains answers: `seq`'s incarnation, which a bridge reserves under after its
    /// take, so it is never above what was used. Without that either, nothing is known,
    /// and the answer is 0: the doctor's sequence is then a fresh number below every will
    /// (accepted, fencing nothing) or one used before (a dedup, which it refuses).
    ///
    /// Negative control: `incarnation()` falling back to `inc` when there is no whole
    /// lease answers 64 at the first assertion after the power loss.
    #[test]
    fn the_doctor_never_answers_a_lost_leases_unused_top() {
        let dir = scratch("inclease-doctor");
        let st = StateDir::open(&dir).expect("open");
        st.set_inc_seam(in_boot(1));
        // Boot 1: the dir's first lease, and three attaches, each reserving a block as
        // the bridge does once its will is registered.
        for want in 1..=3 {
            let inc = st.take_incarnation(0).expect("take");
            assert_eq!(inc, want);
            st.reserve_sequence_block(inc, 16).expect("a block");
        }
        // MID-LEASE the doctor's answer is what was taken, never the top: used 3, top 64.
        assert_eq!(
            (st.incarnation(), st.incarnation_top()),
            (3, INC_LEASE),
            "the doctor answers the incarnation taken, not the lease's top"
        );

        // THE POWER LOSS: the flushed `inc` survives, the never-flushed first lease does
        // not, and `seq` was written back.
        std::fs::remove_file(dir.join("inc-lease")).expect("lose the lease");
        assert_eq!(
            st.incarnation(),
            3,
            "seq's incarnation, never the lost lease's unused top ({INC_LEASE})"
        );
        // A torn lease is no lease, and answers the same.
        std::fs::write(dir.join("inc-lease"), "used=3 top=").expect("tear the lease");
        assert_eq!(st.incarnation(), 3, "a torn lease");
        // `seq` rolled back to incarnation 1's block: at or below the one used.
        std::fs::write(dir.join("seq"), (base(1) + 16).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 1, "a rolled-back seq");
        // Both gone: nothing is known, and the doctor's sequence fences no will.
        std::fs::remove_file(dir.join("seq")).expect("lose seq");
        assert_eq!(st.incarnation(), 0, "nothing known");
        let seq = st
            .reserve_next_sequence(st.incarnation())
            .expect("a sequence");
        assert!(
            seq < will(1),
            "the doctor's sequence {seq:#x} is below every will this node registered"
        );

        // AN OLDER BUILD'S DIR (no lease ever; `inc` was the incarnation, and its attach
        // reserved under it): the doctor answers that incarnation, as it always did.
        std::fs::remove_file(dir.join("inc-lease")).expect("no lease");
        std::fs::write(dir.join("inc"), "9").expect("inc");
        std::fs::write(dir.join("seq"), (base(9) + 5).to_string()).expect("seq");
        assert_eq!(st.incarnation(), 9, "an older build's dir");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A COPIED STATE DIR FLUSHES ITS OWN `inc` BEFORE IT TRUSTS A LEASE.** A copy in
    /// the same boot (`cp -R`, a Finder duplicate, `rsync`, a backup restored) carries a
    /// lease whose boot is this one and whose top equals the copied `inc` — but that top
    /// was flushed in the ORIGINAL dir; the copy's `inc` is a new file the copy wrote
    /// unflushed. Trusted, every attach of the copy would present an incarnation no
    /// flushed `inc` covers, and a power loss would let the next boot take one of them
    /// again (the bus's newest row is one short when the last `live` had not landed),
    /// whose old will then fires unfenced over the new `live`. So the lease names the
    /// very `inc` file and directory it vouches for, and a copy's are other files.
    ///
    /// Negative control: a lease trusted on boot and top alone takes 6 in the copy with
    /// no flush.
    #[test]
    fn a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease() {
        let (a, c) = (scratch("inclease-copy-a"), scratch("inclease-copy-c"));
        let original = StateDir::open(&a).expect("open the original");
        original.set_inc_seam(in_boot(1));
        for want in 1..=5 {
            assert_eq!(original.take_incarnation(0).expect("take"), want);
        }
        // THE COPY, in the same boot: the same bytes in new files.
        let copy = StateDir::open(&c).expect("open the copy");
        for file in ["inc", "inc-lease", "seq"] {
            if a.join(file).exists() {
                std::fs::copy(a.join(file), c.join(file)).expect("copy");
            }
        }
        copy.set_inc_seam(in_boot(1));
        let before = copy.ops();
        assert_eq!(
            copy.take_incarnation(0).expect("take"),
            INC_LEASE + 1,
            "above the copied lease's whole top"
        );
        assert_eq!(
            (copy.ops() - before).fsyncs,
            2,
            "the copy's own `inc` is flushed before its first incarnation is presented"
        );
        let before = copy.ops();
        assert_eq!(copy.take_incarnation(0).expect("take"), INC_LEASE + 2);
        assert_eq!(
            (copy.ops() - before).fsyncs,
            0,
            "and then its own lease is trusted"
        );
        // The original's lease still names the file it flushed: no flush there.
        let before = original.ops();
        assert_eq!(original.take_incarnation(0).expect("take"), 6);
        assert_eq!(
            (original.ops() - before).fsyncs,
            0,
            "the original keeps its lease"
        );
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&c);
    }

    /// **A STATE DIR'S MARK IS ITS OWN, AND A COPY HAS ANOTHER** (finding F4,
    /// the module doc's "Whose row it is"): the same dir, opened again or
    /// renamed within its volume, answers the same mark — so a later bridge
    /// of it reads the rows an earlier one wrote as the dir's — while a copy
    /// holding every byte of it, the `node` file included, answers another,
    /// as does the same dir under another node id. Sixteen hex digits.
    ///
    /// Negative control: the mark as a function of the dir's BYTES (here, the
    /// `node` file the copy shares) answers the same for the copy — what the
    /// directory's own identity is there to refuse.
    #[cfg(unix)]
    #[test]
    fn a_state_dirs_mark_is_its_own_and_a_copy_has_another() {
        const NODE: &str = "n-0123456789abcdef";
        let boot = crate::durability::test_boot(1);
        let open = |path: &Path| {
            let state = StateDir::open(path).expect("open the state dir");
            state.set_inc_seam(IncSeam {
                boot: Some(Some(boot)),
                ..IncSeam::default()
            });
            state
        };
        let (a, c) = (scratch("mark-a"), scratch("mark-c"));
        let original = open(&a);
        std::fs::write(a.join("node"), format!("{NODE}\n")).expect("the node");
        let mark = original
            .mark(NODE)
            .expect("a mark where the platform names an inode");
        assert_eq!(mark.len(), 16, "{mark}");
        assert!(mark.bytes().all(|b| b.is_ascii_hexdigit()), "{mark}");
        assert_eq!(
            open(&a).mark(NODE).as_deref(),
            Some(mark.as_str()),
            "the same dir, opened again, is the same dir"
        );
        // A COPY: every byte of the dir, in a directory of its own.
        let copy = open(&c);
        std::fs::copy(a.join("node"), c.join("node")).expect("copy the node");
        let copied = copy.mark(NODE).expect("the copy's mark");
        assert_ne!(copied, mark, "a copy of the dir is another dir");
        assert_ne!(
            original.mark("n-fedcba9876543210").as_deref(),
            Some(mark.as_str()),
            "another node's"
        );
        // A CLONED MACHINE can repeat both local filesystem names. Keep the
        // very same directory metadata and change only the injected kernel
        // boot, so this does not pass merely because a local copy got a new inode.
        original.set_inc_seam(IncSeam {
            boot: Some(Some(crate::durability::test_boot(2))),
            ..IncSeam::default()
        });
        assert_ne!(original.mark(NODE).as_deref(), Some(mark.as_str()));
        original.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        assert_eq!(
            original.mark(NODE),
            None,
            "no boot identity grants no adoption"
        );
        original.set_inc_seam(IncSeam {
            boot: Some(Some(boot)),
            ..IncSeam::default()
        });
        assert_eq!(original.mark(NODE).as_deref(), Some(mark.as_str()));
        // RENAMED within its volume: the same directory, so the same mark.
        let moved = scratch("mark-moved");
        std::fs::rename(&a, &moved).expect("rename the original");
        assert_eq!(
            open(&moved).mark(NODE).as_deref(),
            Some(mark.as_str()),
            "a rename keeps the dir"
        );
        // NEGATIVE CONTROL: a mark of the dir's bytes is the copy's too.
        let of_bytes = |dir: &Path| {
            let node = std::fs::read(dir.join("node")).expect("the node");
            format!("{:016x}", aterm_hash::fnv1a_64(&node))
        };
        assert_eq!(
            of_bytes(&moved),
            of_bytes(&c),
            "the copy holds the same bytes"
        );
        let _ = std::fs::remove_dir_all(&moved);
        let _ = std::fs::remove_dir_all(&c);
    }

    /// **THE SHIPPED TRUST DECISION LEASES ON THIS MACHINE**: with no seam, the kernel's
    /// boot session and the dir's own device decide (`trusted_boot`), and on macOS — the
    /// build whose `F_FULLFSYNC` the lease exists to spare — they must vouch for a dir
    /// on the boot volume's data volume (where `~` and the temp dir live), so five
    /// attaches cost ONE flushed write. Every other lease test stands its boot in; this
    /// is the one that holds the speed-up to the path that ships.
    ///
    /// Negative control: `trusted_boot` answering `None` (`this_boot` failing, or
    /// `on_boot_volume` refusing the data volume) makes the five takes ten fsyncs.
    #[test]
    fn the_shipped_trust_decision_leases_a_dir_on_the_boot_volume() {
        let dir = scratch("inclease-shipped");
        let st = StateDir::open(&dir).expect("open");
        let boot = crate::durability::this_boot();
        let on_boot = on_boot_volume(&dir);
        if cfg!(target_vendor = "apple") {
            assert!(boot.is_some(), "macOS names its boot session");
            assert!(on_boot, "{dir:?} is on the boot volume");
        }
        let before = st.ops();
        let taken: Vec<u64> = (0..5)
            .map(|_| st.take_incarnation(0).expect("take"))
            .collect();
        assert_eq!(taken, [1, 2, 3, 4, 5]);
        let fsyncs = (st.ops() - before).fsyncs;
        eprintln!(
            "MEASURED five shipped takes: {fsyncs} fsyncs (boot {boot:?}, on boot volume {on_boot})"
        );
        let want = if boot.is_some() && on_boot { 2 } else { 10 };
        assert_eq!(fsyncs, want, "five takes on the shipped trust decision");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The model's page-cache files laid down in `st`'s dir: `inc` (`c_inc`), `inc-lease`
    /// (`c_used`, `c_top`, `c_lb`: none when `c_lb` is 0) and `seq` (the end of a block
    /// of incarnation `c_seq`: none when it is 0). The model is one dir whose `inc` is
    /// the file every lease was flushed into (a copy of the dir is
    /// `a_copied_state_dir_flushes_its_own_inc_before_trusting_a_lease`'s), so the lease
    /// names the `inc` laid down.
    ///
    /// A file the state holds is written over in place and only one it does not hold is
    /// removed: the bytes are the state's either way, at one call per file.
    fn lay_down(st: &StateDir, s: &aterm_spec::interp::State) {
        let v = |name: &str| u64::try_from(s[name]).expect("a model value is not negative");
        let put = |file: &str, held: Option<String>| match held {
            Some(text) => std::fs::write(st.path(file), text).expect(file),
            None => match std::fs::remove_file(st.path(file)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => panic!("{file}: {e}"),
                _ => {}
            },
        };
        put("inc", (v("c_inc") > 0).then(|| v("c_inc").to_string()));
        // After `inc`: the lease names the `inc` laid down.
        put(
            "inc-lease",
            (v("c_lb") > 0).then(|| {
                IncLease {
                    used: v("c_used"),
                    top: v("c_top"),
                    boot: Some(boot_id(u8::try_from(v("c_lb")).expect("a small boot"))),
                    file: st.inc_file(),
                }
                .render()
            }),
        );
        put(
            "seq",
            (v("c_seq") > 0).then(|| (base(v("c_seq")) + 64).to_string()),
        );
    }

    /// The files read back as the model's page-cache variables
    /// `(c_inc, c_used, c_top, c_lb, c_seq)`.
    fn read_back(st: &StateDir) -> [i64; 5] {
        let int = |n: u64| i64::try_from(n).expect("a small value");
        let (used, top, lb) = st.lease().map_or((0, 0, 0), |l| {
            let boot = l.boot.expect("the walk always names its boot");
            assert!(boot.iter().all(|b| *b == boot[0]), "a stand-in boot id");
            (l.used, l.top, u64::from(boot[0]))
        });
        [
            int(st.incarnation_top()),
            int(used),
            int(top),
            int(lb),
            int(st.sequence() >> 32),
        ]
    }

    /// Run the REAL take from every planned state of `states` (what `model` reaches), at
    /// each kill point, with `mutant`'s defect, and name every one whose files are not
    /// the model's successor (or whose answer is not the model's `pn`) — or only the
    /// first, when `first` (a negative control asks only that one exists). A kill point
    /// the take does not reach (a take that does not flush) must complete it as the
    /// model's `Take` does; a state whose flush the model's bound cuts off is not
    /// compared. Each call takes in a dir of its own, so calls run side by side.
    fn diverging_takes(
        model: &aterm_spec::derive::Model,
        states: &[aterm_spec::interp::State],
        mutant: u8,
        first: bool,
    ) -> (usize, Vec<String>) {
        let span = model
            .consts
            .iter()
            .find(|(name, _)| *name == "Span")
            .map(|(_, v)| u64::try_from(*v).expect("a positive span"))
            .expect("the model declares Span");
        let walk = if first { "control" } else { "walk" };
        let dir = scratch(&format!("inclease-{walk}-{mutant}"));
        let st = StateDir::open(&dir).expect("open");
        let mut seen = std::collections::BTreeSet::new();
        let (mut compared, mut diverged) = (0, Vec::new());
        for s in states {
            let key = ["boot", "c_inc", "c_used", "c_top", "c_lb", "c_seq", "pc"].map(|k| s[k]);
            if s["pc"] != 1 || !seen.insert(key) {
                continue;
            }
            let completed = ["Take", "TakeFlushed"]
                .iter()
                .find_map(|a| model.successors(a, s).into_iter().next());
            for (kill, action) in [
                (None, None),
                (Some(IncStep::Renamed), Some("KillRenamed")),
                (Some(IncStep::Flushed), Some("KillFlushed")),
            ] {
                let killed = action.and_then(|a| model.successors(a, s).into_iter().next());
                let (expect, dies) = match (killed, &completed) {
                    (Some(k), _) => (k, true),
                    (None, Some(c)) => (c.clone(), false),
                    (None, None) => continue, // the model's bound, not a step
                };
                if kill.is_some() && !dies && s["tr"] == 0 {
                    continue; // a flush the bound cut off is no kill point
                }
                lay_down(&st, s);
                st.set_inc_seam(IncSeam {
                    boot: Some(Some(boot_id(u8::try_from(s["boot"]).expect("small")))),
                    kill_at: kill,
                    mutant,
                    span: Some(span),
                    off_volume: false,
                });
                let got = st.take_incarnation(0);
                let files = read_back(&st);
                let want = ["c_inc", "c_used", "c_top", "c_lb", "c_seq"].map(|k| expect[k]);
                let answer_ok = match (&got, dies) {
                    (Err(_), true) => true,
                    (Ok(inc), false) => i64::try_from(*inc).ok() == Some(s["pn"]),
                    _ => false,
                };
                compared += 1;
                if !answer_ok || files != want {
                    diverged.push(format!(
                        "from {s:?} killed at {kill:?}: took {got:?} (model pn {}), files \
                         {files:?}, model {want:?}",
                        s["pn"]
                    ));
                    if first {
                        let _ = std::fs::remove_dir_all(&dir);
                        return (compared, diverged);
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        (compared, diverged)
    }

    /// **THE INCARNATION LEASE REFINES ITS MODEL, at every kill point, from every state
    /// a run of kills, power losses, write-backs and older builds' attaches reaches.**
    ///
    /// `aterm-models-session`'s `IncarnationLease` proves, over its whole bounded space,
    /// that a trusted lease names a durable top, that every presented incarnation is
    /// covered on the media, and that none is presented twice (Tier-0). This runs the
    /// shipped `take_incarnation` from each distinct planned state the model reaches —
    /// the files laid down as the model's page cache, this boot stood in, killed after
    /// the flush's rename, after the flush, or not at all — and requires the files it
    /// leaves and the incarnation it answers to be the model's successor. A power loss is
    /// the model's own step (the files it leaves are the model's media), so the take is
    /// held to the model in every boot the walk reaches.
    ///
    /// Each defect the model was written to catch (`Buggy` 1–3) is the take's own mutant
    /// of the same number; the defective take refines the defective model, which the
    /// Tier-0 suite shows breaks the fence. NEGATIVE CONTROL: the committed model against
    /// each defective take diverges, so the walk is not blind to the defects.
    /// `Buggy = 4` (the will sent while the flush is in flight) is the bridge's order,
    /// not the take's: its files are the committed ones, and `bring_presence_up` calls
    /// the will only after `take_incarnation` returned.
    #[test]
    fn the_incarnation_lease_refines_its_model() {
        use aterm_spec::interp::{reachable_states, with_consts};
        let committed = aterm_models_session::incarnation_lease_model();
        let walk = |buggy: u8| {
            // A defective machine keeps going past its first duplicate, so its space is
            // walked over one power loss (every Tier-0 counterexample needs one), the
            // committed machine's over the committed two.
            let model = match buggy {
                0 => committed.clone(),
                _ => with_consts(&committed, &[("Buggy", i64::from(buggy)), ("MaxBoot", 2)]),
            };
            (
                buggy,
                diverging_takes(&model, &reachable_states(&model), buggy, false),
            )
        };
        // Over one power loss and a higher bound: defect 2 shows only on the retry of a
        // run-out lease, whose committed flush lands above the committed bound.
        let roomier = with_consts(&committed, &[("MaxTop", 7), ("MaxBoot", 2)]);
        let roomier_states = reachable_states(&roomier);
        // The committed walk, the largest space and about half the test's time, on a
        // thread of its own beside the rest: each walk takes in a dir of its own and
        // mostly waits on its file system calls.
        let (walks, controls) = std::thread::scope(|scope| {
            let committed_walk = scope.spawn(|| walk(0));
            let defective: Vec<_> = (1..=3u8).map(walk).collect();
            let controls: Vec<_> = (1..=3u8)
                .map(|mutant| {
                    (
                        mutant,
                        diverging_takes(&roomier, &roomier_states, mutant, true),
                    )
                })
                .collect();
            let committed_walk = committed_walk
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            (
                std::iter::once(committed_walk)
                    .chain(defective)
                    .collect::<Vec<_>>(),
                controls,
            )
        });
        for (buggy, (compared, diverged)) in walks {
            eprintln!("IncarnationLease Buggy = {buggy}: {compared} takes held to the model");
            assert!(
                compared > 100,
                "Buggy = {buggy}: only {compared} takes compared"
            );
            assert!(
                diverged.is_empty(),
                "Buggy = {buggy}: the take is not the model's in {} of {compared}:\n{}",
                diverged.len(),
                diverged.join("\n")
            );
        }
        for (mutant, (compared, diverged)) in controls {
            assert!(
                !diverged.is_empty(),
                "the take's defect {mutant} matched the committed model in all {compared} takes"
            );
        }
    }

    /// The REAL reader's resume point from a `FabricReaderResume` state: the state's
    /// lineage — the model's two slots, `(g - 1, f2)` and `(g, f1)` where they exist — is
    /// built by astream's own [`astream_broker::Lineage::from_steps`] (exactly the
    /// lineages its `Hello` decoder accepts), the
    /// witness names generation `rg` of one log, [`LogWitness::continues`] answers where
    /// it continues on generation `g`, and [`StateDir::forget_log_offsets`] clamps (or
    /// forgets) a topic cursor at `c` against head `h`. Answers the cursor it leaves.
    /// `rebase_at_head` is the NEGATIVE CONTROL: the reader before lineage, which reads
    /// no continuation and forgets.
    fn resume_through(
        st: &StateDir,
        s: &std::collections::BTreeMap<&'static str, i64>,
        top: i64,
        rebase_at_head: bool,
    ) -> i64 {
        let at = |name: &str| u64::try_from(s[name]).expect("a model offset");
        let (g, rg, h, c) = (at("g"), at("rg"), at("h"), at("c"));
        let mut steps = Vec::new();
        if g >= 2 && s["f2"] != top {
            steps.push((g - 1, at("f2")));
        }
        if g >= 1 && s["f1"] != top {
            steps.push((g, at("f1")));
        }
        let lineage = astream_broker::Lineage::from_steps(&steps).expect("a lineage astream reads");
        let log = |generation| astream_broker::LogIdentity {
            id: [0x5a; 16],
            generation,
        };
        let witness = LogWitness {
            high: c,
            id: Some(log(rg)),
        };
        let keep_below = if rebase_at_head {
            None
        } else {
            witness.continues(Some(log(g)), Some(&lineage))
        };
        let mut cursors = BTreeMap::new();
        cursors.insert("t".to_string(), TopicCursor { next: c });
        st.set_topics("s-r", &cursors).expect("write the cursor");
        st.forget_log_offsets(h, Some(log(g)), keep_below, Some(&lineage))
            .expect("forget or clamp");
        i64::try_from(st.topics("s-r")["t"].next).expect("an offset")
    }

    /// TIER-1 OF `FabricReaderResume` (`aterm-models-session`), the reader-resume law:
    /// from EVERY state the model reaches in which the reader decides where to resume, the
    /// real reader ([`resume_through`]: the real `LogWitness::continues` over the state's
    /// lineage, then the real `StateDir::forget_log_offsets`) must leave the cursor the
    /// model's `Decide` picks — the projection is the state with `r` the real cursor and
    /// the decision taken, which the model must admit as its `Decide` step — and the
    /// model's `Resume` from there keeps both laws. The real reader's answer depends only
    /// on `(g, rg, h, c, f1, f2)`, so each distinct one is run once.
    ///
    /// NEGATIVE CONTROLS. The reader that re-bases at the head (the rule before lineage)
    /// is rejected by the committed model; and the real reader matches none of the
    /// defective models (`Buggy` 1–3: resume at the head, at the newest fork only, a
    /// continuation trusted beyond the window), so the walk is not blind to the defects
    /// the registry sweep (`aterm-models`' `non_vacuity`) shows break the law. Its
    /// lineages have at most two steps; astream's own window is run end to end in
    /// `durability.rs`'s `a_reader_older_than_the_lineage_window_forgets`.
    #[test]
    fn the_reader_resume_refines_its_model() {
        use aterm_spec::interp;
        let committed = aterm_models_session::fabric_reader_resume_model();
        let top = committed
            .consts
            .iter()
            .find(|(name, _)| *name == "Top")
            .map(|(_, v)| *v)
            .expect("the model's Top");
        let dir = scratch("resume-tier1");
        let st = StateDir::open(&dir).expect("open");
        let mut real: BTreeMap<[i64; 6], i64> = BTreeMap::new();
        // The real reader's cursor for a state, run once per distinct input.
        let mut run = |s: &std::collections::BTreeMap<&'static str, i64>| {
            let key = ["g", "rg", "h", "c", "f1", "f2"].map(|v| s[v]);
            *real
                .entry(key)
                .or_insert_with(|| resume_through(&st, s, top, false))
        };
        // Walk `model`'s reachable deciding states; answer (compared, real diverged, whether
        // the re-based reader was refused). The committed model is held to every one; a
        // defective model needs only one divergence, and the re-based reader only one
        // refusal, so each runs until it has it.
        let mut walk = |model: &aterm_spec::derive::Model, committed: bool| {
            let (mut compared, mut diverged, mut rebase_refused) = (0, Vec::new(), false);
            for state in interp::reachable_states(model) {
                let deciding = state["pc"] == 0 && state["down"] == 0 && state["rg"] < state["g"];
                if !deciding {
                    continue;
                }
                let cursor = run(&state);
                let decided = |r| {
                    let mut next = state.clone();
                    next.insert("r", r);
                    next.insert("pc", 1);
                    next
                };
                compared += 1;
                let taken = decided(cursor);
                if interp::admits(model, &state, &taken) == Some("Decide") {
                    let mut resumed = taken.clone();
                    assert!(model.fire("Resume", &mut resumed), "Resume from {taken:?}");
                    // A defective model's own `Resume` breaks its law: only the committed
                    // one is held to them.
                    for law in ["NoSkipInWindow", "AnotherLogOutsideWindow"]
                        .into_iter()
                        .filter(|_| committed)
                    {
                        assert!(
                            model.check_invariant(law, &resumed),
                            "{}: the real resume to {cursor} breaks {law} from {state:?}",
                            model.name
                        );
                    }
                } else {
                    diverged.push(format!("{state:?}: real {cursor}"));
                    if !committed {
                        break;
                    }
                }
                if committed && !rebase_refused {
                    let rebase = resume_through(&st, &state, top, true);
                    rebase_refused =
                        interp::admits(model, &state, &decided(rebase)) != Some("Decide");
                }
            }
            (compared, diverged, rebase_refused)
        };

        let (compared, diverged, rebase_refused) = walk(&committed, true);
        eprintln!("FabricReaderResume: {compared} resumes held to the model");
        assert!(compared > 100, "only {compared} resumes compared");
        assert!(
            diverged.is_empty(),
            "the real reader is not the model's in {} of {compared}:\n{}",
            diverged.len(),
            diverged.join("\n")
        );
        assert!(
            rebase_refused,
            "NEGATIVE CONTROL: the reader that re-bases at the head matched the model in all \
             {compared} resumes"
        );
        for buggy in 1..=3 {
            let (compared, diverged, _) = walk(&interp::with_buggy(&committed, buggy), false);
            assert!(
                !diverged.is_empty(),
                "the real reader matched the defective model Buggy = {buggy} in all {compared} \
                 resumes"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real clamp keeps retained floors, but uncertainty must precede every
    /// deletion or cursor rewrite. All files belong to this exclusive directory.
    #[test]
    fn a_generation_clamp_preserves_uncertain_refill_evidence() {
        let dir = aterm_tempfile::TempDir::new().expect("owned state");
        let st = StateDir::open(dir.path()).expect("open");
        let sid = "s-kept";
        let topics = BTreeMap::from([("topic".to_string(), TopicCursor { next: 9 })]);
        st.set_topics(sid, &topics).unwrap();
        st.set_seen_off(sid, 8).unwrap();
        st.set_topic_floors(
            sid,
            &BTreeMap::from([("topic".to_string(), 1), ("gone".to_string(), 7)]),
        )
        .unwrap();
        st.forget_log_offsets(9, None, Some(6), None).unwrap();
        assert_eq!(
            st.topic_floors(sid).unwrap(),
            BTreeMap::from([("topic".into(), 1)])
        );
        assert_eq!(st.topics(sid)["topic"].next, 6);
        assert_eq!(st.seen_off(sid), Some(5));
        st.forget_log_offsets(9, None, None, None).unwrap();
        assert!(st.topic_floors(sid).unwrap().is_empty());

        let mut preserved = Vec::new();
        for keep in [Some(6), None] {
            for bytes in [b"not a floor".as_slice(), b"", b"topic 1\ntopic 2", b"\xff"] {
                st.set_topics(sid, &topics).unwrap();
                st.set_seen_off(sid, 8).unwrap();
                let floor = dir.path().join("unhandled").join(sid);
                std::fs::write(&floor, bytes).unwrap();
                let before_topics = std::fs::read(dir.path().join("topics").join(sid)).unwrap();
                let before_seen = std::fs::read(dir.path().join("seen").join(sid)).unwrap();
                let result = st.forget_log_offsets(9, None, keep, None);
                preserved.push(
                    result.is_err()
                        && std::fs::read(&floor).is_ok_and(|got| got == bytes)
                        && std::fs::read(dir.path().join("topics").join(sid))
                            .is_ok_and(|got| got == before_topics)
                        && std::fs::read(dir.path().join("seen").join(sid))
                            .is_ok_and(|got| got == before_seen),
                );
            }
        }
        assert!(
            preserved.into_iter().all(|kept| kept),
            "CLAMP_DROPPED_UNCERTAIN_REFILL_EVIDENCE"
        );
    }

    /// A whole in-flight entry at `seq`, with a log witness, for the torn-file tests.
    fn in_flight_entry(seq: u64) -> InFlight {
        InFlight {
            pin: PublishPin {
                seq,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
            },
            subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        }
    }

    /// **A TORN FILE AFTER AN UNCLEAN STOP IN AN EARLIER BOOT IS SET ASIDE, AND
    /// THE SAME DAMAGE WITHIN ONE BOOT IS REFUSED** — finding F1 on the real
    /// filesystem ([`StateDir::set_aside_torn`]).
    ///
    /// The last bridge ran in boot 1 and did not stop cleanly (`run` says so, and
    /// its lease names boot 1). Beside a whole in-flight entry and a whole
    /// reservation, a power cut left `inflight/8` cut part-way through its face,
    /// `sent/s-a.2` cut part-way through its identity and `keys/s-a` cut inside
    /// its line, and tore one file of every other kind to nothing — `logpin`
    /// too. Read in boot 2, every torn file of a [`TornKind`] is moved into
    /// `quarantine/` with its exact bytes, the readers find the whole ones and
    /// nothing of the torn ones, a second sweep moves nothing, and a name
    /// already taken there gets a suffix rather than overwrite; `logpin`, which
    /// holds what no reboot ends, stays where the mirror refuses it.
    /// NEGATIVE CONTROL: the same dir read in boot 1 again — a `kill -9`, which
    /// the page cache outlived — moves nothing, and the readers refuse the
    /// damage as they always did.
    #[test]
    fn a_torn_file_after_an_unclean_stop_is_set_aside_and_within_a_boot_refused() {
        for unclean in [true, false] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("torn")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the last bridge's run, in boot 1");
            st.take_incarnation(0).expect("its attach");
            let whole = in_flight_entry(7);
            st.set_in_flight(&whole).expect("a whole entry");
            let pin = PublishPin {
                identity: Some(PublishIdentity {
                    subject: whole.subject.clone(),
                    digest: "ab".repeat(32),
                }),
                ..whole.pin.clone()
            };
            st.set_post_pin("s-a", 1, &pin)
                .expect("a whole reservation");
            // THE CUT: a prefix of what each write held, as a power cut may leave it.
            let entry = in_flight_entry(8).render();
            let cut_entry = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
            std::fs::write(root.join("inflight/8"), cut_entry).expect("cut entry");
            let reservation = PublishPin {
                seq: 9,
                ..pin.clone()
            }
            .render();
            let cut_pin = &reservation.as_bytes()[..reservation.len() - 20];
            std::fs::write(root.join("sent/s-a.2"), cut_pin).expect("cut reservation");
            for (place, name) in [
                ("acks", "s-a.4"),
                ("keys", "s-a"),
                ("unhandled", "s-a"),
                ("", "logpin"),
            ] {
                std::fs::create_dir_all(root.join(place)).expect("its dir");
                std::fs::write(root.join(place).join(name), b"").expect("torn to nothing");
            }
            std::fs::write(root.join("keys/s-a"), b"k1 12 identity /f/x").expect("cut key");

            if unclean {
                st.set_inc_seam(in_boot(2));
                assert!(
                    st.torn_files_possible().expect("run record reads"),
                    "boot 2, after boot 1's bridge stopped uncleanly"
                );
                let moved = st.set_aside_torn().expect("the sweep");
                let from: Vec<&str> = moved.iter().map(|m| m.from.as_str()).collect();
                assert_eq!(
                    from,
                    [
                        "inflight/8",
                        "sent/s-a.2",
                        "acks/s-a.4",
                        "keys/s-a",
                        "unhandled/s-a",
                    ],
                    "every torn file, kind by kind"
                );
                for m in &moved {
                    assert_eq!(m.to, format!("{QUARANTINE}/{}", m.from.replace('/', ".")));
                    assert!(
                        !root.join(&m.from).exists(),
                        "{} is moved, not copied",
                        m.from
                    );
                }
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                    cut_entry,
                    "its exact bytes, never deleted"
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/sent.s-a.2")).expect("kept"),
                    cut_pin
                );
                assert_eq!(
                    st.in_flight().expect("the journal reads"),
                    vec![(7, whole.clone())]
                );
                assert_eq!(st.post_pin("s-a", 2).expect("reads"), None);
                assert_eq!(st.post_pin("s-a", 1).expect("reads"), Some(pin.clone()));
                assert_eq!(st.receipt_pin("s-a", 4).expect("reads"), None);
                assert!(st.topic_floors("s-a").expect("reads").is_empty());
                assert_eq!(
                    st.logpin().expect("reads").as_deref(),
                    Some(""),
                    "the mirror's pin stays, for the mirror to refuse"
                );
                st.publication_offset_resets(None)
                    .expect("a log move's reset reads");
                assert!(st.set_aside_torn().expect("again").is_empty(), "idempotent");
                // A second cut at a name the quarantine holds keeps both.
                std::fs::write(root.join("inflight/8"), b"").expect("torn again");
                assert_eq!(
                    st.publish_quarantine_file("quarantine/inflight.8", b"replacement")
                        .expect_err("the actual publication cannot replace an occupant")
                        .kind(),
                    io::ErrorKind::AlreadyExists
                );
                assert_eq!(
                    std::fs::read(root.join("inflight/8")).expect("source remains"),
                    b""
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("sentinel remains"),
                    cut_entry
                );
                let again = st.set_aside_torn().expect("the next boot's sweep");
                assert_eq!(again.len(), 1);
                assert_eq!(again[0].to, format!("{QUARANTINE}/inflight.8.1"));
                assert_eq!(
                    std::fs::read(root.join("quarantine/inflight.8")).expect("kept"),
                    cut_entry,
                    "the first one is not overwritten"
                );
            } else {
                assert!(
                    !st.torn_files_possible().expect("run record reads"),
                    "the same boot: nothing a kill does tears a file"
                );
                assert!(st.set_aside_torn().expect("no sweep").is_empty());
                assert!(!root.join(QUARANTINE).exists(), "nothing moved");
                assert_eq!(
                    st.in_flight().expect_err("refused").kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    st.post_pin("s-a", 2).expect_err("refused").kind(),
                    io::ErrorKind::InvalidData
                );
                assert!(st.publication_offset_resets(None).is_err());
                assert!(st.topic_floors("s-a").is_err());
                assert!(st
                    .logpin()
                    .expect("read")
                    .is_some_and(|t| crate::mirror::Pin::parse(&t).is_err()));
            }
        }
    }

    /// A LINK PLANTED AT THE QUARANTINE'S NAME IS NEVER WRITTEN THROUGH: the sweep
    /// refuses, nothing lands where the link points, and the torn file stays where
    /// its reader refuses it.
    #[test]
    fn a_planted_quarantine_link_is_refused_and_nothing_moves() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("planted")
            .tempdir_in("/tmp")
            .expect("scratch");
        let elsewhere = aterm_tempfile::Builder::new()
            .prefix("elsewhere")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.begin_run()
            .expect("a run in boot 1, never stopped cleanly");
        st.set_inc_seam(in_boot(2));
        std::fs::write(root.join("sent/s-a.1"), b"").expect("torn to nothing");
        std::os::unix::fs::symlink(elsewhere.path(), root.join(QUARANTINE)).expect("plant");
        let refused = st.set_aside_torn().expect_err("a link is no quarantine");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        assert!(root.join("sent/s-a.1").exists(), "nothing moved");
        assert_eq!(
            std::fs::read_dir(elsewhere.path()).expect("list").count(),
            0,
            "nothing written through the link"
        );
    }

    /// Neither a sweep nor a clean close follows a planted source parent.
    /// The pending write and run marker survive refusal; an outside sentinel
    /// is never removed or rewritten, for a link or a non-directory occupant.
    #[test]
    fn a_planted_source_parent_is_refused_before_sweep_or_clean_close() {
        for kind in TornKind::ALL {
            for linked in [true, false] {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("source-parent")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let elsewhere = aterm_tempfile::Builder::new()
                    .prefix("source-target")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let st = StateDir::open(dir.path()).expect("open");
                st.set_inc_seam(in_boot(1));
                st.begin_run().expect("open run");
                let name = if kind == TornKind::InFlight {
                    "8"
                } else {
                    "s-a.1"
                };
                let rel = format!("{}/{name}", kind.place());
                st.write(&rel, "12").expect("a pending write");
                std::fs::remove_file(st.path(&rel)).expect("remove owned fixture");
                std::fs::remove_dir(st.path(kind.place())).expect("replace owned parent");
                let sentinel = elsewhere.path().join(name);
                std::fs::write(&sentinel, b"external evidence").expect("outside sentinel");
                if linked {
                    std::os::unix::fs::symlink(elsewhere.path(), st.path(kind.place()))
                        .expect("plant parent link");
                } else {
                    std::fs::write(st.path(kind.place()), b"parent sentinel")
                        .expect("plant non-directory");
                }
                let run = std::fs::read(st.path("run")).expect("original run");
                st.set_inc_seam(in_boot(2));
                assert_eq!(
                    st.set_aside_torn()
                        .expect_err("refuse source parent")
                        .kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    st.end_run_clean().expect_err("refuse clean close").kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    std::fs::read(&sentinel).expect("outside remains"),
                    b"external evidence"
                );
                assert_eq!(std::fs::read(st.path("run")).expect("run remains"), run);
                assert!(st.unflushed.lock().expect("pending").contains(&rel));
                assert!(
                    !st.path(QUARANTINE).exists(),
                    "no evidence publication before admission"
                );
                if linked {
                    assert!(std::fs::symlink_metadata(st.path(kind.place()))
                        .expect("link remains")
                        .is_symlink());
                } else {
                    assert_eq!(
                        std::fs::read(st.path(kind.place())).expect("parent remains"),
                        b"parent sentinel"
                    );
                }
            }
        }
    }

    /// An existing marker that cannot be read is never an older build's
    /// absent marker. A genuine missing run has the legacy positive control;
    /// invalid bytes, a directory, a dangling link, and (off root) an unreadable
    /// clean marker preserve all evidence.
    #[test]
    fn an_existing_run_marker_never_authorizes_legacy_fallback() {
        use std::os::unix::fs::PermissionsExt as _;
        for occupant in ["invalid", "directory", "dangling", "unreadable"] {
            if occupant == "unreadable" && aterm_uds::peer::our_uid() == 0 {
                continue; // Root can read a mode-000 file.
            }
            let dir = aterm_tempfile::Builder::new()
                .prefix("run-marker")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(in_boot(1));
            st.take_incarnation(0).expect("legacy lease");
            st.set_inc_seam(in_boot(2));
            assert!(st
                .torn_files_possible()
                .expect("genuine absence uses legacy lease"));
            let run = st.path("run");
            let clean = LastRun {
                boot: Some(boot_id(1)),
                clean: true,
            }
            .render();
            match occupant {
                "invalid" => std::fs::write(&run, b"not a run\n").expect("invalid marker"),
                "directory" => std::fs::create_dir(&run).expect("nonregular marker"),
                "dangling" => std::os::unix::fs::symlink(dir.path().join("missing"), &run)
                    .expect("dangling marker"),
                _ => {
                    std::fs::write(&run, &clean).expect("valid clean marker");
                    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o000))
                        .expect("make unreadable");
                }
            }
            std::fs::write(st.path("sent/s-a.1"), b"torn evidence").expect("candidate");
            assert!(st.last_run().is_err(), "the occupant is not absence");
            assert!(st.torn_files_possible().is_err(), "no legacy verdict");
            assert!(st.set_aside_torn().is_err(), "no sweep");
            assert!(st.begin_run().is_err(), "no replacement on start");
            assert!(st.end_run_clean().is_err(), "no replacement on stop");
            assert_eq!(
                std::fs::read(st.path("sent/s-a.1")).expect("source remains"),
                b"torn evidence"
            );
            assert!(!st.path(QUARANTINE).exists());
            match occupant {
                "invalid" => assert_eq!(
                    std::fs::read(&run).expect("invalid evidence remains"),
                    b"not a run\n"
                ),
                "directory" => assert!(std::fs::symlink_metadata(&run)
                    .expect("directory remains")
                    .is_dir()),
                "dangling" => assert_eq!(
                    std::fs::read_link(&run).expect("link remains"),
                    dir.path().join("missing")
                ),
                _ => {
                    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o600))
                        .expect("restore owned marker");
                    assert_eq!(
                        std::fs::read_to_string(&run).expect("clean evidence remains"),
                        clean
                    );
                }
            }
        }
    }

    /// **ONLY AN UNCLEAN STOP IN AN EARLIER BOOT CAN HAVE TORN A FILE**
    /// ([`StateDir::torn_files_possible`], astream's unclean-stop rule), read off
    /// the durable `run` record — and, for a dir without one, off its lease's
    /// boot. NOT within one boot, on the boot volume or off it, nor in a boot
    /// nobody can name (no evidence of a reboot is no sweep); NOT after a clean
    /// stop; and an existing record that does not read is refused.
    #[test]
    fn only_an_unclean_stop_in_an_earlier_boot_can_have_torn_a_file() {
        let unnamed = IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        };
        for off_volume in [false, true] {
            let seam = |b| IncSeam {
                off_volume,
                ..in_boot(b)
            };
            let dir = aterm_tempfile::Builder::new()
                .prefix("unclean")
                .tempdir_in("/tmp")
                .expect("scratch");
            let st = StateDir::open(dir.path()).expect("open");
            st.set_inc_seam(seam(1));
            assert!(
                !st.torn_files_possible().expect("missing run"),
                "a new dir: nothing ran"
            );
            st.begin_run().expect("a run in boot 1");
            assert_eq!(
                st.last_run().expect("run record reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: false
                })
            );
            st.take_incarnation(0).expect("its attach");
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "the same boot, killed"
            );
            st.set_inc_seam(seam(2));
            assert!(
                st.torn_files_possible().expect("run reads"),
                "boot 2, the run never closed"
            );
            st.set_inc_seam(unnamed);
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "a boot nobody can name"
            );
            st.set_inc_seam(seam(1));
            st.end_run_clean().expect("a clean stop in boot 1");
            assert_eq!(
                st.last_run().expect("run record reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: true
                })
            );
            st.set_inc_seam(seam(2));
            assert!(
                !st.torn_files_possible().expect("run reads"),
                "boot 2, after a clean stop"
            );
            st.begin_run().expect("boot 2's run");
            st.set_inc_seam(seam(3));
            assert!(
                st.torn_files_possible().expect("run reads"),
                "boot 3, boot 2's run never closed"
            );
            // A DIR WITHOUT THE RECORD (a build before it) is judged by its
            // lease's boot, which a dir off the boot volume never names.
            std::fs::remove_file(dir.path().join("run")).expect("no record");
            st.set_inc_seam(seam(1));
            st.take_incarnation(0)
                .expect("an older build's attach, boot 1");
            st.set_inc_seam(seam(2));
            assert_eq!(
                st.torn_files_possible().expect("legacy missing run"),
                !off_volume,
                "the lease's boot, where it names one"
            );
        }
    }

    /// **THE QUARANTINE'S COUNT IS THE TORN FILES THE OWNER HAS NOT YET TAKEN
    /// AWAY, AND NOTHING ELSE** ([`StateDir::quarantined_torn`]): what tells
    /// aterm its entry for the owner's look has ended (`link quarantine
    /// torn=0`). A dir nothing was set aside in has no quarantine (`None`).
    /// After boot 1's unclean stop, boot 2's start sets a cut in-flight entry
    /// aside and retires a whole one: the count is the torn one alone — not
    /// the retired entry, a hidden temp a move stages, or a link planted
    /// there. The owner removes the torn file, and the count is 0 while the
    /// retired entry stays. A file planted at the quarantine's name is refused.
    #[test]
    fn the_quarantine_counts_the_torn_files_the_owner_has_not_taken_away() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("held")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        assert_eq!(st.quarantined_torn().expect("no quarantine reads"), None);
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("boot 1's run");
        st.set_in_flight(&in_flight_entry(7))
            .expect("a whole entry");
        let entry = in_flight_entry(8).render();
        std::fs::write(
            root.join("inflight/8"),
            &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3],
        )
        .expect("a cut entry");
        st.set_inc_seam(in_boot(2));
        assert_eq!(st.set_aside_torn().expect("the sweep").len(), 1);
        assert_eq!(
            st.retire_in_flight_of_an_earlier_boot()
                .expect("the retirement")
                .len(),
            1
        );
        let quarantine = root.join(QUARANTINE);
        std::fs::write(quarantine.join(".inflight.9.tmp.1"), b"staged").expect("a temp");
        std::os::unix::fs::symlink("inflight.8", quarantine.join("a-link")).expect("a link");
        assert_eq!(
            st.quarantined_torn().expect("the count"),
            Some(1),
            "the torn entry alone"
        );
        std::fs::remove_file(quarantine.join("inflight.8")).expect("the owner removes it");
        assert_eq!(
            st.quarantined_torn().expect("the count"),
            Some(0),
            "none torn left, the retired entry kept"
        );
        assert!(quarantine
            .join(format!("{RETIRED_PREFIX}inflight.7"))
            .exists());
        std::fs::remove_dir_all(&quarantine).expect("the owner empties it");
        std::fs::write(&quarantine, b"planted").expect("a file at the name");
        assert_eq!(
            st.quarantined_torn().expect_err("not a directory").kind(),
            io::ErrorKind::InvalidData
        );
    }

    /// **AFTER A CLEAN STOP, DAMAGE IS A BUG'S, AND A REBOOT DOES NOT CHANGE
    /// THAT** — the review of finding F1's fix, against astream's clean close.
    /// A bridge in boot 1 writes a reservation and an in-flight entry and is
    /// killed; its successor in the same boot inherits them unflushed, writes a
    /// receipt pin, and stops cleanly: every one of the three is flushed where
    /// it is (no copy written, nothing renamed), each directory whose entries
    /// moved is flushed, and only then is `run` written, saying `clean` — the
    /// one file the stop writes. A bug then damages the reservation. In boot
    /// 2 nothing is set aside and its reader refuses
    /// it, fail-closed. NEGATIVE CONTROL: the same run killed instead of
    /// stopped — boot 2 sets the damage aside, as after a power cut.
    #[test]
    fn after_a_clean_stop_damage_is_refused_across_a_reboot() {
        for clean in [true, false] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("clean")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let killed = StateDir::open(root).expect("open");
            killed.set_inc_seam(in_boot(1));
            killed.begin_run().expect("the first run");
            let entry = in_flight_entry(7);
            killed.set_in_flight(&entry).expect("an entry");
            killed
                .set_post_pin("s-a", 1, &entry.pin)
                .expect("a reservation");
            drop(killed);
            let st = StateDir::open(root).expect("its successor, in the same boot");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the second run");
            st.set_receipt_pin("s-a", 4, &entry.pin)
                .expect("a receipt pin");
            if clean {
                st.refuse_writes_of(Some("sent/s-a.1"));
                assert!(
                    st.end_run_clean().is_err(),
                    "a controlled failed flush attempt"
                );
                assert_eq!(
                    st.last_run().expect("run reads").map(|run| run.clean),
                    Some(false)
                );
                st.refuse_writes_of(None);
                let before = st.ops();
                let flushed = st.end_run_clean().expect("a clean stop");
                let ops = st.ops() - before;
                assert_eq!(
                    (ops.replaces, ops.renames),
                    (1, 1),
                    "the three files of the two runs flushed where they are: only `run` written"
                );
                // ORDERED, ENDED BY ONE FULL FLUSH (sim item S6): a barrier
                // a file, one a directory whose entries moved (`inflight`,
                // `sent`, `acks`), one the record's bytes, and the root's full
                // flush last.
                assert_eq!((ops.barriers, ops.fsyncs), (3 + 3 + 1, 1));
                assert_eq!(
                    flushed,
                    Flushed {
                        files: 3,
                        dirs: 3,
                        ordered: 7,
                        full: 1,
                    }
                );
                assert_eq!(
                    st.last_run().expect("run reads").map(|run| run.clean),
                    Some(true)
                );
            }
            std::fs::write(root.join("sent/s-a.1"), b"7 key identity\n").expect("a bug's damage");
            st.set_inc_seam(in_boot(2));
            let moved = st.set_aside_torn().expect("the sweep");
            if clean {
                assert!(moved.is_empty(), "nothing a clean stop left can be torn");
                assert!(st.post_pin("s-a", 1).is_err(), "refused, fail-closed");
            } else {
                assert_eq!(
                    moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                    ["sent/s-a.1"],
                    "CONTROL: a run that never closed is judged unclean"
                );
            }
        }
    }

    /// **WHAT A FLUSH IN PLACE PASSES OVER, AND WHAT IT REFUSES**
    /// ([`StateDir::flush_in_place`], as its doc says): a regular file is
    /// flushed where it is (`true`, one barrier); a name gone since and a
    /// directory planted at it are passed over (`false`, nothing flushed); a
    /// link planted at it is refused by the no-follow open — the error, so
    /// the stop that meets one claims nothing clean — and what it names is
    /// never flushed through.
    #[test]
    fn a_flush_in_place_passes_over_what_is_gone_and_refuses_a_link() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("in-place")
            .tempdir_in("/tmp")
            .expect("scratch");
        let elsewhere = aterm_tempfile::Builder::new()
            .prefix("in-place-target")
            .tempdir_in("/tmp")
            .expect("scratch");
        let st = StateDir::open(dir.path()).expect("open");
        st.set_inc_seam(in_boot(1));
        st.write("keys/s-a", "k\n").expect("a key file");
        let flush = aterm_durable::Flush::Ordered;
        let before = st.ops();
        assert!(st
            .flush_in_place("keys/s-a", flush)
            .expect("a regular file"));
        assert_eq!((st.ops() - before).barriers, 1, "flushed where it is");
        let before = st.ops();
        assert!(!st.flush_in_place("keys/s-b", flush).expect("gone"));
        std::fs::create_dir(st.path("keys/s-c")).expect("a planted directory");
        assert!(!st.flush_in_place("keys/s-c", flush).expect("a directory"));
        let target = elsewhere.path().join("outside");
        std::fs::write(&target, b"outside").expect("what a link would name");
        std::os::unix::fs::symlink(&target, st.path("keys/s-d")).expect("a planted link");
        assert!(
            st.flush_in_place("keys/s-d", flush).is_err(),
            "a link is the open's error, never passed over as if gone"
        );
        assert_eq!(
            (st.ops() - before).barriers,
            0,
            "nothing passed over or refused is flushed"
        );
    }

    /// **FINDING F2, DECIDED: A WHOLE IN-FLIGHT ENTRY OF AN EARLIER BOOT IS
    /// RETIRED, NEVER PUBLISHED AGAIN** — on the real filesystem
    /// ([`StateDir::retire_in_flight_of_an_earlier_boot`]).
    ///
    /// The last bridge ran in boot 1 and did not stop cleanly, leaving two whole
    /// in-flight entries beside one a power cut cut short. In boot 2 the
    /// retirement moves the two whole ones into `quarantine/` under `retired.`
    /// with their exact bytes, and leaves the cut one to the torn sweep (whose
    /// reader refuses it until then): the retirement never takes what that
    /// sweep judges. The journal then reads empty — nothing for the attach to
    /// publish again — a second retirement moves nothing, and an entry at a
    /// name the quarantine holds gets a suffix rather than overwrite it.
    /// NEGATIVE CONTROLS: in boot 1 again (a `kill -9`, which the page cache
    /// outlived) and after a clean stop (which flushed every unlink, so an
    /// entry there is one still in flight), nothing is retired, and the
    /// entries stay for the attach to publish again, as before.
    #[test]
    fn a_whole_in_flight_entry_of_an_earlier_boot_is_retired_and_within_a_boot_kept() {
        for (case, boot, clean) in [
            ("an unclean stop, then a reboot", 2, false),
            ("a kill in the same boot", 1, false),
            ("a clean stop, then a reboot", 2, true),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("retire")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("boot 1's run");
            let (seven, nine) = (in_flight_entry(7), in_flight_entry(9));
            st.set_in_flight(&seven).expect("a whole entry");
            st.set_in_flight(&nine).expect("another");
            let on_disk = std::fs::read(root.join("inflight/7")).expect("its bytes");
            if clean {
                st.end_run_clean().expect("a clean stop");
            }
            let entry = in_flight_entry(8).render();
            let cut = &entry.as_bytes()[..entry.find("/s-a/").expect("the face") + 3];
            std::fs::write(root.join("inflight/8"), cut).expect("a cut entry");
            st.set_inc_seam(in_boot(boot));
            let retired = st
                .retire_in_flight_of_an_earlier_boot()
                .expect("the retirement");
            if boot == 1 || clean {
                assert!(retired.is_empty(), "{case}: nothing is retired");
                assert!(!root.join(QUARANTINE).exists(), "{case}: nothing moved");
                for seq in [7, 9] {
                    assert!(
                        root.join(format!("inflight/{seq}")).exists(),
                        "{case}: inflight/{seq} stays, for the attach to publish again"
                    );
                }
                continue;
            }
            assert_eq!(
                retired,
                [7, 9]
                    .map(|seq| SetAside {
                        from: format!("inflight/{seq}"),
                        to: format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.{seq}"),
                    })
                    .to_vec(),
                "every whole entry, and only those"
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
                on_disk,
                "its exact bytes, never deleted"
            );
            assert_eq!(
                st.in_flight()
                    .expect_err("the cut one, the torn sweep's")
                    .kind(),
                io::ErrorKind::InvalidData
            );
            let torn = st.set_aside_torn().expect("the torn sweep");
            assert_eq!(
                torn.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                ["inflight/8"]
            );
            assert!(
                st.in_flight().expect("the journal reads").is_empty(),
                "nothing left for the attach to publish again"
            );
            assert!(
                st.retire_in_flight_of_an_earlier_boot()
                    .expect("again")
                    .is_empty(),
                "idempotent"
            );
            st.set_in_flight(&seven)
                .expect("an entry at a retired name");
            let again = st.retire_in_flight_of_an_earlier_boot().expect("again");
            assert_eq!(
                again.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(),
                [format!("{QUARANTINE}/{RETIRED_PREFIX}inflight.7.1")]
            );
            assert_eq!(
                std::fs::read(root.join("quarantine/retired.inflight.7")).expect("kept"),
                on_disk,
                "the first one is not overwritten"
            );
        }
    }

    /// **AN ATTACH OF THIS BOOT ON RECORD STOPS THE RETIREMENT** — the review
    /// of finding F2's fix ([`StateDir::attached_in_this_boot`]). The last
    /// bridge of boot 1 attached and did not stop cleanly, leaving a whole
    /// entry. In boot 2 the disk refuses `begin_run`, so `run` still says
    /// "open, boot 1" — and then an attach of boot 2 takes an incarnation
    /// anyway (an older build's, which did not wait for `run`), its lease
    /// naming boot 2: the entry may be a live session's, and nothing is moved.
    /// NEGATIVE CONTROLS: with no attach in boot 2 (the lease boot 1 wrote),
    /// and off the boot volume (a lease that names no boot), the entry is
    /// retired, as the first bridge of a boot retires it.
    #[test]
    fn an_attach_of_this_boot_on_record_stops_the_retirement() {
        for (case, off_volume, attached_in_boot_2) in [
            ("a lease of boot 2", false, true),
            ("a lease of boot 1", false, false),
            ("off the boot volume", true, true),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("retire-lease")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            let seam = |b| IncSeam {
                off_volume,
                ..in_boot(b)
            };
            st.set_inc_seam(seam(1));
            st.begin_run().expect("boot 1's run");
            st.take_incarnation(0).expect("boot 1's attach");
            st.set_in_flight(&in_flight_entry(7))
                .expect("a whole entry");
            st.set_inc_seam(seam(2));
            st.refuse_writes_of(Some("run"));
            assert_eq!(
                st.begin_run().expect_err("the disk refuses `run`").kind(),
                io::ErrorKind::StorageFull,
                "{case}"
            );
            assert_eq!(
                st.last_run().expect("run reads"),
                Some(LastRun {
                    boot: Some(boot_id(1)),
                    clean: false
                }),
                "{case}: `run` still names boot 1"
            );
            if attached_in_boot_2 {
                st.take_incarnation(0).expect("boot 2's attach");
            }
            let retired = st
                .retire_in_flight_of_an_earlier_boot()
                .expect("the retirement");
            if attached_in_boot_2 && !off_volume {
                assert!(retired.is_empty(), "{case}: nothing is retired");
                assert!(root.join("inflight/7").exists(), "{case}: the entry stays");
            } else {
                assert_eq!(
                    retired.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
                    ["inflight/7"],
                    "{case}: NEGATIVE CONTROL"
                );
            }
        }
    }

    /// **A CUT IN-FLIGHT ENTRY THAT STILL READS IS SET ASIDE TOO** — the review
    /// of finding F1's fix. A real file is cut part-way, not only to nothing,
    /// and three cuts of a witnessed reply's entry read whole: just before its
    /// `log=` line (no witness), inside its hex body at an even length (a
    /// shorter body, no witness), and inside the witness's generation (a lower
    /// one). Kept, the first two are unwitnessed references that
    /// `Bridge::settle_in_flight` refuses at every attach — finding F1's wedge
    /// again. After an unclean stop in an earlier boot each is moved aside,
    /// with its exact bytes, and the whole entry beside them stays.
    #[test]
    fn a_cut_in_flight_entry_that_still_reads_is_set_aside_after_an_unclean_reboot() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("cut")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        let reply = |seq| InFlight {
            body: b"v=1 t=5 re=42 text=a reply".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.12", "5a".repeat(16))),
            ..in_flight_entry(seq)
        };
        let whole = reply(7);
        st.set_in_flight(&whole).expect("a whole entry");
        for seq in [8, 9, 10] {
            st.set_in_flight(&reply(seq))
                .expect("an entry the cut will take");
        }
        // Each cut, at a place in its own entry's text.
        let at = |seq: u64, text: &str| match seq {
            // just before `log=`
            8 => text.find("log=").expect("its witness"),
            // six bytes into the body: an even hex length
            9 => text.match_indices('\n').nth(1).expect("its face line").0 + 1 + 12,
            // inside the generation: `.12\n` to `.1`
            _ => text.len() - 2,
        };
        for seq in [8, 9, 10] {
            let path = root.join(format!("inflight/{seq}"));
            let text = std::fs::read_to_string(&path).expect("as written");
            let cut = &text[..at(seq, &text)];
            std::fs::write(&path, cut).expect("the cut");
            let read = InFlight::parse(seq, cut).expect("PRECONDITION: the cut still reads");
            assert_eq!(read.log.is_some(), seq == 10, "{cut:?}");
        }
        st.set_inc_seam(in_boot(2));
        let moved = st.set_aside_torn().expect("the sweep");
        assert_eq!(
            moved.iter().map(|m| m.from.as_str()).collect::<Vec<_>>(),
            ["inflight/10", "inflight/8", "inflight/9"],
            "every cut entry, and not the whole one"
        );
        assert_eq!(st.in_flight().expect("reads"), vec![(7, whole)]);
    }

    /// **WITHIN ONE BOOT NOTHING IS SET ASIDE, ON ANY VOLUME** — the review of
    /// finding F1's fix: a boot nobody can name (and a dir off the boot volume)
    /// gives the page cache no voucher, but no reboot either. Damage there is
    /// a bug, and a reservation a sweep set aside would publish its post again
    /// at a fresh sequence — a second copy — so the reader refuses it, as it
    /// always did.
    #[test]
    fn damage_in_a_boot_nobody_can_name_is_refused_not_set_aside() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("unnamed")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(IncSeam {
            boot: Some(None),
            ..IncSeam::default()
        });
        st.take_incarnation(0).expect("an attach");
        std::fs::create_dir_all(root.join("sent")).expect("sent/");
        std::fs::write(root.join("sent/s-a.1"), b"12 key ident").expect("a damaged reservation");
        assert!(st.set_aside_torn().expect("no sweep").is_empty());
        assert!(st.post_pin("s-a", 1).is_err(), "refused, fail-closed");
    }

    /// **THE SWEEP OF THE TORN KINDS NEVER MOVES `logpin`** — the review of
    /// finding F1's fix: the pin holds the node's lifetime total of mirrored
    /// bytes (ruling Q13's cap) and its alias key (ruling Q6), which no reboot
    /// ends, so it is judged with its durable copies, by its own step
    /// ([`StateDir::reconcile_logpin`]), and with no copies on the media (a dir
    /// only an older build mirrored in) it stays where the mirror refuses it.
    #[test]
    fn a_torn_logpin_is_left_for_the_mirror_to_refuse() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(in_boot(1));
        st.take_incarnation(0).expect("the last attach, in boot 1");
        std::fs::write(root.join("logpin"), b"").expect("torn to nothing");
        st.set_inc_seam(in_boot(2));
        assert!(st.set_aside_torn().expect("the sweep").is_empty());
        assert_eq!(
            st.reconcile_logpin().expect("the reconcile"),
            Reconciled::Kept,
            "no floor on the media: the pin holds the only copies"
        );
        assert_eq!(st.logpin().expect("read").as_deref(), Some(""));
    }

    /// A pin of one batch, key `key`, total `total`.
    fn mirror_pin(key: [u8; 16], total: u64) -> crate::mirror::Pin {
        crate::mirror::Pin {
            inc: 1,
            total,
            key: Some(key),
            slots: vec![crate::mirror::PinSlot {
                pseq: 9,
                sid: "s-a".to_string(),
                at: aterm_slog::BusCursor::new(4, 2, 1),
                body: "v=2 t=1 ep=4 seq=2 ms=1 op=open node=t1 k=turn src=screen conf=strong"
                    .to_string(),
            }],
        }
    }

    /// **RULINGS Q6 AND Q13 ACROSS A POWER CUT, ON THE REAL FILESYSTEM: A TORN
    /// PIN WITH ITS DURABLE COPIES IS SET ASIDE AFTER AN UNCLEAN STOP IN AN
    /// EARLIER BOOT, A WHOLE ONE WRITTEN FROM THEM IN ITS PLACE, AND A WHOLE
    /// ONE IS RAISED TO THE FLOOR** ([`StateDir::reconcile_logpin`]).
    ///
    /// The last bridge ran in boot 1, wrote the alias key to `logkey` and a
    /// floor to `logfloor` (both durable), pinned a batch, and did not stop
    /// cleanly; a power cut left the pin cut short of its last bytes. In boot
    /// 2 the pin is moved into `quarantine/logpin` with its exact bytes, the
    /// copies stay, and a whole pin written from them, durably, takes its
    /// place: no batch, the key, the floor — of the version an older build
    /// refuses, so one that ran here next stops rather than find no pin and
    /// mint a second key. A second reconcile changes nothing; a pin gone
    /// again (a bridge killed between the move and the write) is written
    /// again. A whole pin below the floor is rewritten at the floor, its
    /// batch and key kept. NEGATIVE CONTROLS: the same torn pin read in boot
    /// 1 again (a `kill -9`), with no floor on the media, with no key in
    /// `logkey` (the pin may hold the only copy of one), after a clean stop —
    /// which flushes the pin — and an OLDER BUILD'S torn pin over the copies
    /// (it mirrored past a floor it does not raise), each is kept, for the
    /// mirror to refuse.
    #[test]
    fn a_torn_logpin_with_its_copies_is_set_aside_after_an_unclean_stop_and_kept_otherwise() {
        let key = [0x5a; 16];
        for (world, key_copy, floor_copy, reboot, clean, older) in [
            (
                "an unclean stop, then a reboot",
                true,
                true,
                true,
                false,
                false,
            ),
            ("a kill, in the same boot", true, true, false, false, false),
            ("no floor on the media", true, false, true, false, false),
            ("no key in logkey", false, true, true, false, false),
            ("a clean stop, then a reboot", true, true, true, true, false),
            (
                "an older build's pin over the copies",
                true,
                true,
                true,
                false,
                true,
            ),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("logpin")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            st.set_inc_seam(in_boot(1));
            st.begin_run().expect("the last bridge's run, in boot 1");
            st.take_incarnation(0).expect("its attach");
            if key_copy {
                st.set_logkey(&key).expect("the key's copy");
            }
            if floor_copy {
                assert_eq!(st.raise_logfloor(5000).expect("the floor"), 5000);
            }
            st.set_logpin(&mirror_pin(key, 3000).render())
                .expect("the first pin");
            let pin = mirror_pin(key, 4000);
            st.set_logpin(&pin.render()).expect("the pin");
            if clean {
                let before = st.ops();
                st.end_run_clean().expect("a clean stop");
                let ops = st.ops() - before;
                assert_eq!(
                    (ops.replaces, ops.barriers, ops.fsyncs),
                    (1, 4, 1),
                    "{world}: the pin flushed where it is, `sent/` (which every state dir has) \
                     and the root ordered after it, then `run`'s bytes ordered and the root \
                     flushed in full"
                );
            }
            let mut text = pin.render();
            if older {
                text = text.replacen("logpin v=2", "logpin v=1", 1);
            }
            let cut = &text.as_bytes()[..text.len() - 7];
            std::fs::write(root.join("logpin"), cut).expect("cut short");
            if reboot {
                st.set_inc_seam(in_boot(2));
            }
            let done = st.reconcile_logpin().expect("the reconcile");
            if world == "an unclean stop, then a reboot" {
                assert_eq!(
                    done,
                    Reconciled::SetAside(SetAside {
                        from: "logpin".to_string(),
                        to: format!("{QUARANTINE}/logpin"),
                    }),
                    "{world}"
                );
                assert_eq!(
                    std::fs::read(root.join("quarantine/logpin")).expect("kept"),
                    cut,
                    "its exact bytes, never deleted"
                );
                let placed = st.logpin().expect("reads").expect("a pin in its place");
                assert!(
                    crate::mirror::Pin::by_this_build(placed.as_bytes()),
                    "of the version an older build refuses: {placed:?}"
                );
                let from_the_copies = crate::mirror::Pin {
                    key: Some(key),
                    total: 5000,
                    ..crate::mirror::Pin::default()
                };
                assert_eq!(
                    crate::mirror::Pin::parse(&placed),
                    Ok(from_the_copies.clone()),
                    "no batch, logkey's key, logfloor's total"
                );
                assert!(
                    !st.unflushed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(LOGPIN),
                    "written durably: no cut brings back the absence"
                );
                assert_eq!(
                    st.logkey().expect("reads"),
                    Some(key),
                    "the key's copy stays"
                );
                assert_eq!(
                    st.logfloor().expect("reads"),
                    Some(5000),
                    "so does the floor"
                );
                assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
                // Gone again: a bridge killed between the move and the write.
                std::fs::remove_file(root.join("logpin")).expect("gone");
                assert_eq!(
                    st.reconcile_logpin().expect("the reconcile"),
                    Reconciled::Restored
                );
                assert_eq!(
                    st.logpin()
                        .expect("reads")
                        .map(|t| crate::mirror::Pin::parse(&t)),
                    Some(Ok(from_the_copies)),
                    "written again"
                );
                // A whole pin with a lower total: the newest, or an older one
                // a cut brought back, which nothing tells apart.
                st.set_logpin(&mirror_pin(key, 100).render())
                    .expect("an older pin");
                assert_eq!(
                    st.reconcile_logpin().expect("the reconcile"),
                    Reconciled::Raised {
                        from: 100,
                        to: 5000
                    }
                );
                let raised =
                    crate::mirror::Pin::parse(&st.logpin().expect("reads").expect("a pin"))
                        .expect("whole");
                assert_eq!(raised, mirror_pin(key, 5000), "its batch and key kept");
                assert_eq!(st.reconcile_logpin().expect("again"), Reconciled::Kept);
            } else {
                assert_eq!(done, Reconciled::Kept, "{world}");
                assert_eq!(
                    st.logpin()
                        .expect("reads")
                        .map(String::into_bytes)
                        .as_deref(),
                    Some(cut),
                    "{world}: kept as it is, for the mirror to refuse"
                );
                assert!(!root.join(QUARANTINE).exists(), "{world}: nothing moved");
            }
        }
    }

    /// THE FIRST PIN OF EACH OPEN IS DURABLE, THE REST ATOMIC
    /// ([`StateDir::set_logpin`]): a visible v=2 may be a failed flush, so
    /// this open establishes its version fence before relying on it. Own
    /// later replacements stay atomic; an error, reopen, removal, replacement
    /// or legacy header invalidates that admission.
    #[test]
    fn the_first_pin_of_this_build_is_durable_and_the_rest_atomic() {
        let older = mirror_pin([7; 16], 10)
            .render()
            .replacen("logpin v=2", "logpin v=1", 1);
        for (world, before) in [
            ("no pin", None),
            ("an older build's pin", Some(older.as_bytes())),
            ("damage that names no version", Some(&b"logpin v="[..])),
            ("this build's pin, torn", Some(&b"logpin v=2 inc=1 n"[..])),
        ] {
            let dir = aterm_tempfile::Builder::new()
                .prefix("logpin")
                .tempdir_in("/tmp")
                .expect("scratch");
            let root = dir.path();
            let st = StateDir::open(root).expect("open");
            if let Some(bytes) = before {
                std::fs::write(root.join("logpin"), bytes).expect("what is there");
            }
            let ops = st.ops();
            st.set_logpin(&mirror_pin([7; 16], 20).render())
                .expect("the pin");
            assert_eq!((st.ops() - ops).fsyncs, 2, "{world}");
            let ops = st.ops();
            st.set_logpin(&mirror_pin([7; 16], 30).render())
                .expect("the next");
            assert_eq!((st.ops() - ops).fsyncs, 0, "{world}: the next is atomic");
        }

        let dir = aterm_tempfile::Builder::new()
            .prefix("logpin")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let pin = mirror_pin([7; 16], 40).render();
        let st = StateDir::open(root).expect("open");
        st.set_inc_seam(IncSeam {
            kill_at: Some(IncStep::Renamed),
            ..Default::default()
        });
        assert!(
            st.set_logpin(&pin).is_err(),
            "the first flush failed after rename"
        );
        assert!(crate::mirror::Pin::by_this_build(
            &std::fs::read(root.join(LOGPIN)).unwrap()
        ));
        st.set_inc_seam(IncSeam::default());
        let ops = st.ops();
        st.set_logpin(&pin).expect("retried");
        assert_eq!((st.ops() - ops).fsyncs, 2, "the retry is still durable");
        let ops = st.ops();
        st.ensure_logpin_durable(|| panic!("a known fence needs no render or rewrite"))
            .expect("already admitted");
        assert_eq!((st.ops() - ops).replaces, 0);

        let st = StateDir::open(root).expect("reopen");
        let ops = st.ops();
        st.ensure_logpin_durable(|| pin.clone())
            .expect("reestablished after reopen");
        assert_eq!((st.ops() - ops).fsyncs, 2);
        for change in ["removed", "replaced", "legacy header"] {
            let path = root.join(LOGPIN);
            match change {
                "removed" => std::fs::remove_file(&path).unwrap(),
                "replaced" => {
                    let staged = root.join("other-pin");
                    std::fs::write(&staged, &pin).unwrap();
                    std::fs::rename(staged, &path).unwrap();
                }
                _ => std::fs::write(&path, pin.replacen("v=2", "v=1", 1)).unwrap(),
            }
            let ops = st.ops();
            st.ensure_logpin_durable(|| pin.clone()).expect(change);
            assert_eq!(
                (st.ops() - ops).fsyncs,
                2,
                "{change}: admission invalidated"
            );
            let ops = st.ops();
            st.set_logpin(&pin).expect("the next own replacement");
            assert_eq!(
                (st.ops() - ops).fsyncs,
                0,
                "{change}: later own writes stay atomic"
            );
        }
        st.refuse_writes_of(Some(LOGPIN));
        assert!(
            st.set_logpin(&pin).is_err(),
            "an ordinary atomic write failed"
        );
        st.refuse_writes_of(None);
        let ops = st.ops();
        st.ensure_logpin_durable(|| pin)
            .expect("readmitted after the error");
        assert_eq!(
            (st.ops() - ops).fsyncs,
            2,
            "any write error revokes admission"
        );
    }

    /// THE DURABLE COPIES READ BACK, AND THE FLOOR IS ONLY EVER RAISED: a
    /// raise below the floor writes nothing; a key or floor that does not
    /// read is refused, never taken as none — and the reconcile that needs
    /// it moves nothing.
    #[test]
    fn the_mirrors_copies_read_back_and_the_floor_is_only_raised() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("copies")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let st = StateDir::open(root).expect("open");
        assert_eq!(st.logkey().expect("none"), None);
        assert_eq!(st.logfloor().expect("none"), None);
        st.set_logkey(&[3; 16]).expect("the key");
        assert_eq!(st.logkey().expect("reads"), Some([3; 16]));
        assert_eq!(st.raise_logfloor(700).expect("raised"), 700);
        let before = st.ops();
        assert_eq!(st.raise_logfloor(600).expect("kept"), 700, "never lowered");
        assert_eq!((st.ops() - before).replaces, 0, "nothing written");
        assert_eq!(st.raise_logfloor(900).expect("raised"), 900);
        assert_eq!(st.logfloor().expect("reads"), Some(900));
        st.set_inc_seam(in_boot(1));
        st.begin_run().expect("a run in boot 1");
        std::fs::write(root.join("logpin"), b"logpin v=2 inc=1 n=0 to").expect("a torn pin");
        st.set_inc_seam(in_boot(2));
        for (name, damage) in [("logfloor", &b"90"[..]), ("logkey", &b"03030303\n"[..])] {
            let good = std::fs::read(root.join(name)).expect("the copy");
            std::fs::write(root.join(name), damage).expect("damaged");
            assert_eq!(
                if name == "logkey" {
                    st.logkey().map(|_| ()).expect_err("refused").kind()
                } else {
                    st.logfloor().map(|_| ()).expect_err("refused").kind()
                },
                io::ErrorKind::InvalidData,
                "{name}"
            );
            assert!(
                st.reconcile_logpin().is_err(),
                "{name}: no move on a copy that does not read"
            );
            assert!(root.join("logpin").exists(), "{name}: the pin stays");
            std::fs::write(root.join(name), good).expect("repaired");
        }
        assert!(matches!(
            st.reconcile_logpin().expect("the reconcile"),
            Reconciled::SetAside(_)
        ));
    }
}
