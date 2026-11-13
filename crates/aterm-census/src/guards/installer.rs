// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `installer-network` guard over `tools/install.sh` ([`Script`]).

use super::{Finding, Rule, Script, Tree};

pub(super) const INSTALLER_NETWORK: Rule = Rule {
    id: "installer-network",
    why: "tools/install.sh runs as `curl | sh` before aterm can verify anything: it fetches \
          only through `web_curl` (`curl --proto …`), never from the GitHub API and never with \
          `gh`",
    scope: &[],
    check: installer_network,
};

/// The one installer, and the one script every guard run must read.
const INSTALLER: &str = "tools/install.sh";

fn installer_network(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    match tree.corpus.scripts().iter().find(|s| s.rel == INSTALLER) {
        Some(script) => out.extend(fences(script)),
        None => out.push(Finding {
            rule: INSTALLER_NETWORK.id,
            path: INSTALLER.to_string(),
            line: 0,
            detail: format!(
                "read no {INSTALLER}, the one script every run must scan: the walk lost the \
                 scripts, so every script rule held vacuously"
            ),
        }),
    }
}

/// Every fence `script` breaks.
fn fences(script: &Script) -> Vec<Finding> {
    let id = INSTALLER_NETWORK.id;
    let mut found = Vec::new();
    let mut transport = false;
    for (no, line) in script.lines() {
        if line.contains("api.github.com") {
            found.push(script.finding(id, no, "names the GitHub API (api.github.com)".into()));
        }
        if let Some(verb) = gh_call(line) {
            found.push(script.finding(id, no, format!("runs `gh {verb}`")));
        }
        let command = command_of(line);
        transport |= command.starts_with("curl --proto");
        if command.starts_with("curl ") && !command.starts_with("curl --proto") {
            found.push(script.finding(id, no, "a `curl` outside `web_curl`".into()));
        }
    }
    if !transport || !script.raw.contains("web_curl()") {
        found.push(script.finding(
            id,
            0,
            "no `web_curl()` transport (`curl --proto …`) is defined".into(),
        ));
    }
    found
}

/// A line's command: its leading blanks, an `if` and a `!` dropped.
fn command_of(line: &str) -> &str {
    let s = line.trim_start();
    match s.strip_prefix("if ") {
        Some(rest) => rest.strip_prefix('!').unwrap_or(rest).trim_start(),
        None => s,
    }
}

/// The `gh` verb a line runs, when it is `api`, `auth` or `release`.
fn gh_call(line: &str) -> Option<&'static str> {
    line.match_indices("gh ").find_map(|(at, _)| {
        let edge = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| matches!(c, ' ' | '\t' | ';' | '(' | '|'));
        let rest = &line[at + 3..];
        ["api", "auth", "release"]
            .into_iter()
            .find(|verb| edge && rest.starts_with(verb))
    })
}

#[cfg(test)]
#[path = "installer_tests.rs"]
mod tests;
