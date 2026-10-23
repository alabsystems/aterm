// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that new tab, split, and window shells spawn off the event loop.

#![cfg(unix)]

use std::path::Path;
use std::time::Instant;

use crate::headless_boot;

use headless_boot::{Instance, boot_session, boot_with, ctl, ctl_ok, field, status};

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
        let boot = status(inst, boot_sid);
        assert!(
            field(&boot, "phase").is_some(),
            "the boot session's status: {boot}"
        );
        assert!(!call.is_finished(), "{args:?} replied before it landed");

        std::fs::write(&hold, b"").expect("release the hold");
        let out = call.join().expect("the caller");
        std::fs::remove_file(&hold).expect("re-arm the hold");
        std::fs::remove_file(&held).expect("re-arm the hold");
        out
    })
}

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
