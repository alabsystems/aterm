// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! PROVENANCE SELF-REPAIR — replacing this process, at most once, when it is
//! provenance-TRACKED while the bundle it runs from is CLEAN.
//!
//! # Why a running process can be stale in a way nothing else fixes
//!
//! macOS decides whether a process is provenance-tracked at `exec`, from two things: the
//! executable image, and the parent. The verdict never changes afterwards, and everything
//! a tracked process writes is tagged — including every object file a build in one of its
//! shells produces, which is what silently refuses a release cut after the ledger claim
//! (`atpkg::provenance`, and the burned build number of v0.83.0).
//!
//! So an aterm that launched from a tagged bundle stays tracked for its whole life, even
//! after the bundle on disk has been repaired. Nothing in the process can revoke it:
//! the tag survives `exec` into a clean image, so re-execing ourselves changes nothing
//! (measured — `atpkg::provenance` carries the table). The ONE escape is a process
//! launchd mints from a clean image, which is exactly what the seamless update handoff
//! already builds: `HandoffLane::OutOfBand` hands the bundle to LaunchServices and the
//! successor is launchd's child with its own application job, while the user's windows
//! and PTYs are adopted across the swap.
//!
//! This module is the TRIGGER that lane never had. It answers one question — may this
//! process ask to be replaced by itself? — and it answers it by MEASUREMENT, never by
//! inference.
//!
//! # Why the answer is a refusal by default
//!
//! A repair is a real process replacement: a freeze, a park, an adoption proof, and a
//! Commit. The user asked for none of it and can see no benefit when it works. So every
//! reading that is not a clean, positive measurement refuses, and a refusal is permanent
//! for this process — there is no retry, no backoff and no second attempt. The cost of
//! refusing wrongly is that a person quits and reopens their terminal one day; the cost
//! of accepting wrongly is a process replacement nobody wanted.
//!
//! Three of the refusals exist because a predicate that looks obvious is inverted here:
//!
//! * `atpkg::provenance::process_is_tracked` fails CLOSED — an unmeasurable probe answers
//!   "tracked", which is right for "route this write through the untracked lane" and
//!   exactly wrong for "replace the user's process". [`RepairFacts::tracked`] therefore
//!   carries the raw `measure_tracked` answer, and `None` refuses.
//! * `carries` answers `false` for a path it could not inspect, so "clean" and "could not
//!   look" are the same value. The prober uses `xattr_names` and refuses on `Err`.
//! * `tagged_files_in`/`tagged_files_under` answer `Scan::default()` — zero carriers, and
//!   zero TOTAL — for a directory they could not read. Empty carriers alone therefore
//!   reads an unreadable bundle as a clean one, and the scan must prove it inspected
//!   something.
//!
//! # The limit, stated rather than implied
//!
//! The measurement proves the bundle carries no tag. It does NOT prove the bundle still
//! holds THIS build — and an out-of-band re-seed is precisely what puts the machine in
//! the state that triggers a repair, so the two could in principle race. That is bounded
//! downstream rather than here: the successor is handed
//! `ATERM_SEAMLESS_TARGET = (build, commit)` of the process asking for the repair, and
//! `seamless::take_target_identity` refuses to complete the adoption proof for anything
//! else. A bundle that changed under us therefore costs a REFUSED repair and a rollback
//! with the terminal intact — never a successor that is not this build. Proving identity
//! here instead would mean a `codesign --deep` on the way to optional background work,
//! to re-derive what the proof already establishes exactly.
//!
//! # What this module is not
//!
//! It is not a security boundary and it grants no authority to run anything: the only
//! image a repair can reach is the bundle this process is already executing, which the
//! handoff derives from `current_exe` at the launch site. It does not clean anything —
//! a bundle that is itself tagged is atpkg's to re-seed, and a repair correctly refuses
//! there, because relaunching from a tagged bundle would produce another tracked
//! process.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// What the impure prober measured. Every field is the RAW answer, so the policy below
/// can tell "clean" apart from "could not look" — the distinction three of the
/// underlying predicates collapse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RepairFacts {
    /// `atpkg::provenance::measure_tracked` — `Some(true)` tracked, `Some(false)` not,
    /// `None` the probe could not be written or read back.
    pub(crate) tracked: Option<bool>,
    /// The executable's own attribute list could be READ, and carries neither
    /// `com.apple.provenance` nor `com.apple.quarantine`. `None` means `xattr_names`
    /// returned `Err` — a failure to look, which is not cleanliness.
    pub(crate) exe_clean: Option<bool>,
    /// The bundle ROOT's own attribute list could be READ and carries neither tag.
    /// `None` means it could not be read.
    ///
    /// Its own field because a browser stamps `com.apple.quarantine` on the DIRECTORY it
    /// downloaded, which the per-file scans below never look at — and because
    /// `quarantined_carrier` answers through `carries`, which reports `false` for a path
    /// it could not inspect. A quarantined bundle is tracked in every invocation,
    /// launchd's included, so reading "could not look" as "clean" here is the shape that
    /// would repair forever.
    pub(crate) bundle_root_clean: Option<bool>,
    /// The recursive bundle scan for `com.apple.provenance`: `Some(true)` only when it
    /// found no carrier AND inspected at least one file. `None` when it inspected none.
    pub(crate) bundle_clean_provenance: Option<bool>,
    /// The same scan for `com.apple.quarantine`.
    pub(crate) bundle_clean_quarantine: Option<bool>,
    /// `DYLD_INSERT_LIBRARIES` is unset. A tagged library injected from outside the
    /// bundle is a carrier the bundle scan structurally cannot see, and the launch
    /// environment is a merge, so the successor would inherit it.
    pub(crate) no_dyld_insert: bool,
    /// This process runs from a `.app` bundle. Without one there is no out-of-band lane
    /// and a repair could only fork, which inherits the tag.
    pub(crate) bundled: bool,
    /// This process is ALREADY the successor of a same-image handoff. Latched from the
    /// environment rather than from the measurement, so a repair chain can never exceed
    /// length one even if every measurement above is wrong.
    pub(crate) same_image_successor: bool,
}

