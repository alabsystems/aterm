// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! github-auth-door, gh-spawn and agh-fx: GitHub is reached through agh's doors
//! (docs/DESIGN-agh-2026-10-19.md §16.8).
//!
//! * `github-auth-door`: agh spells the credential header in one file,
//!   `crates/agh/src/wire/door.rs`, so no other code of agh can build it past the
//!   Router; elsewhere, a Bearer header beside a GitHub API origin is a credential
//!   sent around agh.
//! * `gh-spawn`: a `gh` child process, the lanes agh replaces milestone by
//!   milestone; each file that still spawns one carries a cap the migration
//!   lowers.
//! * `agh-fx`: agh's clocks, sleeps, sockets and round trips cross its one
//!   effect seam (`crates/agh/src/fx/`), so a tape holds every one of them and a
//!   replay never answers one from the real world; aterm-http's client is built
//!   in `crates/agh/src/wire/http.rs` alone.
//!
//! Each reads shipped code only: a test file (`crate::is_test_file`) and a
//! test-only item (`shipped_lines`) are skipped, and so is this crate, whose
//! tables spell what it bans. A one-line literal's contents are the TEXT view's
//! bytes between a pair of quotes the CODE view keeps on that line, paired left to
//! right; the lexer blanks a multi-line literal's quotes too, so the door's first
//! clause also reads the TEXT view's bytes wherever the CODE view blanked them.
//!
//! An exemption is a cap, and nothing polices the caps: a file with fewer matches
//! than its cap, or none, is no finding. That is the owner's census ruling of
//! 2026-10-16, which dropped `shared-application`'s stale-row finding as the
//! registry pattern AGENTS.md rejects.

use std::path::Path;

use super::rules::{
    CENSUS_CRATE, ClockShape, PRODUCT, UseLeaf, first_hit, type_alias, use_leaves, use_statements,
};
use super::{Bound, Finding, Line, Rule, SourceFile, Tree, hits, is_word};

pub(super) const GITHUB_AUTH_DOOR: Rule = Rule {
    id: "github-auth-door",
    why: "the credential header is spelled outside agh's one door (crates/agh/src/wire/door.rs), \
          or a Bearer header is built beside a GitHub API origin elsewhere: a credential that \
          skips the Router",
    scope: &["crates/agh/"],
    check: github_auth_door,
};

pub(super) const GH_SPAWN: Rule = Rule {
    id: "gh-spawn",
    why: "a `gh` child process outside the caps the agh migration lowers: GitHub is read \
          through agh (`aterm agh`), whose identity routing `gh` does not have",
    scope: &[],
    check: gh_spawn,
};

pub(super) const AGH_FX: Rule = Rule {
    id: "agh-fx",
    why: "a clock, a sleep, a socket or a GitHub round trip in agh that skips its effect seam \
          (crates/agh/src/fx/), so a tape misses it and a replay answers it from the real \
          world; or aterm-http's client outside crates/agh/src/wire/http.rs",
    scope: &["crates/agh/"],
    check: agh_fx,
};

/// agh's source, where the header's name lives in one file.
const AGH: &str = "crates/agh/";
/// The one file of agh that spells the credential header.
const AGH_DOOR: &str = "crates/agh/src/wire/door.rs";
/// agh's library source, which `agh-fx` reads.
const AGH_SRC: &str = "crates/agh/src/";
/// agh's effect seam, the home of every clock, sleep and round trip.
const AGH_FX_HOME: &str = "crates/agh/src/fx/";
/// The one file of agh that builds aterm-http's client.
const AGH_HTTP: &str = "crates/agh/src/wire/http.rs";

/// The API origins whose presence makes a file's Bearer literal a GitHub credential.
const GITHUB_API_HOSTS: &[&str] = &["api.github.com", "uploads.github.com"];

/// The files outside agh that still build a GitHub credential header, each with its cap and
/// why.
const AUTH_HEADER_CAPS: &[(&str, usize)] = &[
    // The curl upload lane's header file; M3 deletes it.
    ("crates/aterm-release/src/publish.rs", 1),
    // The machines controller's REST door for its machine user's SSH-key writes, which agh
    // M1's GET-only transport refuses (init-system S14); agh design §16.4 step 5a
    // (`users().keys()`) moves them into agh and deletes it.
    ("crates/aterm-machines/src/port/github_rest.rs", 1),
];

