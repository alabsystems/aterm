// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::run;
use super::*;

fn launches(path: &str, src: &str) -> Vec<String> {
    lifeline_findings(&SourceFile::outside(path.to_string(), src))
        .into_iter()
        .map(|f| format!(":{}:{}", f.line, f.detail))
        .collect()
}

#[test]
fn a_scope_that_misses_the_one_boot_is_a_finding() {
    let lost =
        |found: &[Finding]| found.len() == 1 && found[0].detail.starts_with("read no launch in ");
    assert!(lost(&run(&HEADLESS_LIFELINE, &[])));

    let armed = "fn boot(root: &Path) {\n    cmd.arg(\"--headless\");\n    \
                 let l = launch_isolation::lifeline(&mut cmd, root);\n}\n";
    let elsewhere = run(&HEADLESS_LIFELINE, &[("crates/aterm/tests/t.rs", armed)]);
    assert!(lost(&elsewhere), "{elsewhere:?}");
    assert!(run(&HEADLESS_LIFELINE, &[(ANCHOR_LAUNCH_FILE, armed)]).is_empty());
}

#[test]
fn every_unarmed_launch_is_found() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "an array trustfmt broke",
            "fn launch(root: &Path) -> Child {\n    let mut cmd = Command::new(env!(\"CARGO_BIN_EXE_aterm\"));\n    cmd.args([\n        \"--no-reroute\",\n        \"--headless\",\n        \"--control-sock\",\n    ])\n    .stdin(Stdio::null());\n    cmd.spawn().expect(\"launch\")\n}\n",
            ":5:unarmed rust launch (1 --headless or link broker launch(es), 0 lifeline arm call(s)",
        ),
        (
            "a broker started on one line",
            "fn serve(sock: &str, log: &str) -> Child {\n    let mut cmd = aterm_uds::spawnfd::command(ATERM);\n    cmd.args([\"link\", \"broker\", sock, log]).stdin(Stdio::null());\n    cmd.spawn().expect(\"broker\")\n}\n",
            ":3:unarmed rust launch (1 --headless or link broker launch(es), 0 lifeline arm call(s)",
        ),
        (
            "a broker array trustfmt broke",
            "fn serve(sock: &str) -> Child {\n    Command::new(ATERM)\n        .args([\n            \"link\",\n            \"broker\",\n            sock,\n        ])\n        .spawn()\n        .expect(\"broker\")\n}\n",
            ":5:unarmed rust launch (1 --headless or link broker launch(es), 0 lifeline arm call(s)",
        ),
        (
            "a script inside a Rust string",
            "#[test]\nfn a_driver_script() {\n    let script = r#\"\nset -u\nB=$1 S=$2/s.sock\n\"$B\" --headless --control-sock \"$S\" </dev/null &\n\"#;\n}\n",
            ":6:unarmed shell launch",
        ),
        (
            "a second launch beside one arm",
            "fn first(root: &Path) -> (Child, Lifeline) {\n    cmd.args([\"--headless\", \"--no-reroute\"]);\n    let lifeline = launch_isolation::lifeline(&mut cmd, root);\n}\n\nfn second(root: &Path) -> Child {\n    // launch_isolation::lifeline(&mut cmd, root);\n    cmd.arg(\"--headless\").stdin(Stdio::null());\n}\n",
            ":8:unarmed rust launch (2 --headless or link broker launch(es), 1 lifeline arm call(s)",
        ),
        (
            "a test whose Drop is its only teardown",
            "fn headless(tmp: &Path) -> Command {\n    let mut cmd = Command::new(env!(\"CARGO_BIN_EXE_aterm\"));\n    cmd.args([\"--headless\", launch_isolation::NO_REROUTE])\n        .stdin(Stdio::null());\n    cmd\n}\n",
            ":3:unarmed rust launch (1 --headless or link broker launch(es), 0 lifeline arm call(s)",
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
                 fn broken_array(root: &Path) -> (Child, Lifeline) {\n    cmd.args([\n        \"--headless\",\n        \"--control-sock\",\n    ]);\n    let lifeline = launch_isolation::lifeline(&mut cmd, root);\n}\n\
                 fn spawn_broker(dir: &Path, args: &[&str]) -> BrokerChild {\n    cmd.args([\"link\", \"broker\"]).args(args);\n    let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut cmd, dir).expect(\"arm\");\n}\n";
    let found = launches("crates/aterm/tests/a.rs", armed);
    assert!(
        found.is_empty(),
        "an armed launch read as unarmed: {found:?}"
    );

    let names_only = "fn boot() {\n    eprintln!(\"SKIP: cannot launch aterm --headless\");\n    // cmd.arg(\"--headless\");\n}\n\
                      fn is_the_daemon(line: &str) -> bool {\n    line.contains(\"aterm-gui --headless\") && !line.contains(\"--help\")\n}\n\
                      fn fake(command: &Command) {\n    for flag in [\"--headless\", \"--cpu\"] {\n        assert!(args.iter().any(|a| a == flag));\n    }\n}\n\
                      fn a_client_then_a_check(sock: &Path) {\n    ctl.args([\n        \"ctl\",\n        \"status\",\n    ]);\n    let flags = [\n        \"--headless\",\n        \"--no-reroute\",\n    ];\n}\n\
                      const SCRIPT: &str = r#\"\nF=$1 S=$2/s.sock\n# NO LIFELINE on purpose: \"$F\" is a stand-in that is no aterm.\n\"$F\" --headless --control-sock \"$S\" &\n\"#;\n\
                      const ARMED: &str = r#\"\n\"$A\" --headless --lifeline-fd 0 &\n\"$A\" --headless \\\n    --lifeline-fd 3 &\n\"$A\" --headless --help\n\"#;\n\
                      const USAGE: &str = \"usage: aterm [--cpu] [--headless]\";\n\
                      fn a_job_and_a_report(v: &Value) {\n    job(&[\n        BIN,\n        \"link\",\n        \"broker\",\n    ]);\n    let guarded = v[\"broker\"][\"guarded\"].as_bool();\n    let ls = [\"link\", \"ls\"];\n}\n\
                      fn broker_run(args: &[&str]) {\n    // NO LIFELINE on purpose: every argv here ends the broker before it serves\n    cmd.args([\"link\", \"broker\"]).args(args);\n}\n";
    let found = launches("crates/aterm/tests/b.rs", names_only);
    assert!(found.is_empty(), "CRIES WOLF: {found:?}");
}

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
    assert!(
        launcher_source("crates/aterm-lab/src/codex_alab_proof.rs"),
        "the lab is a dev tool that starts instances and a broker"
    );
    assert!(!launcher_source("crates/aterm-gui/src/lib.rs"));
    assert!(!launcher_source("apps/aterm-mac/tests/x.rs"));
}
