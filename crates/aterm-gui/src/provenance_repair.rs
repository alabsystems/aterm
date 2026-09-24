// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! PROVENANCE OF THE RUNNING APP — measured once per process, logged once, never acted on.
//!
//! macOS decides at `exec` whether a process is provenance-TRACKED, and everything a
//! tracked process or any shell it opens writes carries `com.apple.provenance`
//! (`atpkg::provenance` has the measured rules). This module writes ONE log line saying
//! whether this aterm is tracked and whether the bundle it runs from explains it. It
//! changes nothing: the package store clears its own tags on every pass whatever this
//! process is (`atpkg::provenance::heal_store`), and `aterm pkg doctor` measures on its own.
//!
//! # Why a tracked aterm no longer relaunches itself
//!
//! Until 2026-09-23 a tracked aterm whose bundle read clean asked LaunchServices to relaunch
//! it from that bundle through the update handoff, on the premise that a process launchd
//! starts from a clean image is untracked. Measured on 2026-09-23 (macOS 26.6.2):
//!
//! * The launch lane does not carry the tag. A throwaway ad-hoc-signed `.app`, copied clean
//!   by an untracked launchd job and started as a new instance by a TRACKED process — through
//!   `openApplicationAtURL` as the handoff does, and through `open -n` — wrote untagged files.
//!   So did a tracked instance of such an app relaunching itself after a clean copy had been
//!   swapped in at its own path. Tagged copies of the same apps wrote tagged files.
//!   `crates/aterm-update/tests/provenance_relaunch_probe.rs` repeats this with `open -n`.
//! * aterm does not follow that rule. Pid 2061 (0.91.0) was started by that same lane — a
//!   LaunchServices launch with a launchd job of its own, from `/Applications/aterm.app`
//!   whose root and executable carried no tag — and every file it wrote was tagged, with the
//!   lineage ID of every tagged file since the app's first launch from its downloaded disk
//!   image.
//!
//! So whatever keeps aterm tracked is not in the bundle's attributes, the one relaunch
//! observed came back tracked, and each attempt cost the user a screen freeze and a process
//! swap. The relaunch is retired; the measurement stays, for the log.

/// What the prober measured. Each field is the RAW answer, so "clean" and "could not look"
/// stay distinct in the log line.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProvenanceFacts {
    /// `atpkg::provenance::measure_tracked` — `Some(true)` tracked, `Some(false)` not,
    /// `None` the probe could not be written or read back. Not `process_is_tracked`, which
    /// fails closed to "tracked" and would log a guess as a measurement.
    pub(crate) tracked: Option<bool>,
    /// Neither the bundle ROOT, its executable, nor any file inside it carries
    /// `com.apple.provenance` or `com.apple.quarantine`. `None` when this process does not
    /// run from a `.app` bundle, an attribute list could not be read, or the bundle scan
    /// inspected no file.
    pub(crate) bundle_clean: Option<bool>,
}

/// The one log line for `facts`. Pure, so every shape is pinned by a test.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[must_use]
pub(crate) fn log_line(facts: ProvenanceFacts) -> &'static str {
    match (facts.tracked, facts.bundle_clean) {
        (None, _) => "provenance: could not tell whether this aterm is tracked by macOS",
        (Some(false), _) => {
            "provenance: this aterm is not tracked by macOS, so it does not tag the files its \
             shells write"
        }
        (Some(true), Some(false)) => {
            "provenance: this aterm is tracked by macOS, and its app bundle carries \
             com.apple.provenance or com.apple.quarantine; files its shells write carry the \
             tag, and the package store clears its own"
        }
        (Some(true), Some(true)) => {
            "provenance: this aterm is tracked by macOS although its app bundle is clean; a \
             relaunch was measured not to clear that, so none is attempted; files its shells \
             write carry the tag, and the package store clears its own"
        }
        (Some(true), None) => {
            "provenance: this aterm is tracked by macOS and its app bundle could not be \
             inspected; files its shells write carry the tag, and the package store clears \
             its own"
        }
    }
}