/// The files that still spawn `gh`, each with its cap (design §16.8's table, derived again
/// from a `verify census` run on M1's tree).
const GH_SPAWN_CAPS: &[(&str, usize)] = &[
    // `DOOR_GH`, the tape door named `gh`: a const the rule's shape reads, which spawns nothing.
    ("crates/agh/src/fx/mod.rs", 1),
    // `GhCli`; M2's swap deletes it.
    ("crates/atpkg-publish/src/host/gh.rs", 1),
    // The channel reads; M3.
    ("crates/aterm-release/src/gates.rs", 2),
    // M3.
    ("crates/aterm-release/src/provision.rs", 1),
    // M3.
    ("crates/aterm-release/src/publish.rs", 3),
    // M3.
    ("crates/aterm-release/src/release_steps.rs", 3),
    // M3.
    ("crates/aterm-release/src/site/world.rs", 1),
];

/// Each clock read, sleep or round trip agh makes only through `Fx`, its shape, and what it is.
const AGH_CLOCKS: &[(&str, ClockShape, &str)] = &[
    ("Instant::now", ClockShape::Named, "the monotonic clock"),
    ("SystemTime::now", ClockShape::Named, "the wall clock"),
    ("thread::sleep", ClockShape::Named, "a sleep"),
    (
        "thread::sleep_until",
        ClockShape::Named,
        "a sleep (`thread::sleep_until`)",
    ),
    (
        "thread::sleep_ms",
        ClockShape::Named,
        "a sleep (`thread::sleep_ms`)",
    ),
    (
        "thread::park_timeout",
        ClockShape::Named,
        "a sleep (`thread::park_timeout`)",
    ),
    (
        "elapsed",
        ClockShape::Method,
        "the monotonic clock (`.elapsed()`)",
    ),
    (
        "round_trip",
        ClockShape::Method,
        "a round trip past `Fx::gh`",
    ),
    (
        "TcpStream",
        ClockShape::Named,
        "a socket past the transport",
    ),
];

/// The calls whose first argument names a program to spawn.
const SPAWNERS: &[&str] = &["command(", "Command::new(", "transparent(", "Cmd::new("];

/// Shipped product source that is not a test file and not this crate.
fn shipped_product(file: &SourceFile) -> bool {
    !file.rel.starts_with(CENSUS_CRATE) && !crate::is_test_file(Path::new(&file.rel))
}

/// The cap of `rel` in `caps`; 0 for a file with no row.
fn cap_of(caps: &[(&str, usize)], rel: &str) -> usize {
    caps.iter()
        .find(|(path, _)| *path == rel)
        .map_or(0, |c| c.1)
}

/// The one-line literals of `line`: for each pair of quotes the CODE view keeps, paired left to
/// right, the byte offset of the opening quote and the TEXT view's contents between them.
fn one_line_literals<'a>(line: &Line<'a>) -> Vec<(usize, &'a str)> {
    let quotes: Vec<usize> = line
        .code
        .bytes()
        .enumerate()
        .filter(|&(_, b)| b == b'"')
        .map(|(at, _)| at)
        .collect();
    let (pairs, _unpaired) = quotes.as_chunks::<2>();
    pairs
        .iter()
        .filter_map(|&[open, close]| line.text.get(open + 1..close).map(|text| (open, text)))
        .collect()
}

/// Every run of the TEXT view's bytes the CODE view blanked on `line`: the contents of each
/// literal on it, a multi-line literal's included (its quotes are blanked as well).
fn blanked_runs<'a>(line: &Line<'a>) -> Vec<&'a str> {
    let (code, text) = (line.code.as_bytes(), line.text.as_bytes());
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for at in 0..=text.len() {
        let blanked = at < text.len() && code.get(at) != text.get(at);
        match (blanked, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                if let Some(run) = line.text.get(from..at) {
                    runs.push(run);
                }
                start = None;
            }
            _ => {}
        }
    }
    runs
}

fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle)
}

// github-auth-door

