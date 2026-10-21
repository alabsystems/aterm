// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! headless-lifeline: every throwaway `--headless` instance a test, example,
//! bench or dev tool starts holds a lifeline.
//!
//! An instance only its launcher's `Drop` ends outlives a launcher that is
//! SIGKILLed or timed out. The launch hands the instance the far end of a FIFO
//! the launcher holds (`--lifeline-fd`, `aterm_uds::lifeline`), and the
//! instance quits when the launcher goes.
//!
//! Lexically, per file:
//! * a Rust launch is an `.arg(…)`/`.args([…])` naming `"--headless"`, or an
//!   `.args([` whose elements trustfmt broke one to a line with a
//!   `"--headless"` among them; the file is armed when it holds at least as many
//!   arm calls handed a `&mut` command (`launch_isolation::lifeline`,
//!   `Lifeline::arm`, `arm_for_process`, a local `arm_lifeline`) as unwaived
//!   launches, since a `Command` is often built in one function and armed in its
//!   caller;
//! * a shell launch — in a script, or a script a Rust string holds — is a
//!   program word (`"$B"`, `$A`, `${GUI}`, a path ending in an aterm name), bare
//!   flags, then `--headless`; it is armed when its logical line carries
//!   `--lifeline-fd`;
//! * a Python launch is a `"--headless"` list element; it is armed when the
//!   three lines on either side of it name the lifeline.
//!
//! Any launch is waived by a `NO LIFELINE …: <reason>` comment on its line or
//! in the eight above (an instance meant to outlive its launcher, a stand-in
//! that is no aterm). A launch spelled another way (the flag in a variable, a
//! `format!`ed command line) is not seen.

use super::{Finding, Rule, Script, SourceFile, Tree, is_word};

pub(super) const HEADLESS_LIFELINE: Rule = Rule {
    id: "headless-lifeline",
    why: "a --headless instance its launcher only ends in a Drop outlives a SIGKILLed or \
          timed-out launcher, holding a shell for as long as the machine runs; arm a lifeline \
          (aterm_uds::lifeline, launch_isolation::lifeline) or write a `NO LIFELINE …: <reason>` \
          waiver",
    scope: &[],
    check: headless_lifeline,
};

/// Is `rel` a launcher's source: a crate's tests, examples or benches, or a
/// dev tool that starts instances?
fn launcher_source(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix("crates/") else {
        return false;
    };
    let in_crate = rest.split_once('/').map_or("", |(_, inner)| inner);
    in_crate.starts_with("tests/")
        || in_crate.starts_with("examples/")
        || in_crate.starts_with("benches/")
        || rel.starts_with("crates/aterm-verify/src/")
        || rel.starts_with("crates/aterm-release/src/")
}

/// The launch every live headless suite boots through. A read that does not
/// reach it lost the tree (a moved `tests/`, a narrowed [`launcher_source`]),
/// and a rule that reads no launch holds vacuously. An anchor, not a count: a
/// launcher deleted on purpose moves no number.
const ANCHOR_LAUNCH_FILE: &str = "crates/aterm/tests/support/headless_boot.rs";

fn headless_lifeline(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let mut anchored = false;
    for file in tree.corpus.under(&["crates/"]) {
        if launcher_source(&file.rel) && file.mentions(&["--headless"]) {
            anchored |= file.rel == ANCHOR_LAUNCH_FILE;
            out.extend(lifeline_findings(file));
        }
    }
    for script in &tree.corpus.scripts {
        if launcher_script(&script.rel) && script.raw.contains("--headless") {
            out.extend(script_findings(script));
        }
    }
    if !anchored {
        out.push(Finding {
            rule: HEADLESS_LIFELINE.id,
            path: "crates/".to_string(),
            line: 0,
            detail: format!(
                "read no launch in {ANCHOR_LAUNCH_FILE}, the boot every live headless suite \
                 uses: the scope lost the tree"
            ),
        });
    }
}

/// Is `rel` a script that may start instances: under `tools/`, or under a
/// crate's tests, examples or benches?
fn launcher_script(rel: &str) -> bool {
    rel.starts_with("tools/") || launcher_source(rel)
}

/// How many lines on either side of a Python launch may name its lifeline.
const PY_REACH: usize = 3;

