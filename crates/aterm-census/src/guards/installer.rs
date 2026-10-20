// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! installer-network: `tools/install.sh` reaches the network through its one
//! transport.
//!
//! The installer runs as `curl … | sh` on a machine aterm has never seen, before
//! anything can verify it, so every fetch goes through `web_curl` (`curl --proto
//! =https …`: no downgrade, no redirect off https). It never asks the GitHub API,
//! whose anonymous budget every install shares, and never runs `gh`, which a fresh
//! machine lacks or which holds someone's login. Read as plain lines: a bare `curl`
//! is a line whose command (after an optional `if` and `!`) is `curl` without
//! `--proto`; `api.github.com` and `gh api|auth|release` are refused anywhere.
//!
//! The installer is also the script walk's anchor: a guard run that reads no
//! `tools/install.sh` lost the scripts (a moved `tools/`, a narrowed
//! [`Script`] test), and every rule over them held vacuously.

use super::{Finding, Rule, Script, Tree};

pub(super) const INSTALLER_NETWORK: Rule = Rule {
    id: "installer-network",
    why: "tools/install.sh runs as `curl | sh` before aterm can verify anything: it fetches \
          only through `web_curl` (`curl --proto …`), never from the GitHub API and never with \
          `gh`",
    scope: &[],
    check: installer_network,
};

/// The one installer, and the one script every guard run must read: an
/// anchor, not a count, so a script deleted on purpose moves no number.
const INSTALLER: &str = "tools/install.sh";

fn installer_network(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    match tree.corpus.scripts.iter().find(|s| s.rel == INSTALLER) {
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

/// The `gh` verb a line runs, when it is `api`, `auth` or `release`: `gh ` at the
/// start or after a blank, `;`, `(` or `|`.
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
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const TRANSPORT: &str =
        "web_curl() {\n\tcurl --proto '=https' --proto-redir '=https' \"$@\"\n}\n";

    #[test]
    fn the_installer_fetches_only_through_its_transport() {
        let clean = format!(
            "#!/bin/sh\n{TRANSPORT}if ! web_curl -fsSL \"$url\"; then exit 1; fi\n\
             # a comment may say curl or gh without running them\n"
        );
        assert!(run(&INSTALLER_NETWORK, &[(INSTALLER, &clean)]).is_empty());
        let broken = format!(
            "#!/bin/sh\n{TRANSPORT}curl -fsSL \"$url\"\nif ! curl -L x; then :; fi\n\
             head=$(gh release view)\nx=https://api.github.com/repos\nsign | gh api y\n"
        );
        let found = run(
            &INSTALLER_NETWORK,
            &[(INSTALLER, &broken), ("tools/other.sh", &broken)],
        );
        assert_eq!(
            spots(&found),
            [
                "tools/install.sh:5",
                "tools/install.sh:6",
                "tools/install.sh:7",
                "tools/install.sh:8",
                "tools/install.sh:9",
            ],
            "only the installer is fenced: {found:?}"
        );
        let lost = run(&INSTALLER_NETWORK, &[(INSTALLER, "#!/bin/sh\nwget x\n")]);
        assert_eq!(spots(&lost), ["tools/install.sh:0"]);
    }

    /// A walk that reads no installer lost the scripts: other scripts do not
    /// stand in for it.
    #[test]
    fn a_walk_that_misses_the_installer_is_a_finding() {
        let clean = format!("#!/bin/sh\n{TRANSPORT}");
        for files in [&[][..], &[("tools/other.sh", clean.as_str())][..]] {
            let found = run(&INSTALLER_NETWORK, files);
            assert!(
                found.len() == 1 && found[0].detail.starts_with("read no tools/install.sh"),
                "{found:?}"
            );
        }
        assert!(run(&INSTALLER_NETWORK, &[(INSTALLER, &clean)]).is_empty());
    }
}
