// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE RUNNING POLICY CHANGES IN ONE FN, AND THE COMPILER CHECKS THAT.
//
// harness_policy_cell.rs keeps the supervisor's policy in force in a struct of
// its own (`HarnessPolicyCell`), changed only by `offer`, which takes a
// `StampedHarnessPolicy`; a stamp is made only by `HarnessStampSource::stamp`,
// and the one source is the config watcher's. The three seals below say the
// rest, and Trust checks every body of this crate against them
// (`Trust.Protocol.Seal`; trust `docs/USING_TRUST.md`, "A seal"):
//   * a field of a cell, a stamp or a source is changed only in the fn named
//     as its writer, in that fn's own body, through its `&mut self`;
//   * each is built only in the fn named as its maker, which is called only
//     from the fns named after it (the watcher thread, the window's launch,
//     and the tests that drive the real path);
//   * no value holding the cell (the `App`) or the source is put over
//     another: not by an assignment in this crate, and not by a generic fn of
//     another crate run at such a type, except the ones the seal names as
//     trusted, on the author's word;
//   * no `unsafe impl` of another crate's trait, other than an auto trait
//     (`Send`, `Sync`), stands in this crate unless each seal names the trait
//     as trusted (`retained::ClassType`, which
//     `aterm_objc::declare_class!` implements for the window's Objective-C
//     classes), on the same word. That rule is Trust's review 14, not yet on
//     Trust's main; a seal check without it reads the entry as nothing.
// Read together: the policy the window runs changes only in `offer`, each
// change consumes one stamp, and a stamp comes only from the watcher's read.
// No arm, no thread and no pointer is named, and no pointer analysis is
// behind it. Safe code only.
//
// WHAT IT DOES NOT SAY: that `config.harness`, the mirror Settings reads, is
// written nowhere else (`adopt_harness_policy` and the config lane copy the
// cell into it; `config_watcher_binding_island.rs`, under `--cfg
// trust_binding`, asks Trust's writer census that, as evidence and not a
// proof); what the supervisor host does with the policy it is handed
// (`HostHandle::set_config`); nor that the watcher is started once (a second
// `config_watcher::spawn` would mint from 1 again, and the join would drop its
// posts until they passed the first watcher's: one call site today, in
// `main_entry`). And a seal is a claim about safe code of THIS crate, on the
// word of its `trusts` list for the fns of other crates named there.
clean {
    import Trust.Binding
    namespace HarnessPolicySeal
    open Trust.Protocol

    def cell : Trust.Protocol.Seal :=
      seals "HarnessPolicyCell"
        ["HarnessPolicyCell::offer"]
        ["HarnessPolicyCell::new"]
        ["main_entry", "App::headless_for_test_with_sink",
         "the_newest_stamp_wins_in_either_order"]
        -- Run at `App`, the type that holds the cell, and trusted to put no
        -- `App` over another: the event loop that calls the handler, the
        -- `forget` that ends the window, and the presence driver, which
        -- calls back through `PresenceHost` and can build no `App`.
        ["EventLoop::run_app", "mem::forget",
         -- `run_app`'s closure hands each event to this shim (vendored
         -- winit, `event_loop.rs`), which matches it and calls the
         -- handler's method on the `&mut App` it was given.
         "event_loop::dispatch_event_for_app",
         "presence::drive::refresh_session", "presence::drive::refresh_window",
         "presence::drive::refresh_all", "presence::drive::human_acted_on",
         "presence::drive::deadline", "presence::drive::tick",
         "presence::drive::chrome_line", "presence::drive::status_tail",
         "presence::drive::tell",
         -- An `unsafe impl` of another crate's trait, other than an auto
         -- trait (`Send`, `Sync`), is the crate's word to that crate's
         -- `unsafe` code, whatever it is for, so each seal answers for it. This one is `aterm_objc`'s: the window's Objective-C
         -- classes (a button, a menu target, a delegate) say which class
         -- their instances are. None holds or points to a cell, a stamp or
         -- a source.
         "retained::ClassType"]

    def stamp : Trust.Protocol.Seal :=
      seals "StampedHarnessPolicy"
        []
        ["HarnessStampSource::stamp"]
        ["post_harness",
         "the_newest_stamp_wins_in_either_order",
         "a_late_admission_never_moves_the_watchers_harness_policy",
         "the_policys_move_is_announced_on_every_sessions_stream"]
        ["retained::ClassType"]

    def source : Trust.Protocol.Seal :=
      seals "HarnessStampSource"
        ["HarnessStampSource::stamp"]
        ["HarnessStampSource::new"]
        ["config_watcher::watch",
         "the_newest_stamp_wins_in_either_order",
         "a_late_admission_never_moves_the_watchers_harness_policy",
         "the_watcher_is_the_policys_one_writer",
         "the_policys_move_is_announced_on_every_sessions_stream"]
        ["retained::ClassType"]

    end HarnessPolicySeal
}