fn github_auth_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !shipped_product(file) {
            continue;
        }
        if file.rel.starts_with(AGH) {
            if file.rel != AGH_DOOR {
                door_clause(file, out);
            }
        } else if file.mentions(GITHUB_API_HOSTS) {
            bearer_clause(file, out);
        }
    }
}

/// Clause 1: in agh, a literal holding the header's name outside the door.
fn door_clause(file: &SourceFile, out: &mut Vec<Finding>) {
    if !contains_ignore_case(&file.raw, "authorization") {
        return;
    }
    for line in file.shipped_lines() {
        if blanked_runs(&line)
            .iter()
            .any(|run| contains_ignore_case(run, "authorization"))
        {
            out.push(
                file.finding(
                    GITHUB_AUTH_DOOR.id,
                    line.no,
                    "a literal spells the credential header outside crates/agh/src/wire/door.rs \
                 (name it through `door::AUTHORIZATION` or `door::is_authorization`)"
                        .to_string(),
                ),
            );
        }
    }
}

/// Whether a one-line literal's contents build a Bearer or token credential header: the
/// header spelled whole, the scheme word alone (`"bearer "`, then a push of the token), or the
/// scheme word with a format placeholder straight after it (`format!("Bearer {token}")`).
fn bearer_literal(contents: &str) -> bool {
    let lower = contents.to_ascii_lowercase();
    lower.contains("authorization: bearer")
        || lower.contains("authorization: token")
        || lower == "bearer "
        || lower == "token "
        || lower.starts_with("bearer {")
        || lower.starts_with("token {")
}

/// Clause 2: outside agh, in a file whose shipped literals name a GitHub API origin, a literal
/// that builds a Bearer or token header, past the file's cap.
fn bearer_clause(file: &SourceFile, out: &mut Vec<Finding>) {
    let lines: Vec<Line<'_>> = file.shipped_lines().collect();
    let names_origin = lines.iter().any(|line| {
        blanked_runs(line)
            .iter()
            .any(|run| GITHUB_API_HOSTS.iter().any(|host| run.contains(host)))
    });
    if !names_origin {
        return;
    }
    let cap = cap_of(AUTH_HEADER_CAPS, &file.rel);
    let matching = lines.iter().filter(|line| {
        one_line_literals(line)
            .iter()
            .any(|(_, contents)| bearer_literal(contents))
    });
    for line in matching.skip(cap) {
        out.push(file.finding(
            GITHUB_AUTH_DOOR.id,
            line.no,
            format!(
                "a Bearer header beside a GitHub API origin, past this file's cap of {cap}: \
                 a GitHub credential goes through agh"
            ),
        ));
    }
}

// gh-spawn

fn gh_spawn(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !shipped_product(file) || !file.mentions(&["\"gh\"", "/gh\""]) {
            continue;
        }
        let cap = cap_of(GH_SPAWN_CAPS, &file.rel);
        let matching = file.shipped_lines().filter(spawns_gh);
        for line in matching.skip(cap) {
            out.push(file.finding(
                GH_SPAWN.id,
                line.no,
                format!("spawns `gh`, past this file's cap of {cap}: GitHub is read through agh"),
            ));
        }
    }
}

/// Whether a literal's contents name the `gh` program.
fn names_gh(contents: &str) -> bool {
    contents == "gh" || contents.ends_with("/gh")
}

/// Whether `code` before a literal ends with a spawner's opening call, a word on its own
/// (`spawnfd::command(`, never `run_command(`), whitespace allowed before the literal.
fn after_spawner(before: &str) -> bool {
    let before = before.trim_end_matches([' ', '\t']);
    SPAWNERS.iter().any(|spawner| {
        before
            .strip_suffix(spawner)
            .is_some_and(|head| head.as_bytes().last().is_none_or(|&b| !is_word(b)))
    })
}

/// The offset of the first `=` of a `const` or `static` item's line, past its indentation and
/// an optional `pub`, `pub(crate)` or `pub(super)`.
fn item_initializer(code: &str) -> Option<usize> {
    let head = code.trim_start();
    let head = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|vis| head.strip_prefix(vis))
        .unwrap_or(head);
    if head.starts_with("const ") || head.starts_with("static ") {
        code.find('=')
    } else {
        None
    }
}

