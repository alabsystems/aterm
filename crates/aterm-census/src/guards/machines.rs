// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `machines-root-door`: root tools `aterm machines` runs stay inside its crate.

use super::rules::{CENSUS_CRATE, PRODUCT, test_path};
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

const ROSETTA_INSTALL: (&str, &str, &str) = (
    "crates/atpkg/src/platform/unix.rs",
    "/usr/sbin/softwareupdate",
    r#"["/usr/sbin/softwareupdate","--install-rosetta","--agree-to-license"]"#,
);

pub(super) const MACHINES_ROOT_DOOR: Rule = Rule {
    id: "machines-root-door",
    why: "installer, softwareupdate, visudo, /etc/sudoers.d, pmset, socketfilterfw and \
          systemsetup act as root on a whole Mac: only aterm-machines' agent, under the \
          owner's ruling M1 and one digest-pinned sudoers rule, may name them for execution, \
          and nothing outside the crate reaches `aterm_machines::agent` past \
          `cli::main_entry`. The one execution exception is not root work (ruling R1): \
          atpkg's Rosetta 2 install, the whole argv \
          [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"] in \
          crates/atpkg/src/platform/unix.rs, which installs as the user with no administrator. \
          The GUI installer probe may only compare softwareupdate's executable identity \
          in a closed boolean helper, without binding or executing the path",
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
            if let Some(tool) = ROOT_TOOLS.iter().find(|t| line.text.contains(*t))
                && !is_rosetta_install(file, &line, tool)
                && !is_installer_identity(file, &line, tool)
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

