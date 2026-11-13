// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Band, Launch, Via, line};
use std::path::{Path, PathBuf};

fn launch(headless: bool) -> Launch {
    Launch {
        aterm: PathBuf::from("/b/aterm"),
        headless,
        args: vec!["--columns".into(), "200".into()],
        env: Vec::new(),
        cwd: PathBuf::from("/w/home"),
        sandbox: None,
        reroute: false,
    }
}

/// `lab instance` starts a window in the app band as a job, as it does a headless instance,
/// and either in the agent band as its child.
#[test]
fn the_app_band_starts_a_job_windowed_or_headless_and_the_agent_band_a_child() {
    for headless in [false, true] {
        let l = launch(headless);
        assert_eq!(Via::of(&l, Band::App), Ok(Via::Job), "headless={headless}");
        assert_eq!(
            Via::of(&l, Band::Agent),
            Ok(Via::Child),
            "headless={headless}"
        );
        let sandboxed = Launch {
            sandbox: Some("(version 1)".into()),
            ..l
        };
        assert!(
            Via::of(&sandboxed, Band::App).is_err(),
            "headless={headless}"
        );
        assert_eq!(Via::of(&sandboxed, Band::Agent), Ok(Via::Child));
    }
}

/// An app-band job's process is its line's program: a windowed instance's keeper, so its
/// aterm, the keeper's child, runs in the job's coalition, and as its own responsible
/// process (`--disclaim`, `tests/band.rs`); a child keeper's aterm is charged as the lab is.
#[test]
fn a_windowed_line_starts_its_keeper_and_a_headless_one_aterm_on_its_lifeline() {
    let (keeper, sock) = (Path::new("/s/aterm-lab"), Path::new("/w/run/a.sock"));
    let tail = [
        "--no-reroute",
        "--control-sock",
        "/w/run/a.sock",
        "--columns",
        "200",
    ];
    let windowed = |keeper_flags: &[&str]| -> Vec<String> {
        ["/s/aterm-lab", "keeper"]
            .iter()
            .chain(keeper_flags)
            .chain(&["--", "/b/aterm", "--window"])
            .chain(&tail)
            .map(|a| (*a).to_string())
            .collect()
    };
    let lifeline = aterm_uds::lifeline::STDIN.to_string();
    let headless: Vec<String> = ["/b/aterm", "--headless", aterm_uds::lifeline::FLAG]
        .into_iter()
        .chain([lifeline.as_str()])
        .chain(tail)
        .map(str::to_string)
        .collect();
    assert_eq!(
        line(&launch(false), Via::Job, keeper, sock),
        windowed(&["--disclaim"])
    );
    assert_eq!(
        line(&launch(false), Via::Child, keeper, sock),
        windowed(&[])
    );
    assert_eq!(line(&launch(true), Via::Job, keeper, sock), headless);
    assert_eq!(line(&launch(true), Via::Child, keeper, sock), headless);
}
