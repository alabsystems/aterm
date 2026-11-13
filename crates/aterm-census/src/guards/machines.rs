// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `machines-root-door`: root tools `aterm machines` runs stay inside its crate.

use super::rules::{CENSUS_CRATE, PRODUCT};
use super::{Bound, Finding, Line, Rule, SourceFile, Tree, brace_close, has, hits};

const MACHINES_CRATE: &str = "crates/aterm-machines/";

const ROOT_TOOLS: &[&str] = &[
    "/usr/sbin/installer",
    "/usr/sbin/softwareupdate",
    "/usr/sbin/visudo",
    "/etc/sudoers.d",
    "/usr/bin/pmset",
    "socketfilterfw",
    "systemsetup",
];

const AGENT_PATH: &str = "aterm_machines::agent";
const INSTALLER_PROBE: &str = "crates/aterm-gui/src/clt_install/probe.rs";

/// The admitted-name rule's two paths (`aterm_machines::root_steps`), as aterm-types holds them so
/// the window can stat them with no root and no dependency on aterm-machines: data, each the
/// whole of one `pub const` line, never an argv.
const RULE_PATHS: (&str, [&str; 2]) = (
    "crates/aterm-types/src/macos_setup.rs",
    [
        r#"pubconstADMITTED_NAME_RULE:&str="/private/etc/sudoers.d/aterm-admitted-name";"#,
        r#"pubconstADMITTED_NAME_RULE_REFUSED:&str="/private/etc/sudoers.d/.aterm-admitted-name.refused";"#,
    ],
);

/// `lab mac nux --sudo-asks`' guest rule (`crates/aterm-lab/src/mac_nux.rs`): a script that
/// replaces a tart guest's own NOPASSWD rule, run in that guest through `tart exec`, never on
/// this Mac. Only inside that one function's body.
const LANE_GUEST_RULE: (&str, &str) = ("crates/aterm-lab/src/mac_nux.rs", "sudo_asks_script");

const ROSETTA_INSTALL: (&str, &str, &str) = (
    "crates/atpkg/src/platform/unix.rs",
    "/usr/sbin/softwareupdate",
    r#"["/usr/sbin/softwareupdate","--install-rosetta","--agree-to-license"]"#,
);

pub(super) const MACHINES_ROOT_DOOR: Rule = Rule {
    id: "machines-root-door",
    why: "installer, softwareupdate, visudo, /etc/sudoers.d, pmset, socketfilterfw and \
          systemsetup act as root on a whole Mac: only aterm-machines' agent, under the \
          machines design's rule M1 and one digest-pinned sudoers rule, may name them for \
          execution, and nothing outside the crate reaches `aterm_machines::agent` past \
          `cli::main_entry`. The one execution exception is not root work (rule R1): \
          atpkg's Rosetta 2 install, the whole argv \
          [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"] in \
          crates/atpkg/src/platform/unix.rs, which installs as the user with no administrator. \
          The GUI installer probe may only compare softwareupdate's and installer's executable \
          identities in a closed boolean helper, without binding or executing either path. \
          aterm-types may hold the admitted-name rule's two paths as whole `pub const` lines, \
          which the window stats and never runs, and the mac lab lane's `sudo_asks_script` may \
          name visudo and /etc/sudoers.d in the tart guest's script it builds",
    scope: &[MACHINES_CRATE],
    check: machines_root_door,
};

fn machines_root_door(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut needles: Vec<&str> = ROOT_TOOLS.to_vec();
    needles.push("aterm_machines");
    for file in product(tree) {
        if file.rel.starts_with(MACHINES_CRATE)
            || tree.corpus.test_code().holds(&file.rel)
            || !file.mentions(&needles)
        {
            continue;
        }
        for line in file.shipped_lines() {
            if let Some(tool) = ROOT_TOOLS.iter().find(|t| line.text.contains(*t))
                && !is_rosetta_install(file, &line, tool)
                && !is_installer_identity(file, &line, tool)
                && !is_rule_path(file, &line, tool)
                && !is_lane_guest_rule(file, &line, tool)
            {
                let why = format!(
                    "names `{tool}`, a root tool only aterm-machines' agent runs{}",
                    if *tool == ROSETTA_INSTALL.1 {
                        " (the one execution exception is atpkg's Rosetta 2 install, as the \
                         whole argv array in crates/atpkg/src/platform/unix.rs; the GUI \
                         installer probe may only compare its executable identity)"
                    } else {
                        ""
                    }
                );
                out.push(file.finding(MACHINES_ROOT_DOOR.id, line.no, why));
            }
            if reaches_agent(line.code) {
                let why = format!(
                    "reaches `{AGENT_PATH}`; its one door is `aterm_machines::cli::main_entry`"
                );
                out.push(file.finding(MACHINES_ROOT_DOOR.id, line.no, why));
            }
        }
    }
}

