// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for an isolated instance's log footprint and its refused state root.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::headless_boot::{self, Instance};

fn boot(tag: &str, state: Option<&str>) -> Instance {
    headless_boot::boot_with(
        &format!("atht{tag}"),
        |tmp, cmd| {
            cmd.env("ATERM_LOG", "info")
                .args(["--lines", "24", "--columns", "80"]);
            if let Some(state) = state {
                cmd.env("ATERM_STATE_HOME", tmp.join(state));
            }
        },
        |_| true,
    )
}

fn home_log(tmp: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        tmp.join("home/Library/Logs/aterm/aterm.log")
    } else {
        tmp.join("state/aterm/logs/aterm.log")
    }
}

#[test]
fn an_isolated_state_root_keeps_the_log_and_the_default_does_not() {
    let isolated = boot("s", Some("own-state"));
    assert!(
        isolated.tmp.join("own-state/logs/aterm.log").exists(),
        "the instance's log is under its own state root"
    );
    assert!(
        !home_log(&isolated.tmp).exists() && !home_log(&isolated.tmp).parent().unwrap().exists(),
        "nothing is written to the person's log directory"
    );
    drop(isolated);
    let plain = boot("p", None);
    assert!(home_log(&plain.tmp).exists(), "the default log directory");
}

/// A relative state root is refused on stderr and writes nothing under `HOME`.
#[test]
fn a_refused_state_root_is_said_not_silent() {
    let inst = headless_boot::boot_with(
        "athtr",
        |_, cmd| {
            cmd.env("ATERM_STATE_HOME", "relative-state").args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let stderr = std::fs::read_to_string(&inst.log).unwrap_or_default();
    assert!(stderr.contains("ATERM_STATE_HOME"), "{stderr}");
    assert!(
        !home_log(&inst.tmp).exists(),
        "and no log went to HOME instead"
    );
}
