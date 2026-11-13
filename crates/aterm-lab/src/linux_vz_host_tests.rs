// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_build_is_the_helpers_bin_in_the_dev_profile_into_its_own_target_dir() {
    let args = host_build_args(Path::new("/src/aterm"));
    let text: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        text,
        [
            "--unverified",
            "build",
            "--locked",
            "--offline",
            "-p",
            "aterm-vz",
            "--bin",
            "aterm-vz-host",
            "--config",
            "profile.dev.debug=0",
            "--config",
            "profile.dev.strip=\"debuginfo\"",
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
            "/src/aterm/Cargo.toml",
            "--target-dir",
            "/src/aterm/target/.ubd/vz-host.noindex",
        ]
    );
    assert!(
        !text.iter().any(|a| a == "--release"),
        "debug assertions check every send"
    );
}

#[test]
fn the_executable_is_read_from_the_bins_artifact_line_only() {
    let lines = [
        r#"{"reason":"compiler-artifact","target":{"name":"aterm_vz","kind":["lib"]},"filenames":["/t/libaterm_vz.rlib"],"executable":null}"#,
        r#"{"reason":"compiler-artifact","target":{"name":"aterm-vz-host","kind":["bin"]},"filenames":["/t/debug/aterm-vz-host"],"executable":"/t/debug/aterm-vz-host"}"#,
        r#"{"reason":"build-finished","success":true}"#,
    ]
    .join("\n");
    assert_eq!(
        executable_of(&lines),
        Some(PathBuf::from("/t/debug/aterm-vz-host"))
    );
    assert_eq!(
        executable_of(&lines.replace("aterm-vz-host", "other")),
        None
    );
    assert_eq!(executable_of("not json"), None);
}

#[test]
fn the_virtualization_gate_reads_kern_hv_support_in_the_rows_words() {
    assert!(virtualization_gate(Some(true)).is_ok());
    for (hv, words) in [
        (
            Some(false),
            "COULD NOT RUN: this host lacks Apple silicon with Virtualization.framework, and \
             no command adds it (kern.hv_support is 0)",
        ),
        (
            None,
            "COULD NOT RUN: kern.hv_support did not read, so whether this host has Apple \
             silicon with Virtualization.framework is unknown",
        ),
    ] {
        let fail = virtualization_gate(hv).expect_err("gated");
        assert_eq!((fail.code, fail.msg.as_deref()), (NOT_RUN, Some(words)));
    }
}

#[test]
fn a_floors_words_and_a_lost_guests_say_not_run_once() {
    let floor = "NOT RUN — the volume holding aterm-linux.img, /v, has 19.0 GiB free";
    let fail = layer_fail(aterm_vz::Error::Floor(floor.into()));
    assert_eq!((fail.code, fail.msg.as_deref()), (NOT_RUN, Some(floor)));
    let lost = "NOT RUN — the boot hung before the agent";
    let fail = client_fail(
        ClientError::Lost {
            why: lost.into(),
            tail: String::new(),
        },
        Path::new(""),
        None,
    );
    assert_eq!(fail.msg.as_deref(), Some(lost));
    // The negative control: words without it gain it.
    let fail = client_fail(
        ClientError::Lost {
            why: "the VM was stopped by a Stop (down)".into(),
            tail: String::new(),
        },
        Path::new(""),
        None,
    );
    assert_eq!(
        fail.msg.as_deref(),
        Some("NOT RUN — the VM was stopped by a Stop (down)")
    );
}
