// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A8 — `glance.json`, and the fabric as a session.**
//!
//! Two claims, both end to end against the shipped binaries on A5's sealed
//! fleet:
//!
//! 1. **`glance.json` rows equal the `Last` presence read**, carrying
//!    `attention=` and `fabric=`. The file is what a menu bar reads instead of
//!    holding a capability and blocking a UI thread on a broker round trip
//!    (§9.3), so the one thing it must never be is a SUMMARY that has drifted
//!    from the answer `aterm-link ls` prints. The assertion is therefore an
//!    equality against an INDEPENDENT `Last` read this file does itself — not
//!    against `glance::read`, which would only prove the code agrees with
//!    itself.
//! 2. **`aterm-link tui` renders published records with trust labels first**,
//!    in a real aterm PTY, and `aterm-ctl … search halt` finds the halt. The
//!    tui is an ordinary program in an ordinary terminal (§9.3): nothing here
//!    mocks a screen, and the screen is read back through the same control verbs
//!    an operator would type.
//!
//! ## Two deviations from the rung's own wording, both deliberate
//!
//! * **`@fabric` is not a selector aterm has.** `Selector::parse`
//!   (`crates/aterm-gui/src/control.rs`) is total over three forms — `@.`,
//!   `@<local u64>` and `@<sid>` — and `adopt_injected_identity` accepts a sid
//!   only in the `s-` + 20 hex form (`spawn.rs:406`), so a session cannot be
//!   NAMED `fabric` either. The tui is therefore addressed by the sid of the
//!   session it runs in, `@s-<hex>`, which is the same command with a resolvable
//!   selector. Adding a name alias is a change to `control.rs`, which A8 does
//!   not own.
//! * **The tui runs in the node's OWN boot session**, started by typing at it
//!   through `send`, rather than in a second `aterm-gui` spawned with a wrapper
//!   `$SHELL`. One aterm, one PTY, one bridge — and the transcript is read back
//!   from the same instance that hosts it.
//!
//! ## No sleeps as synchronisation
//!
//! Every wait is `until <observable state>` — a file on disk, a line on a
//! screen, a match from `search`. The bound is a HANG DETECTOR.

#![cfg(unix)]

mod harness;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_link::glance::{self, Glance};
use harness::{until, Fleet, Node, FLEET};

/// `aterm-ctl`, found by [`harness::built_binary`], the finder `aterm-gui` uses: this
/// crate is its own workspace, so there is no `CARGO_BIN_EXE_` for a binary of
/// the aterm workspace and it has to be located rather than declared. A missing
/// one is a hard failure naming the command that produces it — a rung that
/// passed because its subject was not built would be worth nothing.
///
/// AND A STALE ONE IS THE SAME FAILURE IN BETTER CLOTHES, which this doc argued
/// for and did not do: the sentence above was here while nothing checked the
/// binary's AGE, and when that was noticed `target/debug/aterm-ctl` was four and
/// a half hours behind `crates/aterm-types/src/control_verbs.rs` — the file the
/// round was auditing — with this suite driving it green. It is the same finder and
/// guard `gui_binary` uses, taking the crate and the `$…_BIN` exemption as arguments,
/// because "every binary this crate FINDS" is the property worth keeping and
/// one wired call site is not it.
fn ctl_binary() -> PathBuf {
    harness::built_binary("aterm-ctl", "aterm-ctl", "ATERM_CTL_BIN")
}

