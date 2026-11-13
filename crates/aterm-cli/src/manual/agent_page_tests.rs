// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The longest session id a brief names (`s-` and 20 hex digits).
const SID: &str = "s-0123456789abcdef0123";

/// A brief page's lines fit a 100-column terminal, and each `aterm ctl` verb it teaches is
/// one the server's table or the client serves.
fn fits_and_teaches_served_verbs(name: &str, text: &str) {
    for line in text.lines() {
        assert!(
            line.chars().count() <= 100,
            "`aterm help {name}` has a line of {} columns: {line}",
            line.chars().count()
        );
    }
    for (command, verb) in aterm_primer::taught_ctl_verbs(text) {
        assert!(
            aterm_types::control_verbs::spec(&verb).is_some()
                || aterm_ctl::CLIENT_HELP_VERBS.contains(&verb.as_str()),
            "`aterm help {name}` teaches `aterm ctl {command}`, which nothing serves"
        );
    }
}

/// The brief, in a session and out of one, fits 100 columns, teaches only served verbs and
/// keeps its basics (what aterm is, the commands and the rules) on one screen; every section
/// renders under its name, is in the MORE list and fits too; every ELSEWHERE topic has its
/// index line and opens a page; and a word that names no section is refused with the list.
#[test]
fn every_section_renders_and_the_basics_fit_one_screen() {
    for sid in [Some(SID), None] {
        let page = page(sid);
        fits_and_teaches_served_verbs("agent", &page);
        let basics = page.split("\nMORE").next().unwrap_or_default();
        assert!(basics.lines().count() <= 66, "{basics}");
        assert!(
            basics.contains("\nCOMMANDS") && basics.contains("\nRULES"),
            "{basics}"
        );
        for sec in SECTIONS {
            assert!(has_row(&page, sec.name, sec.summary), "{}", sec.name);
        }
        for name in ELSEWHERE {
            let line = super::super::index_line(name).expect("an ELSEWHERE topic is indexed");
            assert!(has_row(&page, name, line), "{name}");
            assert_eq!(super::super::render(Some(name), None, None).1, 0, "{name}");
        }
        assert!(has_row(&page, TOPICS_ROW.0, TOPICS_ROW.1));
        assert_eq!(super::super::render(Some(TOPICS_ROW.0), None, None).1, 0);
    }
    assert!(page(Some(SID)).contains(&format!("session {SID}")));
    for sec in SECTIONS {
        let text = section(sec.name).expect("a listed section renders");
        fits_and_teaches_served_verbs(&format!("agent {}", sec.name), &text);
    }
    assert!(section("bogus").is_none());
    let refusal = unknown_section("bogus");
    assert!(
        SECTIONS
            .iter()
            .all(|sec| has_row(&refusal, sec.name, sec.summary))
    );
}

/// Whether `text` has a list row that names `name`, then at least two spaces, then `line`.
fn has_row(text: &str, name: &str, line: &str) -> bool {
    text.lines().any(|row| {
        row.strip_prefix("  ")
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| rest.starts_with("  ") && rest.trim_start() == line)
    })
}