/// Whether `line` spawns `gh`: a one-line literal naming it as a spawner's first argument, or in
/// a `const` or `static` item's initializer.
fn spawns_gh(line: &Line<'_>) -> bool {
    let initializer = item_initializer(line.code);
    one_line_literals(line).iter().any(|&(open, contents)| {
        names_gh(contents)
            && (line.code.get(..open).is_some_and(after_spawner)
                || initializer.is_some_and(|eq| open > eq))
    })
}

// agh-fx

fn agh_fx(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(&[AGH_SRC]) {
        if crate::is_test_file(Path::new(&file.rel)) {
            continue;
        }
        let doors = !file.rel.starts_with(AGH_FX_HOME);
        let client = file.rel != AGH_HTTP;
        // Imports are read whole, as the names they bind; only their spans are blanked in the
        // view clause 1 reads, so an import is named once, at its first line.
        let uses = use_statements(file);
        for line in file.shipped_lines() {
            let code = uses
                .executable
                .get(&line.no)
                .map_or(line.code, String::as_str);
            if doors && let Some((needle, what)) = first_hit(code, AGH_CLOCKS.iter(), |_| true) {
                out.push(file.finding(
                    AGH_FX.id,
                    line.no,
                    format!("`{needle}`: {what}, outside agh's effect seam (`agh::fx`)"),
                ));
                continue;
            }
            // A `type` alias of a clock's type binds it as an import does
            // (`type Clock = std::time::Instant;`, then `Clock::now()`), as supervise-fx reads one.
            if doors && let Some((needle, what)) = type_alias(code).as_ref().and_then(agh_imported)
            {
                out.push(file.finding(AGH_FX.id, line.no, imported(needle, what)));
                continue;
            }
            if client && (names_http_client(line.code) || extern_renames_http(line.code)) {
                out.push(file.finding(AGH_FX.id, line.no, HTTP_CLIENT.to_string()));
            }
        }
        for (no, stmt) in &uses.statements {
            let leaves = use_leaves(stmt);
            if doors && let Some((needle, what)) = leaves.iter().find_map(agh_imported) {
                out.push(file.finding(AGH_FX.id, *no, imported(needle, what)));
            } else if client && !names_http_client(stmt) && leaves.iter().any(renames_http) {
                // A group naming `aterm_http::` is already a line's finding.
                out.push(file.finding(AGH_FX.id, *no, HTTP_CLIENT.to_string()));
            }
        }
    }
}

/// Clause 2's finding: aterm-http named, or bound under another name, outside its one file.
const HTTP_CLIENT: &str = "names aterm-http past `aterm_http::proxy` and `aterm_http::Error`: its \
                           client is built in crates/agh/src/wire/http.rs alone";

/// Clause 1's finding for a clock, sleep or socket bound under a name the lines cannot see.
fn imported(needle: &str, what: &str) -> String {
    format!("imports `{needle}`: {what}, bound where agh's effect seam (`agh::fx`) does not see it")
}

/// Whether one imported name binds aterm-http's crate root under another name
/// (`use aterm_http as http;`), whose paths (`http::Client`) clause 2's line view cannot see.
/// `as _` binds nothing to name.
fn renames_http(leaf: &UseLeaf<'_>) -> bool {
    leaf.path == ["aterm_http"]
        && !leaf.glob
        && leaf
            .alias
            .is_some_and(|alias| alias != "_" && alias != "aterm_http")
}

/// Whether `code` binds aterm-http's crate root under another name with
/// `extern crate aterm_http as http;`.
fn extern_renames_http(code: &str) -> bool {
    let Some(at) = hits(code, "extern", Bound::Word).next() else {
        return false;
    };
    let mut words = code[at..]
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty());
    let head = [(); 4].map(|()| words.next());
    head == [
        Some("extern"),
        Some("crate"),
        Some("aterm_http"),
        Some("as"),
    ] && words
        .next()
        .is_some_and(|alias| alias != "_" && alias != "aterm_http")
}

