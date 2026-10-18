// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A main-thread control verb is refused at once while the main thread is
//! stalled, and only then: the heartbeat's writer against its reader.

use aterm_spec::derive::Model;

/// The writer/reader contract behind `ERR main thread stalled <N>s; retry`, which spares a verb
/// that needs a stuck main thread from waiting out its whole reply deadline. The writer is the
/// watchdog's beat (`beat_into`), stamping the heartbeat's time on every root entry; the reader
/// is `main_stall`, which a control worker asks before posting a hop to the main thread. The
/// hop count `call_main` keeps (`control_media::Hops`) and `metrics reset` are the other actors.
///
/// Time is in ticks of half the bar (`Bar = 2`). `quiet` is the ground truth, ticks since the
/// main thread last entered a root; `stamp`/`stamp_age` are what the writer left the reader.
/// `waiting` is a worker awaiting its hop's reply, `hop` that hop while not yet taken (what
/// `Hops` counts), `waited` how long it has waited. `root` is 0 at the idle park
/// (`AboutToWait`), 1 at a work root, 2 at a designed freeze (startup, a dialog, the update
/// handoff). `Answer` takes the hop and answers in the same turn; `Take` takes it and queues
/// the reply (`settings set`, answered later by the config worker), and `Reply` is that reply.
/// `Probe` records the reader's verdict (`refused`) and the truth (`stalled`): a work root with
/// no beat for the bar, or the idle park with an untaken hop that has waited the bar with no
/// beat. A taken hop whose worker awaits a queued reply waits on the config worker, not on the
/// main thread.
///
/// * `NeverRefusesAMovingMainThread`: a refusal only when stalled.
/// * `RefusesAStalledMainThread`: a stall is always refused.
///
/// `Buggy = 1` admits one of three defects. `MutateCensusStamp` reads the turn census's open
/// stamp, which `metrics reset` zeroes, as the heartbeat's time, so a reset under a stall reads
/// as a thread that never beat. `MutateHeartbeatOnly` judges a waiting hop on the heartbeat's
/// age alone, so verbs arriving after an idle longer than the bar look stalled.
/// `MutateCountsUntilReply` counts a hop until its reply rather than until it is taken, so a
/// slow `settings set` makes a healthy idle thread refuse other verbs. Tier-1: aterm-gui's
/// `main_stall_conforms_to_the_stall_refusal_model` drives the real `beat_into`,
/// `TurnLedger::reset`, `Hops`, `take_hop` and `main_stall` in lockstep and replays all three.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn main_thread_stall_refusal_model() -> Model {
    aterm_spec::ty_model! {
        MainThreadStallRefusal {
            const Buggy = 0;
            const Bar = 2;
            var root = 2;
            var quiet = 0;
            var stamp = 0;
            var stamp_age = 0;
            var hop = 0;
            var waiting = 0;
            var waited = 0;
            var refused = 0;
            var stalled = 0;
            var census = 0;
            var beat_only = 0;
            var until_reply = 0;
            var mutated = 0;

            action MutateCensusStamp when (Buggy == 1 && mutated == 0) {
                census = 1;
                mutated = 1;
            }
            action MutateHeartbeatOnly when (Buggy == 1 && mutated == 0) {
                beat_only = 1;
                mutated = 1;
            }
            action MutateCountsUntilReply when (Buggy == 1 && mutated == 0) {
                until_reply = 1;
                mutated = 1;
            }
            action BeatIdle {
                root = 0;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action BeatWork {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action BeatPark {
                root = 2;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
            }
            action Tick {
                quiet = if quiet <= Bar - 1 { quiet + 1 } else { quiet };
                stamp_age = if stamp_age <= Bar - 1 { stamp_age + 1 } else { stamp_age };
                waited = if waiting == 1 && waited <= Bar - 1 { waited + 1 } else { waited };
            }
            action Post when (waiting == 0) {
                hop = 1;
                waiting = 1;
                waited = 0;
            }
            action Answer when (hop == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                hop = 0;
                waiting = 0;
                waited = 0;
            }
            action Take when (hop == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                hop = 0;
            }
            action Reply when (hop == 0 && waiting == 1 && root <= 1) {
                root = 1;
                quiet = 0;
                stamp = 1;
                stamp_age = 0;
                waiting = 0;
                waited = 0;
            }
            action GiveUp when (waiting == 1) {
                hop = 0;
                waiting = 0;
                waited = 0;
            }
            action Reset {
                stamp = if census == 1 { 0 } else { stamp };
            }
            action Probe {
                refused = if stamp == 1 && stamp_age == Bar && (
                    root == 1 || (
                        root == 0
                            && (hop == 1 || (until_reply == 1 && waiting == 1))
                            && (waited == Bar || beat_only == 1)
                    )
                ) { 1 } else { 0 };
                stalled = if quiet == Bar && (
                    root == 1 || (root == 0 && hop == 1 && waited == Bar)
                ) { 1 } else { 0 };
            }

            invariant NeverRefusesAMovingMainThread: refused <= stalled;
            invariant RefusesAStalledMainThread: stalled <= refused;
        }
    }
}
