// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A sys page's lines fit a 100-column terminal. The one line that is this executable's
/// path (the cpu section's) is data, as long as the folder it was built in.
fn fits(name: &str, text: &str) {
    let exe = std::env::current_exe().map(|p| p.display().to_string());
    for line in text.lines() {
        if exe.as_deref().is_ok_and(|exe| line.trim() == exe) {
            continue;
        }
        assert!(
            line.chars().count() <= 100,
            "`aterm help {name}` has a line of {} columns: {line}",
            line.chars().count()
        );
    }
}

/// The page's VIEWS rows are the server's views, in its order, then the two actions that
/// share the verb, and each of those parses as `sys` takes it: a view the parser gains
/// cannot go unlisted, and a row cannot name one it does not serve.
#[test]
fn the_page_lists_every_view_the_server_takes() {
    use aterm_vitals::sys::{SysQuery, View};
    let words: Vec<&str> = VIEW_ROWS.iter().map(|(w, _)| *w).collect();
    let served: Vec<&str> = View::ALL.iter().map(|v| v.word()).collect();
    assert_eq!(words[..served.len()], served[..]);
    assert_eq!(words[served.len()..], ["name", "policy"]);
    assert!(SysQuery::parse("name job=a1").is_ok());
    let page = page();
    for word in words {
        assert!(
            page.lines().any(|l| l.starts_with(&format!("  {word} "))),
            "{word}: {page}"
        );
    }
}

/// Every section renders under its name, is in the page's MORE list, fits 100 columns and
/// teaches only served `aterm ctl` verbs; the page's basics fit one screen; and a word that
/// names no section is refused with the list.
#[test]
fn every_section_renders_and_the_basics_fit_one_screen() {
    let page = page();
    fits("sys", &page);
    let basics = page.split("\nMORE").next().unwrap_or_default();
    assert!(basics.lines().count() <= 66, "{basics}");
    for sec in SECTIONS {
        let text = section(sec.name).expect("a listed section renders");
        assert!(page.contains(&format!("  {:<9} {}", sec.name, sec.summary)));
        fits(&format!("sys {}", sec.name), &text);
        for (command, verb) in aterm_primer::taught_ctl_verbs(&text) {
            assert!(
                aterm_types::control_verbs::spec(&verb).is_some()
                    || aterm_ctl::CLIENT_HELP_VERBS.contains(&verb.as_str()),
                "`aterm help sys {}` teaches `aterm ctl {command}`, which nothing serves",
                sec.name
            );
        }
    }
    assert!(section("bogus").is_none());
    let refusal = unknown_section("bogus");
    assert!(SECTIONS.iter().all(|sec| refusal.contains(sec.name)));
}

/// A capability's meaning longer than its column wraps at a space onto lines under the column
/// and loses no word, whatever OS's rows the page shows; one that fits stays on its row.
#[test]
fn a_long_capability_meaning_wraps_under_its_column() {
    let meaning =
        "each process's bytes read and written (Windows: any handle's, so no disk figure)";
    let mut row = String::new();
    push_cap_row(&mut row, "proc_io", "yes", meaning);
    fits("sys machine", &row);
    let lines: Vec<&str> = row.lines().collect();
    assert_eq!(lines.len(), 2, "{row}");
    assert!(
        lines[0].starts_with("  proc_io            yes          each process's "),
        "{row}"
    );
    let indent = " ".repeat(34);
    assert!(
        lines[1].starts_with(&indent) && !lines[1][indent.len()..].starts_with(' '),
        "the second line starts under the meaning: {row}"
    );
    let words: Vec<&str> = lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .skip(2)
        .collect();
    assert_eq!(words.join(" "), meaning);

    // The control: a meaning that fits is the one-line row it always was.
    let mut short = String::new();
    push_cap_row(
        &mut short,
        "load",
        "yes",
        "the load average and the run queue",
    );
    assert_eq!(
        short,
        format!(
            "  {:<18} {:<12} the load average and the run queue\n",
            "load", "yes"
        )
    );
}

/// The machine section is read at the call: this machine's OS and core count as the
/// sampler's readers see them now, and every capability row they measure with its word.
#[test]
fn the_machine_section_reads_this_machine_at_the_call() {
    let text = machine_body();
    let cores = aterm_sysprobe::cpu_layout().logical_cores();
    assert!(text.contains(&format!(" {cores} cores: ")), "{text}");
    if let Some(os) = aterm_sysprobe::machine_words().os {
        assert!(text.contains(&os), "{text}");
    }
    for (name, word) in aterm_sysprobe::caps().rows() {
        assert!(text.contains(&format!("  {name} ")), "{name}: {text}");
        assert!(text.contains(&word), "{name}={word}: {text}");
    }
}
