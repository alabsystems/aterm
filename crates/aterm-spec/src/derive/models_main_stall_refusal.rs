// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A main-thread control verb is refused at once while the main thread is
//! stalled, and only then: the heartbeat's writer against its reader.

use super::Model;

/// The writer/reader contract behind `ERR main thread stalled <N>s since
/// <root>; retry`. The WRITER is the watchdog's beat (`beat_into`), which
/// stamps the heartbeat's time on every root entry. The READER is
/// `main_stall`, which a control worker asks before it posts a hop to the main
/// thread. The hop count `call_main` keeps (`control_media::Hops`) and the
/// census reset `metrics reset` runs are the other two actors.
///
/// The 2026-09-28 incident is the reason for it. The main thread was stuck in
/// AppKit's scene setup before the whole process stopped running, and a verb
/// that needed the main thread could only post its hop and wait out its whole
/// reply deadline for a thread that could not answer, though the heartbeat
/// already showed the thread stuck.
///
/// Time is in ticks of half the bar (`Bar = 2`). `quiet` is the ground truth,
/// ticks since the main thread last entered a root. `stamp`/`stamp_age` are
/// what the writer left for the reader. `waiting` is a worker waiting for its
/// hop's reply, `hop` that hop while the main thread has not yet taken it
/// (what `Hops` counts), and `waited` how long the worker has waited. `root`
/// is 0 at the idle park (`AboutToWait`), 1 at a work root, and 2 at a
/// designed freeze: startup, the initial state; a dialog; the update handoff.
/// `Answer` is the main thread taking the hop and answering it in the same
/// turn. `Take` is the main thread taking it and queueing its reply
/// (`settings set`, answered turns later by the config worker's completion),
/// and `Reply` is that reply. `Probe` records the reader's verdict (`refused`)
/// and, at the same instant, the truth (`stalled`): a work root with no beat
/// for the bar, or the idle park with a hop not yet taken that has waited the
/// bar and no beat in it. A hop already taken, whose worker waits on a queued
/// reply, waits on the config worker, not on the main thread.
///
/// * `NeverRefusesAMovingMainThread`: a refusal only when stalled.
/// * `RefusesAStalledMainThread`: a stall is always refused.
///
/// `Buggy = 1` admits one of three defects, each caught by an invariant.
/// `MutateCensusStamp` has the reader take the turn census's open stamp,
/// which a reset zeroes, for the heartbeat's time: a `metrics reset` under a
/// stall then reads as a thread that never beat, and the stalled thread's
/// verbs wait out their deadlines again. `MutateHeartbeatOnly` judges a
/// waiting hop on the heartbeat's age alone: the first verb after an idle
/// longer than the bar makes every verb that arrives while it is answered
/// look stalled. `MutateCountsUntilReply` counts a hop until its worker gets
/// the reply, not until the main thread takes it: a `settings set` whose
/// write takes the bar makes a healthy thread, parked idle behind it, refuse
/// every other main-thread verb. Tier-1 (`aterm-gui`'s watchdog test
/// `main_stall_conforms_to_the_stall_refusal_model`) drives the real
/// `beat_into`, `TurnLedger::reset`, `Hops`, `take_hop` and `main_stall` in
/// lockstep with this machine, and replays all three defects on the real
/// reader.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn main_thread_stall_refusal_model() -> Model {
    crate::ty_model! {
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
