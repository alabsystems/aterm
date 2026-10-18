// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The supervisor policy has one writer, proved by the compiler.
//
// config_watcher.rs makes the watcher thread the only writer of the `[harness]` policy the window
// runs (`observed_harness`, `post_harness`, `Wake::HarnessPolicyObserved`,
// `App::adopt_harness_policy`): it reads the file in disk order, readable or refused, and posts
// each read's policy once per change; the config lane admits generations under the running
// policy and never writes one; a seamless successor drops every wake it handles before Commit,
// and the Commit handler has the watcher forget what it posted (`REPOST_AFTER_COMMIT`).
//
// This island states the protocol (the watcher's post, its flight through the event loop queue
// and its delivery, the Commit and the watcher's later forget as separate steps, lane admissions
// at any moment), and the Clean CIC kernel checks it in every Trust-lane build, even under
// -Ztrust-verify=off, because an island carries its own authority:
//   * the invariant holds along any run, every interleaving of those events;
//   * with the window live and the watcher settled, one read and its delivery run the file on
//     disk;
//   * the two failure shapes (a second writer; a Commit that raises no forget) are
//     kernel-evaluated counterexamples, so the proof is not vacuous: putting the second writer
//     into `step` stops this crate compiling.
//
// It is a theorem about the model below, not about the Rust; the queue holds one post at a time
// here. The code-bound half is the derived twin,
// `aterm_models_harness::harness_policy_one_writer_model` (the same machine, registered, so
// aterm-models' registry-wide sweeps prove it with the second writer as its `Buggy` member), and
// its Tier-1 binds: `config_watcher::tests::the_watcher_is_the_policys_one_writer` drives the
// real read, post, dedupe, delivery, admission and Commit reset; `app_native`'s
// `a_late_admission_never_moves_the_watchers_harness_policy` drives the real admission path.
//
// A sibling file, like `app_update_handoff_island.rs`: a compiler without the Clean surface fails
// to lex an island, so it sits behind `mod island;` under `cfg(clean_islands)`.
clean {
    namespace HarnessWriter

    -- The world the supervisor policy lives in. disk is the policy the file on
    -- disk writes (its harness lines, read as the launch reads them). hasPosted
    -- and lastPosted are the watcher dedupe: it posts a read only when the policy
    -- differs from the one it last posted. queued and queuedVal are its post in
    -- flight in the event loop queue. inForce is the policy the window runs.
    -- committed says whether the window runs what it handles: a seamless
    -- successor drops every wake handled before its Commit. forgetPending is the
    -- flag the Commit handler raises for the watcher to consume on its next loop.
    structure World where
      disk : Nat
      hasPosted : Bool
      lastPosted : Nat
      queued : Bool
      queuedVal : Nat
      inForce : Nat
      committed : Bool
      forgetPending : Bool

    -- write: the file changes on disk. poll: the watcher reads the file and posts
    -- its policy (THE ONE WRITER). deliver: the window handles that post. admit:
    -- the config lane admits a generation, at any time and in any order relative
    -- to everything else. commit: the Commit handler runs. forget: the watcher
    -- consumes the flag and forgets what it posted.
    inductive Event where
      | write : Nat -> Event
      | poll : Event
      | deliver : Event
      | admit : Nat -> Event
      | commit : Event
      | forget : Event

    def watcherPoll (w : World) : World :=
      if Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)) then w
      else World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending

    def windowDeliver (w : World) : World :=
      if w.queued = true then
        (if w.committed = true then
          World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending
        else
          World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending)
      else w

    def watcherForget (w : World) : World :=
      if w.forgetPending = true then
        World.mk w.disk false w.lastPosted w.queued w.queuedVal w.inForce w.committed false
      else w

    def step (w : World) : Event -> World
      | Event.write p => World.mk p w.hasPosted w.lastPosted w.queued w.queuedVal w.inForce w.committed w.forgetPending
      | Event.poll => watcherPoll w
      | Event.deliver => windowDeliver w
      | Event.admit _ => w
      | Event.commit => World.mk w.disk w.hasPosted w.lastPosted w.queued w.queuedVal w.inForce true true
      | Event.forget => watcherForget w

    -- THE INVARIANT. A post in flight carries the policy last posted; and once
    -- the window runs what it handles, with the watcher settled (it has posted,
    -- nothing is in flight, no forget is pending), the window runs the policy
    -- last posted -- so the next read and its delivery bring it to the disk.
    def Agrees (w : World) : Prop :=
      And (w.queued = true -> w.queuedVal = w.lastPosted)
        (w.committed = true -> w.hasPosted = true -> w.queued = false -> w.forgetPending = false ->
          w.inForce = w.lastPosted)

    -- THE CONFIG LANE WRITES NO POLICY. Its workers finish in no order the
    -- watcher controls; with one writer that order cannot matter.
    theorem admit_moves_nothing (w : World) (p : Nat) : step w (Event.admit p) = w := rfl

    theorem poll_quiet (w : World) (hd : Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk))) :
        watcherPoll w = w := by
      show (if Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)) then w else World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending) = w
      exact if_pos hd

    theorem poll_posts (w : World) (hd : Not (Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)))) :
        watcherPoll w = World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending := by
      show (if Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)) then w else World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending) = World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending
      exact if_neg hd

    theorem poll_agrees (w : World) (h : Agrees w) : Agrees (watcherPoll w) :=
      if hd : Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)) then by
        rw [poll_quiet w hd]
        exact h
      else by
        rw [poll_posts w hd]
        exact And.intro (fun _ => rfl) (fun _ _ hq _ => Bool.noConfusion hq)

    theorem deliver_committed (w : World) (hq : w.queued = true) (hc : w.committed = true) :
        windowDeliver w = World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending := by
      show (if w.queued = true then (if w.committed = true then World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending else World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending) else w) = World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending
      exact Eq.trans (if_pos hq) (if_pos hc)

    theorem deliver_dropped (w : World) (hq : w.queued = true) (hc : Not (w.committed = true)) :
        windowDeliver w = World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending := by
      show (if w.queued = true then (if w.committed = true then World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending else World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending) else w) = World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending
      exact Eq.trans (if_pos hq) (if_neg hc)

    theorem deliver_idle (w : World) (hq : Not (w.queued = true)) : windowDeliver w = w := by
      show (if w.queued = true then (if w.committed = true then World.mk w.disk w.hasPosted w.lastPosted false 0 w.queuedVal w.committed w.forgetPending else World.mk w.disk w.hasPosted w.lastPosted false 0 w.inForce w.committed w.forgetPending) else w) = w
      exact if_neg hq

    theorem deliver_agrees (w : World) (h : Agrees w) : Agrees (windowDeliver w) :=
      if hq : w.queued = true then
        if hc : w.committed = true then by
          rw [deliver_committed w hq hc]
          exact And.intro (fun hf => Bool.noConfusion hf) (fun _ _ _ _ => h.1 hq)
        else by
          rw [deliver_dropped w hq hc]
          exact And.intro (fun hf => Bool.noConfusion hf) (fun hc2 _ _ _ => False.elim (hc hc2))
      else by
        rw [deliver_idle w hq]
        exact h

    theorem forget_fires (w : World) (hf : w.forgetPending = true) :
        watcherForget w = World.mk w.disk false w.lastPosted w.queued w.queuedVal w.inForce w.committed false := by
      show (if w.forgetPending = true then World.mk w.disk false w.lastPosted w.queued w.queuedVal w.inForce w.committed false else w) = World.mk w.disk false w.lastPosted w.queued w.queuedVal w.inForce w.committed false
      exact if_pos hf

    theorem forget_idle (w : World) (hf : Not (w.forgetPending = true)) : watcherForget w = w := by
      show (if w.forgetPending = true then World.mk w.disk false w.lastPosted w.queued w.queuedVal w.inForce w.committed false else w) = w
      exact if_neg hf

    theorem forget_agrees (w : World) (h : Agrees w) : Agrees (watcherForget w) :=
      if hf : w.forgetPending = true then by
        rw [forget_fires w hf]
        exact And.intro h.1 (fun _ hp _ _ => Bool.noConfusion hp)
      else by
        rw [forget_idle w hf]
        exact h

    theorem agrees_holds (w : World) (e : Event) (h : Agrees w) : Agrees (step w e) := by
      cases e with
      | write p => exact h
      | poll => exact poll_agrees w h
      | deliver => exact deliver_agrees w h
      | admit p => exact h
      | commit => exact And.intro h.1 (fun _ _ _ hf => Bool.noConfusion hf)
      | forget => exact forget_agrees w h

    theorem launch_agrees (disk inForce : Nat) (committed : Bool) :
        Agrees (World.mk disk false 0 false 0 inForce committed false) :=
      And.intro (fun hq => Bool.noConfusion hq) (fun _ hp _ _ => Bool.noConfusion hp)

    def run (w : World) : List Event -> World
      | [] => w
      | e :: es => run (step w e) es

    -- ANY RUN: every interleaving of writes, polls, deliveries, admissions, the
    -- Commit and the forget, from the launch state.
    theorem agrees_along_any_run (es : List Event) : (w : World) -> Agrees w -> Agrees (run w es) := by
      induction es with
      | nil => intro w h; exact h
      | cons e es ih => intro w h; exact ih (step w e) (agrees_holds w e h)

    -- THE FILE ON DISK WINS. With the window running what it handles and the
    -- watcher settled, one watcher read and its delivery leave the window running
    -- the policy the file writes, whatever came before.
    theorem the_file_on_disk_wins (w : World) (h : Agrees w) (hc : w.committed = true)
        (hq : w.queued = false) (hf : w.forgetPending = false) :
        (windowDeliver (watcherPoll w)).inForce = w.disk :=
      if hd : Or (w.queued = true) (And (w.hasPosted = true) (w.lastPosted = w.disk)) then by
        rw [poll_quiet w hd]
        have hidle : Not (w.queued = true) := fun hq2 => Bool.noConfusion (Eq.trans hq2.symm hq)
        rw [deliver_idle w hidle]
        exact Or.elim hd (fun hq2 => False.elim (hidle hq2))
          (fun hand => Eq.trans (h.2 hc hand.1 hq hf) hand.2)
      else by
        rw [poll_posts w hd]
        rw [deliver_committed (World.mk w.disk true w.disk true w.disk w.inForce w.committed w.forgetPending) rfl hc]

    -- THE BUG AS IT SHIPPED IS REPRESENTABLE, AND THE KERNEL EVALUATES IT: with a
    -- second writer (the lane adopting its own harness lines), a stale admission
    -- after the kill switch was delivered lifts it, and the deduped poll never
    -- puts it back. One writer keeps it.
    def stepTwoWriters (w : World) : Event -> World
      | Event.admit p => World.mk w.disk w.hasPosted w.lastPosted w.queued w.queuedVal p w.committed w.forgetPending
      | e => step w e

    def runTwoWriters (w : World) : List Event -> World
      | [] => w
      | e :: es => runTwoWriters (stepTwoWriters w e) es

    theorem two_writers_lose_the_kill_switch_for_good :
        (runTwoWriters (World.mk 0 false 0 false 0 1 true false)
          [Event.poll, Event.deliver, Event.admit 1, Event.poll, Event.deliver]).inForce = 1 := rfl

    theorem one_writer_keeps_the_kill_switch :
        (run (World.mk 0 false 0 false 0 1 true false)
          [Event.poll, Event.deliver, Event.admit 1, Event.poll, Event.deliver]).inForce = 0 := rfl

    -- The same for a Commit that raises no forget: the policy posted and dropped
    -- before Commit is never posted again. The forget delivers it.
    def stepNoForget (w : World) : Event -> World
      | Event.commit => World.mk w.disk w.hasPosted w.lastPosted w.queued w.queuedVal w.inForce true false
      | e => step w e

    def runNoForget (w : World) : List Event -> World
      | [] => w
      | e :: es => runNoForget (stepNoForget w e) es

    theorem a_commit_that_forgets_nothing_keeps_a_dropped_policy_out :
        (runNoForget (World.mk 0 false 0 false 0 1 false false)
          [Event.poll, Event.deliver, Event.commit, Event.forget, Event.poll, Event.deliver]).inForce = 1 := rfl

    theorem the_commit_repost_delivers_it :
        (run (World.mk 0 false 0 false 0 1 false false)
          [Event.poll, Event.deliver, Event.commit, Event.forget, Event.poll, Event.deliver]).inForce = 0 := rfl

    end HarnessWriter
}