/// Why this process may not ask to be replaced by itself — `None` when it may.
///
/// Pure, total, and ordered most-fundamental first so the logged reason names the thing
/// an operator would check first. Every arm is a refusal: there is no path through this
/// function that accepts on anything but a complete set of positive measurements.
#[must_use]
pub(crate) fn repair_refusal(facts: RepairFacts) -> Option<&'static str> {
    if facts.same_image_successor {
        return Some("this process is already the successor of a same-image handoff");
    }
    if !facts.bundled {
        return Some("this process does not run from a .app bundle, so it could only fork");
    }
    match facts.tracked {
        Some(true) => {}
        Some(false) => return Some("this process is not provenance-tracked; nothing to repair"),
        None => return Some("this process could not be measured for provenance tracking"),
    }
    match facts.exe_clean {
        Some(true) => {}
        Some(false) => {
            return Some("the bundle's own executable is tagged — a relaunch would be tracked too");
        }
        None => return Some("the bundle's executable could not be inspected"),
    }
    match facts.bundle_root_clean {
        Some(true) => {}
        Some(false) => {
            return Some(
                "the bundle itself is tagged or quarantined, so every invocation of \
                         it is tracked",
            );
        }
        None => return Some("the bundle's own attributes could not be inspected"),
    }
    match facts.bundle_clean_provenance {
        Some(true) => {}
        Some(false) => return Some("a file in the bundle carries com.apple.provenance"),
        None => return Some("the bundle could not be scanned for com.apple.provenance"),
    }
    match facts.bundle_clean_quarantine {
        Some(true) => {}
        Some(false) => return Some("a file in the bundle carries com.apple.quarantine"),
        None => return Some("the bundle could not be scanned for com.apple.quarantine"),
    }
    if !facts.no_dyld_insert {
        return Some("DYLD_INSERT_LIBRARIES is set, so a carrier could be outside the bundle");
    }
    None
}

