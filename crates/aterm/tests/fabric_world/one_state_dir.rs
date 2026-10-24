// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ONE STATE DIR IS ONE BRIDGE** — two REAL bridges on one `--state`.

use crate::harness;

use std::fs::File;

use harness::{FLEET, World, kv, until};

/// The node's own presence row, while it says `live`.
fn live_node_row(w: &World) -> Option<String> {
    w.row(&format!("/f/{FLEET}/pub/{}/node/presence", w.node))
        .filter(|b| b.contains("state=live"))
}

/// The pid the state dir's `pid` file names right now.
fn pid_file(w: &World) -> Option<u32> {
    std::fs::read_to_string(w.state.join("pid"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// **A SECOND BRIDGE ON A HELD STATE DIR NEVER RUNS AS THE NODE: a hand-started
/// one is refused by name; an instance's own stands by, and carries the node
/// once the holder is gone.**
#[test]
fn a_second_bridge_on_one_state_dir_is_refused_or_stands_by_and_takes_over() {
    let mut w = World::boot("one-state", &[]);
    w.wait_ready();
    let holder = w.bridge_pid();
    let row = until("the holder's live node row", || live_node_row(&w));
    assert_eq!(kv(&row, "pid"), Some(holder.to_string().as_str()), "{row}");
    let inc: u64 = kv(&row, "inc").and_then(|n| n.parse().ok()).expect("inc=");

    let command = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("fabric command");
    let words: Vec<&str> = command.split_whitespace().collect();
    assert_eq!(words.get(1..3), Some(&["link", "serve"][..]), "{command}");
    let token_file = w.tmp.join("observer.token");
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&token_file)
            .expect("token file");
        f.write_all(w.token.as_bytes()).expect("write the token");
    }
    let err_path = w.tmp.join("second-bridge.err");
    let mut second = aterm_uds::spawnfd::command(harness::aterm_to_launch());
    let mut second = harness::scratch_spawnfd(&mut second, &w.tmp)
        .args(&words[1..])
        .arg("--sock")
        .arg(&w.ctl_sock)
        .arg("--token-file")
        .arg(&token_file)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::from(
            File::create(&err_path).expect("stderr file"),
        ))
        .spawn()
        .expect("spawn a second bridge");
    let status = until("the hand-started second bridge to exit", || {
        second.try_wait().ok().flatten()
    });
    let err = std::fs::read_to_string(&err_path).unwrap_or_default();
    assert!(!status.success(), "a second bridge ran: {status}\n{err}");
    assert!(
        err.contains(&format!("is held by another bridge (pid {holder})"))
            && err.contains("one state dir is one node"),
        "the refusal names neither the holder nor the remedy:\n{err}"
    );

    let sibling = w.sibling_instance_armed();
    let sibling_fabric = || {
        harness::connect(&sibling.sock, &sibling.token)
            .and_then(|mut c| c.request("fabric status"))
            .map(|r| r.header().to_string())
            .unwrap_or_default()
    };
    until("the sibling's bridge to report the state dir held", || {
        let h = sibling_fabric();
        (kv(&h, "state") == Some("stalled") && kv(&h, "reason") == Some("state-locked"))
            .then_some(())
    });
    assert_eq!(pid_file(&w), Some(holder as u32));
    let row = live_node_row(&w).expect("the node row is still live");
    assert_eq!(kv(&row, "pid"), Some(holder.to_string().as_str()), "{row}");
    assert_eq!(kv(&row, "inc"), Some(inc.to_string().as_str()), "{row}");
    let (a, b) = w.two_sessions();
    let posted = w.verb(&format!("@{a} post to=@{b} kind=note --wait=60000 standby"));
    assert!(
        posted.ok(),
        "post: {} — log:\n{}",
        posted.header(),
        w.log_tail()
    );
    until("the holder's post to land", || {
        w.inbox(&b)
            .iter()
            .any(|r| r.contains("text=standby"))
            .then_some(())
    });

    if let Some(mut gui) = w.take_gui() {
        let _ = gui.kill();
        let _ = gui.wait();
    }
    until("the sibling's bridge to carry the node", || {
        (kv(&sibling_fabric(), "state") == Some("connected")).then_some(())
    });
    let taker = until("the new holder's pid file", || {
        pid_file(&w).filter(|p| *p != holder as u32)
    });
    let row = until("the new holder's live node row", || {
        live_node_row(&w).filter(|r| kv(r, "pid") == Some(taker.to_string().as_str()))
    });
    let taken_at: u64 = kv(&row, "inc").and_then(|n| n.parse().ok()).expect("inc=");
    assert!(
        taken_at > inc,
        "the takeover did not outrank the holder: {row}"
    );
}
