// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Help, generated from the verb table and the exit table (design §15): `agh help`,
//! `agh --help` and `aterm help agh` print the same manual. Every text is built from `VERBS`,
//! `GLOBAL_FLAGS`, `REFUSED`, `error::exit_rows()` and the jq tables; the one authored page is
//! the routing page.

use super::jq::{DIVERGENCES, FORMATS, FUNCTIONS, REFUSED_JQ, SHARED_RULES};
use super::verbs::{FlagSpec, GLOBAL_FLAGS, REFUSED, Takes, VERBS, Verb};
use crate::error::{Error, Result, exit_rows};

/// The longest line an authored text may print.
const MAX_LINE: usize = 100;

/// The purpose line.
const PURPOSE: &str =
    "agh: aterm's GitHub client, for reads that route each request to the right identity";

/// The routing page (design §6.4), the one authored text.
const ROUTING: &[&str] = &[
    "A read walks the owner's read chain in agh.toml, in order: a present profile is used, an",
    "  absent one falls through, an unusable one stops the chain, and anonymous sends nothing.",
    "A writer read (drafts, --include-drafts, --drafts) is exactly the owner's write profile:",
    "  no fallback, never anonymous, and push is checked first.",
    "A write to a release-tier owner needs the repository typed as OWNER/NAME: never @TARGET.",
    "The release floor: alabsystems is release tier whatever agh.toml says, so only a",
    "  release-tier profile writes there, and it writes for release-tier owners only.",
    "Ambient tokens in the environment are never sent; agh whoami reports them.",
    "agh whoami --for REPO --op read|writer-read|write shows the decision.",
];

/// The `release wait` page's extra line.
const BACKGROUND_WAIT: &str = concat!(
    "in Claude Code, run a wait over two minutes as a background Bash ",
    "and read its exit when it ends"
);

/// A flag as its page spells it: `-s, --name VALUE`, with `...` after a Repeat.
fn flag_spec(flag: &FlagSpec) -> String {
    let mut text = String::new();
    if let Some(short) = flag.short {
        text.push('-');
        text.push(short);
        text.push_str(", ");
    }
    text.push_str(flag.name);
    match flag.takes {
        Takes::None => {}
        Takes::Value => text.push_str(" VALUE"),
        Takes::Repeat => text.push_str(" VALUE..."),
    }
    text
}

/// Flag lines, two spaces in, their help aligned.
fn flag_lines(flags: &[FlagSpec]) -> String {
    let specs: Vec<String> = flags.iter().map(flag_spec).collect();
    let width = specs.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let mut text = String::new();
    let indent = " ".repeat(width + 4);
    for (spec, flag) in specs.iter().zip(flags) {
        let words: Vec<String> = flag.help.split(' ').map(str::to_string).collect();
        let help = wrap(&words, &indent, MAX_LINE);
        let mut line = format!("  {spec}");
        line.extend(std::iter::repeat_n(' ', width - spec.chars().count() + 2));
        // The first wrapped line takes the spec's place in the indent.
        text.push_str(&line);
        text.push_str(help.get(indent.len()..).unwrap_or(""));
        if help.is_empty() {
            text.push('\n');
        }
    }
    text
}

/// `agh` alone: the purpose line, `usage: agh VERB [ARGS] [FLAGS]`, every verb's usage, and
/// the pointers `agh help VERB`, `agh help exit`, `agh help jq`, `agh help routing`.
#[must_use]
pub fn brief() -> String {
    let mut text = format!("{PURPOSE}\nusage: agh VERB [ARGS] [FLAGS]\n\n");
    for verb in VERBS {
        text.push_str("  ");
        text.push_str(verb.usage);
        text.push('\n');
    }
    text.push_str(
        "\n  agh help VERB     one verb's page\n  \
         agh help exit     the exit codes and what to do next\n  \
         agh help jq       the jq subset --jq takes\n  \
         agh help routing  which identity a request is sent as\n",
    );
    text
}

/// The global flags, one line each.
fn global_page() -> String {
    format!("GLOBAL FLAGS\n{}", flag_lines(GLOBAL_FLAGS))
}

/// One row per exit: the exit, the slug and the meaning, then the next step.
fn exit_page() -> String {
    let mut text = String::from("EXIT CODES\n");
    let rows = exit_rows();
    let width = rows.iter().map(|r| r.slug.len()).max().unwrap_or(0);
    for row in rows {
        text.push_str(&format!(
            "  {:>3} {:<width$} {}\n",
            row.exit, row.slug, row.meaning
        ));
        text.push_str(&format!("      next: {}\n", row.next));
    }
    text
}

