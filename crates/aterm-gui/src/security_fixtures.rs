// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! NAMED SECURITY POSTURES, one catalog for the unit tests and for a development build's
//! `security-posture` verb ([`crate::security_posture`]), so a capture of a headless instance and a
//! unit assertion show the same MACOS ACCESS block.

use aterm_containment::FdaScope;
use aterm_containment::consent::{
    DrClass, FdaState, Folder, ProbeLabel, ResetSubject, SpikeEvidence, TccutilPresence,
};

use crate::native_settings::{DevToolsFacts, MacosAccess, MacosAccessRow};

/// The fixture copy's reset subject, `com.example.fixture`: read once from a
/// exclusively owned scratch bundle, the only way a [`ResetSubject`] exists.
fn fixture_subject() -> Option<ResetSubject> {
    static SUBJECT: std::sync::OnceLock<Option<ResetSubject>> = std::sync::OnceLock::new();
    SUBJECT
        .get_or_init(|| {
            let scratch = aterm_tempfile::TempDir::new().ok()?;
            let root = scratch.path().join("Fixture.app");
            let contents = root.join("Contents");
            std::fs::create_dir_all(&contents)
                .and_then(|()| {
                    std::fs::write(
                        contents.join("Info.plist"),
                        "<plist><dict><key>CFBundleIdentifier</key>\
                         <string>com.example.fixture</string></dict></plist>",
                    )
                })
                .ok()?;
            ResetSubject::read(&root, &[])
        })
        .clone()
}

/// The base posture: an installed copy with the healthy anchor, Full Disk Access `fda`, a
/// designated requirement of class `dr`, and the measured `scope` of a grant.
pub(crate) fn access(fda: FdaState, dr: DrClass, scope: FdaScope) -> MacosAccess {
    MacosAccess {
        enabled: true,
        anchor: aterm_containment::ImageAnchor::Live,
        fda,
        probe: match fda {
            FdaState::Granted => ProbeLabel::OpenOk,
            FdaState::Denied => ProbeLabel::OpenEperm,
            FdaState::Unknown => ProbeLabel::RefusedOutOfBundle,
        },
        dr,
        evidence: SpikeEvidence {
            fda_scope: scope,
            ..SpikeEvidence::UNMEASURED
        },
        bundle_id: fixture_subject(),
        install: "installed",
        running: Some("/Applications/fixture.app/Contents/MacOS/fixture".to_string()),
        sessions_total: 2,
        sessions_adopted: 1,
        covers: Vec::new(),
        uncovered: vec!["file-provider-domains"],
        unmeasured: vec![
            "documents",
            "desktop",
            "downloads",
            "network-volumes",
            "removable-volumes",
            "app-data",
        ],
        tccutil: TccutilPresence::Executable,
        warmup_offered: true,
        warmup_live: false,
        warmup_rows: Vec::new(),
        reset: None,
        claimants: None,
        trash_tool: true,
        retire_live: false,
        retired: None,
        retire_status: None,
        debugger: crate::debugger_access::DebuggerFacts::default(),
        devtools: DevToolsFacts::default(),
        askpass_relay_off: false,
    }
}

/// A census with the running copy (Developer ID when `running_stable`, else ad hoc), an ad-hoc
/// `.rollback` beside it and a Developer-ID copy from another team.
pub(crate) fn census(running_stable: bool) -> aterm_containment::consent::Claimants {
    use aterm_containment::consent::{Claimant, Claimants, Enumeration, classify_dr};
    let copy = |path: &str, dr_text: &str, signing: &'static str, team: Option<&str>| Claimant {
        path: std::path::PathBuf::from(path),
        dr: classify_dr(dr_text),
        dr_text: dr_text.to_string(),
        signing,
        team: team.map(str::to_string),
        running: path == "/Applications/aterm.app",
        links: Vec::new(),
    };
    let dev_id = |team: &str| {
        format!(
            "designated => identifier \"x\" and anchor apple generic and certificate \
             leaf[subject.OU] = \"{team}\""
        )
    };
    let running = if running_stable {
        copy(
            "/Applications/aterm.app",
            &dev_id("T"),
            "developer-id",
            Some("T"),
        )
    } else {
        copy(
            "/Applications/aterm.app",
            "designated => cdhash H\"00\"",
            "adhoc",
            None,
        )
    };
    Claimants {
        found: vec![
            running,
            copy(
                "/Applications/aterm.app.rollback",
                "designated => cdhash H\"aa\"",
                "adhoc",
                None,
            ),
            copy(
                "/Users//a/Old/aterm.app",
                &dev_id("U"),
                "developer-id",
                Some("U"),
            ),
        ],
        enumeration: Enumeration::Complete,
    }
}

/// Every protected folder's warm-up row read Denied: macOS stopped asking.
pub(crate) fn denied_rows() -> Vec<MacosAccessRow> {
    Folder::ALL
        .iter()
        .map(|folder| (*folder, crate::consent_warmup::WarmupRow::Denied))
        .collect()
}

/// The catalog's names, in the order `security-posture list` says them.
pub(crate) const NAMES: &[&str] = &[
    "granted",
    "denied",
    "denied-folders",
    "rebuild",
    "anchor-fault",
    "other-copies",
    "no-tccutil",
    "no-bundle-id",
    "warmup-live",
];

/// The posture named `name`, or `None` for a name the catalog does not hold.
pub(crate) fn named(name: &str) -> Option<MacosAccess> {
    let granted = || access(FdaState::Granted, DrClass::Identity, FdaScope::Unknown);
    let denied = || access(FdaState::Denied, DrClass::Identity, FdaScope::Unknown);
    let denied_folders = || MacosAccess {
        warmup_rows: denied_rows(),
        ..denied()
    };
    Some(match name {
        "granted" => granted(),
        "denied" => denied(),
        "denied-folders" => denied_folders(),
        "rebuild" => MacosAccess {
            dr: DrClass::Cdhash,
            ..denied_folders()
        },
        "anchor-fault" => MacosAccess {
            anchor: aterm_containment::ImageAnchor::Deleted,
            ..granted()
        },
        "other-copies" => MacosAccess {
            claimants: Some(census(true)),
            ..granted()
        },
        "no-tccutil" => MacosAccess {
            tccutil: TccutilPresence::Missing,
            ..denied_folders()
        },
        "no-bundle-id" => MacosAccess {
            bundle_id: None,
            ..denied_folders()
        },
        "warmup-live" => MacosAccess {
            warmup_live: true,
            ..denied()
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the verb lists stages a posture, and the postures differ.
    #[test]
    fn every_listed_name_is_a_distinct_posture() {
        let postures: Vec<MacosAccess> = NAMES
            .iter()
            .map(|name| named(name).unwrap_or_else(|| panic!("{name} is in the catalog")))
            .collect();
        for (name, access) in NAMES.iter().zip(&postures) {
            let expected = (*name != "no-bundle-id").then_some("com.example.fixture");
            assert_eq!(
                access.bundle_id.as_ref().map(ResetSubject::as_str),
                expected,
                "{name}: the subject is read from the owned fixture bundle"
            );
        }
        for (i, a) in postures.iter().enumerate() {
            for b in &postures[i + 1..] {
                assert_ne!(a, b, "two names, one posture");
            }
        }
        assert!(named("nonsense").is_none());
    }
}
