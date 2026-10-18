// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LIVE: a new tab's, split's or window's shell starts off the event loop.
//!
//! A tab's PTY spawn (folder check, fork, the wait for the child's exec) ran in
//! the event-loop turn that asked for it, so a spawn stalled on a dead network
//! volume froze every window and every main-thread verb for up to 12 s. It now
//! runs on the `aterm-spawner` thread (`aterm-gui`'s `tab_spawn.rs`) for a
//! `spawn`, a `spawn split=`, a `tab new` and a `spawn connected=… place=tab`,
//! and each replies only once the new session is registered (`tab new`
//! counting its tab). A new window's does too: Window ▸ New Window (`invoke
//! NewWindow`, the one window path headless takes, keeping the window logical)
//! replies once the window is booked, and the window is made when its session
//! lands.
//!
//! Driven through the real binary with the spawn held in flight by the dev seam
//! `ATERM_DEBUG_SPAWN_HOLD=<path>` (the spawner creates `<path>.held`, then
//! waits for `<path>`):
//!
//! * while the spawn is held, a main-thread verb (`status` of the boot session)
//!   answers, and the caller is still waiting;
//! * once released, `spawn` replies `OK <sid>`, and that sid answers `status`
//!   at once; `spawn split=v` does the same, with the window's tab now in two
//!   panes; `tab new` replies the window's active index and its tab count; a
//!   connected preset's tab is connected to its origin before the reply;
//! * a new window does not exist while its spawn is held, and once released
//!   its session lands in it, answering `status`;
//! * a wire spawn's agent identity is checked on the spawner too, before the
//!   fork: one it refuses is the caller's reply only once the hold is released.
//!
//! NEGATIVE CONTROL: with the spawn back on the event loop (the hold then runs
//! in the turn, or not at all), `status` cannot answer while the hold stands,
//! or the spawner never takes the spawn: the main-thread hop gives up after
//! 30 s, answers after the spawn's reply once the seam's minute is out, or the
//! caller finishes before `<path>.held` exists, and the test fails each way.
//!
//! Every wait is a one-minute hang detector, never a latency budget.

#![cfg(unix)]

use std::path::Path;
use std::time::Instant;

use crate::headless_boot;

use headless_boot::{Instance, boot_session, boot_with, ctl, ctl_ok, field, status};

/// Run `args` with the spawner holding its spawn: the loop answers a
/// main-thread verb (`status` of `boot_sid`) while it is out and the caller
/// waits; once released, the caller's reply (its stdout) comes back, a success.
fn held_round(inst: &Instance, boot_sid: &str, args: &[&str]) -> String {
    let out = held_call(inst, boot_sid, args);
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{args:?} failed: {reply} {:?}",
        out.stderr
    );
    reply
}

