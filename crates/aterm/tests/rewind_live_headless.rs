// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rewind over a real PTY: a view opened on the instant before a `clear` shows that screen while the live grid stays cleared.

use crate::headless_boot::{self, Instance};
use crate::live_ctl::{Ctl, fields};

/// An isolated headless instance whose sessions record their screens.
fn boot() -> Instance {
    headless_boot::boot_with(
        "atrw",
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

fn rows(text: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(text)
        .lines()
        .map(|row| row.trim_end().to_string())
        .collect()
}

fn trimmed(screen: Vec<String>) -> Vec<String> {
    screen
        .into_iter()
        .map(|row| row.trim_end().to_string())
        .collect()
}

/// R0 exit item 3.
#[test]
fn a_view_on_the_instant_before_a_clear_shows_that_screen_and_the_live_grid_stays_cleared() {
    let inst = boot();
    let mut ctl = Ctl::connect(&inst.sock);
    ctl.type_line("printf 'row-%02d\\n' $(seq 1 40); echo printed-all");
    ctl.expect_on_screen("^printed-all");
    ctl.ok("await idle 500 timeout=60000");
    let before = trimmed(ctl.text());
    assert!(
        before.iter().any(|row| row == "row-40"),
        "fixture: the forty rows are on screen: {before:#?}"
    );
    let status = ctl.ok("temporal status");
    let tick: u64 = fields(&status)["latest_tick"]
        .parse()
        .unwrap_or_else(|_| panic!("a latest_tick: {status}"));

    ctl.type_line("clear");
    ctl.ok("await gone ^row- timeout=60000");
    let cleared = trimmed(ctl.text());
    assert!(
        !cleared.iter().any(|row| row.starts_with("row-")),
        "fixture: clear cleared the screen: {cleared:#?}"
    );

    let open = ctl.ok(&format!("rewind open at={tick}"));
    let open_fields = fields(&open);
    assert_eq!(open_fields.get("view"), Some(&"open"), "{open}");
    let judged = ctl.ok("temporal status");
    let witnessed: u64 = fields(&judged)["witnessed"]
        .parse()
        .unwrap_or_else(|_| panic!("a witnessed count: {judged}"));
    match open_fields.get("faithful") {
        Some(&"yes") => assert!(
            witnessed > 0,
            "faithful only once a witness was judged: {open} / {judged}"
        ),
        Some(&"unchecked") => {}
        other => panic!(
            "a replay of plain output matches its witnesses or none judged it: {other:?} ({open})"
        ),
    }
    let (header, body) = ctl.bytes("rewind text");
    assert!(
        header.starts_with(&format!("OK {}", body.len())),
        "{header}"
    );
    let past = rows(&body);
    assert_eq!(
        past, before,
        "the view shows the screen from before the clear"
    );
    let (_, replayed) = ctl.bytes(&format!("temporal {tick}"));
    assert_eq!(
        past,
        rows(&replayed),
        "the view is the recorder's replay of that instant"
    );

    let live = trimmed(ctl.text());
    assert_eq!(live, cleared, "the live grid is untouched by the view");
    assert_ne!(
        past, live,
        "negative control: the view is not the live screen"
    );

    let closed = ctl.ok("rewind close");
    assert_eq!(fields(&closed).get("view"), Some(&"closed"), "{closed}");
    assert!(
        ctl.line("rewind text")
            .starts_with("ERR rewind: no Rewind view is open"),
        "a closed view has no text"
    );
}
