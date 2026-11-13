// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The process table on Windows: read from the kernel in process, where no `ps` exists.

use super::{
    UNREADABLE_TABLE, alive, background_or_unreadable, descendants_in, held_of, ids, image_word,
    is_our_ancestor, kernel_rows, owned_by_aterm, parent_of, started_ms, table, upgrade,
};
use std::process::{Child, Stdio};

/// A `cmd.exe` under this test that waits on a line of its input, which nothing writes.
struct Waiting(Child);

impl Waiting {
    fn start() -> Self {
        use std::os::windows::process::CommandExt as _;
        let mut cmd = aterm_uds::spawnfd::command("cmd.exe");
        cmd.args(["/D", "/C"])
            .raw_arg("set /p held=")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Self(cmd.spawn().expect("cmd.exe starts"))
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_table_names_this_process_and_its_child_by_their_real_parents() {
    let me = std::process::id();
    let child = Waiting::start();
    let t = table();
    let row = |pid: u32| {
        t.iter()
            .find(|(p, _, _)| *p == pid)
            .unwrap_or_else(|| panic!("pid {pid} is in a table of {} rows", t.len()))
    };

    let exe = std::env::current_exe().expect("this test's image");
    let stem = exe
        .file_stem()
        .expect("an image name")
        .to_string_lossy()
        .to_ascii_lowercase();
    let (_, my_parent, my_name) = row(me);
    assert_eq!(
        my_name.as_str(),
        stem,
        "the image name of {}: lowercase, its .exe dropped",
        exe.display()
    );
    assert_eq!(parent_of(me), Some(*my_parent), "one reader of a parent");

    let (_, child_parent, child_name) = row(child.pid());
    assert_eq!(*child_parent, me, "the child this test started names it");
    assert_eq!(child_name.as_str(), "cmd", "cmd.exe is cmd");
    assert_eq!(parent_of(child.pid()), Some(me));
    assert_eq!(
        descendants_in(&t, me).map(|d| d.contains(&(child.pid(), "cmd".to_string()))),
        Some(true)
    );
    assert!(
        !background_or_unreadable(me, &t).contains(&UNREADABLE_TABLE.to_string()),
        "a table that was read"
    );
    assert_eq!(background_or_unreadable(me, &[]), [UNREADABLE_TABLE]);
    assert!(is_our_ancestor(me, &t));
    assert!(!is_our_ancestor(child.pid(), &t), "a child is no ancestor");
    assert!(
        !is_our_ancestor(child.pid(), &[]),
        "nor by the links the kernel gives where the table has none"
    );
}

/// Windows reuses pids and never rewrites a recorded parent: a row keeps its parent only where
/// that process was created no later than the row's own.
#[test]
fn no_row_of_one_sweep_is_older_than_the_parent_it_names() {
    let rows = kernel_rows();
    assert!(
        rows.iter().any(|r| r.pid == std::process::id()),
        "the sweep read {} rows",
        rows.len()
    );
    let younger_parents: Vec<(u32, u32)> = rows
        .iter()
        .filter(|r| r.ppid != 0)
        .filter_map(|r| {
            let parent = rows.iter().find(|p| p.pid == r.ppid)?;
            (parent.start? > r.start?).then_some((r.pid, r.ppid))
        })
        .collect();
    assert!(
        younger_parents.is_empty(),
        "(child, parent) pairs: {younger_parents:?}"
    );
}

#[test]
fn an_image_name_is_the_word_a_reader_matches() {
    for (image, word) in [
        ("claude.exe", "claude"),
        ("Claude.EXE", "claude"),
        ("claude", "claude"),
        ("aterm-gui.exe", "aterm-gui"),
        ("OpenConsole.exe", "openconsole"),
        ("pwsh.exe", "pwsh"),
        ("System", "system"),
        (".exe", ".exe"),
        ("", ""),
    ] {
        assert_eq!(image_word(image), word, "{image:?}");
    }
    assert!(owned_by_aterm((7, &image_word("Aterm-GUI.exe"))));
    assert!(owned_by_aterm((7, &image_word("aterm.exe"))));
    assert!(!owned_by_aterm((7, &image_word("claude.exe"))));
}

#[test]
fn a_held_child_is_described_from_the_kernel() {
    let me = std::process::id();
    let child = Waiting::start();
    let pid = child.pid();
    assert!(alive(pid));

    let t = table();
    let held = held_of(me, &[(pid, "cmd".to_string())], &t);
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!((held[0].pid, held[0].name.as_str()), (pid, "cmd"));
    assert_eq!(
        (held[0].seat, held[0].exact),
        (upgrade::Seat::Top, false),
        "this process's own child sits at the top, and its joined arguments are never the \
         command byte for byte: {held:?}"
    );
    assert!(
        held[0].command.contains("set /p held="),
        "this process's own child is described by its command line: {:?}",
        held[0].command
    );
    let other = held_of(me.wrapping_add(1), &[(pid, "cmd".to_string())], &t);
    assert_eq!(
        other.iter().map(|h| h.command.as_str()).collect::<Vec<_>>(),
        [""],
        "another process's child is named, its command line left unread"
    );
    assert!(
        held_of(me, &[(0x7fff_fff0, "gone".to_string())], &t).is_empty(),
        "a process that is gone is not described"
    );

    let ticks = aterm_uds::process::read_creation_filetime(i32::try_from(pid).unwrap())
        .expect("the child's creation time");
    let born = aterm_sysprobe::procs::birth_of_filetime(ticks).expect("after 1970");
    assert_eq!(started_ms(pid), u64::try_from(born).ok());
    let mine = started_ms(me).expect("this process's creation time");
    assert!(Some(mine) <= started_ms(pid), "a parent is no younger");
    assert_eq!(started_ms(0x7fff_fff0), None, "no such process");
}

#[test]
fn no_process_has_a_group_or_a_terminal() {
    let me = std::process::id();
    assert_eq!(ids(me), None, "Windows has no process group");
    assert_eq!(super::live_terminal(me), None);
    assert_eq!(super::opener(me, parent_of(me).unwrap_or(0)), None);
}