/// One of [`RULE_PATHS`]' lines, whole.
fn is_rule_path(file: &SourceFile, line: &Line<'_>, tool: &str) -> bool {
    let (rel, lines) = RULE_PATHS;
    file.rel == rel && tool == "/etc/sudoers.d" && lines.contains(&squeeze(line.text).as_str())
}

/// A line inside the body of [`LANE_GUEST_RULE`]'s function, naming visudo or /etc/sudoers.d.
fn is_lane_guest_rule(file: &SourceFile, line: &Line<'_>, tool: &str) -> bool {
    let (rel, func) = LANE_GUEST_RULE;
    if file.rel != rel || !matches!(tool, "/usr/sbin/visudo" | "/etc/sudoers.d") {
        return false;
    }
    let lexed = file.lexed();
    let line_at: usize = lexed
        .code
        .split_inclusive('\n')
        .take(line.no - 1)
        .map(str::len)
        .sum();
    let definition = format!("fn {func}");
    hits(&lexed.code, &definition, Bound::Word).any(|at| {
        let Some(open) = lexed.code[at..].find('{').map(|offset| at + offset) else {
            return false;
        };
        brace_close(&lexed.code, open).is_some_and(|close| open < line_at && line_at < close)
    })
}

fn is_installer_identity(file: &SourceFile, line: &Line<'_>, tool: &str) -> bool {
    if file.rel != INSTALLER_PROBE
        || !matches!(tool, "/usr/sbin/softwareupdate" | "/usr/sbin/installer")
        || line.text.matches(tool).count() != 1
    {
        return false;
    }
    let lexed = file.lexed();
    let line_at: usize = lexed
        .code
        .split_inclusive('\n')
        .take(line.no - 1)
        .map(str::len)
        .sum();
    let Some(literal) = line.text.find(tool) else {
        return false;
    };
    let literal_at = line_at + literal;
    // The whole helper must consume the path as a boolean, never expose it as a value.
    hits(&lexed.code, "fn", Bound::Word).any(|at| {
        let Some(open) = lexed.code[at..].find('{').map(|offset| at + offset) else {
            return false;
        };
        let Some(close) = brace_close(&lexed.code, open) else {
            return false;
        };
        let signature = squeeze(&lexed.text[at..open]);
        let body = squeeze(&lexed.text[open + 1..close]);
        let softwareupdate = "path==::std::path::Path::new(\"/usr/sbin/softwareupdate\")";
        let installer = "path==::std::path::Path::new(\"/usr/sbin/installer\")";
        let closed = match signature.as_str() {
            "fnis_softwareupdate(path:&::std::path::Path)->bool" => body == softwareupdate,
            "fnis_install_process(path:&::std::path::Path)->bool" => {
                body == format!("{softwareupdate}||{installer}")
                    || body == format!("{installer}||{softwareupdate}")
            }
            _ => false,
        };
        at <= literal_at
            && literal_at < close
            && closed
            // The rule checks one root-tool finding per line. Every root-tool spelling on
            // that line must belong to this helper, including when both comparisons share it.
            && ROOT_TOOLS.iter().all(|other| {
                line.text.match_indices(*other).all(|(offset, _)| {
                    let here = line_at + offset;
                    open < here && here < close
                })
            })
    })
}

fn is_rosetta_install(file: &SourceFile, line: &Line<'_>, tool: &str) -> bool {
    let (rel, admitted, argv) = ROSETTA_INSTALL;
    if file.rel != rel
        || tool != admitted
        || ROOT_TOOLS
            .iter()
            .any(|t| *t != admitted && line.text.contains(t))
        || line.text.matches(admitted).count() != 1
    {
        return false;
    }
    let quoted = format!("\"{admitted}\"");
    let Some(at) = line.text.find(&quoted) else {
        return false;
    };
    let lines: Vec<&str> = file.lines().map(|l| l.text).collect();
    let here = line.no - 1;
    let before = std::iter::once(&line.text[..at])
        .chain(lines[..here].iter().rev().copied())
        .map(str::trim_end)
        .find(|t| !t.is_empty());
    if before.is_none_or(|t| !t.ends_with('[')) {
        return false;
    }
    let rest = std::iter::once(&line.text[at..])
        .chain(lines[here + 1..].iter().take(8).copied())
        .collect::<Vec<_>>()
        .join("\n");
    let body = &argv[1..argv.len() - 1];
    squeeze(&rest)
        .strip_prefix(body)
        .is_some_and(|tail| tail.starts_with(']') || tail.starts_with(",]"))
}

fn squeeze(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    let mut escaped = false;
    for c in text.chars() {
        if quoted {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => quoted = false,
                _ => {}
            }
        } else if c == '"' {
            quoted = true;
            out.push(c);
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

fn product<'a>(tree: &Tree<'a>) -> impl Iterator<Item = &'a SourceFile> {
    tree.corpus
        .under(PRODUCT)
        .filter(|f| !f.rel.starts_with(CENSUS_CRATE))
}

fn reaches_agent(code: &str) -> bool {
    has(code, AGENT_PATH, Bound::Word)
        || (code.contains("aterm_machines::{") && has(code, "agent", Bound::Word))
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
