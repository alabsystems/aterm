// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Earliest private development-window guard adoption. Debug and explicit
//! `dev-seams` builds retain it; ordinary releases refuse it. Arguments and descriptor
//! numbers mint no budget or Timing/cohort authority. Actual descriptions retain only
//! the original lane and optional Functional memory reservation already admitted.

use std::ffi::OsString;

pub(super) struct Guard {
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    held: Option<aterm_toolchain::record_lock::CloseOnlyLock>,
    #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
    _memory: Option<aterm_capacity::RetainedClaim>,
}

impl Guard {
    pub(super) fn is_present(&self) -> bool {
        #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
        {
            self.held.is_some()
        }
        #[cfg(not(all(unix, any(debug_assertions, feature = "dev-seams"))))]
        {
            false
        }
    }
}

fn window_context(argv: &[OsString]) -> Result<(), String> {
    let Some((program, rest)) = argv.split_first() else {
        return Err("lab window guard requires the actual aterm window entry".into());
    };
    if std::path::Path::new(program)
        .file_name()
        .is_none_or(|name| name != "aterm")
    {
        return Err("lab window guard is unavailable through an executable alias".into());
    }
    let scan = &rest[..super::payload_boundary(rest)];
    if !scan.iter().any(|word| word == "--window")
        || scan
            .first()
            .is_none_or(|word| !word.as_encoded_bytes().starts_with(b"-"))
        || scan.iter().any(|word| {
            matches!(
                word.to_str(),
                Some(
                    "--headless"
                        | "--session"
                        | "--diagnose"
                        | "--help"
                        | "-h"
                        | "--version"
                        | "-V"
                        | "--list-fonts"
                        | "--no-control-sock"
                )
            )
        })
    {
        return Err(
            "lab window guard requires an explicit window, not a headless/session/command entry"
                .into(),
        );
    }
    let mut sockets = scan
        .windows(2)
        .filter(|words| words[0] == "--control-sock")
        .map(|words| &words[1]);
    let Some(socket) = sockets.next().filter(|_| sockets.next().is_none()) else {
        return Err("lab window guard requires one enabled control socket".into());
    };
    if socket == "0"
        || socket
            .to_str()
            .is_some_and(|socket| socket.eq_ignore_ascii_case("off"))
        || (!socket.is_empty() && !std::path::Path::new(socket).is_absolute())
    {
        return Err("lab window guard requires one enabled control socket".into());
    }
    Ok(())
}

pub(super) fn enter(argv: &mut Vec<OsString>) -> Result<Guard, String> {
    let offered = aterm_types::lab_window_guard::take(argv)?;
    if let Some(offered) = offered {
        window_context(argv)?;
        #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
        {
            let ledger = aterm_capacity::Ledger::user()
                .ok_or("lab window guard has no account capacity directory")?;
            aterm_uds::private_file::check_private_dir(ledger.dir())
                .map_err(|error| format!("lab window guard account directory: {error}"))?;
            let parent = ledger
                .dir()
                .canonicalize()
                .map_err(|error| format!("lab window guard canonical directory: {error}"))?;
            let expected = parent.join(aterm_types::lab_window_guard::LOCK_NAME);
            let ledger = aterm_capacity::Ledger::at(parent);
            // SAFETY: this is the first entry operation, before threads, descriptor reuse or
            // child spawning. The explicit native lab/keeper launch continuously retains its
            // genuine close-only description through spawn and adoption; the independently
            // resolved account lane is not supplied by argv/HOME markers. The adopter checks
            // the exact descriptor/physical lock and consumes that original slot only after
            // successful validation, returning a close-on-exec opaque guard retained by main.
            #[allow(unsafe_code)]
            let held = unsafe {
                aterm_toolchain::record_lock::CloseOnlyLock::adopt_inherited(
                    offered.lane,
                    &expected,
                )
            }
            .map_err(|error| format!("lab window guard adoption: {error}"))?;
            let memory = match offered.memory {
                Some(reference) => {
                    // SAFETY: same earliest controlled handoff as the lane above. The
                    // original Functional claim has already been consumed into close-only
                    // custody and remains continuously held by the sender. Only the actual
                    // inherited description plus its exact Detached record under this
                    // independently selected ledger may be retained; no argv budget, PID
                    // credit, cohort writer or new admission is accepted here.
                    #[allow(unsafe_code)]
                    let memory = unsafe {
                        aterm_capacity::RetainedClaim::adopt_inherited(
                            reference.descriptor,
                            &ledger,
                            &reference.claim,
                        )
                    }
                    .map_err(|error| format!("lab window memory guard adoption: {error}"))?;
                    Some(memory)
                }
                None => None,
            };
            return Ok(Guard {
                held: Some(held),
                _memory: memory,
            });
        }
        #[cfg(not(all(unix, any(debug_assertions, feature = "dev-seams"))))]
        {
            let _ = offered;
            return Err(
                "lab window guard requires a Unix debug or explicit dev-seams build".into(),
            );
        }
    }
    Ok(Guard {
        #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
        held: None,
        #[cfg(all(unix, any(debug_assertions, feature = "dev-seams")))]
        _memory: None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn words(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn development_guard_adoption_requires_an_explicit_private_window() {
        let good = words(&[
            "/private/build/aterm",
            "--window",
            "--control-sock",
            "/private/lab.sock",
            "--columns",
            "120",
        ]);
        assert!(window_context(&good).is_ok());
        assert!(
            window_context(&words(&["aterm", "--window", "--control-sock", ""])).is_ok(),
            "ordinary private windows retain their default-socket Commons route"
        );
        for bad in [
            words(&["aterm", "--control-sock", "/private/lab.sock"]),
            words(&[
                "aterm-cli",
                "--window",
                "--control-sock",
                "/private/lab.sock",
            ]),
            words(&[
                "aterm-gui",
                "--window",
                "--control-sock",
                "/private/lab.sock",
            ]),
            words(&[
                "aterm",
                "ctl",
                "--window",
                "--control-sock",
                "/private/lab.sock",
            ]),
            words(&["aterm", "--window"]),
            words(&["aterm", "--window", "--control-sock", "off"]),
            words(&["aterm", "--window", "--control-sock", "OFF"]),
            words(&["aterm", "--window", "--control-sock", "Off"]),
            words(&["aterm", "--control-sock", "--window"]),
            words(&["aterm", "--window", "--control-sock", "relative/socket"]),
            words(&[
                "aterm",
                "--window",
                "--control-sock",
                "/private/a",
                "--control-sock",
                "/private/b",
            ]),
        ] {
            assert!(window_context(&bad).is_err(), "{bad:?}");
        }
        for forbidden in [
            "--session",
            "--headless",
            "--diagnose",
            "--help",
            "--version",
            "--no-control-sock",
        ] {
            let mut bad = good.clone();
            bad.push(forbidden.into());
            assert!(window_context(&bad).is_err(), "{bad:?}");
        }
    }
}
