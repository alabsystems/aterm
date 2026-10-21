// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Seals on the running harness policy (`harness_policy_cell.rs`), checked by Trust against
// every body of this crate (`Trust.Protocol.Seal`; trust `docs/USING_TRUST.md`, "A seal"):
//   * a field of a cell, a stamp or a source changes only in the fn named as its writer, in
//     that fn's own body, through its `&mut self`;
//   * each is built only in the fn named as its maker, called only from the fns named after
//     it (the watcher thread, the window's launch, and the tests that drive the real path);
//   * no value holding the cell (the `App`) or the source is put over another, by an
//     assignment in this crate or by a generic fn of another crate run at that type, except
//     the fns the seal names as trusted;
//   * no `unsafe impl` of another crate's non-auto trait stands in this crate unless each
//     seal names the trait as trusted (`retained::ClassType`, which
//     `aterm_objc::declare_class!` implements for the window's Objective-C classes). A Trust
//     without that rule reads the entry as nothing.
// Together: the policy the window runs changes only in `offer`, each change consumes one
// stamp, and a stamp comes only from the watcher's read. Safe code only; no pointer analysis.
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
//     classes), on the same word. That rule is Trust's review 14, on Trust's
//     main since trust `ebfd92f24d` (2026-10-16).
// Read together: the policy the window runs changes only in `offer`, each
// change consumes one stamp, and a stamp comes only from the watcher's read.
// No arm, no thread and no pointer is named, and no pointer analysis is
// behind it. Safe code only.
//
// WHERE IT IS CHECKED: on the seal lane only (`--cfg trust_seals`,
// harness_policy_cell.rs); the installed seal, trust `321aaeda7`, has no
// `Trust.Protocol.Seal`. All four rules are on Trust's main as of trust
// `ebfd92f24d`. A toolchain seal promoted from it or later checks each of
// them against every body of this crate in every native build that compiles
// the dependencies, once harness_policy_cell.rs mounts this island without
// the cfg; no promoted seal contains it yet. Measured 2026-10-16 with a
// stage-1 trustc of `ebfd92f24d` (`targo --unverified rustc -p aterm-gui
// --lib`): the three seals hold, the `ClassType` entry answering for its
// `unsafe impl` at 12 types (58,867 bodies, 278,164 instances read). A check
// build (`--profile check`) builds the dependencies without MIR, so the
// check cannot read their bodies: since trust `537da4cd8c` (2026-10-16, the
// fix `332dfad2ee`; fixtures `clean_island_seal_check_build_drop_glue`,
// `_clone_shim`, `_std_call`, `_std_vtable`, `_std_fn_pointer`,
// `_dep_coroutine` and `_dep_const_fn_pointer`, each beside a `_holds` twin)
// it refuses all three seals, each note saying "the check could not read the
// body of" one of them, where `ebfd92f24d` stopped with `missing optimized
// MIR` for winit's `EventLoopProxy<Wake>` drop and no word of a seal. Each
// refusal also says a type too large to read was taken to hold one, which is
// false here (trust known gap `cb_n1_f1_unread_body_carries_cut_note`).
// Measured 2026-10-17 with a stage-1 trustc of `537da4cd8c`: that check
// build refuses the three seals, and the full build holds all three (58,933
// bodies, 263,075 instances read; again on aterm `7b43aa8d60`, 59,402 bodies
// and 267,309 instances, with Trust main's seal check unchanged since
// `537da4cd8c`). On aterm `21a2393efb` the full build
// refused `cell` at one newer statement: the session journal's `list`, then a
// provided method of `JournalFs`, called `scan` at its own `Self` with a
// `&mut dyn FnMut`, which the check took to hold a cell; `list` is now a fn
// of `&dyn JournalFs` (session_log_journal_fs.rs), whose `scan` is read
// where each `dyn` is made. Measured 2026-10-19 on aterm `ee3e477883`, with a
// stage-1 trustc built from trust `269edd02c3`'s tree (it contains
// `537da4cd8c`): `stamp` and `source` hold, and the full build refused `cell`
// at one newer statement, `App::on_pill_act` (app_presence.rs, 49f7582226,
// the pill's `+N`) running `aterm_messages`' provided
// `Desk::next_need_after` at `&App`. It reads the table through `Desk`'s
// getters and is handed no `&mut`, so `cell`'s `trusts` names it. Measured
// 2026-10-20 on aterm `7a5a5183b4`, which carries that entry, with the same
// stage-1 trustc (built 2026-10-19 from trust `a597e0d6d9`, whose compiler
// sources `269edd02c3` leaves as they were): the full build holds all three,
// the entry answering for `Desk::next_need_after` at `App` (69,496 bodies,
// 311,070 instances read; 9m 28s).
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
// word of its `trusts` list for the fns of other crates named there. On
// Trust main, `docs/USING_TRUST.md` lists 24 KNOWN GAPS of the check (review
// 6's 22 at `ebfd92f24d`, and two false notes the check-build fix's review
// found), recorded and not fixed, 8 of them false passes (the seal holds and
// code of the crate puts one holder's value over another's). Its reading of
// generic code is where Trust says a seal is weakest: a seal that holds is
// evidence, not a proof.
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