/// [`held_round`]'s hold and checks, with the caller's output however it ended.
/// The hold is re-armed for the next round once the reply shows the spawner is
/// past it.
fn held_call(inst: &Instance, boot_sid: &str, args: &[&str]) -> std::process::Output {
    let hold = inst.tmp.join("hold");
    let held = inst.tmp.join("hold.held");
    std::thread::scope(|scope| {
        let call = scope.spawn(|| ctl(inst, args));
        let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !held.exists() {
            assert!(
                Instant::now() < give_up && !call.is_finished(),
                "the spawner never took the spawn of {args:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // The loop answers a main-thread verb while the spawn is out...
        let boot = status(inst, boot_sid);
        assert!(
            field(&boot, "phase").is_some(),
            "the boot session's status: {boot}"
        );
        // ...and the caller still waits: the reply comes with the landed session.
        assert!(!call.is_finished(), "{args:?} replied before it landed");

        std::fs::write(&hold, b"").expect("release the hold");
        let out = call.join().expect("the caller");
        std::fs::remove_file(&hold).expect("re-arm the hold");
        std::fs::remove_file(&held).expect("re-arm the hold");
        out
    })
}

/// The sid a `spawn` reply names.
fn named_sid(reply: &str) -> &str {
    reply
        .split_whitespace()
        .find(|t| t.starts_with("s-"))
        .unwrap_or_else(|| panic!("spawn names its sid: {reply}"))
}

#[test]
fn a_held_tab_spawn_leaves_the_loop_answering_and_replies_once_registered() {
    let inst = boot_with(
        "aterm-tabspawn",
        |tmp: &Path, cmd| {
            cmd.env("ATERM_DEBUG_SPAWN_HOLD", tmp.join("hold"));
        },
        |_| true,
    );
    let (_, boot_sid) = boot_session(&inst);
    let reply = held_round(&inst, &boot_sid, &["spawn"]);
    let born = status(&inst, named_sid(&reply));
    assert!(
        field(&born, "phase").is_some(),
        "the new sid answers at once: {born}"
    );
}

#[test]
fn a_held_split_tab_new_or_connected_tab_leaves_the_loop_answering() {
    let inst = boot_with(
        "aterm-splitspawn",
        |tmp: &Path, cmd| {
            cmd.env("ATERM_DEBUG_SPAWN_HOLD", tmp.join("hold"));
        },
        |_| true,
    );
    let (_, boot_sid) = boot_session(&inst);
    let reply = held_round(&inst, &boot_sid, &["spawn", "split=v"]);
    let born = status(&inst, named_sid(&reply));
    assert!(
        field(&born, "phase").is_some(),
        "the split's sid answers at once: {born}"
    );
    let layout = ctl_ok(&inst, &["panes"]);
    assert_eq!(
        field(&layout, "panes"),
        Some("2"),
        "the boot tab is split in two: {layout}"
    );

    let reply = held_round(&inst, &boot_sid, &["tab", "new"]);
    assert_eq!(
        reply.split_whitespace().collect::<Vec<_>>(),
        ["OK", "1", "2"],
        "the new second tab is active: {reply}"
    );

    // A connected preset's tab: replied once landed and connected, with the
    // newborn driving the boot session it observes.
    let of = format!("of={boot_sid}");
    let reply = held_round(
        &inst,
        &boot_sid,
        &["spawn", "connected=controller", "place=tab", &of],
    );
    let born = named_sid(&reply);
    assert!(
        field(&status(&inst, born), "phase").is_some(),
        "the connected tab's sid answers at once: {reply}"
    );
    let edges = ctl_ok(&inst, &[&format!("@{boot_sid}"), "edges"]);
    assert!(
        edges
            .lines()
            .any(|row| row.starts_with(&format!("{born} {boot_sid} "))),
        "the newborn drives its origin as it lands: {edges}"
    );
}

/// Window ▸ New Window spawns the window's first shell off the loop: the
/// `invoke` replies once it is booked, the loop answers while the spawn is
/// held and no window exists yet, and once released the window is made with
/// its session in it. Window ids are never reused, so the boot window is 0 and
/// the new one 1.
#[test]
fn a_held_new_window_leaves_the_loop_answering_and_lands_once_spawned() {
    let inst = boot_with(
        "aterm-winspawn",
        |tmp: &Path, cmd| {
            cmd.env("ATERM_DEBUG_SPAWN_HOLD", tmp.join("hold"));
        },
        |_| true,
    );
    let (_, boot_sid) = boot_session(&inst);
    let hold = inst.tmp.join("hold");
    let held = inst.tmp.join("hold.held");
    let in_window = |window: &str| -> Vec<String> {
        ctl_ok(&inst, &["sessions"])
            .lines()
            .filter(|row| field(row, "window") == Some(window))
            .filter_map(|row| row.split_whitespace().nth(1).map(str::to_string))
            .collect()
    };
    let booked = ctl_ok(&inst, &["invoke", "NewWindow"]);
    assert!(booked.starts_with("OK"), "booked: {booked}");
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !held.exists() {
        assert!(
            Instant::now() < give_up,
            "the spawner never took the new window's spawn"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The loop answers a main-thread verb while the spawn is out, and the
    // window is not made before its session.
    let boot = status(&inst, &boot_sid);
    assert!(
        field(&boot, "phase").is_some(),
        "the boot session's status: {boot}"
    );
    assert!(
        in_window("1").is_empty(),
        "no window before its session lands"
    );

    std::fs::write(&hold, b"").expect("release the hold");
    let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let landed = loop {
        let born = in_window("1");
        if let [sid] = born.as_slice() {
            break sid.clone();
        }
        assert!(
            Instant::now() < give_up,
            "the new window never landed: {born:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let born = status(&inst, &landed);
    assert!(
        field(&born, "phase").is_some(),
        "the new window's session answers: {born}"
    );
}

/// A wire spawn's agent identity is checked where its shell starts, on the
/// spawner, before the fork: never in the request's event-loop turn, since the
/// identities folder can sit on a volume that stops answering. `@<worker>
/// spawn` passes the worker's identity down; with that identity's folder gone,
/// the spawner refuses it, and the caller, which waited while the spawn was
/// held, hears `ERR identity worker: ...` once it is released.
///
/// NEGATIVE CONTROL: with the check back in the turn, the refusal is the reply
/// before the spawner ever takes the spawn, and `held_call` fails on "the
/// spawner never took the spawn".
#[test]
fn a_wire_spawns_identity_is_refused_on_the_spawner_not_in_the_turn() {
    let inst = boot_with(
        "aterm-idspawn",
        |tmp: &Path, cmd| {
            cmd.env("ATERM_DEBUG_SPAWN_HOLD", tmp.join("hold"))
                .env("ATERM_STATE_HOME", tmp.join("state"));
        },
        |_| true,
    );
    let (_, boot_sid) = boot_session(&inst);
    let reply = held_round(&inst, &boot_sid, &["spawn", "identity=worker"]);
    let worker = named_sid(&reply).to_string();
    std::fs::remove_dir_all(inst.tmp.join("state/identities/worker"))
        .expect("the worker's identity, made by its spawn");

    let aimed = format!("@{worker}");
    let out = held_call(&inst, &boot_sid, &[&aimed, "spawn"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success() && said.contains("identity worker: "),
        "refused, in the request's words: {said}"
    );
}
