// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `machines-root-door`: the root tools `aterm machines` runs on a managed Mac stay inside
//! its crate (docs/DESIGN-machines-2026-10-16.md §3.6).
//!
//! The owner's ruling M1 authorizes root actions for the `aterm machines agent` lane alone;
//! aterm's install path keeps "no admin prompt, no privileged helper". A line of shipped code
//! elsewhere that names one of these tools, or reaches the agent past the one door
//! (`aterm_machines::cli::main_entry`, whose `agent` arm is the only caller of
//! `agent::main`), would be root work outside that lane. `scutil --set` and system-domain
//! `launchctl` writes have no literal a line rule can tell from their read-only uses, so the
//! crate's own review keeps them.

use super::rules::{CENSUS_CRATE, PRODUCT, test_path};
use super::{Bound, Finding, Rule, SourceFile, Tree, has};

/// The one crate that may name them.
const MACHINES_CRATE: &str = "crates/aterm-machines/";

/// The tools, by the literal a call would spell: the path for the ones whose bare name is
/// an ordinary word in this tree (`installer`, `softwareupdate` in prose and manifest
/// strategies, `pmset -g` read by the perf lanes), the bare name for the two nothing else
/// has a use for.
const ROOT_TOOLS: &[&str] = &[
    "/usr/sbin/installer",
    "/usr/sbin/softwareupdate",
    "/usr/sbin/visudo",
    "/etc/sudoers.d",
    "/usr/bin/pmset",
    "socketfilterfw",
    "systemsetup",
];

/// The agent's module, named by path.
const AGENT_PATH: &str = "aterm_machines::agent";

pub(super) const MACHINES_ROOT_DOOR: Rule = Rule {
    id: "machines-root-door",
    why: "installer, softwareupdate, visudo, /etc/sudoers.d, pmset, socketfilterfw and \
          systemsetup act as root on a whole Mac: only aterm-machines' agent, under the \
          owner's ruling M1 and one digest-pinned sudoers rule, may name them, and nothing \
          outside the crate reaches `aterm_machines::agent` past `cli::main_entry`",
    scope: &[MACHINES_CRATE],
    check: machines_root_door,
};

fn machines_root_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut needles: Vec<&str> = ROOT_TOOLS.to_vec();
    needles.push("aterm_machines");
    for file in product(tree) {
        if file.rel.starts_with(MACHINES_CRATE) || test_path(&file.rel) || !file.mentions(&needles)
        {
            continue;
        }
        for line in file.shipped_lines() {
            let why = if let Some(tool) = ROOT_TOOLS.iter().find(|t| line.text.contains(*t)) {
                Some(format!(
                    "names `{tool}`, a root tool only aterm-machines' agent runs"
                ))
            } else if reaches_agent(line.code) {
                Some(format!(
                    "reaches `{AGENT_PATH}`; its one door is `aterm_machines::cli::main_entry`"
                ))
            } else {
                None
            };
            if let Some(why) = why {
                out.push(file.finding(MACHINES_ROOT_DOOR.id, line.no, why));
            }
        }
    }
}

/// Product source this crate does not own (the census spells every name it bans).
fn product<'a>(tree: &Tree<'a>) -> impl Iterator<Item = &'a SourceFile> {
    tree.corpus
        .under(PRODUCT)
        .filter(|f| !f.rel.starts_with(CENSUS_CRATE))
}

/// A line of code naming the agent module: `aterm_machines::agent…`, or a
/// `use aterm_machines::{…agent…}` group.
fn reaches_agent(code: &str) -> bool {
    has(code, AGENT_PATH, Bound::Word)
        || (code.contains("aterm_machines::{") && has(code, "agent", Bound::Word))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn only_aterm_machines_names_a_root_tool_or_reaches_its_agent() {
        let outside = "fn a() { spawnfd::command(\"/usr/bin/pmset\").args([\"-a\", \"sleep\", \"0\"]); }\n\
                       // pmset and systemsetup in a comment say nothing\n\
                       const DROP_IN: &str = \"/etc/sudoers.d/aterm\";\n\
                       fn b() { let argv = [\"/usr/libexec/ApplicationFirewall/socketfilterfw\", \"--setglobalstate\", \"on\"]; }\n\
                       fn c() { aterm_machines::agent::main(&args, &sys); }\n\
                       use aterm_machines::{agent, cli};\n\
                       fn d() -> ExitCode { aterm_machines::cli::main_entry(forwarded) }\n\
                       fn e() { let s = \"systemsetup -getremotelogin\"; }\n\
                       fn f() { let s = \"softwareupdate --install-rosetta\"; }\n\
                       #[cfg(test)]\nmod tests {\n    const T: &str = \"/usr/sbin/installer\";\n}\n";
        let found = run(
            &MACHINES_ROOT_DOOR,
            &[
                ("crates/aterm-cli/src/lib.rs", outside),
                (
                    "crates/aterm-machines/src/items/power.rs",
                    "const PMSET: &str = \"/usr/bin/pmset\";\nfn a() { crate::agent::main(a, s); }\n",
                ),
                (
                    "crates/aterm-machines-extra/src/lib.rs",
                    "const V: &str = \"/usr/sbin/visudo\";\n",
                ),
                (
                    "crates/aterm-gui/tests/updates.rs",
                    "const S: &str = \"/usr/sbin/softwareupdate\";\n",
                ),
                (
                    "apps/aterm-mac/src/helper.rs",
                    "fn a() { run(\"/usr/sbin/installer\", &[\"-pkg\", p]); }\n",
                ),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "apps/aterm-mac/src/helper.rs:1",
                "crates/aterm-cli/src/lib.rs:1",
                "crates/aterm-cli/src/lib.rs:3",
                "crates/aterm-cli/src/lib.rs:4",
                "crates/aterm-cli/src/lib.rs:5",
                "crates/aterm-cli/src/lib.rs:6",
                "crates/aterm-cli/src/lib.rs:8",
                "crates/aterm-machines-extra/src/lib.rs:1",
            ],
            "{found:#?}"
        );
    }
}
