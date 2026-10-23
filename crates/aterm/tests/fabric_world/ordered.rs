// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end ordered durability and selective escalation tests for the fabric broker.

use crate::harness::{self, FLEET, World, until};

use std::path::PathBuf;

/// Creates an ordered-durability broker world with no horizon, or `None` if strict.
fn ordered_world(tag: &str) -> Option<World> {
    let mut opts = harness::broker_opts(aterm_link::durability::Durability::Ordered);
    opts.durability = astream_broker::Durability::Ordered { horizon: None };
    let w = World::boot_opts(tag, opts);
    let served = w
        .broker
        .as_ref()
        .expect("the world serves its broker")
        .served_durability();
    if served.served != astream_broker::Tier::Ordered {
        eprintln!(
            "ordered is served {:?} here ({:?}): every record is durable when visible, nothing to \
             tell apart",
            served.served, served.reason
        );
        return None;
    }
    Some(w)
}

/// `person`'s cap file.
fn person_cap(w: &World, person: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = w.tmp.join(format!("{person}.cap"));
    let caps: String = aterm_link::enable::person_grants(FLEET, person)
        .iter()
        .map(|g| harness::cap_line(g))
        .collect();
    std::fs::write(&path, caps).expect("write the person's cap");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    path
}

/// Run `aterm fabric <args>` as a person on this world's fleet.
fn fabric(w: &World, args: &[&str]) -> (String, String) {
    let cmd_line = std::fs::read_to_string(w.tmp.join("fabric.cmd")).expect("the fabric command");
    #[expect(
        clippy::disallowed_types,
        reason = "the harness's bounded reader (`output_within`) takes a std Command; the child \
                  is `aterm fabric`, run to its exit, so an inherited descriptor lives no longer"
    )]
    let mut cmd = std::process::Command::new(harness::aterm_to_launch());
    harness::prepare_gui_environment(&mut cmd);
    cmd.arg("fabric")
        .args(args)
        .env("ATERM_FABRIC_COMMAND", cmd_line.trim())
        .env("HOME", w.tmp.join("home"))
        .env("XDG_CONFIG_HOME", w.tmp.join("cfg"))
        .env("XDG_RUNTIME_DIR", w.tmp.join("run"));
    let out = harness::output_within(&mut cmd, "aterm fabric");
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert!(
        out.status.success(),
        "aterm fabric {args:?}: {:?}\n{stdout}\n{stderr}",
        out.status
    );
    (stdout, stderr)
}

/// The offset `@<off>` that `text` names after `marker`.
fn at_off(text: &str, marker: &str) -> u64 {
    let rest = text
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("no `{marker}` in:\n{text}"));
    rest.trim_start_matches('@')
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no offset after `{marker}` in:\n{text}"))
}

/// `from` posts `text` (one word) to the session `to`, and waits until `to`'s inbox shows it.
fn agent_note(w: &World, from: &str, to: &str, text: &str) -> u64 {
    let posted = w.verb(&format!("@{from} post to=@{to} kind=note -- {text}"));
    assert!(posted.ok(), "{}", posted.header());
    until(&format!("{to} to see `{text}`"), || {
        w.inbox(to)
            .iter()
            .find(|row| row.ends_with(&format!("text={text}")))
            .map(|row| at_off(row, "off="))
    })
}

/// **WHAT A PERSON WOULD MISS IS FLUSHED IN FULL BEFORE IT IS SHOWN; AN
/// AGENT'S NOTE IS NOT.** On an ordered broker with aterm's table and no
/// horizon:
#[test]
fn an_ordered_broker_flushes_a_persons_records_before_it_shows_them() {
    let Some(w) = ordered_world("ord") else {
        return;
    };
    w.wait_ready();
    let broker = w.broker.as_ref().expect("the world serves its broker");
    let (a, b) = w.two_sessions();
    let full = || broker.flush_counts().full;

    let note = agent_note(&w, &a, &b, "before");
    assert!(
        broker.durable_head() <= note,
        "an agent's note is not escalated: durable {} at @{note}",
        broker.durable_head()
    );
    assert_eq!(full(), 0, "no full flush was spent on machine records");

    let cap = person_cap(&w, "h-x");
    let cap = cap.to_string_lossy();
    let (out, _) = fabric(&w, &["halt", "--cap-file", &cap, "stop", "now"]);
    let halt = at_off(&out, "halt is ON at ");
    assert!(
        broker.durable_head() > halt,
        "the halt @{halt} is durable when `halt` prints: durable {}",
        broker.durable_head()
    );
    assert!(
        halt > note && broker.durable_head() > note,
        "the whole file"
    );
    assert_eq!(full(), 1, "one full flush, the halt's");
    let (out, _) = fabric(&w, &["halt", "--cap-file", &cap, "off"]);
    let lift = at_off(&out, "halt is OFF at ");
    assert!(broker.durable_head() > lift, "a lift is escalated too");
    let after_lift = full();
    assert_eq!(after_lift, 2);

    let note = agent_note(&w, &a, &b, "after");
    assert!(broker.durable_head() <= note, "@{note} rides the horizon");

    let asked = w.verb(&format!("@{a} post to=h-x kind=ask dl=600000 -- ping"));
    assert!(asked.ok(), "{}", asked.header());
    let ask = harness::kv(asked.header(), "off")
        .and_then(|o| o.parse::<u64>().ok())
        .expect("an ask answers its offset");
    assert!(broker.durable_head() > ask, "mail to a person is escalated");
    let mailed = full();
    assert_eq!(mailed, after_lift + 1, "one full flush, the ask's");
    let note = agent_note(&w, &a, &b, "unread");
    assert!(broker.durable_head() <= note);
    let (out, err) = fabric(&w, &["inbox", "--cap-file", &cap]);
    assert!(
        out.contains(&format!("@{ask} ")),
        "the ask is in the inbox:\n{out}"
    );
    let upto = at_off(&err, "marked read through ");
    assert!(upto >= ask, "{err}");
    assert!(
        broker.durable_head() > note,
        "the read mark's full flush covers the note @{note} before it: durable {}",
        broker.durable_head()
    );
    assert_eq!(full(), mailed + 1, "one full flush, the read mark's");
}