/// Every unarmed, unwaived launch in one script: shell launches in any script,
/// and `"--headless"` list elements in a Python one.
fn script_findings(script: &Script) -> Vec<Finding> {
    let lines: Vec<&str> = script.raw.lines().collect();
    let waived = |i: usize| {
        lines[i.saturating_sub(WAIVER_REACH)..=i]
            .iter()
            .any(|l| waiver(l))
    };
    let python = script.is_python();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("//") || !line.contains("--headless") {
            continue;
        }
        if shell_launch(line) {
            let logical = logical_line(&lines, i);
            if !armed_shell(&logical) && !waived(i) {
                out.push(script.finding(
                    HEADLESS_LIFELINE.id,
                    i + 1,
                    "unarmed shell launch (no --lifeline-fd, no NO LIFELINE waiver)".to_string(),
                ));
            }
        } else if python && python_launch(line) {
            let near = &lines[i.saturating_sub(PY_REACH)..(i + PY_REACH + 1).min(lines.len())];
            if !line.contains("--help")
                && !near.iter().any(|l| l.contains("lifeline"))
                && !waived(i)
            {
                out.push(
                    script.finding(
                        HEADLESS_LIFELINE.id,
                        i + 1,
                        "unarmed python launch (the statement names no lifeline, no NO LIFELINE \
                     waiver)"
                            .to_string(),
                    ),
                );
            }
        }
    }
    out
}

/// A `"--headless"` (or `'--headless'`) list element: first on its line, or
/// after a `[`, `,` or `(`.
fn python_launch(line: &str) -> bool {
    ["\"--headless\"", "'--headless'"].iter().any(|elem| {
        line.match_indices(elem).any(|(at, _)| {
            let before = line[..at].trim_end_matches([' ', '\t']);
            before.is_empty() || before.ends_with(['[', ',', '('])
        })
    })
}

/// The logical line starting at `i`: backslash continuations joined.
fn logical_line(lines: &[&str], i: usize) -> String {
    let mut logical = lines[i].to_string();
    let mut j = i;
    while j + 1 < lines.len() && lines[j].trim_end().ends_with('\\') {
        j += 1;
        logical.push('\n');
        logical.push_str(lines[j]);
    }
    logical
}

/// Is a shell launch's logical line armed (or only a `--help` probe)?
fn armed_shell(logical: &str) -> bool {
    help_probe(logical) || logical.contains("--lifeline-fd")
}

/// How many lines above a launch its waiver may sit.
const WAIVER_REACH: usize = 8;
/// How many lines an `.args([` broken one element to a line is read for.
const ARGS_REACH: usize = 40;

/// Every unarmed, unwaived launch in one file.
fn lifeline_findings(file: &SourceFile) -> Vec<Finding> {
    let raw: Vec<&str> = file.raw.lines().collect();
    let text: Vec<&str> = file.lexed().text.lines().collect();
    let waived = |i: usize| {
        raw[i.saturating_sub(WAIVER_REACH)..=i]
            .iter()
            .any(|l| waiver(l))
    };
    let mut out = Vec::new();
    let mut sites = Vec::new();
    let mut arms = 0usize;
    let mut in_args = 0usize;
    for (i, line) in text.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        if arms_a_lifeline(line) {
            arms += 1;
        }
        let was_in = in_args > 0;
        if in_args > 0 {
            in_args = if closes_args(line) { 0 } else { in_args - 1 };
        } else if opens_args(line) {
            in_args = ARGS_REACH;
        }
        if line.contains("--headless") && shell_launch(line) {
            if armed_shell(&logical_line(&text, i)) || waived(i) {
                continue;
            }
            out.push(file.finding(
                HEADLESS_LIFELINE.id,
                i + 1,
                "unarmed shell launch (no --lifeline-fd, no NO LIFELINE waiver)".to_string(),
            ));
        } else if (arg_names_headless(line) || (was_in && line.contains("\"--headless\"")))
            && !waived(i)
        {
            sites.push(i);
        }
    }
    if sites.len() > arms {
        for &i in &sites {
            out.push(file.finding(
                HEADLESS_LIFELINE.id,
                i + 1,
                format!(
                    "unarmed rust launch ({} --headless launch(es), {arms} lifeline arm call(s) \
                     in the file)",
                    sites.len()
                ),
            ));
        }
    }
    out
}

/// `NO LIFELINE …: <reason>`, the reason at least ten characters.
fn waiver(raw: &str) -> bool {
    raw.match_indices("NO LIFELINE").any(|(at, _)| {
        let rest = &raw[at + "NO LIFELINE".len()..];
        if rest.bytes().next().is_some_and(is_word) {
            return false;
        }
        rest.split_once(':')
            .is_some_and(|(_, reason)| reason.trim_start().chars().count() >= 10)
    })
}

