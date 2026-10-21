// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE REVIEWER'S INSTRUMENT for the account stats' record (slice A1; kept in the tree, linted with
// `--all-targets`): usage-tap reading files the real `aterm harness tap` wrote, in the order given,
// recorded the way the window's host records one at a decided turn end — the freshness rule
// (`tap::Fresh::admit`), what it saw beside the reading before (`tap::Fresh::beside`: a refill), the
// default home's key (`profile::default_home_key`) and `accounts::note_reading` in one store
// transaction — into a SCRATCH harness state, under `main`. For the real-Claude lab, whose Claude Code
// runs on a scratch `CLAUDE_CONFIG_DIR` and a fake token, so that the host's own attribution (the
// machine's stored login in the default home) never records it there. It prints, per file, whether
// the reading was fresh, what it carried in tenths, and the ledger rows it wrote — never a path.
// `targo --unverified run -q -p aterm-agent --example tap_record -- <scratch-harness-state> <user-home> <reading.json>...`
use std::path::PathBuf;

use aterm_agent::harness::accounts::{self, Store};
use aterm_agent::harness::cli::ACCOUNTS_OWN_LABEL;
use aterm_agent::harness::profile;
use aterm_agent::harness::tap::{Fresh, Reading};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(state), Some(home)) = (args.next(), args.next()) else {
        eprintln!("usage: tap_record <scratch-harness-state> <user-home> <reading.json>...");
        std::process::exit(2);
    };
    let store = Store::at(&PathBuf::from(state));
    let key = profile::default_home_key(&PathBuf::from(home));
    println!("key: {}", key.map_or("none".to_owned(), |k| k.to_string()));
    let mut fresh = Fresh::default();
    for (n, file) in args.enumerate() {
        let text = std::fs::read_to_string(&file).expect("a reading file");
        let reading = Reading::of_json(&text).expect("a reading of this format");
        let beside = fresh.beside(&reading);
        if !fresh.admit(&reading) {
            println!("{n}: not fresh");
            continue;
        }
        let read = reading.tap_read(beside, key);
        let now = reading.at;
        let mut note = None;
        let events = store
            .transact(now, &mut note, |state| {
                Ok(accounts::note_reading(
                    state,
                    ACCOUNTS_OWN_LABEL,
                    &read,
                    now,
                ))
            })
            .expect("recorded");
        let x10 = |w: Option<accounts::WindowRead>| {
            w.map_or("-".to_owned(), |w| match w.refilled {
                Some(from) => format!("{} (refilled from {from})", w.pct_x10),
                None => w.pct_x10.to_string(),
            })
        };
        println!(
            "{n}: fresh, seen before {}, 5h {}, 7d {}, model {}, rows: {}",
            read.seen_before,
            x10(read.five_hour),
            x10(read.seven_day),
            reading.model.as_deref().unwrap_or("-"),
            events
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
}