/// The clock, sleep or socket of [`AGH_CLOCKS`] one imported name binds, if any, read as
/// supervise-fx reads an import ([`use_leaves`]): the whole of a needle imported
/// (`use std::thread::{self, sleep};`, then a bare `sleep(d)`), or its one word
/// (`use std::net::TcpStream;`); a head of it renamed (`Instant as Clock`, `std::thread as t`);
/// or its module globbed in (`std::thread::*`). A method (`.elapsed()`, `.round_trip(`) is not
/// imported, `as _` binds nothing to call, and a head imported under its own name
/// (`use std::time::Instant;`) keeps the path a line is read for.
fn agh_imported(leaf: &UseLeaf<'_>) -> Option<(&'static str, &'static str)> {
    if leaf.alias == Some("_") {
        return None;
    }
    AGH_CLOCKS
        .iter()
        .filter(|(_, shape, _)| matches!(shape, ClockShape::Named))
        .find_map(|&(needle, _, what)| {
            let parts: Vec<&str> = needle.split("::").collect();
            let ends = |k: usize| leaf.path.ends_with(&parts[..k]);
            let renamed_head = leaf.alias.is_some()
                && (1..parts.len()).any(|k| ends(k) && leaf.alias != Some(parts[k - 1]));
            let bound = if leaf.glob {
                parts.len() > 1 && ends(parts.len() - 1)
            } else {
                ends(parts.len()) || renamed_head
            };
            bound.then_some((needle, what))
        })
}

/// Whether `code` names an `aterm_http::` path other than `aterm_http::proxy…` or
/// `aterm_http::Error`, a `use aterm_http::{…}` group included.
fn names_http_client(code: &str) -> bool {
    const PATH: &str = "aterm_http::";
    hits(code, PATH, Bound::Sub).any(|at| {
        let word_start = at == 0 || !is_word(code.as_bytes()[at - 1]);
        let rest = &code[at + PATH.len()..];
        word_start && !["proxy", "Error"].iter().any(|ok| word_leads(rest, ok))
    })
}

