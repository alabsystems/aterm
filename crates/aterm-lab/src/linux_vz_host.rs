// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vz lane's helper in the lab (docs/DESIGN-vz-guest-2026-10-23.md §5.1, slice V2):
//! [`build_host`] builds `aterm-vz-host` with the `targo` that built the lab, and
//! [`place_host`] copies it to `vz.noindex/host/<sha256>/` and signs the copy there ad hoc
//! with `crates/aterm-vz/aterm-vz-host.entitlements` (`com.apple.security.virtualization`
//! only), never in place in `target/`, then reclaims the copies no helper needs.
//!
//! V2's `vz-host up|exec|status|down` measure verbs are gone: the lane's own verbs drive
//! the VM under `LAB_LINUX_DRIVER=vz` ([`crate::linux_vz_driver`]), which builds and places
//! the helper with these.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_uds::spawnfd::Stdio;
use aterm_vz::client::{ClientError, failed_words, not_run_words};
use aterm_vz::record::{Paths, Record};

use crate::linux::{NOT_RUN, not_run};
use crate::pyjson::J;
use crate::{Fail, LAB, Res};

/// The helper's package and executable.
pub const HOST: &str = aterm_vz::sign::EXE;

/// The helper's own target directory in the checkout at `repo`, apart from targo's.
#[must_use]
pub fn target_dir(repo: &Path) -> PathBuf {
    repo.join("target").join("vz-host.noindex")
}

/// The dev profile's changes for the helper's build: no debug info, and none left from the
/// standard library. The dev profile's unpacked debug info leaves stabs naming each object
/// file and source by its absolute path (853 under one worktree's `target/`, V4's review),
/// so the bytes, and the content address §5.1 compares, differed per checkout, and a session
/// from another worktree replaced the VM where §7 swaps the share. Stripping alone is not
/// enough: the linker's `LC_UUID` hashes the stabs before the strip, so two target
/// directories still built two addresses (V4, measured); with no debug info the linker sees
/// no checkout path at all.
pub const HOST_PROFILE: [&str; 2] = ["profile.dev.debug=0", "profile.dev.strip=\"debuginfo\""];

/// `targo`'s arguments for the helper's build in the checkout at `repo`: the dev profile,
/// whose debug assertions check every hand-written send's prototype against the
/// Objective-C runtime (`checked_msg`) at each boot, with no debug info ([`HOST_PROFILE`])
/// so the same sources build the same bytes in any checkout.
#[must_use]
pub fn host_build_args(repo: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--unverified",
        "build",
        "--locked",
        "--offline",
        "-p",
        "aterm-vz",
        "--bin",
        HOST,
        "--config",
        HOST_PROFILE[0],
        "--config",
        HOST_PROFILE[1],
        "--message-format",
        "json-render-diagnostics",
        "--manifest-path",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(repo.join("Cargo.toml").into_os_string());
    args.push("--target-dir".into());
    args.push(target_dir(repo).into_os_string());
    args
}

/// The helper's executable, as the build's artifact line names it.
#[must_use]
pub fn executable_of(json_lines: &str) -> Option<PathBuf> {
    json_lines.lines().find_map(|line| {
        let j = J::parse(line).ok()?;
        let target = j.get("target")?;
        let ours = j.get("reason")?.as_str() == Some("compiler-artifact")
            && target.get("name")?.as_str() == Some(HOST)
            && target
                .get("kind")?
                .as_arr()?
                .iter()
                .any(|k| k.as_str() == Some("bin"));
        if !ours {
            return None;
        }
        j.get("executable")?.as_str().map(PathBuf::from)
    })
}