/// The per-process latch. A repair is offered AT MOST ONCE: the verdict is taken, not
/// read, so no completion path, refusal or re-entry can produce a second attempt.
#[derive(Debug, Default)]
pub(crate) enum RepairPosture {
    /// Nothing measured yet, and no prober running.
    #[default]
    Unmeasured,
    /// A prober thread is in flight; the trigger must not start another.
    Measuring,
    /// Measured eligible and not yet consumed.
    Eligible,
    /// Measured ineligible, or consumed. Terminal for this process either way — the
    /// reason is logged once when it is recorded, never re-derived.
    Settled,
}

impl RepairPosture {
    /// Whether the trigger should start the one prober. Flips to [`Self::Measuring`] so
    /// the caller cannot start a second.
    pub(crate) fn begin_measuring(&mut self) -> bool {
        if matches!(self, Self::Unmeasured) {
            *self = Self::Measuring;
            return true;
        }
        false
    }

    /// Record what the prober measured. A refusal settles permanently.
    pub(crate) fn record(&mut self, refusal: Option<&'static str>) {
        *self = match refusal {
            None => Self::Eligible,
            Some(_) => Self::Settled,
        };
    }

    /// Whether an eligible verdict is waiting. Read by the trigger so the cheap gates
    /// run before the expensive ones; the verdict itself is consumed downstream.
    pub(crate) fn is_eligible(&self) -> bool {
        matches!(self, Self::Eligible)
    }

    /// Settle permanently without consuming — for a refusal discovered after the
    /// measurement (a gate that can never pass in this process).
    pub(crate) fn settle(&mut self) {
        *self = Self::Settled;
    }

    /// CONSUME an eligible verdict. Returns true at most once per process: the posture
    /// settles on the way out, so a refusal downstream is never retried.
    pub(crate) fn take_eligible(&mut self) -> bool {
        if matches!(self, Self::Eligible) {
            *self = Self::Settled;
            return true;
        }
        false
    }
}

/// MEASURE this process and the bundle it runs from. Impure and off the event loop: it
/// writes one probe file, reads a handful of attribute lists, and walks the bundle once.
///
/// Every reading is the RAW answer — see [`RepairFacts`] for why `carries`,
/// `tagged_files_*` and `process_is_tracked` are each the WRONG call here.
///
/// `same_image_successor` is passed in rather than read here so the chain bound does not
/// depend on any measurement being right.
#[cfg(target_os = "macos")]
#[must_use]
pub(crate) fn measure_repair_facts(same_image_successor: bool) -> RepairFacts {
    use atpkg::provenance::{
        PROVENANCE_XATTR, QUARANTINE_XATTR, measure_tracked, tagged_files_under, xattr_names,
    };

    let mut facts = RepairFacts {
        same_image_successor,
        no_dyld_insert: std::env::var_os("DYLD_INSERT_LIBRARIES").is_none(),
        ..RepairFacts::default()
    };
    // The probe goes in `$TMPDIR`, never inside the `.app`: a tracked write there would
    // tag the very bundle being measured and break its `codesign --deep` seal.
    facts.tracked = measure_tracked(&std::env::temp_dir());

    // NOT canonicalized: `apply_staged_update_now` hands the launch lane the RAW
    // `current_exe`, and a scan that proved a different path clean would prove nothing
    // about the bundle LaunchServices is given.
    let Some(exe) = std::env::current_exe().ok() else {
        // No executable path, no bundle, no repair. Leaving `bundled` false refuses.
        return facts;
    };
    // The SAME derivation the out-of-band lane hands LaunchServices, so the bundle this
    // scan proves clean cannot be a different one from the bundle that would be launched.
    let Some(bundle) = crate::app_update_handoff::app_bundle_root(&exe) else {
        return facts;
    };
    facts.bundled = true;
    // `xattr_names`, not `carries`: `carries` answers `false` for a path it could not
    // inspect, which would read "could not look" as "clean".
    facts.exe_clean = xattr_names(&exe).ok().map(|names| {
        !names
            .iter()
            .any(|n| n == PROVENANCE_XATTR || n == QUARANTINE_XATTR)
    });
    facts.bundle_root_clean = xattr_names(&bundle).ok().map(|names| {
        !names
            .iter()
            .any(|n| n == PROVENANCE_XATTR || n == QUARANTINE_XATTR)
    });
    // `total > 0` is mandatory: an unreadable directory yields a zero-carrier,
    // zero-total `Scan`, so empty carriers ALONE would read as clean.
    for (attr, slot) in [
        (PROVENANCE_XATTR, &mut facts.bundle_clean_provenance),
        (QUARANTINE_XATTR, &mut facts.bundle_clean_quarantine),
    ] {
        let scan = tagged_files_under(&bundle, attr);
        *slot = (scan.total > 0).then_some(scan.carriers.is_empty());
    }
    facts
}

