// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! github-auth-door, gh-spawn and agh-fx: GitHub is reached through agh's doors. agh-fx's
//! third clause keeps every child agh spawns in its child door, `crates/agh/src/fx/child.rs`.

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
    why: "a `gh` child process: GitHub is read through agh (`aterm agh`) and written through \
          the REST lane (`aterm_update_core::github_rest`), and `gh` has neither's identity \
          routing",
    scope: &[],
    check: gh_spawn,
};

pub(super) const AGH_FX: Rule = Rule {
    id: "agh-fx",
    why: "a clock, a sleep, a socket or a GitHub round trip in agh that skips its effect seam \
          (crates/agh/src/fx/), so a tape misses it and a replay answers it from the real \
          world; or aterm-http's client outside crates/agh/src/wire/http.rs; or a child process \
          spawned outside crates/agh/src/fx/child.rs",
    scope: &["crates/agh/"],
    check: agh_fx,
};

const AGH: &str = "crates/agh/";
const AGH_DOOR: &str = "crates/agh/src/wire/door.rs";
const AGH_SRC: &str = "crates/agh/src/";
const AGH_FX_HOME: &str = "crates/agh/src/fx/";
const AGH_HTTP: &str = "crates/agh/src/wire/http.rs";
const AGH_CHILD: &str = "crates/agh/src/fx/child.rs";

/// The `aterm_uds` modules that spawn a child, one row each: the module, the needle code
/// naming it is found by, and the path an import of it is reported as. Each needle is the bare
/// module name but `deadline::`, a path lead, because `deadline` is a common variable name.
/// `spawnfd` and `deadline` hold the command builders, `pspawn` a `posix_spawn` of any program,
/// and `fixture_git` a `git` command (in test and debug builds).
const SPAWN_MODULES: &[(&str, &str, &str)] = &[
    ("spawnfd", "spawnfd", "aterm_uds::spawnfd"),
    ("deadline", "deadline::", "aterm_uds::deadline"),
    ("pspawn", "pspawn", "aterm_uds::pspawn"),
    ("fixture_git", "fixture_git", "aterm_uds::fixture_git"),
];

/// What else names a child process in agh's code: std's command. These and
/// [`SPAWN_MODULES`]' needles are each a finding outside [`AGH_CHILD`].
const STD_SPAWNS: &[&str] = &["std::process::Command", "Command::new"];

const GITHUB_API_HOSTS: &[&str] = &["api.github.com", "uploads.github.com"];

const AUTH_HEADER_CAPS: &[(&str, usize)] = &[
    ("crates/aterm-update-core/src/github_rest.rs", 1),
    ("crates/aterm-machines/src/port/github_rest.rs", 1),
];

const GH_SPAWN_CAPS: &[(&str, usize)] = &[
    // `DOOR_GH`, the tape door named `gh`: a const the rule's shape reads, which spawns nothing.
    ("crates/agh/src/fx/mod.rs", 1),
    // `GH_ABSOLUTE`: `auth import --from-gh`'s one gh lookup (§12).
    ("crates/agh/src/identity/import.rs", 1),
];

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

const SPAWNERS: &[&str] = &["command(", "Command::new(", "transparent(", "Cmd::new("];

/// Is `file` product code outside the census crate, and no test code
/// ([`super::rules::TestCode::holds`])?
fn shipped_product(tree: &Tree<'_>, file: &SourceFile) -> bool {
    !file.rel.starts_with(CENSUS_CRATE) && !tree.corpus.test_code().holds(&file.rel)
}