/// Whether `rest` opens with the word `word` (`Error`, never `ErrorKind`).
fn word_leads(rest: &str, word: &str) -> bool {
    rest.strip_prefix(word)
        .is_some_and(|after| after.as_bytes().first().is_none_or(|&b| !is_word(b)))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    /// The header's name, spelled so this file's own literals hold it only where a test plants
    /// it.
    const HEADER: &str = "Authorization";

    #[test]
    fn the_header_name_in_agh_outside_the_door_is_found() {
        let one_line = format!("const H: &str = \"{HEADER}\";\n");
        let multi = format!("const H: &str = \"first line\n{HEADER}: x\n\";\n");
        let found = run(
            &GITHUB_AUTH_DOOR,
            &[
                ("crates/agh/src/x.rs", &one_line),
                ("crates/agh/src/multi.rs", &multi),
            ],
        );
        assert_eq!(
            spots(&found),
            ["crates/agh/src/multi.rs:2", "crates/agh/src/x.rs:1"]
        );
    }

    #[test]
    fn the_door_an_identifier_a_comment_and_a_test_are_no_finding() {
        let literal = format!("const H: &str = \"{HEADER}\";\n");
        let in_test_mod = format!(
            "fn a() {{}}\n#[cfg(test)]\nmod tests {{\n    const H: &str = \"{HEADER}\";\n}}\n"
        );
        let found = run(
            &GITHUB_AUTH_DOOR,
            &[
                ("crates/agh/src/wire/door.rs", &literal),
                (
                    "crates/agh/src/ident.rs",
                    "let h = door::AUTHORIZATION;\nlet y = door::is_authorization(n);\n",
                ),
                (
                    "crates/agh/src/comment.rs",
                    &format!("// the {HEADER} header\nfn a() {{}}\n"),
                ),
                ("crates/agh/tests/t.rs", &literal),
                ("crates/agh/src/gated.rs", &in_test_mod),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new());
    }

    #[test]
    fn a_bearer_header_beside_a_github_api_origin_is_found() {
        let origin = "const API: &str = \"https://api.github.com\";\n";
        let header = format!("let h = \"{HEADER}: Bearer x\";\n");
        let with_origin = format!("{origin}{header}");
        let bare_bearer = format!("{origin}let p = \"bearer \";\n");
        let token_file = format!("{origin}let p = \"token file\";\n");
        let found = run(
            &GITHUB_AUTH_DOOR,
            &[
                ("crates/x/src/with_origin.rs", &with_origin),
                ("crates/x/src/no_origin.rs", &header),
                ("crates/x/src/bare_bearer.rs", &bare_bearer),
                ("crates/x/src/token_file.rs", &token_file),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/x/src/bare_bearer.rs:2",
                "crates/x/src/with_origin.rs:2"
            ]
        );
    }

    #[test]
    fn a_formatted_bearer_header_beside_a_github_api_origin_is_found() {
        let origin = "const API: &str = \"https://api.github.com\";\n";
        let bearer = "let h = format!(\"Bearer {t}\");\n";
        let token = "let h = format!(\"token {t}\");\n";
        let found = run(
            &GITHUB_AUTH_DOOR,
            &[
                ("crates/x/src/bearer.rs", &format!("{origin}{bearer}")),
                ("crates/x/src/token.rs", &format!("{origin}{token}")),
                ("crates/x/src/no_origin.rs", bearer),
            ],
        );
        assert_eq!(
            spots(&found),
            ["crates/x/src/bearer.rs:2", "crates/x/src/token.rs:2"]
        );
    }

    #[test]
    fn the_publish_lane_holds_one_bearer_header_and_no_more() {
        let origin = "const API: &str = \"https://uploads.github.com\";\n";
        let one = format!("{origin}let h = \"{HEADER}: token x\";\n");
        let two = format!("{one}let g = \"{HEADER}: token y\";\n");
        let publish = "crates/aterm-release/src/publish.rs";
        assert_eq!(
            spots(&run(&GITHUB_AUTH_DOOR, &[(publish, &one)])),
            Vec::<String>::new()
        );
        assert_eq!(
            spots(&run(&GITHUB_AUTH_DOOR, &[(publish, &two)])),
            [format!("{publish}:3")]
        );
    }

    #[test]
    fn a_gh_child_is_found_past_its_files_cap() {
        let gh_rs = "crates/atpkg-publish/src/host/gh.rs";
        let found = run(
            &GH_SPAWN,
            &[
                (
                    "crates/x/src/spawn.rs",
                    "let c = aterm_uds::spawnfd::command(\"gh\");\n\
                     let d = Command::new(\"/opt/homebrew/bin/gh\");\n\
                     const GH: &str = \"gh\";\n",
                ),
                (
                    gh_rs,
                    "let c = command(\"gh\");\nlet d = command(\"gh\");\n",
                ),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                format!("{gh_rs}:2"),
                "crates/x/src/spawn.rs:1".to_string(),
                "crates/x/src/spawn.rs:2".to_string(),
                "crates/x/src/spawn.rs:3".to_string(),
            ]
        );
    }

    #[test]
    fn a_gh_literal_that_spawns_nothing_or_sits_within_a_cap_is_no_finding() {
        let found = run(
            &GH_SPAWN,
            &[
                ("crates/x/src/assert.rs", "assert_eq!(x, \"gh\");\n"),
                (
                    "crates/atpkg-publish/src/host/gh.rs",
                    "let c = command(\"gh\");\n",
                ),
                ("crates/x/tests/t.rs", "let c = command(\"gh\");\n"),
                (
                    "crates/aterm-census/src/guards/x.rs",
                    "let c = command(\"gh\");\n",
                ),
                ("crates/x/src/run.rs", "let c = run_command(\"gh\");\n"),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new());
    }

    #[test]
    fn a_clock_a_sleep_a_socket_or_a_round_trip_in_agh_is_found() {
        let lines = [
            "let t = Instant::now();",
            "std::thread::sleep(d);",
            "let e = t.elapsed();",
            "self.transport.round_trip(req)",
            "Transport::round_trip(&*t, req)",
            "use std::net::TcpStream;",
            "aterm_http::Client::new(trust, proxy, d)",
            "use aterm_http::{proxy::EnvSource, Client};",
        ];
        let client = lines.join("\n") + "\n";
        let found = run(&AGH_FX, &[("crates/agh/src/client.rs", &client)]);
        let expected: Vec<String> = (1..=lines.len())
            .map(|no| format!("crates/agh/src/client.rs:{no}"))
            .collect();
        assert_eq!(spots(&found), expected);
    }

    #[test]
    fn a_sleep_or_a_clock_bound_by_an_import_is_found_at_the_import() {
        let lines = [
            "use std::thread::{self, sleep};",
            "fn a(d: Duration) { sleep(d); }",
            "use std::time::Instant as Clock;",
            "fn b() { let t = Clock::now(); }",
            "use std::thread::*;",
            "use std::{",
            "    net::TcpStream as Socket,",
            "};",
            "fn c(t: Instant) { std::thread::sleep_until(t); }",
            "fn d(d: Duration) { std::thread::park_timeout(d); }",
            "type Clock = std::time::Instant;",
            "fn r() -> Clock { Clock::now() }",
            "pub type Wall = std::time::SystemTime;",
        ];
        let client = lines.join("\n") + "\n";
        let found = run(&AGH_FX, &[("crates/agh/src/client.rs", &client)]);
        assert_eq!(
            spots(&found),
            [1, 3, 5, 6, 9, 10, 11, 13].map(|no| format!("crates/agh/src/client.rs:{no}"))
        );
    }

    #[test]
    fn aterm_http_bound_under_another_name_is_found_outside_its_file() {
        let renamed = "use aterm_http as http;\nfn a() { http::Client::new(t, p, d); }\n\
                       use {aterm_http as h2, std::fmt};\n\
                       extern crate aterm_http as h3;\n";
        let not_renamed = "use aterm_http as _;\nuse aterm_http::proxy::{self as p};\n\
                           extern crate aterm_http;\n";
        let found = run(
            &AGH_FX,
            &[
                ("crates/agh/src/client.rs", renamed),
                ("crates/agh/src/fx/mod.rs", "use aterm_http as http;\n"),
                ("crates/agh/src/wire/http.rs", renamed),
                ("crates/agh/src/other.rs", not_renamed),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/agh/src/client.rs:1",
                "crates/agh/src/client.rs:3",
                "crates/agh/src/client.rs:4",
                "crates/agh/src/fx/mod.rs:1",
            ]
        );
    }

    #[test]
    fn an_import_under_its_own_name_or_in_the_seam_is_no_finding() {
        let imports = "use std::time::Instant;\nuse std::thread;\nuse std::time::Instant as _;\n\
                       use std::time::{Duration, Instant};\ntype D = std::time::Duration;\n\
                       type Instant = std::time::Instant;\n";
        let found = run(
            &AGH_FX,
            &[
                ("crates/agh/src/client.rs", imports),
                (
                    "crates/agh/src/fx/mod.rs",
                    "use std::thread::{self, sleep};\nuse std::time::Instant as Clock;\n",
                ),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new());
    }

    #[test]
    fn the_seam_a_definition_a_field_and_a_string_are_no_finding() {
        let clocks = "let t = Instant::now();\nstd::thread::sleep(d);\nlet e = t.elapsed();\n\
                      let s = SystemTime::now();\n";
        let found = run(
            &AGH_FX,
            &[
                ("crates/agh/src/fx/mod.rs", clocks),
                (
                    "crates/agh/src/wire/scripted.rs",
                    "fn round_trip(&self, req: WireRequest<'_>) -> Result<WireResponse, WireError> {\n}\n",
                ),
                (
                    "crates/agh/src/wait.rs",
                    "self.fx.sleep(d)?;\nlet s = beat.elapsed;\nreceipt.elapsed_ms = ms;\n\
                     // Instant::now is the seam's\nlet w = \"thread::sleep\";\n",
                ),
                ("crates/agh/tests/t.rs", clocks),
                (
                    "crates/agh/src/wire/http.rs",
                    "aterm_http::Client::new(trust, proxy, d)\n",
                ),
                (
                    "crates/agh/src/identity/mod.rs",
                    "use aterm_http::proxy::{EnvSource, ProcessEnv};\n",
                ),
                (
                    "crates/agh/src/wire/mod.rs",
                    "let e = aterm_http::Error::TooLarge;\n",
                ),
                ("crates/aterm-agent/src/supervise/fx.rs", clocks),
            ],
        );
        assert_eq!(spots(&found), Vec::<String>::new());
    }
}
