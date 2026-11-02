// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's judged wait ([`crate::harness::recv_judged`]), its fixture config
//! ([`crate::harness::prepare_fixture_config`]), its own images ([`crate::harness::own_image`])
//! and its `aterm` children's environment ([`crate::harness::kept_from_runner`]), checked;
//! mounted only by the ordinary fabric_world test target.

use std::time::{Duration, Instant};

use crate::harness::recv_judged;

/// A wait judged at 50 ms (the timeout branch under test, not a budget) ends COULD NOT RUN naming
/// the pid when the reader says the child is held, fails naming what never came otherwise, and
/// returns an item that came.
#[test]
fn a_wait_that_runs_out_on_a_held_child_ends_could_not_run_and_any_other_silence_fails() {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let pid = 4242;
    let hang = Duration::from_millis(50);
    let ended = |held: Option<bool>| {
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recv_judged(
                &rx,
                Instant::now(),
                hang,
                pid,
                |p| held.filter(|_| p == pid),
                || "the child's readiness line".to_owned(),
            )
        }));
        let why = out.expect_err("nothing came");
        why.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let held = ended(Some(true));
    assert!(
        held.starts_with(aterm_uds::host::COULD_NOT_RUN) && held.contains("(pid 4242)"),
        "expected COULD NOT RUN, got: {held}"
    );
    for not in [Some(false), None] {
        let failed = ended(not);
        assert!(
            failed.starts_with("the child's readiness line"),
            "{not:?}: {failed}"
        );
    }
    tx.send("listening".into()).expect("the channel is open");
    assert_eq!(
        recv_judged(&rx, Instant::now(), hang, pid, |_| Some(true), String::new),
        "listening"
    );
}

/// THE FABRIC WORLDS DECLINE INSTALLS, so a spawn in one runs the world's stand-in and never
/// starts a download of the agent CLI: the config every fixture seeds
/// ([`crate::harness::prepare_fixture_config`]) says no to unattended installs by atpkg's own
/// reading ([`atpkg::adoption::unattended_install_off`]), which a spawn's install obeys.
#[test]
fn the_fixture_config_declines_installs() {
    let root = aterm_tempfile::Builder::new()
        .prefix("atl-consent-")
        .tempdir()
        .expect("scratch root");
    crate::harness::prepare_fixture_config(root.path());
    let written = std::fs::read_to_string(root.path().join("cfg/aterm/aterm.toml"))
        .expect("the fixture config");
    assert_eq!(written, crate::harness::FIXTURE_GUI_CONFIG);
    let store = atpkg::store::Layout {
        prefix: root.path().join("pkg"),
    };
    assert_eq!(
        atpkg::adoption::unattended_install_off(&store, &atpkg::config::parse_packages(&written)),
        Some(atpkg::adoption::UPDATES_OFF),
        "{written}"
    );
}

/// **AN OWN IMAGE GOES ONLY WITH ITS FOLDER.** What [`crate::harness::own_image`] hands back
/// removes nothing: its copy, once it wrote its line, stays as long as the folder it was made in
/// (here a world's scratch's stand-in, kept if the copy never writes its line), so a session can
/// run it until its world goes. NEGATIVE CONTROL: the copy is under that folder and goes with it,
/// so no copy outlives its world.
#[test]
fn an_own_image_goes_only_with_its_folder() {
    let mut dir = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("atl-own-image-")
            .tempdir()
            .expect("scratch root")
            .keep(),
    );
    let image = crate::harness::own_image(dir.path(), "qzown");
    dir.ran();
    assert!(
        image.is_file(),
        "own_image's copy went before its folder: {}",
        image.display()
    );
    let root = dir.path().to_path_buf();
    drop(dir);
    assert!(
        !root.exists() && !image.exists(),
        "NEGATIVE CONTROL: the copy outlived its folder: {}",
        image.display()
    );
}

/// Set on the run of this test binary that
/// [`the_harness_s_aterm_children_go_without_the_runner_s_aterm_settings_but_the_mark`] checks
/// the commands in.
const RUNNER_SETTINGS: &str = "FABRIC_WORLD_RUNNER_SETTINGS";

/// The line that run writes once every command it built passed.
const CHILDREN_KEPT: &str = "the harness's aterm children are kept from the runner's settings";

/// **THE HARNESS'S `aterm` CHILDREN GO WITHOUT THE RUNNER'S `ATERM_*` SETTINGS BUT THE VERIFY
/// RUN'S MARK** ([`crate::harness::kept_from_runner`]): a broker
/// ([`crate::harness::broker_command`]) and an owner world's `aterm <args>`
/// ([`crate::owner_ask::cli_command`]) remove each marker the runner carries but the mark, which
/// they leave as the runner has it. The commands are built in this test binary run again with
/// those settings in its environment, so the check never rests on the terminal it runs from.
/// NEGATIVE CONTROLS: a broker built with no strip keeps `ATERM_CHILD`, and `cli`'s strip of
/// every `ATERM_*` removes the mark.
#[test]
fn the_harness_s_aterm_children_go_without_the_runner_s_aterm_settings_but_the_mark() {
    use aterm_uds::process::{TRUST_CHECK_VAR, VERIFY_RUN_VAR};
    const SETTINGS: [&str; 3] = ["ATERM_CHILD", "ATERM_PARENT_SESSION_ID", TRUST_CHECK_VAR];
    if std::env::var_os(RUNNER_SETTINGS).is_some() {
        let built = [
            ("a broker", crate::harness::broker_command("aterm", &[])),
            (
                "an owner world's cli",
                crate::owner_ask::cli_command("aterm", std::path::Path::new("/oa-root"), &[]),
            ),
        ];
        for (what, cmd) in &built {
            let edits: Vec<_> = cmd.get_envs().collect();
            for name in SETTINGS {
                assert!(
                    edits.contains(&(std::ffi::OsStr::new(name), None)),
                    "{what} keeps the runner's {name}: {edits:?}"
                );
            }
            assert!(
                !edits.iter().any(|(name, _)| *name == VERIFY_RUN_VAR),
                "{what} changes the verify run's mark: {edits:?}"
            );
        }
        println!("{CHILDREN_KEPT}");
        return;
    }
    let mark = std::env::var_os(VERIFY_RUN_VAR).unwrap_or_else(|| "a-run".into());
    let mut run = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
    run.args([
        "--exact",
        "harness_tests::the_harness_s_aterm_children_go_without_the_runner_s_aterm_settings_but_the_mark",
        "--nocapture",
        "--quiet",
        "--test-threads=1",
    ])
    .env(RUNNER_SETTINGS, "1")
    .env(VERIFY_RUN_VAR, mark)
    .stdin(aterm_uds::spawnfd::Stdio::null());
    for name in SETTINGS {
        run.env(name, "the-runner's");
    }
    let out = aterm_uds::deadline::output_until_complete_stdout(
        &mut run,
        Instant::now() + crate::harness::DEADLINE,
    )
    .unwrap_or_else(|e| panic!("the check's run exits within the hang detector: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.lines().any(|line| line == CHILDREN_KEPT),
        "the check's run ({}): {stdout}{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