/// The routing page.
fn routing_page() -> String {
    let mut text = String::from("ROUTING\n");
    for line in ROUTING {
        text.push_str("  ");
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// Words joined into lines of at most `width` characters, each starting with `indent`.
fn wrap(words: &[String], indent: &str, width: usize) -> String {
    let mut text = String::new();
    let mut line = String::from(indent);
    for word in words {
        if line.len() > indent.len() && line.len() + 1 + word.len() > width {
            text.push_str(&line);
            text.push('\n');
            line = String::from(indent);
        }
        if line.len() > indent.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if line.len() > indent.len() {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// The jq page: the functions, the formats, the refused constructs with their rewrites, and
/// the divergences from jq 1.7.1.
fn jq_page() -> String {
    let mut text = String::from(
        "JQ\n  --jq runs agh's jq subset, jq 1.7.1's dialect, as jq -r -c -S would print it.\n",
    );
    text.push_str("  functions:\n");
    let functions: Vec<String> = FUNCTIONS
        .iter()
        .map(|spec| {
            let arities: Vec<String> = spec.arities.iter().map(ToString::to_string).collect();
            format!("{}/{}", spec.name, arities.join(","))
        })
        .collect();
    text.push_str(&wrap(&functions, "    ", MAX_LINE));
    text.push_str("  formats:\n");
    let formats: Vec<String> = FORMATS.iter().map(|f| format!("@{f}")).collect();
    text.push_str(&wrap(&formats, "    ", MAX_LINE));
    text.push_str("  refused, with what to write instead:\n");
    for row in REFUSED_JQ {
        text.push_str(&format!("    {}: {}\n", row.names.join(" "), row.next));
    }
    text.push_str("  rules agh shares with jq 1.7.1:\n");
    for rule in SHARED_RULES {
        text.push_str(&format!("    {rule}\n"));
    }
    text.push_str("  where agh differs from jq 1.7.1:\n");
    for divergence in DIVERGENCES {
        text.push_str(&format!("    {divergence}\n"));
    }
    text
}

/// The whole manual: the brief, every verb's page, the global flags, the exit table, the
/// routing page and the jq page. `aterm help agh` prints it.
#[must_use]
pub fn manual() -> String {
    let mut parts = vec![brief()];
    parts.extend(VERBS.iter().map(verb_page));
    parts.push(global_page());
    parts.push(exit_page());
    parts.push(routing_page());
    parts.push(jq_page());
    parts.join("\n")
}

/// One verb's page: usage, summary, flags with their help, the gh command it replaces, the
/// example; for `release wait`, the line that tells an agent in Claude Code to run a wait
/// longer than two minutes as a background Bash.
#[must_use]
pub fn verb_page(verb: &Verb) -> String {
    let mut text = format!("usage: {}\n{}\n", verb.usage, verb.summary);
    if verb.path == ["release", "wait"] {
        text.push_str(BACKGROUND_WAIT);
        text.push('\n');
    }
    if !verb.flags.is_empty() {
        text.push_str("FLAGS\n");
        text.push_str(&flag_lines(verb.flags));
    }
    text.push_str("the global flags are on agh help\n");
    if verb.gh != "-" {
        text.push_str(&format!("REPLACES {}\n", verb.gh));
    }
    text.push_str(&format!("EXAMPLE {}\n", verb.example));
    text
}

/// `[]` → `manual()`; `exit`, `jq`, `routing`; a verb path → `verb_page`; words that only
/// begin verb paths (`release`) → the usage and summary of each verb under them; a refused
/// path → its why and next.
///
/// # Errors
///
/// Usage: `no help for: {words}`, next `agh help`.
pub fn render(words: &[String]) -> Result<String> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => return Ok(manual()),
        ["exit"] => return Ok(exit_page()),
        ["jq"] => return Ok(jq_page()),
        ["routing"] => return Ok(routing_page()),
        _ => {}
    }
    // The longest path the words begin with: `release view TAG` is `release view`'s page.
    let verb = VERBS
        .iter()
        .filter(|verb| words.starts_with(verb.path))
        .max_by_key(|verb| verb.path.len());
    let refused = REFUSED
        .iter()
        .filter(|r| words.starts_with(r.path))
        .max_by_key(|r| r.path.len());
    if let Some(verb) = verb
        && refused.is_none_or(|r| r.path.len() < verb.path.len())
    {
        return Ok(verb_page(verb));
    }
    if let Some(refused) = refused {
        return Ok(format!(
            "{} is refused: {}\nnext: {}\n",
            refused.path.join(" "),
            refused.why,
            refused.next
        ));
    }
    let under: Vec<&Verb> = VERBS
        .iter()
        .filter(|verb| verb.path.len() > words.len() && verb.path.starts_with(&words))
        .collect();
    if !under.is_empty() {
        let mut text = String::new();
        for verb in under {
            text.push_str(&format!("  {}\n    {}\n", verb.usage, verb.summary));
        }
        return Ok(text);
    }
    Err(Error::usage(format!("no help for: {}", words.join(" "))).with_next("agh help"))
}