/// Measure this process once, off the event loop, and log [`log_line`]. Every later call is
/// a no-op, so the event loop may call this on every pass.
#[cfg(target_os = "macos")]
pub(crate) fn log_once_in_background() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // A thread that will not spawn logs nothing; nothing depends on the line.
    let _ = std::thread::Builder::new()
        .name("aterm-provenance".to_string())
        .spawn(|| {
            // A probe write, two attribute reads, two walks of the bundle, and a log line
            // nothing waits on.
            crate::qos::set_self(crate::qos::Role::Background);
            aterm_log::info!("{}", log_line(measure()));
        });
}

/// MEASURE this process and the bundle it runs from: one probe file in `$TMPDIR`, two
/// attribute reads, and a walk of the bundle for each of the two attributes.
#[cfg(target_os = "macos")]
#[must_use]
fn measure() -> ProvenanceFacts {
    use atpkg::provenance::{
        PROVENANCE_XATTR, QUARANTINE_XATTR, measure_tracked, tagged_files_under, xattr_names,
    };

    // The probe goes in `$TMPDIR`, never inside the `.app`: a tracked write there would
    // tag the very bundle being measured and break its `codesign --deep` seal.
    let tracked = measure_tracked(&std::env::temp_dir());
    // `xattr_names`, not `carries`: `carries` answers `false` for a path it could not
    // inspect, which would log "could not look" as "clean".
    let clean = |path: &std::path::Path| {
        xattr_names(path).ok().map(|names| {
            !names
                .iter()
                .any(|n| n == PROVENANCE_XATTR || n == QUARANTINE_XATTR)
        })
    };
    let bundle_clean = std::env::current_exe().ok().and_then(|exe| {
        let bundle = crate::app_update_handoff::app_bundle_root(&exe)?;
        // `total > 0` is mandatory: an unreadable directory scans as zero carriers AND zero
        // files, so empty carriers alone would log "could not look" as "clean".
        let scan_clean = |attr| {
            let scan = tagged_files_under(&bundle, attr);
            (scan.total > 0).then_some(scan.carriers.is_empty())
        };
        Some(
            clean(&bundle)?
                && clean(&exe)?
                && scan_clean(PROVENANCE_XATTR)?
                && scan_clean(QUARANTINE_XATTR)?,
        )
    });
    ProvenanceFacts {
        tracked,
        bundle_clean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape gets exactly one line, and a reading that could not be taken is never
    /// logged as a clean one.
    #[test]
    fn every_measurement_is_one_line_that_says_what_was_measured() {
        let line = |tracked, bundle_clean| {
            log_line(ProvenanceFacts {
                tracked,
                bundle_clean,
            })
        };
        for bundle_clean in [None, Some(true), Some(false)] {
            assert!(line(None, bundle_clean).contains("could not tell"));
            assert!(line(Some(false), bundle_clean).contains("not tracked"));
        }
        assert!(line(Some(true), Some(false)).contains("its app bundle carries"));
        assert!(line(Some(true), None).contains("could not be inspected"));
        for bundle_clean in [None, Some(true), Some(false)] {
            for tracked in [None, Some(true), Some(false)] {
                let text = line(tracked, bundle_clean);
                assert!(text.starts_with("provenance: "), "{text}");
                assert!(!text.contains('\n'), "one line: {text}");
            }
        }
    }

    /// THE SHAPE THAT USED TO RELAUNCH THE APP — tracked, from a clean bundle — now yields
    /// a log line and nothing else. The handoff has no same-image authority left but the
    /// QA seam, so no other path can ask for that relaunch either.
    #[test]
    fn a_tracked_process_from_a_clean_bundle_is_logged_and_not_relaunched() {
        let line = log_line(ProvenanceFacts {
            tracked: Some(true),
            bundle_clean: Some(true),
        });
        assert!(line.contains("although its app bundle is clean"), "{line}");
        assert!(line.contains("none is attempted"), "{line}");
        // An irrefutable pattern over the one remaining variant: a same-image handoff is
        // the QA seam or nothing, and a relaunch variant added back stops this compiling.
        let crate::app_update_handoff::SameImageHandoff::DebugSeam =
            crate::app_update_handoff::SameImageHandoff::DebugSeam;
    }
}
