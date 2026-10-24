// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Help text rendered from the verb and exit tables.

use super::jq::{DIVERGENCES, FORMATS, FUNCTIONS, REFUSED_JQ, SHARED_RULES};
use super::verbs::{FlagSpec, GLOBAL_FLAGS, REFUSED, Takes, VERBS, Verb};
use crate::error::{Error, Result, exit_rows};

const MAX_LINE: usize = 100;

const PURPOSE: &str =
    "agh: aterm's GitHub client, for reads that route each request to the right identity";

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

const BACKGROUND_WAIT: &str = concat!(
    "in Claude Code, run a wait over two minutes as a background Bash ",
    "and read its exit when it ends"
);

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
        text.push_str(&line);
        text.push_str(help.get(indent.len()..).unwrap_or(""));
        if help.is_empty() {
            text.push('\n');
        }
    }
    text
}

/// `agh` alone: purpose line, usage, verb list, and help topic pointers.
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

fn global_page() -> String {
    format!("GLOBAL FLAGS\n{}", flag_lines(GLOBAL_FLAGS))
}

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

fn routing_page() -> String {
    let mut text = String::from("ROUTING\n");
    for line in ROUTING {
        text.push_str("  ");
        text.push_str(line);
        text.push('\n');
    }
    text
}

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

/// The whole manual: brief, verb pages, global flags, exit table, routing, and jq.
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

/// One verb's page: usage, summary, flags with their help, the gh command it replaces, the example.
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

/// Renders help for `words` (empty for the full manual, a topic, or a verb path).
/// # Errors
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