/// An arm call handed a `&mut` command.
fn arms_a_lifeline(line: &str) -> bool {
    ["lifeline(", "Lifeline::arm(", "arm_for_process("]
        .iter()
        .any(|door| {
            line.match_indices(door).any(|(at, _)| {
                let rest = line[at + door.len()..].trim_start();
                rest.strip_prefix("&mut")
                    .is_some_and(|r| r.starts_with(char::is_whitespace))
            })
        })
}

/// `.arg(…)` or `.args(…)` whose parenthesis holds `"--headless"` before it closes.
fn arg_names_headless(line: &str) -> bool {
    [".arg(", ".args("].iter().any(|open| {
        line.match_indices(open).any(|(at, _)| {
            let inner = &line[at + open.len()..];
            let inner = inner.split(')').next().unwrap_or(inner);
            inner.contains("\"--headless\"")
        })
    })
}

/// A line that ends by opening an `.args(` array: `.args([`, `.args(&[`,
/// `.args(vec![` or a bare `.args(`.
fn opens_args(line: &str) -> bool {
    line.match_indices(".args(").any(|(at, _)| {
        let rest = line[at + 6..].trim();
        rest.is_empty() || matches!(rest, "[" | "&[" | "vec![")
    })
}

/// A line that closes the array (`]` then `)`, a `,` between allowed) or
/// starts with `)`.
fn closes_args(line: &str) -> bool {
    line.trim_start().starts_with(')')
        || line.match_indices(']').any(|(at, _)| {
            let rest = line[at + 1..].trim_start();
            let rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
            rest.starts_with(')')
        })
}

/// ` --help` as a word: a usage probe, not an instance.
fn help_probe(logical: &str) -> bool {
    logical.match_indices("--help").any(|(at, _)| {
        at > 0
            && logical.as_bytes()[at - 1].is_ascii_whitespace()
            && logical.as_bytes().get(at + 6).is_none_or(|&b| !is_word(b))
    })
}

/// A shell launch: a program word, any bare flags, then `--headless`.
fn shell_launch(line: &str) -> bool {
    let b = line.as_bytes();
    (0..b.len()).any(|i| {
        let starts = i == 0 || !(is_word(b[i - 1]) || b[i - 1] == b'$');
        starts
            && line.is_char_boundary(i)
            && program_words(&line[i..])
                .into_iter()
                .any(|end| flags_then_headless(&line[i + end..]))
    })
}

/// Where each program word starting `s` may end: `"$X"`, `$X`, `${X}` (quoted
/// or not), or a path whose last component is an aterm name.
fn program_words(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut ends = Vec::new();
    let var = |from: usize| -> Option<usize> {
        let mut j = from;
        let braced = b.get(j) == Some(&b'{');
        if braced {
            j += 1;
        }
        let name = b[j.min(b.len())..]
            .iter()
            .take_while(|&&c| is_word(c))
            .count();
        if name == 0 {
            return None;
        }
        j += name;
        if braced && b.get(j) == Some(&b'}') {
            j += 1;
        }
        Some(j)
    };
    if s.starts_with("\"$")
        && let Some(end) = var(2)
        && b.get(end) == Some(&b'"')
    {
        ends.push(end + 1);
    }
    if s.starts_with('$')
        && let Some(end) = var(1)
    {
        ends.push(end);
    }
    let run = b
        .iter()
        .take_while(|&&c| !(c.is_ascii_whitespace() || c == b'"' || c == b'\'' || c == b'='))
        .count();
    let token = &s[..run];
    if let Some(at) = token.rfind("/aterm")
        && token[at + 6..].bytes().all(|c| is_word(c) || c == b'-')
    {
        ends.push(if b.get(run) == Some(&b'"') {
            run + 1
        } else {
            run
        });
    }
    ends
}

