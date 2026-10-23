// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
         -- `Desk`'s provided `next_need_after`, which the pill's `+N` runs
         -- at `&App` (`App::on_pill_act`): it reads the table through
         -- `Desk`'s getters and is handed no `&mut`.
         "presence::drive::Desk::next_need_after",
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