/// Whether THIS process is already the successor of a same-image handoff.
///
/// `ATERM_UPDATED_FROM` carries the build the predecessor was running, and every
/// successor gets it — a real update's, the QA seam's, and a repair's alike. When it
/// names OUR OWN build the predecessor ran this same image, which only a same-image
/// handoff does.
///
/// It must be LATCHED at startup ([`latch_same_image_successor`]) because the launcher
/// unsets the variable before any shell spawns, so that it never leaks to the user's
/// children — long before the repair trigger would read it. An unlatched read therefore
/// answers `true`: "assume we are a successor", which REFUSES a repair. That is the
/// fail-closed direction, and it is what bounds the chain at one even if the latch is
/// ever moved after the unset by mistake.
#[must_use]
pub(crate) fn same_image_successor() -> bool {
    *SAME_IMAGE_SUCCESSOR.get().unwrap_or(&true)
}

/// Latch [`same_image_successor`] from the raw environment value, at startup, BEFORE the
/// launcher unsets it. Idempotent; a second call is ignored.
pub(crate) fn latch_same_image_successor(updated_from: Option<&std::ffi::OsStr>) {
    let same_image = updated_from
        .is_some_and(|from| from == std::ffi::OsStr::new(crate::build_info::BUILD_NUMBER));
    let _ = SAME_IMAGE_SUCCESSOR.set(same_image);
}