fn cap_of(caps: &[(&str, usize)], rel: &str) -> usize {
    caps.iter()
        .find(|(path, _)| *path == rel)
        .map_or(0, |c| c.1)
}

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
    hay.as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn github_auth_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !shipped_product(tree, file) {
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

fn door_clause(file: &SourceFile, out: &mut Vec<Finding>) {
    if !contains_ignore_case(file.raw(), "authorization") {
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

fn bearer_literal(contents: &str) -> bool {
    contains_ignore_case(contents, "authorization: bearer")
        || contains_ignore_case(contents, "authorization: token")
        || contents.eq_ignore_ascii_case("bearer ")
        || contents.eq_ignore_ascii_case("token ")
        || contents
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer {"))
        || contents
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("token {"))
}

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

fn gh_spawn(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !shipped_product(tree, file) || !file.mentions(&["\"gh\"", "/gh\""]) {
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

fn names_gh(contents: &str) -> bool {
    contents == "gh" || contents.ends_with("/gh")
}

fn after_spawner(before: &str) -> bool {
    let before = before.trim_end_matches([' ', '\t']);
    SPAWNERS.iter().any(|spawner| {
        before
            .strip_suffix(spawner)
            .is_some_and(|head| head.as_bytes().last().is_none_or(|&b| !is_word(b)))
    })
}

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

fn spawns_gh(line: &Line<'_>) -> bool {
    let initializer = item_initializer(line.code);
    one_line_literals(line).iter().any(|&(open, contents)| {
        names_gh(contents)
            && (line.code.get(..open).is_some_and(after_spawner)
                || initializer.is_some_and(|eq| open > eq))
    })
}

fn agh_fx(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(&[AGH_SRC]) {
        if tree.corpus.test_code().holds(&file.rel) {
            continue;
        }
        let doors = !file.rel.starts_with(AGH_FX_HOME);
        let client = file.rel != AGH_HTTP;
        let child = file.rel != AGH_CHILD;
        let uses = use_statements(file);
        for line in file.shipped_lines() {
            let code = uses
                .executable
                .get(&line.no)
                .map_or(line.code, String::as_str);
            if child && let Some(needle) = names_a_spawn(code) {
                out.push(file.finding(AGH_FX.id, line.no, spawns_outside(needle)));
                continue;
            }
            if doors && let Some((needle, what)) = first_hit(code, AGH_CLOCKS.iter(), |_| true) {
                out.push(file.finding(
                    AGH_FX.id,
                    line.no,
                    format!("`{needle}`: {what}, outside agh's effect seam (`agh::fx`)"),
                ));
                continue;
            }
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
            if child && let Some(needle) = leaves.iter().find_map(imports_a_spawn) {
                out.push(file.finding(AGH_FX.id, *no, spawns_outside(needle)));
            } else if doors && let Some((needle, what)) = leaves.iter().find_map(agh_imported) {
                out.push(file.finding(AGH_FX.id, *no, imported(needle, what)));
            } else if client && !names_http_client(stmt) && leaves.iter().any(renames_http) {
                out.push(file.finding(AGH_FX.id, *no, HTTP_CLIENT.to_string()));
            }
        }
    }
}

fn spawns_outside(needle: &str) -> String {
    format!("`{needle}`: spawns a child outside agh's child door (`{AGH_CHILD}`)")
}

/// The first of [`SPAWN_MODULES`]' needles and [`STD_SPAWNS`] `code` names, not inside a
/// longer identifier (a needle that ends in `::` leads a path, so only its start is bounded).
fn names_a_spawn(code: &str) -> Option<&'static str> {
    let bytes = code.as_bytes();
    let mut needles = SPAWN_MODULES
        .iter()
        .map(|(_, needle, _)| *needle)
        .chain(STD_SPAWNS.iter().copied());
    needles.find(|needle| {
        hits(code, needle, Bound::Sub).any(|at| {
            let starts = at == 0 || !is_word(bytes[at - 1]);
            let ends =
                needle.ends_with("::") || bytes.get(at + needle.len()).is_none_or(|&b| !is_word(b));
            starts && ends
        })
    })
}

/// A `use` leaf that binds `std::process::Command`, or anything under one of
/// [`SPAWN_MODULES`], the module itself and a glob of it included.
fn imports_a_spawn(leaf: &UseLeaf<'_>) -> Option<&'static str> {
    if leaf.path == ["std", "process", "Command"] && !leaf.glob {
        return Some("std::process::Command");
    }
    SPAWN_MODULES.iter().find_map(|(module, _, named)| {
        leaf.path
            .starts_with(&["aterm_uds", *module])
            .then_some(*named)
    })
}

const HTTP_CLIENT: &str = "names aterm-http past `aterm_http::proxy` and `aterm_http::Error`: its \
                           client is built in crates/agh/src/wire/http.rs alone";

fn imported(needle: &str, what: &str) -> String {
    format!("imports `{needle}`: {what}, bound where agh's effect seam (`agh::fx`) does not see it")
}

fn renames_http(leaf: &UseLeaf<'_>) -> bool {
    leaf.path == ["aterm_http"]
        && !leaf.glob
        && leaf
            .alias
            .is_some_and(|alias| alias != "_" && alias != "aterm_http")
}

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

fn names_http_client(code: &str) -> bool {
    const PATH: &str = "aterm_http::";
    hits(code, PATH, Bound::Sub).any(|at| {
        let word_start = at == 0 || !is_word(code.as_bytes()[at - 1]);
        let rest = &code[at + PATH.len()..];
        word_start && !["proxy", "Error"].iter().any(|ok| word_leads(rest, ok))
    })
}

fn word_leads(rest: &str, word: &str) -> bool {
    rest.strip_prefix(word)
        .is_some_and(|after| after.as_bytes().first().is_none_or(|&b| !is_word(b)))
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
