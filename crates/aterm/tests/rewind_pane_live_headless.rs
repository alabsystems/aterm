// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind R3 over a real PTY: a scripted session (shell prompts, a progress bar, vim's alternate screen) replays as the Aos pane task, and its trace replays alike in a second process.

use std::os::unix::fs::PermissionsExt;

use aterm_aos_pane::trace::PaneTrace;

use crate::headless_boot::{self, Instance};
use crate::live_ctl::{Ctl, fields};

/// An isolated headless instance whose sessions record their screens.
fn boot(prefix: &str) -> Instance {
    headless_boot::boot_with(
        prefix,
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            std::fs::write(&cfg, format!("temporal_recording = true\n{base}"))
                .expect("write the config");
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    )
}

/// Let the session go quiet for more than a witness interval, so its next slice takes a witness.
fn quiet(ctl: &mut Ctl) {
    ctl.ok("await idle 1200 timeout=60000");
}

/// The value after ` path=`, the last field (a state directory may hold a space).
fn path_of(line: &str) -> std::path::PathBuf {
    let (_, path) = line
        .split_once(" path=")
        .unwrap_or_else(|| panic!("a path: {line}"));
    std::path::PathBuf::from(path.trim_end())
}

fn num(f: &std::collections::BTreeMap<&str, &str>, key: &str, line: &str) -> u64 {
    f.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("a `{key}=` count: {line}"))
}

/// R3 exit items: the faithful rate on a scripted corpus, and in-process against two-process export and import.
#[test]
fn a_scripted_session_replays_as_the_pane_task_here_and_in_another_process() {
    let vim = std::path::Path::new("/usr/bin/vim");
    assert!(
        vim.exists(),
        "fixture: the corpus runs vim ({})",
        vim.display()
    );
    let inst = boot("atr3");
    let mut ctl = Ctl::connect(&inst.sock);

    for (i, cmd) in [
        "echo prompt-one",
        "ls / >/dev/null; echo prompt-two",
        "cd /; pwd; echo prompt-three",
    ]
    .iter()
    .enumerate()
    {
        ctl.type_line(cmd);
        ctl.expect_on_screen(&format!("^prompt-{}", ["one", "two", "three"][i]));
        quiet(&mut ctl);
    }

    ctl.type_line(
        "bar='####################'; i=0; while [ $i -le 20 ]; do printf '\\r[%-20s] %3d%%' \
         \"${bar:0:$i}\" $((i*5)); i=$((i+1)); sleep 0.15; done; echo; echo bar-done",
    );
    ctl.expect_on_screen("^bar-done");
    quiet(&mut ctl);

    ctl.type_line("vim -u NONE -i NONE -N -n");
    ctl.expect_on_screen("^~");
    quiet(&mut ctl);
    ctl.send("ihello from vim");
    ctl.send("\x1b");
    ctl.expect_on_screen("^hello.from.vim");
    quiet(&mut ctl);
    ctl.send(":q!");
    ctl.ok("key enter");
    quiet(&mut ctl);
    ctl.type_line("echo vim-closed");
    ctl.expect_on_screen("^vim-closed");
    quiet(&mut ctl);
    ctl.type_line("echo the-end");
    ctl.expect_on_screen("^the-end");

    let line = ctl.ok("rewind check export");
    eprintln!("rewind check export: {line}");
    let f = fields(&line);
    let (witnesses, matched) = (num(&f, "witnesses", &line), num(&f, "matched", &line));
    assert!(
        witnesses >= 6 && num(&f, "slices", &line) > 0,
        "REACH: the corpus is witnessed along the way: {line}"
    );
    assert_eq!(f.get("verdict"), Some(&"pass"), "{line}");
    assert_eq!(
        matched, witnesses,
        "the faithful rate is every witness: {line}"
    );

    let path = path_of(&line);
    let bytes = std::fs::read(&path).expect("the exported trace");
    let trace = PaneTrace::decode(&bytes).expect("this process decodes the trace");
    assert_eq!(
        (trace.head_state.short(), trace.head_chain.short()),
        (f["state"].to_string(), f["chain"].to_string()),
        "the file's head is the exporting process's replay"
    );
    let mut by_reply = std::collections::BTreeMap::<String, usize>::new();
    for r in &trace.effects {
        *by_reply.entry(r.payload.short()).or_default() += 1;
    }
    let quiet_slices = by_reply.values().max().copied().unwrap_or(0);
    eprintln!(
        "slices that replied: {} of {}",
        trace.effects.len() - quiet_slices,
        trace.effects.len()
    );
    assert!(
        by_reply.len() >= 2,
        "REACH: some slice of the corpus replied, so RepliesMatch judged a reply: {by_reply:?}"
    );

    let other = boot("atr3b");
    let mut there = Ctl::connect(&other.sock);
    let checked = there.ok(&format!("aos check {}", path.display()));
    eprintln!("aos check in another process: {checked}");
    let g = fields(&checked);
    assert_eq!(g.get("task"), Some(&"aterm.pane.screen"), "{checked}");
    assert_eq!(g.get("verdict"), Some(&"pass"), "{checked}");
    let exported: usize = f["exported"].parse().expect("an exported chain");
    assert!(exported < num(&f, "chains", &line) as usize, "{line}");
    for key in ["state", "chain"] {
        assert_eq!(
            g.get(key),
            f.get(key),
            "`{key}=` here and there: {line} / {checked}"
        );
    }

    let forged = other.tmp.join("forged.aos");
    let mut flipped = bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 1;
    std::fs::write(&forged, &flipped).expect("write the forged trace");
    std::fs::set_permissions(&forged, std::fs::Permissions::from_mode(0o600)).expect("0600");
    let refused = there.line(&format!("aos check {}", forged.display()));
    assert!(
        refused.starts_with("ERR aos check refused=seal"),
        "negative control: a flipped byte is refused: {refused}"
    );
    assert!(PaneTrace::decode(&flipped).is_err(), "here too");
}