/// Run one `aterm-ctl` command against a node's control socket, as an operator
/// would: the real client, resolving the instance token out of the socket's
/// sibling token file by itself.
fn ctl(node: &Node, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(ctl_binary());
    cmd.arg("--sock").arg(&node.ctl_sock).args(args);
    // The caller's own aterm identity must not leak in: `aterm-ctl` locates the
    // instance hosting the CALLING terminal when it is not told otherwise, and a
    // test run from inside aterm would then read the developer's own screen.
    for var in [
        "ATERM_SESSION_ID",
        "ATERM_PARENT_SESSION_ID",
        "ATERM_CONTROL_SOCK",
        "ATERM_LAUNCH_NONCE",
    ] {
        cmd.env_remove(var);
    }
    let out = cmd.output().expect("run aterm-ctl");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// The node's visible screen, one row per line.
fn screen(node: &Node, sid: &str) -> Vec<String> {
    let (ok, text) = ctl(node, &[&format!("@{sid}"), "text"]);
    assert!(ok, "aterm-ctl text failed: {text}");
    text.lines().map(str::to_string).collect()
}

/// Every `key=value` token of a body's first line — the test's OWN reading of a
/// presence row, kept independent of `glance`'s so an equality between them
/// means something.
fn tokens(body: &[u8]) -> BTreeMap<String, String> {
    String::from_utf8_lossy(body)
        .split('\n')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|t| t.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The whole `Last{/f/<F>/pub/*/*/presence}` answer, paged the way §5.2 says it
/// must be: on the RESUME cursor, never on a short page.
fn last_presence(fleet: &Fleet) -> Vec<(String, BTreeMap<String, String>)> {
    let mut god = fleet.god();
    let filter = format!("/f/{FLEET}/pub/*/*/presence");
    let mut after = String::new();
    let mut out = Vec::new();
    loop {
        let (page, _, resume) = god.last_page(&filter, &after, 512).expect("Last");
        for (_, subject, body) in &page {
            out.push((subject.clone(), tokens(body)));
        }
        if resume.is_empty() {
            return out;
        }
        after = resume;
    }
}

/// Run the shipped `aterm-link glance` against this fleet and read the file back.
fn run_glance(fleet: &Fleet, state: &Path) -> Glance {
    let cap = fleet.cap_file("glance", &[format!("ro:/f/{FLEET}/pub/>")]);
    let out = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
        .args(["glance", "--fleet", FLEET, "--broker", &fleet.addr])
        .args(harness::fleet_child_flags(&fleet.key_file))
        .args([
            "--cap-file",
            cap.to_string_lossy().as_ref(),
            "--state",
            state.to_string_lossy().as_ref(),
        ])
        .output()
        .expect("run aterm-link glance");
    assert!(
        out.status.success(),
        "aterm-link glance failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        PathBuf::from(&printed),
        glance::path(state),
        "glance must print the path it wrote"
    );
    let text = std::fs::read_to_string(&printed).expect("read glance.json");
    Glance::from_json(&text).unwrap_or_else(|| panic!("glance.json did not parse:\n{text}"))
}

/// **`glance.json` IS the `Last` presence read** — every row, every token, plus
/// `attention=` and `fabric=` on every one of them.
///
/// The `attention=` row is forged into the log under the fleet-root cap, as a
/// node on ANOTHER host would publish it — this test is about `glance` carrying
/// whatever the presence read holds rather than a fixed set of tokens it knows
/// about, so the row it reads has to come from a host it does not run. (The
/// local bridge does write `attention=` now, from the session's own `meta`; see
/// `r1_bridge_addressing.rs` for the writer's own test. It writes `-` for the
/// session here, which has no escalation on it.)
#[test]
fn glance_json_rows_equal_the_last_presence_read_with_attention_and_fabric() {
    let mut fleet = Fleet::boot("glance");
    let node = fleet.node("glance-a", &[]);
    node.wait_ready(&fleet);
    let (sid, _) = node.session();

    // A row from a host this test does not run: a live session on `n-remote`
    // with an escalation on it. `attention=` is pct-encoded on the wire.
    let remote_sid = "s-00000000000000000001";
    let remote_subject = format!("/f/{FLEET}/pub/n-remote/{remote_sid}/presence");
    let remote_body = format!(
        "v=1 t={} inc=1 state=live hold=0 holder=- fabric=connected \
         attention=needs%20a%20human%20on%20the%20deploy",
        aterm_link::now_ms()
    );
    fleet
        .god()
        .publish(
            astream_cap::producer_id_of("n-remote"),
            1,
            &remote_subject,
            remote_body.as_bytes(),
        )
        .expect("publish the remote presence row");

    let state = fleet.tmp.join("glance-state");
    // BOTH READS IN ONE `until`. Presence is last-value and the fleet is live:
    // a row can change between the file being written and the comparison read,
    // and a single-shot compare would be a flake with a plausible failure
    // message. The loop ends on the two agreeing, which they do as soon as the
    // fleet is quiet; the bound is the hang detector.
    let (file, last) = until("glance.json to equal the Last presence read", || {
        let file = run_glance(&fleet, &state);
        let last = last_presence(&fleet);
        (file.rows.len() == last.len()
            && file
                .rows
                .iter()
                .zip(&last)
                .all(|(row, (subject, want))| row.subject == *subject && agrees(row, want)))
        .then_some((file, last))
    });

    assert!(!file.truncated, "this fleet is far short of the row cap");
    assert_eq!(file.fleet, FLEET);
    assert!(
        last.len() >= 3,
        "the fleet should hold the node row, its session's row and the remote row: {last:?}"
    );

    // The two tokens the human actually reads are on EVERY row, present or `-`.
    for row in &file.rows {
        for key in ["attention", "fabric"] {
            assert!(
                row.fields.contains_key(key),
                "{} has no {key}=: {:?}",
                row.subject,
                row.fields
            );
        }
    }
    // The node's own rows say the fabric is up; the forged remote row carries
    // its escalation through verbatim, still pct-encoded as it crossed the bus.
    let node_row = file
        .rows
        .iter()
        .find(|r| r.node == node.node && r.owner == sid)
        .unwrap_or_else(|| panic!("no row for {sid}: {:?}", file.rows));
    // A SESSION ROW MAKES NO REACHABILITY CLAIM. `fabric=` used to be the
    // literal `connected` on every session row, and the only `Will` the bridge
    // registers is on the NODE face — so a dead node's session rows all still
    // said `connected` while its own node row said `gone`. `glance` defines the
    // field as "whether that session's node can still be reached" and is a
    // pass-through, so the honest value is the absent one `GUARANTEED` renders
    // as `-`: unknown, rather than a constant that cannot answer the question.
    assert_eq!(node_row.field("fabric"), glance::ABSENT);
    assert_eq!(node_row.field("attention"), glance::ABSENT);
    // The NODE's own row still carries it, because that is the row the will
    // rewrites to `gone fabric=disconnected` when the bridge dies.
    let own_row = file
        .rows
        .iter()
        .find(|r| r.node == node.node && r.owner == "node")
        .unwrap_or_else(|| panic!("no node row: {:?}", file.rows));
    assert_eq!(own_row.field("fabric"), "connected");
    let remote_row = file
        .rows
        .iter()
        .find(|r| r.subject == remote_subject)
        .unwrap_or_else(|| panic!("no remote row: {:?}", file.rows));
    assert_eq!(
        remote_row.field("attention"),
        "needs%20a%20human%20on%20the%20deploy"
    );
    assert_eq!(remote_row.field("fabric"), "connected");
}

/// Whether a written row carries exactly the tokens the `Last` read holds. The
/// two guaranteed keys are allowed to be present-as-`-` where the body had none;
/// nothing else may be added, dropped or changed.
fn agrees(row: &glance::Row, want: &BTreeMap<String, String>) -> bool {
    for (k, v) in &row.fields {
        match want.get(k) {
            Some(w) => {
                if w != v {
                    return false;
                }
            }
            None => {
                if v != glance::ABSENT {
                    return false;
                }
            }
        }
    }
    want.keys().all(|k| row.fields.contains_key(k))
}

/// **The fabric as a session.** `aterm-link tui` runs in the node's own aterm
/// PTY, renders the fleet transcript with the trust label FIRST on every line,
/// and `aterm-ctl @<sid> search halt` finds the halt a human broadcast.
///
/// ORDER IS THE CORRECTNESS PROPERTY here, not the formatting: a label that
/// arrives after the content it labels has already lost, because the content was
/// read first. So the assertion is not "the screen mentions trust" but "the row
/// the halt is on BEGINS with the label", and `search` is asked for the label
/// and the kind together — a match for `trust=human halt` exists only if the two
/// are adjacent, in that order, on one row.
#[test]
fn the_tui_renders_a_halt_with_its_trust_label_first_and_search_finds_it() {
    let mut fleet = Fleet::boot("tui");
    let node = fleet.node("tui-a", &[]);
    node.wait_ready(&fleet);
    let (sid, _) = node.session();

    // The shell has to have drawn something before it is typed at — not a
    // synchronisation sleep but the observable "this PTY is alive".
    until("the boot session's shell to draw", || {
        screen(&node, &sid)
            .iter()
            .any(|r| !r.trim().is_empty())
            .then_some(())
    });

    // READ-ONLY. The tui holds one `ro:` grant over the whole fleet: it renders
    // the conversation and cannot publish a word of it, which is what a
    // transcript window should be able to prove about itself.
    let cap = fleet.cap_file("tui", &[format!("ro:/f/{FLEET}/>")]);
    let cmd = format!(
        "{} tui --fleet {FLEET} --broker {} {} --cap-file {} --state {}",
        env!("CARGO_BIN_EXE_aterm-link"),
        fleet.addr,
        harness::fleet_child_flags(&fleet.key_file).join(" "),
        cap.display(),
        fleet.tmp.join("tui-state").display()
    );
    let reply = node.verb(&format!("@{sid} send {cmd}\\n"));
    assert!(reply.ok(), "send: {}", reply.header());

    // The transport NAME is derived, not spelled: this fixture serves sealed
    // only with `--features sealed`, and hard-coding `tcp+sealed` made this the
    // one test that failed in a default build for saying what it was told.
    let want_transport = format!("transport={}", harness::fleet_transport(fleet.key()).name());
    until("the tui to announce itself on the fleet wire", || {
        screen(&node, &sid)
            .iter()
            .any(|r| r.contains("aterm-link tui fleet=f1") && r.contains(&want_transport))
            .then_some(())
    });

    // THE HALT, from a human, on the fleet broadcast face (§9.3).
    let halt_subject = format!("/f/{FLEET}/fleet/h-andrew/halt");
    let halt_body = format!(
        "v=1 t={} state=on reason=main%20broken",
        aterm_link::now_ms()
    );
    let (halt_offset, _) = fleet
        .god()
        .publish(
            astream_cap::producer_id_of("h-andrew"),
            1,
            &halt_subject,
            halt_body.as_bytes(),
        )
        .expect("broadcast the halt");

    // THE ROW BEGINS WITH THE LABEL. Not "contains" — begins.
    let want = format!("trust=human halt @{halt_offset} from=h-andrew");
    let rows = until("the tui to render the halt, label first", || {
        let rows = screen(&node, &sid);
        rows.iter().any(|r| r.starts_with(&want)).then_some(rows)
    });
    let row = rows
        .iter()
        .find(|r| r.starts_with(&want))
        .expect("just found");
    assert!(
        row.contains("state=on") && row.contains("reason=main broken"),
        "the halt's own fields must be on the row: {row}"
    );
    // NOTHING BEFORE THE LABEL, and only one label on the row: the record's
    // content cannot have put a second one there.
    assert_eq!(row.matches("trust=").count(), 1, "{row}");

    // `aterm-ctl @<sid> search halt` — the operator's own command. `@fabric` is
    // not a selector aterm has; see the module note.
    let (ok, found) = ctl(&node, &[&format!("@{sid}"), "search", "halt"]);
    assert!(ok, "aterm-ctl search failed: {found}");
    assert!(
        found.lines().count() >= 1 && !found.trim().is_empty(),
        "search halt found nothing on:\n{}",
        rows.join("\n")
    );
    // And the label is adjacent to the kind, in that order, on some row — the
    // ordering property asked of the SCREEN rather than of the renderer.
    let (ok, ordered) = ctl(&node, &[&format!("@{sid}"), "search", "trust=human halt"]);
    assert!(ok, "aterm-ctl search failed: {ordered}");
    assert!(
        !ordered.trim().is_empty(),
        "the label does not immediately precede the kind on any row:\n{}",
        rows.join("\n")
    );

    // THE INVARIANT OVER THE WHOLE SCREEN, which is the one a wrapped line does
    // not break: `trust=` occurs on a row only at column 0, and only once.
    //
    // A record longer than the terminal is WRAPPED by the terminal, so "every
    // row starts with a label" is simply false — a continuation row starts with
    // whatever the wrap landed on. What must hold is that a continuation can
    // never LOOK like a record header, and that is what reserving the `trust=`
    // token buys: content spelling it renders `trust\=`, so any row that holds
    // the bare token holds a label this receiver computed, at its start.
    for row in &rows {
        let row = row.trim_end();
        if !row.contains("trust=") {
            continue;
        }
        assert!(
            row.starts_with("trust="),
            "a label away from column 0 — a wrapped row could be read as a record: \
             {row:?}\n{}",
            rows.join("\n")
        );
        assert_eq!(row.matches("trust=").count(), 1, "{row:?}");
    }
}