/// Build the helper from the checkout at `repo` with the `targo` that built the lab.
///
/// # Errors
/// No `targo` is known, the build failed, or it named no executable.
pub fn build_host(repo: &Path) -> Result<PathBuf, Fail> {
    let targo = option_env!("CARGO")
        .ok_or_else(|| Fail::die("vz-host: the lab was built by no cargo it can name"))?;
    let t0 = Instant::now();
    let out = aterm_uds::spawnfd::command(targo)
        .args(host_build_args(repo))
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Fail::die(format!("vz-host: cannot run {targo}: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(12).collect();
        return Err(Fail::die(format!(
            "vz-host: the helper did not build ({targo} --unverified build -p aterm-vz --bin {HOST}):\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )));
    }
    let built = executable_of(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| Fail::die("vz-host: the helper's build named no executable"))?;
    println!(
        "vz-host: built {} in {:.1} s",
        built.display(),
        t0.elapsed().as_secs_f64()
    );
    Ok(built)
}

/// Copy `built` to `vz/host/<sha256>/` and sign it there ([`aterm_vz::sign::place`],
/// [`aterm_vz::sign::codesign`]), then reclaim every other copy no helper needs: the one the
/// record at `paths` names is kept, so is one whose helper was started and has not yet proved
/// it ran, and so is one a live helper of any state directory runs from (it holds the copy's
/// `ran` marker, [`aterm_vz::sign::InUse`]).
///
/// # Errors
/// The copy or its signing failed.
pub fn place_host(built: &Path, vz: &Path, paths: &Paths) -> Result<aterm_vz::sign::Placed, Fail> {
    let host = vz.join("host");
    let placed = aterm_vz::sign::place(built, &host, &mut |copy, ents| {
        aterm_vz::sign::codesign(copy, ents)
    })
    .map_err(|why| Fail::die(format!("vz-host: {why}")))?;
    println!(
        "vz-host: {} ({})",
        placed.exe.display(),
        if placed.fresh {
            "signed and placed"
        } else {
            "already there"
        }
    );
    let record = Record::read(&paths.record());
    for gone in aterm_vz::sign::reclaim(&host, &placed.sha256, record.as_ref()) {
        println!("vz-host: reclaimed {}", gone.display());
    }
    Ok(placed)
}

/// What a client error ends the run with: the helper `exe`'s refusals in the lab's words.
pub(crate) fn client_fail(
    e: ClientError,
    exe: &Path,
    lab_replaced: Option<&aterm_vz::client::Replaced>,
) -> Fail {
    match e {
        ClientError::Failed { error, tail } => {
            print_tail(&tail);
            Fail::exit(NOT_RUN, failed_words(&error, exe, lab_replaced))
        }
        ClientError::Lost { why, tail } => {
            print_tail(&tail);
            Fail::exit(NOT_RUN, not_run_words(&why))
        }
        ClientError::Hung(words) => Fail::exit(NOT_RUN, words),
        ClientError::NoHelper(why) => Fail::exit(
            NOT_RUN,
            not_run_words(&format!("{why} ({LAB} linux vz-host up first)")),
        ),
        other => Fail::die(format!("vz-host: {other}")),
    }
}

fn print_tail(tail: &str) {
    if !tail.is_empty() {
        println!("vz-host: the console's last lines:");
        for line in tail.lines() {
            println!("  | {line}");
        }
    }
}

pub(crate) fn layer_fail(e: aterm_vz::Error) -> Fail {
    match e {
        // The floor's words are NOT RUN already (`space::below_bring_up`).
        aterm_vz::Error::Floor(why) => Fail::exit(NOT_RUN, not_run_words(&why)),
        aterm_vz::Error::Input(why) => not_run(why),
        aterm_vz::Error::Io(why) => Fail::die(format!("vz-host: {why}")),
    }
}

/// The `virtualization` hardware row's gate (§13), read as `kern.hv_support` by sysctl with
/// no framework loaded ([`aterm_sysprobe::hv_support`]): COULD NOT RUN in the row's words
/// where this Mac has no hypervisor, or the read failed.
pub(crate) fn virtualization_gate(hv_support: Option<bool>) -> Result<(), Fail> {
    let row = aterm_uds::host::tool("virtualization");
    match hv_support {
        Some(true) => Ok(()),
        Some(false) => Err(Fail::exit(
            NOT_RUN,
            format!("COULD NOT RUN: {} (kern.hv_support is 0)", row.refusal()),
        )),
        None => Err(Fail::exit(
            NOT_RUN,
            format!(
                "COULD NOT RUN: kern.hv_support did not read, so whether this host has {} is \
                 unknown",
                row.needs_label()
            ),
        )),
    }
}

/// `linux vz-host …`.
///
/// # Errors
/// A usage error, or a step that failed.
pub fn cmd_vz_host(rest: &[String]) -> Res {
    match rest {
        [] => {}
        [verb] if verb == "place" => {}
        [verb, ..] if matches!(verb.as_str(), "up" | "exec" | "status" | "down") => {
            return Err(Fail::usage(format!(
                "linux vz-host {verb}: V2's measure verb is the lane's own now: \
                 `LAB_LINUX_DRIVER=vz {LAB} linux {verb}` ({LAB} linux help)"
            )));
        }
        other => {
            return Err(Fail::usage(format!(
                "linux vz-host takes only `place`, not {other:?} ({LAB} linux help)"
            )));
        }
    }
    let repo = crate::paths::physical(&crate::paths::paths().repo);
    let vz = crate::linux_vz_driver::root()?;
    let built = build_host(&repo)?;
    let placed = place_host(&built, &vz, &Paths::at(&vz))?;
    println!("vz-host: sha256 {}", placed.sha256);
    Ok(0)
}

#[cfg(test)]
mod tests {
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
                "/src/aterm/target/vz-host.noindex",
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
}