/// Bare flags, then `--headless` followed by a space, a `\` or the line's end.
fn flags_then_headless(mut rest: &str) -> bool {
    loop {
        let token_start = rest.trim_start();
        if token_start.len() == rest.len() {
            return false;
        }
        if let Some(after) = token_start.strip_prefix("--headless")
            && (after.is_empty()
                || after.starts_with(char::is_whitespace)
                || after.starts_with('\\'))
        {
            return true;
        }
        let len = token_start
            .find(char::is_whitespace)
            .unwrap_or(token_start.len());
        let token = &token_start[..len];
        let flag = token
            .strip_prefix("--")
            .or_else(|| token.strip_prefix('-'))
            .is_some_and(|f| {
                f.bytes().next().is_some_and(|c| c.is_ascii_alphabetic())
                    && f.bytes().all(|c| is_word(c) || c == b'-')
            });
        if !flag {
            return false;
        }
        rest = &token_start[len..];
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::run;
    use super::*;

    fn launches(path: &str, src: &str) -> Vec<String> {
        lifeline_findings(&SourceFile::new(path.to_string(), src.to_string()))
            .into_iter()
            .map(|f| format!(":{}:{}", f.line, f.detail))
            .collect()
    }

    #[test]
    fn a_scope_that_misses_the_one_boot_is_a_finding() {
        let lost = |found: &[Finding]| {
            found.len() == 1 && found[0].detail.starts_with("read no launch in ")
        };
        assert!(lost(&run(&HEADLESS_LIFELINE, &[])));

        let armed = "fn boot(root: &Path) {\n    cmd.arg(\"--headless\");\n    \
                     let l = launch_isolation::lifeline(&mut cmd, root);\n}\n";
        // Armed launches elsewhere do not stand in for the boot.
        let elsewhere = run(&HEADLESS_LIFELINE, &[("crates/aterm/tests/t.rs", armed)]);
        assert!(lost(&elsewhere), "{elsewhere:?}");
        assert!(run(&HEADLESS_LIFELINE, &[(ANCHOR_LAUNCH_FILE, armed)]).is_empty());
    }

    /// Each unarmed launch shape, with the line and shape its finding prints.
    #[test]
    fn every_unarmed_launch_is_found() {
        let cases: &[(&str, &str, &str)] = &[
            (
                "an array trustfmt broke",
                "fn launch(root: &Path) -> Child {\n    let mut cmd = Command::new(env!(\"CARGO_BIN_EXE_aterm\"));\n    cmd.args([\n        \"--no-reroute\",\n        \"--headless\",\n        \"--control-sock\",\n    ])\n    .stdin(Stdio::null());\n    cmd.spawn().expect(\"launch\")\n}\n",
                ":5:unarmed rust launch (1 --headless launch(es), 0 lifeline arm call(s)",
            ),
            (
                "a script inside a Rust string",
                "#[test]\nfn a_driver_script() {\n    let script = r#\"\nset -u\nB=$1 S=$2/s.sock\n\"$B\" --headless --control-sock \"$S\" </dev/null &\n\"#;\n}\n",
                ":6:unarmed shell launch",
            ),
            (
                "a second launch beside one arm",
                "fn first(root: &Path) -> (Child, Lifeline) {\n    cmd.args([\"--headless\", \"--no-reroute\"]);\n    let lifeline = launch_isolation::lifeline(&mut cmd, root);\n}\n\nfn second(root: &Path) -> Child {\n    // launch_isolation::lifeline(&mut cmd, root);\n    cmd.arg(\"--headless\").stdin(Stdio::null());\n}\n",
                ":8:unarmed rust launch (2 --headless launch(es), 1 lifeline arm call(s)",
            ),
            (
                "a test whose Drop is its only teardown",
                "fn headless(tmp: &Path) -> Command {\n    let mut cmd = Command::new(env!(\"CARGO_BIN_EXE_aterm\"));\n    cmd.args([\"--headless\", launch_isolation::NO_REROUTE])\n        .stdin(Stdio::null());\n    cmd\n}\n",
                ":3:unarmed rust launch (1 --headless launch(es), 0 lifeline arm call(s)",
            ),
            (
                "a flag before the headless one, in a script",
                "const S: &str = r#\"\n\"$A\" --cpu --headless &\n\"#;\n",
                ":2:unarmed shell launch",
            ),
            (
                "a waiver with no reason",
                "const S: &str = r#\"\n# NO LIFELINE:\n\"$A\" --headless &\n\"#;\n",
                ":3:unarmed shell launch",
            ),
            (
                "a waiver too far above",
                "const S: &str = r#\"\n# NO LIFELINE on purpose: a stand-in that sleeps itself out\n\n\n\n\n\n\n\n\n\"$A\" --headless &\n\"#;\n",
                ":11:unarmed shell launch",
            ),
            (
                "a path to the binary",
                "const S: &str = r#\"\ntarget/debug/aterm-gui --headless --control-sock x &\n\"#;\n",
                ":2:unarmed shell launch",
            ),
        ];
        for (why, src, expect) in cases {
            let found = launches("crates/aterm/tests/t.rs", src);
            assert!(
                found.iter().any(|f| f.starts_with(expect)),
                "MISS — {why}: {found:?}\n{src}"
            );
        }
    }

    #[test]
    fn armed_waived_and_non_launch_shapes_are_silent() {
        let armed = "fn headless(root: &Path) -> Command {\n    cmd.args([\"--headless\", launch_isolation::NO_REROUTE]);\n    cmd\n}\n\
                     #[test]\nfn held() {\n    let mut cmd = headless(&root);\n    let lifeline = launch_isolation::lifeline(&mut cmd, &root);\n}\n\
                     fn sibling(tmp: &Path) -> Child {\n    cmd.arg(\"--headless\")\n        .arg(\"--control-sock\");\n    arm_lifeline(&mut cmd, tmp);\n}\n\
                     fn rosetta(u: &Path, scratch: &Path) -> Result<(Child, Lifeline), String> {\n    instance.args([\"--headless\", \"--cpu\"]);\n    let lifeline =\n        aterm_uds::lifeline::Lifeline::arm(&mut instance, scratch).map_err(|e| e.to_string())?;\n}\n\
                     fn broken_array(root: &Path) -> (Child, Lifeline) {\n    cmd.args([\n        \"--headless\",\n        \"--control-sock\",\n    ]);\n    let lifeline = launch_isolation::lifeline(&mut cmd, root);\n}\n";
        assert!(launches("crates/aterm/tests/a.rs", armed).is_empty());

        let names_only = "fn boot() {\n    eprintln!(\"SKIP: cannot launch aterm --headless\");\n    // cmd.arg(\"--headless\");\n}\n\
                          fn is_the_daemon(line: &str) -> bool {\n    line.contains(\"aterm-gui --headless\") && !line.contains(\"--help\")\n}\n\
                          fn fake(command: &Command) {\n    for flag in [\"--headless\", \"--cpu\"] {\n        assert!(args.iter().any(|a| a == flag));\n    }\n}\n\
                          fn a_client_then_a_check(sock: &Path) {\n    ctl.args([\n        \"ctl\",\n        \"status\",\n    ]);\n    let flags = [\n        \"--headless\",\n        \"--no-reroute\",\n    ];\n}\n\
                          const SCRIPT: &str = r#\"\nF=$1 S=$2/s.sock\n# NO LIFELINE on purpose: \"$F\" is a stand-in that is no aterm.\n\"$F\" --headless --control-sock \"$S\" &\n\"#;\n\
                          const ARMED: &str = r#\"\n\"$A\" --headless --lifeline-fd 0 &\n\"$A\" --headless \\\n    --lifeline-fd 3 &\n\"$A\" --headless --help\n\"#;\n\
                          const USAGE: &str = \"usage: aterm [--cpu] [--headless]\";\n";
        let found = launches("crates/aterm/tests/b.rs", names_only);
        assert!(found.is_empty(), "CRIES WOLF: {found:?}");
    }

    /// Scripts under tools/ or a crate's tests: a shell launch armed by its
    /// logical line, a Python one by the lines around it; both waivable.
    #[test]
    fn a_scripts_launches_hold_a_lifeline() {
        let shell = "#!/bin/bash\n\"$A\" --headless --control-sock \"$S\" &\n\
                     \"$A\" --headless --lifeline-fd 0 &\n\"$A\" --cpu --headless \\\n    \
                     --lifeline-fd 3 &\n# \"$A\" --headless &\n\
                     # NO LIFELINE on purpose: a stand-in that is no aterm\n\"$F\" --headless &\n\
                     \"$A\" --headless --help\n";
        let python = "import subprocess\nproc = subprocess.Popen([\n    aterm,\n    \"--headless\",\n    \
                      \"--control-sock\",\n    sock,\n])\n\n\n\n\
                      args = [aterm, \"--headless\"] + lifeline.args\n\
                      print('--headless')\n";
        let found = run(
            &HEADLESS_LIFELINE,
            &[
                ("tools/drive.sh", shell),
                ("tools/drive.py", python),
                ("crates/aterm/tests/fixtures/run.sh", shell),
                ("docs/run.sh", shell),
            ],
        );
        let spots: Vec<String> = found
            .iter()
            .filter(|f| f.line > 0)
            .map(|f| format!("{}:{}", f.path, f.line))
            .collect();
        assert_eq!(
            spots,
            [
                "crates/aterm/tests/fixtures/run.sh:2",
                "tools/drive.py:4",
                "tools/drive.sh:2",
            ],
            "{found:#?}"
        );
    }

    #[test]
    fn only_launcher_sources_are_read() {
        assert!(launcher_source("crates/aterm/tests/x.rs"));
        assert!(launcher_source("crates/aterm-gpu/examples/a/b.rs"));
        assert!(launcher_source(
            "crates/aterm-verify/src/perf/arena/start.rs"
        ));
        assert!(!launcher_source("crates/aterm-gui/src/lib.rs"));
        assert!(!launcher_source("apps/aterm-mac/tests/x.rs"));
    }
}