fn is_installer_identity(file: &SourceFile, line: &Line<'_>, tool: &str) -> bool {
    if file.rel != INSTALLER_PROBE
        || tool != ROSETTA_INSTALL.1
        || line.text.matches(tool).count() != 1
        || ROOT_TOOLS
            .iter()
            .any(|other| *other != tool && line.text.contains(other))
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
        at <= literal_at
            && literal_at < close
            && squeeze(&lexed.text[at..open])
                == "fnis_softwareupdate(path:&::std::path::Path)->bool"
            && squeeze(&lexed.text[open + 1..close])
                == format!("path==::std::path::Path::new(\"{tool}\")")
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

    #[test]
    fn only_the_rosetta_install_argv_in_atpkgs_platform_file_names_softwareupdate() {
        let rel = ROSETTA_INSTALL.0;
        let admitted = "/// The install.\n\
                        const ROSETTA_INSTALL: [&str; 3] = [\n\
                        \x20   \"/usr/sbin/softwareupdate\",\n\
                        \x20   \"--install-rosetta\",\n\
                        \x20   \"--agree-to-license\",\n\
                        ];\n\
                        const ONE_LINE: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];\n\
                        // a comment naming /usr/sbin/softwareupdate says nothing\n";
        let near_misses = "fn a() { let mut c = spawnfd::command(\"/usr/sbin/softwareupdate\"); }\n\
                           const ALL: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install\", \"--all\"];\n\
                           const MORE: [&str; 4] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\", \"--restart\"];\n\
                           const LESS: [&str; 2] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\"];\n\
                           const SUDO: [&str; 4] = [\"/usr/bin/sudo\", \"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];\n\
                           fn b() { run(\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"); }\n\
                           const TWO: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"]; const I: &str = \"/usr/sbin/installer\";\n\
                           const TWICE: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"]; const S: &str = \"/usr/sbin/softwareupdate\";\n\
                           const SPACED: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta \", \"--agree-to-license\"];\n";
        assert!(
            run(&MACHINES_ROOT_DOOR, &[(rel, admitted)]).is_empty(),
            "the admitted argv, wrapped and on one line"
        );
        let found = run(
            &MACHINES_ROOT_DOOR,
            &[
                (rel, &format!("{admitted}{near_misses}")),
                ("crates/atpkg/src/machine.rs", admitted),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/atpkg/src/machine.rs:3",
                "crates/atpkg/src/machine.rs:7",
                "crates/atpkg/src/platform/unix.rs:9",
                "crates/atpkg/src/platform/unix.rs:10",
                "crates/atpkg/src/platform/unix.rs:11",
                "crates/atpkg/src/platform/unix.rs:12",
                "crates/atpkg/src/platform/unix.rs:13",
                "crates/atpkg/src/platform/unix.rs:14",
                "crates/atpkg/src/platform/unix.rs:15",
                "crates/atpkg/src/platform/unix.rs:16",
                "crates/atpkg/src/platform/unix.rs:17",
            ],
            "{found:#?}"
        );
        assert!(
            found
                .iter()
                .filter(|f| f.detail.contains("softwareupdate"))
                .all(|f| f
                    .detail
                    .contains("one execution exception is atpkg's Rosetta 2 install")),
            "a refused spelling names the admitted one: {found:#?}"
        );
    }

    #[test]
    fn the_installer_probe_only_names_softwareupdate_as_a_closed_path_identity() {
        let identity = "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n";
        let wrapped = "fn is_softwareupdate(path: &::std::path::Path) -> bool {\n\
                       path == ::std::path::Path::new(\n\
                       \"/usr/sbin/softwareupdate\"\n\
                       )\n\
                       }\n";
        for allowed in [identity, wrapped] {
            assert!(
                run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, allowed)]).is_empty(),
                "a complete boolean identity, regardless of wrapping: {allowed}"
            );
        }
        for refused in [
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { let tool = ::std::path::Path::new(\"/usr/sbin/softwareupdate\"); path == tool }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { spawnfd::command(\"/usr/sbin/softwareupdate\"); true }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { extra(); path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { path != ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> &::std::path::Path { ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/bin/pmset\") }\n",
            "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") } const TOOL: &str = \"/usr/sbin/softwareupdate\";\n",
        ] {
            let found = run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, refused)]);
            assert!(
                !found.is_empty(),
                "an executable path must not escape: {refused}"
            );
        }
        assert!(
            !run(
                &MACHINES_ROOT_DOOR,
                &[("crates/aterm-gui/src/other.rs", identity)]
            )
            .is_empty(),
            "the read-only probe does not open other files"
        );
    }

    #[test]
    fn an_alias_with_an_executing_constructor_is_not_a_path_identity() {
        let source = "use crate::ExecutingPath as Path;\n\
                      #[derive(PartialEq)] struct ExecutingPath;\n\
                      impl ExecutingPath { fn new(executable: &str) -> &'static Self { let _ = spawnfd::command(executable).status(); &ExecutingPath } }\n\
                      fn is_softwareupdate(path: &Path) -> bool { path == Path::new(\"/usr/sbin/softwareupdate\") }\n";
        let found = run(&MACHINES_ROOT_DOOR, &[(INSTALLER_PROBE, source)]);
        assert_eq!(spots(&found), [format!("{INSTALLER_PROBE}:4")]);
    }

    #[test]
    fn admitted_tool_spellings_never_hide_agent_access_on_the_same_line() {
        for (rel, admitted) in [
            (
                INSTALLER_PROBE,
                "fn is_softwareupdate(path: &::std::path::Path) -> bool { path == ::std::path::Path::new(\"/usr/sbin/softwareupdate\") }",
            ),
            (
                ROSETTA_INSTALL.0,
                "const ARGV: [&str; 3] = [\"/usr/sbin/softwareupdate\", \"--install-rosetta\", \"--agree-to-license\"];",
            ),
        ] {
            let source =
                format!("{admitted} fn forbidden() {{ aterm_machines::agent::main(a, s); }}\n");
            let found = run(&MACHINES_ROOT_DOOR, &[(rel, &source)]);
            assert_eq!(found.len(), 1, "{found:#?}");
            assert!(found[0].detail.contains("reaches `aterm_machines::agent`"));
        }
    }
}