static SAME_IMAGE_SUCCESSOR: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    /// The one fact set that accepts: tracked, clean everywhere, measured everywhere.
    fn eligible() -> RepairFacts {
        RepairFacts {
            tracked: Some(true),
            exe_clean: Some(true),
            bundle_root_clean: Some(true),
            bundle_clean_provenance: Some(true),
            bundle_clean_quarantine: Some(true),
            no_dyld_insert: true,
            bundled: true,
            same_image_successor: false,
        }
    }

    #[test]
    fn the_only_accepted_shape_is_a_complete_set_of_positive_measurements() {
        assert_eq!(repair_refusal(eligible()), None);
    }

    /// EVERY unmeasurable reading refuses. This is the half of the policy that exists
    /// because three of the underlying predicates answer "clean" for "could not look":
    /// `carries` is false on an unreadable path, the scans return a zero-total `Scan`
    /// for an unreadable directory, and `process_is_tracked` fails closed to `true`.
    #[test]
    fn an_unmeasurable_reading_never_accepts() {
        for (facts, expect) in [
            (
                RepairFacts {
                    tracked: None,
                    ..eligible()
                },
                "could not be measured",
            ),
            (
                RepairFacts {
                    exe_clean: None,
                    ..eligible()
                },
                "could not be inspected",
            ),
            (
                RepairFacts {
                    bundle_root_clean: None,
                    ..eligible()
                },
                "own attributes could not be inspected",
            ),
            (
                RepairFacts {
                    bundle_clean_provenance: None,
                    ..eligible()
                },
                "could not be scanned for com.apple.provenance",
            ),
            (
                RepairFacts {
                    bundle_clean_quarantine: None,
                    ..eligible()
                },
                "could not be scanned for com.apple.quarantine",
            ),
        ] {
            let refusal = repair_refusal(facts).expect("an unmeasured reading must refuse");
            assert!(refusal.contains(expect), "{refusal}");
        }
    }

    /// A process that is NOT tracked has nothing to repair, and a bundle that is itself
    /// tagged or quarantined would produce another tracked process — the two shapes that
    /// would make a repair pointless and endless respectively.
    #[test]
    fn nothing_to_repair_and_nothing_a_repair_could_fix_both_refuse() {
        let untracked = repair_refusal(RepairFacts {
            tracked: Some(false),
            ..eligible()
        })
        .expect("refuses");
        assert!(untracked.contains("nothing to repair"), "{untracked}");

        let tagged_exe = repair_refusal(RepairFacts {
            exe_clean: Some(false),
            ..eligible()
        })
        .expect("refuses");
        assert!(tagged_exe.contains("would be tracked too"), "{tagged_exe}");

        let quarantined = repair_refusal(RepairFacts {
            bundle_root_clean: Some(false),
            ..eligible()
        })
        .expect("refuses");
        assert!(quarantined.contains("every invocation"), "{quarantined}");

        for facts in [
            RepairFacts {
                bundle_clean_provenance: Some(false),
                ..eligible()
            },
            RepairFacts {
                bundle_clean_quarantine: Some(false),
                ..eligible()
            },
        ] {
            assert!(
                repair_refusal(facts).is_some_and(|r| r.contains("carries com.apple.")),
                "a tagged file in the bundle refuses"
            );
        }
    }

    /// The two structural refusals that do not depend on any measurement: no bundle
    /// means the only lane is the fork lane, which inherits the tag; and a successor of
    /// a same-image handoff must never start another, however the xattrs read.
    #[test]
    fn a_chain_of_length_one_and_a_bundle_are_both_structural() {
        let unbundled = repair_refusal(RepairFacts {
            bundled: false,
            ..eligible()
        })
        .expect("refuses");
        assert!(unbundled.contains("could only fork"), "{unbundled}");

        // The successor refusal outranks everything, including a fact set that is
        // otherwise perfectly eligible — that is what bounds the chain at one.
        let successor = repair_refusal(RepairFacts {
            same_image_successor: true,
            ..eligible()
        })
        .expect("refuses");
        assert!(successor.contains("already the successor"), "{successor}");
    }

    /// An injected library is a carrier the bundle scan cannot see, and the launch
    /// environment is a merge, so the successor would inherit it.
    #[test]
    fn an_injected_library_refuses_because_the_scan_cannot_see_it() {
        let refusal = repair_refusal(RepairFacts {
            no_dyld_insert: false,
            ..eligible()
        })
        .expect("refuses");
        assert!(refusal.contains("DYLD_INSERT_LIBRARIES"), "{refusal}");
    }

    /// The latch offers a repair at most once: one prober, one consumption, and a
    /// refusal downstream is never retried because the take already settled it.
    #[test]
    fn the_latch_offers_a_repair_at_most_once() {
        let mut posture = RepairPosture::default();
        assert!(
            posture.begin_measuring(),
            "the first trigger starts the prober"
        );
        assert!(
            !posture.begin_measuring(),
            "a second trigger must not start another"
        );
        posture.record(None);
        assert!(
            posture.take_eligible(),
            "the eligible verdict is consumable once"
        );
        assert!(
            !posture.take_eligible(),
            "…and only once, however the attempt ended"
        );

        let mut refused = RepairPosture::default();
        assert!(refused.begin_measuring());
        refused.record(Some("a reason"));
        assert!(!refused.take_eligible(), "a refusal is never consumable");
        assert!(!refused.begin_measuring(), "and never re-measured");
    }
}
