// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_messages::{HOLD_WARN, TITLE_CAP};

fn fill_of(msg: &Message) -> Option<u16> {
    msg.meter.as_ref().and_then(|m| m.fill_permille)
}

fn stats_of(msg: &Message) -> &str {
    msg.meter.as_ref().map_or("", |m| m.stats.as_str())
}

fn busy(msg: &Message) -> bool {
    msg.meter.as_ref().is_some_and(|m| m.busy)
}

#[test]
fn a_folded_desk_is_said_as_a_calm_record() {
    use crate::restore::LayoutFold;
    assert!(layout_folded(&LayoutFold::default()).is_none());
    assert!(
        layout_folded(&LayoutFold {
            front: vec![7],
            ..LayoutFold::default()
        })
        .is_none(),
        "a front list alone moved nothing"
    );
    let said = |windows, tabs| {
        let m = layout_folded(&LayoutFold {
            windows,
            tabs,
            front: vec![0],
        })
        .expect("something folded");
        assert_eq!(m.severity, Severity::Info, "a record, never a warning");
        assert_eq!(m.glyph, glyph(NOTED));
        assert_eq!(m.hold, Hold::Default, "on glass");
        assert_eq!(m.key.as_deref(), Some(KEY_LAYOUT_FOLDED));
        (m.title.clone(), m.detail.clone())
    };
    let (windows, detail) = said(6, 0);
    assert!(
        windows.contains('6') && detail.len() == 1,
        "{windows} {detail:?}"
    );
    let (tabs, detail) = said(0, 44);
    assert!(
        tabs.contains("44") && detail.len() == 1,
        "{tabs} {detail:?}"
    );
    assert_ne!(said(1, 0).0, said(6, 0).0.replace('6', "1"), "singular");
    let (_, detail) = said(2, 4);
    assert_eq!(
        detail.len(),
        3,
        "both folds, each with its limit: {detail:?}"
    );
    assert!(detail[1].contains('4'), "{detail:?}");
}

const ALL_WRITES: DevSharedWrites = DevSharedWrites {
    what: "the PATH links, the shell hooks and the agent primer",
    repair: "`aterm pkg repair` and `aterm agents install`",
};

fn dev_behind() -> aterm_update::dev_channel::DevLag {
    aterm_update::dev_channel::DevLag::Behind {
        latest: "v0.93.0".into(),
        releases: Some(2),
    }
}

fn every_dev_standing() -> Vec<aterm_update::dev_channel::DevLag> {
    use aterm_update::dev_channel::DevLag;
    vec![
        dev_behind(),
        DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(1),
        },
        DevLag::Behind {
            latest: "v1.0.0".into(),
            releases: None,
        },
        DevLag::Current {
            latest: "v0.93.0".into(),
        },
        DevLag::Ahead {
            latest: "v0.93.0".into(),
        },
    ]
}

#[test]
fn a_dev_builds_standing_is_a_record() {
    use crate::message_reporters::{Attention, attention};
    use aterm_update::dev_channel::DevLag;
    let behind = dev_build(&dev_behind()).expect("behind is recorded");
    assert_eq!(behind.severity, Severity::Info);
    assert_eq!(behind.hold, Hold::LogOnly);
    assert_eq!(behind.key.as_deref(), Some(KEY_DEV_BUILD));
    assert_eq!(attention(&behind), Ok(Attention::Record));
    assert_eq!(
        aterm_messages::text::glass_title_fault(&behind.title),
        None,
        "the title keeps the glass form, should the owner rule it onto the glass"
    );
    let one = DevLag::Behind {
        latest: "v0.93.0".into(),
        releases: Some(1),
    };
    let one = dev_build(&one).expect("behind is recorded").title;
    assert!(one.contains("v0.93.0"), "{one}");
    assert_ne!(one, behind.title, "one release is not two");
    let major = DevLag::Behind {
        latest: "v1.0.0".into(),
        releases: None,
    };
    assert!(
        dev_build(&major).is_some_and(|m| m.title.contains("v1.0.0")),
        "an unknown count names the release"
    );
    for quiet in [
        DevLag::Current {
            latest: "v0.93.0".into(),
        },
        DevLag::Ahead {
            latest: "v0.93.0".into(),
        },
    ] {
        assert_eq!(dev_build(&quiet), None, "{quiet:?}: nothing to say");
    }
}

#[test]
fn a_dev_build_that_writes_the_release_setup_says_what_and_what_puts_it_back() {
    let entry = dev_build_entry(ALL_WRITES);
    let msg = entry.message().expect("the words pass the rule");
    assert_eq!(msg.severity, Severity::Warn);
    assert_eq!(msg.hold, Hold::LogOnly, "an entry is never a live row");
    assert_eq!(entry.key(), "update.review.dev-build.writes");
    assert_ne!(
        msg.key.as_deref(),
        Some(KEY_DEV_BUILD),
        "the channel never supersedes it"
    );
    assert_ne!(
        msg.key.as_deref(),
        Some(KEY_DEV_BUILD_WRITES),
        "an exception never shares a key with a row or a record"
    );
    assert!(
        msg.title.contains("lost"),
        "the loss is named: {}",
        msg.title
    );
    assert_eq!(aterm_messages::text::glass_title_fault(&msg.title), None);
    assert!(
        msg.detail[0].contains(ALL_WRITES.what),
        "it says what it writes: {:?}",
        msg.detail
    );
    let said = msg.detail.join(" ");
    assert!(said.contains("`aterm pkg repair`"), "{said}");
    assert!(said.contains("`aterm agents install`"), "{said}");
    assert_eq!(dev_shared_writes(false, true, true), Some(ALL_WRITES));
    assert_eq!(
        dev_shared_writes(false, true, false),
        Some(DevSharedWrites {
            what: "the PATH links and the shell hooks",
            repair: "`aterm pkg repair`",
        })
    );
    let primer = dev_shared_writes(false, false, true).expect("the primer alone is a write");
    assert_eq!(
        primer,
        DevSharedWrites {
            what: "the agent primer",
            repair: "`aterm agents install`",
        }
    );
    assert_eq!(dev_shared_writes(false, false, false), None);
    for packages in [false, true] {
        for prime in [false, true] {
            assert_eq!(dev_shared_writes(true, packages, prime), None);
        }
    }
    let only_primer = dev_build_entry(primer).message().expect("the words pass");
    assert!(
        only_primer.detail[0].contains(primer.what),
        "{:?}",
        only_primer.detail
    );
    let said = only_primer.detail.join(" ");
    assert!(said.contains("`aterm agents install`"), "{said}");
    assert!(
        !said.contains("pkg repair"),
        "`aterm pkg repair` does not put the primer back: {said}"
    );
}

fn every_posture() -> Vec<ApplyPosture> {
    use ApplyPosture as P;
    let mut postures = vec![
        P::Automatic,
        P::AtNextLaunch,
        P::ManualByConfig,
        P::VetoedByEnv {
            var: "ATERM_DEBUG_RELAUNCH_NUDGE",
        },
        P::ManualOnlyLatched { lapses: true },
        P::ManualOnlyLatched { lapses: false },
        P::TrialSpent,
    ];
    for image_kept in [false, true] {
        for why in HandoffUnavailable::ALL {
            for veto in [
                None,
                Some(AutoApplyVeto::Config),
                Some(AutoApplyVeto::Env {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                }),
            ] {
                postures.push(P::HandoffDisabled {
                    why,
                    veto,
                    image_kept,
                });
            }
        }
    }
    postures
}

#[test]
fn a_build_held_by_the_install_switch_names_the_switch() {
    use ApplyPosture as P;
    let named = format!(
        "\u{201c}{}\u{201d} is off",
        crate::prefs::UPDATE_INSTALL_LABEL
    );
    for posture in every_posture() {
        let held = matches!(
            posture,
            P::ManualByConfig
                | P::HandoffDisabled {
                    veto: Some(AutoApplyVeto::Config),
                    ..
                }
        );
        let sentence = staged_detail(posture);
        assert_eq!(sentence.contains(&named), held, "{posture:?}: {sentence}");
        assert!(
            !sentence.contains("automatic install is off"),
            "{posture:?}: {sentence}"
        );
    }
    assert_eq!(staged_detail(P::ManualByConfig), named);
}

#[test]
fn a_self_retry_is_a_record_and_a_stopped_lane_says_its_cause() {
    let retry = needs_install(
        "Couldn't install aterm v0.92.0",
        TRIES_AGAIN,
        Severity::Warn,
        1234,
    );
    assert_eq!(retry.hold, Hold::LogOnly, "a record, never a row");
    let stopped = needs_install_because(
        "Couldn't install aterm v0.92.0",
        short_cause("rename: No space left on device (os error 28)"),
        INSTALL_FROM_MENU,
        Severity::Warn,
        1234,
    );
    assert_ne!(stopped.hold, Hold::LogOnly, "a row");
    assert_eq!(
        stopped.detail.first().map(String::as_str),
        Some("disk full")
    );
    assert_eq!(stopped.actions, [Intent::ApplyUpdate { build: 1234 }]);
    for (said, want) in [
        ("No space left on device", Some("disk full")),
        ("ENOSPC", Some("disk full")),
        ("Read-only file system", Some("read-only disk")),
        (
            "Operation not permitted (os error 1)",
            Some("no permission"),
        ),
        ("the handoff timed out", None),
    ] {
        assert_eq!(short_cause(said), want, "{said}");
    }
}

#[test]
fn every_flow_row_completes_in_its_finished_form() {
    use crate::messages_host::assert_records_its_finished_words as completes;
    let dl = |total| {
        progress(
            &aterm_update::Progress::Downloading {
                version: "0.91.0".into(),
                bytes_done: 31_000_000,
                bytes_total: total,
            },
            None,
            "0.91.0",
        )
    };
    completes(&dl(74_000_000), "Downloaded aterm v0.91.0");
    completes(&dl(0), "Downloaded aterm v0.91.0");
    let check = progress(
        &aterm_update::Progress::Verifying {
            version: "0.91.0".into(),
        },
        None,
        "0.91.0",
    );
    completes(&check, "Verified aterm v0.91.0");
    completes(&installing("0.91.0"), "Installed aterm v0.91.0");
    completes(&finishing("0.91.0"), "Installed aterm v0.91.0");
    completes(&finishing(""), "Installed update");
    completes(&installing(""), "Installed update");
    let r = crate::messages_host::restatement_of(&finishing("0.91.0"));
    assert_eq!(
        r.finished,
        Some(Some("Installed aterm v0.91.0".to_string()))
    );
}

#[test]
fn every_update_row_carries_an_admitted_glyph_and_a_capped_title() {
    let rows = [
        progress(
            &aterm_update::Progress::Downloading {
                version: "0.48.0".into(),
                bytes_done: 1,
                bytes_total: 2,
            },
            None,
            "",
        ),
        progress(
            &aterm_update::Progress::Verifying {
                version: "0.48.0".into(),
            },
            None,
            "",
        ),
        staged("0.48.0", 7, Some(ApplyPosture::Automatic)),
        staged("0.48.0", 7, Some(ApplyPosture::ManualByConfig)),
        download_postponed("later"),
        download_withheld("0.48.0", "held: build 7 is quarantined"),
        download_failed("0.48.0", "zip sha256 mismatch"),
        installing("0.48.0"),
        finishing("0.48.0"),
        landed("0.48.0", 7, 0, None),
        landed("0.48.0", 7, 2, Some(Duration::from_secs(42))),
        switch_started(Some("0.48.0"), "0.47.0"),
        switch_stopped(Some("0.48.0"), "0.47.0", false, "child died"),
        outcome("Update installed", "x", Severity::Info),
        needs_install("Update installed", "x", Severity::Info, 7),
        needs_install("Couldn't install aterm v0.48.0", "x", Severity::Warn, 7),
        failed("Couldn't finish the update", "", false),
        health_warning(
            HealthKind::Install,
            aterm_update::health_failing_title("apply"),
            "3 checks",
        ),
        health_recovered("Couldn't install updates", "since Sep 14"),
        scrollback_lost(12_345, 3),
        scrollback_lost(1, 1),
        scrollback_withheld(12_345, 3),
        scrollback_left_behind(12_345, 3, 10_000, 0, 0),
        scrollback_left_behind(12_345, 3, 0, 345, 0),
        scrollback_left_behind(12_345, 3, 0, 0, 345),
        settings_drafts_not_reopened(&[draft("font_family", "Menlo")], 0, None),
        settings_drafts_not_reopened(&[], 2, None),
        dev_build(&dev_behind()).expect("a record"),
        dev_build_entry(ALL_WRITES)
            .message()
            .expect("the words pass"),
    ];
    for m in &rows {
        assert_eq!(m.tag, tags::UPDATE, "{}", m.title);
        assert!(m.title.chars().count() <= TITLE_CAP, "{}", m.title);
        assert!(
            Glyph::ALLOWED.contains(&m.glyph.ch()) && m.glyph != Glyph::FALLBACK,
            "{}: {:?}",
            m.title,
            m.glyph
        );
    }
    assert_eq!(
        software_update(),
        Intent::OpenSettings {
            route: "/updates".into()
        }
    );
    assert_eq!(software_update().label(), "Software Update");
    for ch in [FLOW_GLYPH, NEEDS_YOU, MOVING_IN, NOTED, READY] {
        assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
    }
}

#[test]
fn a_spent_trial_is_recorded_on_hold_not_ready() {
    let m = staged("0.95.0", 11, Some(ApplyPosture::TrialSpent));
    assert_eq!(m.severity, Severity::Info);
    assert_eq!(m.glyph.ch(), NOTED);
    assert_eq!(m.hold, Hold::LogOnly);
    assert!(m.actions.is_empty(), "no press");
    assert!(
        m.detail
            .iter()
            .any(|line| line.contains(NEWER_RELEASE_REPLACES)),
        "{:?}",
        m.detail
    );
    let ready = staged("0.95.0", 11, Some(ApplyPosture::Automatic));
    assert_eq!(ready.title, staged_title("0.95.0"));
    assert_eq!(ready.glyph.ch(), READY);
}

#[test]
fn update_reports_map_to_the_flow_row() {
    use aterm_update::Progress as P;
    let m = progress(
        &P::Downloading {
            version: "0.48.0".into(),
            bytes_done: 45_000_000,
            bytes_total: 74_000_000,
        },
        None,
        "",
    );
    assert_eq!(m.glyph.ch(), MOVING_IN);
    assert_eq!(m.severity, Severity::Info);
    assert_eq!(m.detail, vec![DOWNLOADING]);
    assert_eq!(m.reveal_after, Some(PROGRESS_GRACE));
    assert_eq!(
        stats_of(&m),
        crate::toolchain_words::byte_stats(45_000_000, 74_000_000)
    );
    assert_eq!(fill_of(&m), Some(608));
    assert!(
        m.meter.as_ref().is_some_and(|m| m.amount.is_some()),
        "the bytes are the ETA's input"
    );
    assert!(!busy(&m), "a known total is a fill, not motion");
    assert_eq!(
        m.hold,
        Hold::Live {
            stale_after: STALE_UPDATE
        }
    );
    assert_eq!(m.key.as_deref(), Some(KEY_PROGRESS));
    assert!(m.actions.is_empty(), "no capsule but the implicit Details");
    let m = progress(
        &P::Downloading {
            version: "0.48.0".into(),
            bytes_done: 45_000_000,
            bytes_total: 0,
        },
        None,
        "",
    );
    assert_eq!(fill_of(&m), None);
    assert!(busy(&m));
    assert_eq!(stats_of(&m), "45 MB");
    let m = progress(
        &P::Verifying {
            version: "0.48.0".into(),
        },
        None,
        "",
    );
    assert_eq!(m.glyph.ch(), FLOW_GLYPH, "checking is the working mark");
    assert_eq!(m.detail, vec![CHECKING]);
    assert!(busy(&m), "checking has no fraction: the row moves");
    let cause = "zip sha256 mismatch: expected ab12, got cd34 — see                      https://github.com/alabsystems/aterm/releases/tag/v0.48.0 for the assets";
    let m = progress(
        &P::Failed {
            detail: cause.into(),
        },
        None,
        "0.48.0",
    );
    assert_eq!(m.severity, Severity::Warn);
    assert_eq!(m.detail[0], DOWNLOAD_FAILED);
    assert_eq!(m.detail[1..].join(" "), cause, "the whole sentence, kept");
    assert_eq!(m.hold, Hold::LogOnly, "a self-retry is a record");
    assert_eq!(m.key, None, "a record never supersedes the live flow row");
    assert!(
        m.actions.is_empty(),
        "a retried download asks nothing of anyone"
    );
    let m = progress(
        &P::Deferred {
            detail: "the network is metered; checking again in 10 min".into(),
        },
        None,
        "0.48.0",
    );
    assert_eq!(m.hold, Hold::LogOnly);
    assert_eq!(m.key, None);
    assert_eq!(
        m.detail,
        vec!["the network is metered; checking again in 10 min"]
    );
    let m = progress(
        &P::Withheld {
            detail: "held: build 7 was signed by machine m11, which the machine roster \
                     has revoked"
                .into(),
        },
        None,
        "0.48.0",
    );
    assert_eq!(m.hold, Hold::LogOnly);
    assert_eq!(m.key, None);
    assert!(
        !m.detail.iter().any(|d| d.contains("again")),
        "{:?}",
        m.detail
    );
    assert_eq!(flow_title("Installing", ""), "Installing update");
    assert_eq!(Severity::Warn.default_hold(), HOLD_WARN);
}

#[test]
fn every_update_message_earns_its_row() {
    use crate::message_reporters::{Attention, attention};
    use aterm_update::Progress as P;
    let mut all = vec![
        progress(
            &P::Downloading {
                version: "0.91.0".into(),
                bytes_done: 1,
                bytes_total: 2,
            },
            None,
            "",
        ),
        progress(
            &P::Downloading {
                version: "0.91.0".into(),
                bytes_done: 1,
                bytes_total: 0,
            },
            None,
            "",
        ),
        progress(
            &P::Verifying {
                version: "0.91.0".into(),
            },
            None,
            "",
        ),
        progress(&P::Failed { detail: "x".into() }, None, "0.91.0"),
        progress(&P::Deferred { detail: "x".into() }, None, "0.91.0"),
        progress(&P::Withheld { detail: "x".into() }, None, "0.91.0"),
        installing("0.91.0"),
        installing(""),
        finishing("0.91.0"),
        landed("0.91.0", 7, 0, Some(Duration::from_secs(42))),
        switch_started(Some("0.91.0"), "0.90.0"),
        switch_stopped(Some("0.91.0"), "0.90.0", true, "x"),
        switch_stopped(Some("0.91.0"), "0.90.0", false, "x"),
        outcome("Couldn't install aterm v0.91.0", "x", Severity::Info),
        outcome("Update waits", "x", Severity::Warn),
        needs_install(
            "Couldn't install aterm v0.91.0",
            INSTALL_FROM_MENU,
            Severity::Warn,
            7,
        ),
        needs_install(
            &crate::app_update_screen::update_installed_title(Some("9.9.9")),
            crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
            Severity::Info,
            7,
        ),
        needs_install(
            crate::app_update_screen::UPDATE_DIDNT_INSTALL,
            INSTALL_FROM_MENU,
            Severity::Warn,
            7,
        ),
        failed("Couldn't finish the update", "", false),
        failed(
            crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
            crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        ),
        waits_for_you(crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
        waits_for_you(crate::App::UNSAVED_SETTINGS_BLOCKS_APPLY),
        waits_for_you(crate::App::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY),
        health_recovered("Couldn't install updates", "since Sep 14"),
        staged("0.91.0", 7, None),
        scrollback_lost(12_345, 3),
        scrollback_withheld(12_345, 3),
        scrollback_left_behind(12_345, 3, 10_000, 0, 0),
        scrollback_left_behind(12_345, 3, 0, 345, 0),
        scrollback_left_behind(12_345, 3, 0, 0, 345),
        settings_drafts_not_reopened(&[draft("font_family", "Menlo")], 1, None),
    ];
    for lag in every_dev_standing() {
        all.extend(dev_build(&lag));
    }
    for packages in [false, true] {
        for prime in [false, true] {
            all.extend(
                dev_shared_writes(false, packages, prime)
                    .map(|w| dev_build_entry(w).message().expect("the words pass")),
            );
        }
    }
    for posture in every_posture() {
        all.push(staged("0.91.0", 7, Some(posture)));
    }
    for class in ["apply", "pipeline", "stage", "manifest"] {
        all.push(health_warning(
            HealthKind::of_class(class),
            aterm_update::health_failing_title(class),
            "3 failed checks in a row since 2026-09-14T22:04:36Z: x.",
        ));
    }
    let mut classes = std::collections::BTreeSet::new();
    for m in &all {
        let class =
            attention(m).unwrap_or_else(|why| panic!("{why}: {:?} ({:?})", m.title, m.hold));
        classes.insert(format!("{class:?}"));
        if matches!(m.hold, Hold::Live { .. }) && m.severity < Severity::Warn {
            assert_eq!(class, Attention::Progress, "{}", m.title);
        }
    }
    assert_eq!(
        classes.len(),
        4,
        "the lane exercises progress, decision, failure and record: {classes:?}"
    );
}

#[test]
fn a_failed_update_keeps_its_whole_cause() {
    let cause = format!(
        "the release asset could not be verified: {}see \
         https://github.com/alabsystems/aterm/releases/tag/v0.92.0 for the signed assets",
        "the digest did not match the manifest; ".repeat(12)
    );
    assert!(cause.chars().count() > 500);
    let m = progress(
        &aterm_update::Progress::Failed {
            detail: cause.clone(),
        },
        None,
        "0.92.0",
    );
    assert_eq!(m.detail[0], DOWNLOAD_FAILED);
    let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        words(&m.detail[1..].join(" ")).replace(";", ""),
        words(&cause).replace(";", ""),
        "every word of the cause, the URL whole"
    );
    assert!(m.detail.iter().any(|l| l.contains("releases/tag/v0.92.0")));
}

#[test]
fn a_staged_row_says_how_the_update_installs_and_never_asks_for_a_restart() {
    use ApplyPosture as P;
    assert_eq!(
        staged("0.67.0", 7, Some(P::Automatic)).hold,
        Hold::LogOnly,
        "the automatic lane's staged build takes no row"
    );
    for posture in every_posture() {
        let m = staged("0.67.0", 7, Some(posture));
        if staged_is_decision(Some(posture)) {
            assert_eq!(m.glyph.ch(), MOVING_IN, "a build to install (ruling 303)");
            assert_eq!(m.severity, Severity::Info, "a decision, not a confirmation");
            assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
            assert!(!busy(&m), "a ready row waits; it does not move");
        } else {
            let title = if posture == P::TrialSpent {
                "aterm v0.67.0 is on hold"
            } else {
                "aterm v0.67.0 is ready"
            };
            assert_eq!(m.title, title, "{posture:?}");
            assert_eq!(m.hold, Hold::LogOnly, "{posture:?}: nothing to press");
            assert!(m.meter.is_none());
        }
        assert!(!m.title.to_lowercase().contains("click"), "{}", m.title);
        assert_eq!(m.key.as_deref(), Some(KEY_PROGRESS));
        assert_eq!(
            m.actions.contains(&Intent::ApplyUpdate { build: 7 }),
            matches!(
                posture,
                P::ManualByConfig | P::VetoedByEnv { .. } | P::ManualOnlyLatched { lapses: false }
            ),
            "{posture:?}: {:?}",
            m.actions
        );
        let sentence = m.detail.join("; ");
        assert_eq!(sentence, staged_detail(posture), "{posture:?}");
        assert!(
            m.detail
                .iter()
                .all(|l| l.chars().count() <= DETAIL_LINE_CAP && !l.ends_with('\u{2026}')),
            "{posture:?}: {:?}",
            m.detail
        );
        for word in [
            "build 7",
            "verified",
            "in place",
            "staging",
            "shells keep running",
        ] {
            assert!(
                !sentence.contains(word),
                "{posture:?}: {word:?} in {sentence}"
            );
        }
        let lower = sentence
            .replace("ATERM_DEBUG_RELAUNCH_NUDGE", "<seam>")
            .to_lowercase();
        assert!(
            !lower.contains("restart") && !lower.contains("relaunch"),
            "{posture:?} asks for a restart: {sentence}"
        );
        if let P::HandoffDisabled {
            why,
            veto,
            image_kept,
        } = posture
        {
            assert!(
                sentence.starts_with(why.cause()),
                "{posture:?}: the cause leads: {sentence}"
            );
            assert_eq!(
                sentence.contains("once every terminal is closed"),
                !image_kept,
                "{posture:?}: {sentence}"
            );
            assert_eq!(
                sentence.contains(INSTALL_FROM_MENU),
                veto.is_some(),
                "{posture:?}: {sentence}"
            );
            assert!(
                !lower.contains("keep running") && !lower.contains("survive"),
                "with the handoff off the line may neither promise an in-place \
                 landing nor threaten a kill the admission gate forbids: {sentence}"
            );
        }
    }
    assert_eq!(staged_detail(P::Automatic), INSTALLS_BY_ITSELF);
    for posture in every_posture() {
        assert_eq!(staged_detail_lines(Some(posture)).len(), 1, "{posture:?}");
    }
    let m = staged("0.67.0", 7, None);
    assert_eq!(m.detail, vec![staged_detail_unknown()]);
    assert_eq!(staged_detail_unknown(), INSTALL_FROM_MENU);
    assert_eq!(m.actions, vec![Intent::ApplyUpdate { build: 7 }]);
    assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
    assert!(crate::native_update_auto_intent::LANDS_WITHIN < Duration::from_secs(60));
    assert!(!INSTALLS_BY_ITSELF.contains("0 min"));
}

#[test]
fn a_posture_restatement_carries_the_full_words() {
    use ApplyPosture as P;
    let r = restate_apply_posture("0.67.0", 7, P::Automatic);
    assert_eq!(r.hold, Some(Hold::LogOnly));
    assert_eq!(r.detail, Some(vec![INSTALLS_BY_ITSELF.to_string()]));
    assert_eq!(r.actions, Some(Vec::new()));
    let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: false });
    assert_eq!(r.actions, Some(vec![Intent::ApplyUpdate { build: 7 }]));
    assert_eq!(r.hold, Some(Hold::For(HOLD_STAGED_MANUAL)));
    assert_eq!(r.severity, Some(Severity::Info));
    assert_eq!(r.meter, Some(None));
    let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: true });
    assert_eq!(r.hold, Some(Hold::LogOnly));
    assert_eq!(r.meter, Some(None));
    let r = restate_apply_posture(
        "0.67.0",
        7,
        P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: None,
            image_kept: false,
        },
    );
    assert_eq!(r.actions, Some(Vec::new()), "the handoff off strips it");
    let r = restate_apply_posture("0.67.0", 7, P::AtNextLaunch);
    assert_eq!(r.hold, Some(Hold::LogOnly));
    assert_eq!(r.actions, Some(Vec::new()), "a press would only be refused");
}

#[test]
fn the_staged_row_carries_install_only_under_a_manual_posture() {
    use ApplyPosture as P;
    assert_eq!(apply_capsule_for(Some(P::Automatic), 3), None);
    assert_eq!(
        apply_capsule_for(Some(P::ManualOnlyLatched { lapses: true }), 3),
        None
    );
    for why in HandoffUnavailable::ALL {
        assert_eq!(
            apply_capsule_for(
                Some(P::HandoffDisabled {
                    why,
                    veto: None,
                    image_kept: false
                }),
                3
            ),
            None
        );
    }
    for manual in [
        P::ManualByConfig,
        P::VetoedByEnv {
            var: "ATERM_DEBUG_RELAUNCH_NUDGE",
        },
        P::ManualOnlyLatched { lapses: false },
    ] {
        assert_eq!(
            apply_capsule_for(Some(manual), 3),
            Some(Intent::ApplyUpdate { build: 3 }),
            "{manual:?}"
        );
    }
    assert_eq!(
        apply_capsule_for(None, 3),
        Some(Intent::ApplyUpdate { build: 3 }),
        "no posture keeps the manual affordance"
    );
}

#[test]
fn the_scrollback_row_counts_the_lines_and_the_tabs() {
    let m = scrollback_lost(12_345, 3);
    assert_eq!(m.severity, Severity::Warn);
    assert_eq!(
        m.hold,
        Hold::Default,
        "a loss is on glass, not only on record"
    );
    assert_eq!(m.key.as_deref(), Some(KEY_SCROLLBACK));
    assert_eq!(m.detail[0], "12345 older lines stayed behind in 3 tabs");
    assert!(
        m.detail.iter().any(|l| l.contains("history_lost=")),
        "{:?}",
        m.detail
    );
    assert_eq!(
        scrollback_lost(1, 1).detail[0],
        "1 older line stayed behind in 1 tab"
    );
}

#[test]
fn the_scrollback_row_says_a_withheld_carry_as_the_policys_request() {
    let text = |m: &Message| format!("{} | {}", m.title, m.detail.join(" | "));
    let comfort = "the newest lines came across";
    let asked = "the new version asked this update to carry no scrollback";

    let failed = scrollback_left_behind(12_345, 3, 0, 0, 0);
    assert_eq!(failed, scrollback_lost(12_345, 3), "a failure is unchanged");
    assert!(text(&failed).contains(comfort), "{}", text(&failed));
    assert!(!text(&failed).contains(asked), "{}", text(&failed));

    let withheld = scrollback_left_behind(12_345, 3, 12_345, 0, 0);
    assert_eq!(withheld, scrollback_withheld(12_345, 3));
    assert_eq!(
        withheld.detail[0],
        "12345 older lines stayed behind in 3 tabs"
    );
    assert!(text(&withheld).contains(asked), "{}", text(&withheld));
    assert!(
        !text(&withheld).contains("came across") && !text(&withheld).contains("on screen"),
        "no claim about what crossed: {}",
        text(&withheld)
    );

    let mixed = scrollback_left_behind(12_345, 3, 10_000, 0, 0);
    assert_eq!(mixed.detail[0], "12345 older lines stayed behind in 3 tabs");
    assert!(
        mixed.detail[1].starts_with("10000 of them because the new version asked"),
        "{:?}",
        mixed.detail
    );
    assert!(!text(&mixed).contains("came across"), "{}", text(&mixed));

    let held = scrollback_left_behind(500, 1, 0, 500, 0);
    assert_eq!(held.detail[0], "500 older lines stayed behind in 1 tab");
    assert_eq!(
        held.detail[1],
        "500 of them in panes kept open after their command ended, whose screen alone \
         crossed",
    );
    assert!(
        !text(&held).contains("came across") && !text(&held).contains(asked),
        "{}",
        text(&held)
    );
    let held_and_failed = scrollback_left_behind(12_345, 3, 0, 500, 0);
    assert!(
        !text(&held_and_failed).contains("came across"),
        "{}",
        text(&held_and_failed)
    );

    let held_carry_failed = scrollback_left_behind(500, 1, 0, 0, 500);
    assert_eq!(
        held_carry_failed.detail[0],
        "500 older lines stayed behind in 1 tab"
    );
    assert!(
        held_carry_failed.detail[1].starts_with(
            "500 of them in panes kept open after their command ended, which this update \
             failed to carry"
        ),
        "{:?}",
        held_carry_failed.detail
    );
    assert!(
        !text(&held_carry_failed).contains("came across")
            && !text(&held_carry_failed).contains("on screen")
            && !text(&held_carry_failed).contains(asked),
        "{}",
        text(&held_carry_failed)
    );
    let held_failed_beside_a_tab = scrollback_left_behind(12_345, 3, 0, 0, 500);
    assert!(
        !text(&held_failed_beside_a_tab).contains("came across"),
        "{}",
        text(&held_failed_beside_a_tab)
    );
    let by_design_and_failed = scrollback_left_behind(800, 2, 0, 300, 500);
    assert!(
        by_design_and_failed.detail[1].starts_with("300 of them in panes kept open")
            && by_design_and_failed.detail[2].starts_with("500 of them in panes kept open")
            && by_design_and_failed.detail[2].ends_with("which this update failed to carry"),
        "one line per cause: {:?}",
        by_design_and_failed.detail
    );
    assert_ne!(failed.title, withheld.title);
    assert_eq!(held.title, withheld.title, "by design is no failure");
    for failure in [
        &mixed,
        &held_and_failed,
        &held_carry_failed,
        &held_failed_beside_a_tab,
        &by_design_and_failed,
    ] {
        assert_eq!(failure.title, failed.title, "{:?}", failure.detail);
    }

    for m in [
        &failed,
        &withheld,
        &mixed,
        &held,
        &held_and_failed,
        &held_carry_failed,
        &held_failed_beside_a_tab,
        &by_design_and_failed,
    ] {
        assert_eq!(m.severity, Severity::Warn, "{}", m.title);
        assert_eq!(m.hold, Hold::Default, "a loss is on glass: {}", m.title);
        assert_eq!(m.key.as_deref(), Some(KEY_SCROLLBACK), "{}", m.title);
        assert!(text(m).contains("history_lost="), "{}", text(m));
    }
}

fn draft(key: &str, text: &str) -> crate::restore::SettingsDraftRestore {
    crate::restore::SettingsDraftRestore {
        key: key.to_string(),
        text: text.to_string(),
    }
}

#[test]
fn a_settings_draft_the_update_could_not_reopen_is_said_with_its_text() {
    let m = settings_drafts_not_reopened(&[draft("font_family", "Menlo Nerd")], 0, None);
    assert_eq!(m.severity, Severity::Warn);
    assert_eq!(
        m.hold,
        Hold::Default,
        "a loss is on glass, not only on record"
    );
    assert_eq!(m.key.as_deref(), Some(KEY_SETTINGS_DRAFTS));
    assert_eq!(m.detail[0], "font_family: Menlo Nerd");
    assert!(
        m.actions.iter().any(|intent| matches!(
            intent,
            Intent::OpenSettings { route }
                if route == crate::native_settings::SettingsRoute::Home.path()
        )),
        "{:?}",
        m.actions
    );
    let words = m.detail.join(" ").to_lowercase();
    assert!(
        !words.contains("restart") && !words.contains("relaunch"),
        "{words}"
    );

    let unread = settings_drafts_not_reopened(&[draft("a", "1")], 2, None);
    assert!(
        unread
            .detail
            .iter()
            .any(|line| line == "2 drafts could not be read"),
        "{:?}",
        unread.detail
    );
    let many = (0..20)
        .map(|index| draft(&format!("k{index}"), "v"))
        .collect::<Vec<_>>();
    let capped = settings_drafts_not_reopened(&many, 0, None);
    assert!(
        capped.detail.iter().any(|line| line == "and 4 more"),
        "{:?}",
        capped.detail
    );
}

#[test]
fn the_switch_and_the_landing_share_the_flow_row() {
    let i = installing("9.9.9");
    assert_eq!(i.detail, vec![INSTALLING]);
    assert!(busy(&i));
    assert_eq!(
        i.hold,
        Hold::Live {
            stale_after: STALE_HANDOFF
        }
    );
    let f = finishing("9.9.9");
    assert_eq!(f.detail, vec![FINISHING]);
    assert!(busy(&f));
    assert_eq!(f.key, i.key);
    let l = landed("9.9.9", 7, 0, None);
    assert!(
        l.detail.is_empty(),
        "no claim about the shells: {:?}",
        l.detail
    );
    assert_eq!(l.glyph.ch(), '\u{2713}');
    assert_eq!(
        l.hold,
        Hold::LogOnly,
        "the echo is the glass's; the words a record"
    );
    assert_eq!(l.severity, Severity::Success);
    assert_eq!(l.key, None);
}

#[test]
fn the_landing_names_the_tabs_it_repainted() {
    let one = landed("0.92.0", 7, 1, None);
    assert_eq!(one.detail[0], "1 tab repainted");
    assert!(
        one.detail[1..]
            .join(" ")
            .starts_with("its screen was blank after the update"),
        "{:?}",
        one.detail
    );
    assert!(
        one.detail.join(" ").contains("asked to redraw"),
        "{:?}",
        one.detail
    );
    assert_eq!(one.severity, Severity::Success);
    assert_eq!(one.hold, Hold::LogOnly, "still a record (ruling 141)");
    assert!(
        one.actions.is_empty(),
        "nothing to press: {:?}",
        one.actions
    );
    let three = landed("0.92.0", 7, 3, None);
    assert_eq!(three.detail[0], "3 tabs repainted");
    assert!(
        three.detail[1..].join(" ").starts_with("their screens"),
        "{:?}",
        three.detail
    );
    assert!(landed("0.92.0", 7, 0, None).detail.is_empty());
}

#[test]
fn the_update_records_say_what_happened_when() {
    let s = switch_started(Some("0.79.0"), "0.78.0");
    assert!(s.detail[0].contains("aterm v0.78.0"), "{:?}", s.detail);
    assert_eq!(s.hold, Hold::LogOnly);
    let busy = "the park missed its budget on every rung (a PTY reader missed the 20 ms \
                handoff park deadline); the machine is busy, the candidate is not in \
                question, and the ladder retries";
    let said = |retries, why| {
        switch_stopped(Some("0.79.0"), "0.78.0", retries, why)
            .detail
            .join(" ")
    };
    let waiting = switch_stopped(Some("0.79.0"), "0.78.0", true, busy);
    assert!(
        waiting.detail.join(" ").contains("busy"),
        "{:?}",
        waiting.detail
    );
    assert_eq!(waiting.severity, Severity::Info);
    let unnamed = said(true, "x");
    assert!(!unnamed.contains(':'), "{unnamed}");
    let storage = "the park missed its budget on every rung (visible checkpoint set could \
                   not reserve bounded storage; update handoff stayed in place); the \
                   machine is busy, the candidate is not in question, and the ladder \
                   retries";
    for why in [
        storage,
        "visible checkpoint set could not reserve bounded storage; update handoff stayed \
         in place",
    ] {
        assert_eq!(said(true, why), unnamed, "{why}");
    }
    let not = switch_stopped(
        Some("0.79.0"),
        "0.78.0",
        false,
        "handoff proof ended TimedOut",
    );
    assert_ne!(not.detail.join(" "), said(false, "x"), "a timeout is named");
    assert_eq!(not.severity, Severity::Warn);
    let cap = "overlap handoff failed safely: the park's capture refused: tab 1 of window 1 \
               has 300 panes, more than the 256 in one tab an update can carry";
    assert!(
        said(false, cap).contains("300 panes"),
        "{}",
        said(false, cap)
    );
    assert_ne!(said(true, cap), said(false, cap));
    for m in [s, waiting, not] {
        let words = std::iter::once(m.title.clone())
            .chain(m.detail.clone())
            .collect::<Vec<_>>()
            .join(" ");
        for jargon in [
            "switch",
            "carried",
            "on its own",
            "build ",
            "park",
            "rung",
            "ladder",
            "candidate",
        ] {
            assert!(!words.contains(jargon), "{jargon:?} in {words:?}");
        }
    }
    let took = landed("0.79.0", 7, 0, Some(Duration::from_secs(42)));
    assert!(took.detail[0].contains(&span_words(Duration::from_secs(42))));
    assert_eq!(took.hold, Hold::LogOnly);
    for (secs, words) in [
        (42, "42 s"),
        (60, "1 min"),
        (185, "3 min 5 s"),
        (7200, "2 h"),
        (7800, "2 h 10 min"),
    ] {
        assert_eq!(span_words(Duration::from_secs(secs)), words);
    }
}

#[test]
fn every_stand_down_reason_is_said_in_a_persons_words() {
    for why in [
        "the terminal never offered a moment to pause in within the hold cap (120 s \
             after the successor dialled; the park was last held back because a video \
             recording is running)",
        "the terminal never offered a moment to pause in within the hold cap (120 s \
             after the successor dialled; the park was last held back because a session's \
             command has exited but its pane is still open)",
        "the terminal never offered a moment to pause in within the hold cap (120 s \
             after the successor dialled; the park was last held back because a keystroke \
             landed in an aterm window inside the typing gap)",
        "the terminal never offered a moment to pause in within the hold cap (120 s \
             after the successor dialled; the park was last held back because a session \
             has output waiting that its reader has not taken)",
        "the terminal never offered a moment to pause in within the hold cap (120 s \
             after the successor dialled)",
        "every terminal session closed while the successor booted",
        "build 9's handoff policy is being read before the fork lane parks; the \
             attempt after it ends parks under it",
        "build 9's pre-park verification is being read before the cold lane \
             relaunches; the attempt after it ends relaunches under it",
        "input or PTY output arrived before automatic reader park",
        "terminal activity has not reached the automatic-update quiet epoch",
        "a PTY reader missed the 20 ms handoff park deadline",
        "handoff proof capture exceeded the 20 ms deadline",
        "a PTY session closed during automatic reader park",
        "structural activity revoked the handoff while the successor held the claim",
        "a video recording is running",
    ] {
        let words = switch_stopped(Some("0.79.0"), "0.78.0", true, why)
            .detail
            .join(" ");
        for jargon in ["park", "rung", "PTY", "successor", "hold cap", "epoch"] {
            assert!(!words.contains(jargon), "{why}: {words}");
        }
    }
}

#[test]
fn a_failed_install_record_never_splices_the_ledgers_text() {
    let named = with_named_cause(
        "another download replaced it",
        "overlap handoff failed safely: handoff proof ended ChildDied",
    );
    assert!(named.starts_with("another download replaced it"), "{named}");
    assert!(
        !named.contains("ChildDied") && !named.contains("handoff"),
        "{named}"
    );
    assert_eq!(
        with_named_cause(
            "the next check downloads it again",
            "updater apply preflight became stale"
        ),
        "the next check downloads it again"
    );
}

#[test]
fn tabs_not_kept_offers_no_action_that_cannot_help() {
    let msg = cold_lane_tabs_not_kept("open restore lock: Permission denied (os error 13)");
    assert_eq!(msg.severity, Severity::Warn);
    assert!(msg.actions.is_empty(), "{:?}", msg.actions);
    assert_ne!(msg.key.as_deref(), Some(KEY_OUTCOME));
}

#[test]
fn a_capture_refusal_record_names_the_tabs_it_is_about() {
    assert!(
        capture_refused_waiting(
            "overlap handoff failed safely: the park's capture refused session 3: visible \
             checkpoint set could not be committed canonically: too many sessions",
            Duration::ZERO,
        )
        .starts_with("one tab couldn't move")
    );
    let layout = capture_refused_waiting(
        "overlap handoff failed safely: the park's capture refused: handoff layout could not \
         be committed canonically",
        Duration::ZERO,
    );
    assert!(
        layout.starts_with("the open tabs couldn't move"),
        "{layout}"
    );
    let over_cap = capture_refused_waiting(OVER_CAP_MESSAGE, Duration::ZERO);
    assert_eq!(
        over_cap.split(" \u{b7} ").next(),
        crate::update_apply_trouble::named_clause(OVER_CAP_MESSAGE),
        "the cause in the Version menu's words: {over_cap}"
    );
    assert!(
        over_cap.ends_with("tries again when you close some"),
        "{over_cap}"
    );
    assert!(
        layout.ends_with("tries again when the tabs change"),
        "{layout}"
    );
    for said in [layout, over_cap] {
        assert!(!said.contains("that tab"), "{said}");
    }
    for message in [
        "overlap handoff failed safely: the park's capture refused: 16500 tabs are open, more \
         than the 16384 (64 windows of 256) an update can carry",
        "the park's capture refused: 16500 tabs are open, more than the 16384 (64 windows of \
         256) an update can carry",
    ] {
        let said = capture_refused_waiting(message, Duration::ZERO);
        assert!(
            said.starts_with("16500 tabs are open") && said.ends_with("when you close some"),
            "{said}"
        );
    }
}

const OVER_CAP_MESSAGE: &str = "overlap handoff failed safely: could not write the \
                                authenticated handoff manifest (the open tabs' records are \
                                over the 4194304-byte limit one handover carries)";

#[test]
fn a_refusal_that_came_back_says_when_the_next_try_is() {
    use crate::app_native::{CAPTURE_REFUSAL_PROBE, capture_refusal_spacing};
    let left = |refusals| capture_refusal_spacing(refusals) - Duration::from_millis(3);
    let first = capture_refused_waiting(OVER_CAP_MESSAGE, left(1));
    assert!(
        first.ends_with("tries again when you close some"),
        "{first}"
    );
    assert!(!first.contains("soonest"), "{first}");
    let second = capture_refused_waiting(OVER_CAP_MESSAGE, left(2));
    assert!(second.ends_with("in 10 minutes at the soonest"), "{second}");
    let third = capture_refused_waiting(OVER_CAP_MESSAGE, left(3));
    assert!(third.ends_with("in 30 minutes at the soonest"), "{third}");
    let one_tab = capture_refused_waiting(
        "overlap handoff failed safely: the park's capture refused session 3: visible \
         checkpoint set could not be committed canonically: too many sessions",
        left(2),
    );
    assert!(
        one_tab.ends_with("tries again when that tab changes, in 10 minutes at the soonest"),
        "{one_tab}"
    );
    let near = capture_refused_waiting(OVER_CAP_MESSAGE, Duration::from_secs(90));
    assert!(near.ends_with(", in 2 minutes at the soonest"), "{near}");
    let one = capture_refused_waiting(
        OVER_CAP_MESSAGE,
        CAPTURE_REFUSAL_PROBE + Duration::from_secs(1),
    );
    assert!(one.ends_with(", in a minute at the soonest"), "{one}");
    assert_eq!(
        capture_refused_waiting(OVER_CAP_MESSAGE, left(3))
            .split(" \u{b7} ")
            .next(),
        crate::update_apply_trouble::named_clause(OVER_CAP_MESSAGE)
    );
}

#[test]
fn the_download_instant_round_trips_the_carry_for_its_build() {
    let now = Instant::now();
    let wall = SystemTime::now();
    let at = now - Duration::from_secs(42);
    let ms = verified_unix_ms(Some((7, at)), 7, now, wall).expect("this build");
    assert_eq!(
        verified_unix_ms(Some((7, at)), 8, now, wall),
        None,
        "another build"
    );
    assert_eq!(verified_unix_ms(None, 7, now, wall), None);
    let (build, back) = carried_verification(Some(ms), 7, now, wall).expect("carried");
    assert_eq!(build, 7);
    let took = now.saturating_duration_since(back);
    assert!(
        took >= Duration::from_secs(41) && took <= Duration::from_secs(43),
        "{took:?}"
    );
    assert_eq!(carried_verification(None, 7, now, wall), None);
}

#[test]
fn an_outcome_is_a_record_unless_the_person_acts_on_it() {
    let retry = outcome(
        "Couldn't install aterm v9.9.9",
        "will try again by itself",
        Severity::Info,
    );
    assert_eq!(retry.hold, Hold::LogOnly, "a self-retry is a record");
    assert!(retry.actions.is_empty());
    assert_eq!(retry.key.as_deref(), Some(KEY_OUTCOME));
    let warn = outcome("Update waits", "x", Severity::Warn);
    assert_eq!(warn.hold, Hold::LogOnly);
    assert_eq!(warn.actions, vec![software_update()], "the page's re-offer");
    let stopped = needs_install(
        "Couldn't install aterm v9.9.9",
        INSTALL_FROM_MENU,
        Severity::Warn,
        7,
    );
    assert_eq!(stopped.hold, Hold::Default, "the warning's hold");
    assert_eq!(stopped.actions, vec![Intent::ApplyUpdate { build: 7 }]);
    assert_eq!(stopped.glyph.ch(), NEEDS_YOU);
    assert_ne!(stopped.key, Some(KEY_PROGRESS.to_string()));
    let installed = needs_install(
        &crate::app_update_screen::update_installed_title(Some("9.9.9")),
        crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
        Severity::Info,
        7,
    );
    assert_eq!(installed.hold, Hold::For(HOLD_STAGED_MANUAL));
    assert_eq!(installed.actions, vec![Intent::ApplyUpdate { build: 7 }]);
    assert_eq!(installed.glyph.ch(), MOVING_IN);
    let ready = staged("9.9.9", 7, Some(ApplyPosture::ManualByConfig));
    assert_eq!(installed.title, ready.title, "one sentence…");
    assert_eq!(installed.glyph, ready.glyph, "…one mark");
    assert_eq!(
        installed.detail,
        [crate::app_update_screen::UPDATE_INSTALLED_DETAIL]
    );
    let bare = failed("Couldn't finish the update", "", false);
    assert!(bare.detail.is_empty(), "the capsules are the press");
    assert_eq!(bare.hold, Hold::Default);
    assert_eq!(bare.severity, Severity::Warn);
    assert_eq!(bare.actions, vec![software_update()]);
    let blocked = failed(
        crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
        crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
        true,
    );
    assert!(blocked.actions.is_empty(), "its words are the press");
}

#[test]
fn the_health_warning_names_the_half_and_since_when() {
    let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release manifests \
                exist but cannot be downloaded.";
    for class in ["apply", "pipeline", "stage", "manifest"] {
        let m = health_warning(
            HealthKind::of_class(class),
            aterm_update::health_failing_title(class),
            body,
        );
        assert!(m.title.starts_with("Couldn't "), "{}", m.title);
        assert_eq!(m.detail[0], "since Sep 14", "{class}");
        assert!(
            m.detail[1..]
                .join(" ")
                .contains("release manifests exist but cannot be downloaded."),
            "{:?}",
            m.detail
        );
        assert_eq!(m.hold, Hold::Default);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.key.as_deref(), Some(HealthKind::of_class(class).key()));
        assert_eq!(HealthKind::of_row(&m), Some(HealthKind::of_class(class)));
        assert_eq!(m.actions, vec![software_update()]);
        assert!(!m.detail.join(" ").contains("click"));
    }
    let undated = health_warning(
        HealthKind::Check,
        aterm_update::health_failing_title("manifest"),
        "no date here",
    );
    assert_eq!(undated.detail, vec!["no date here"]);
    let healed = health_recovered("Couldn't install updates", "since Sep 14");
    assert_eq!(
        healed.detail,
        vec!["after \"Couldn't install updates\", since Sep 14"]
    );
    assert_eq!(healed.hold, Hold::LogOnly);
}

#[test]
fn the_disabled_handoff_line_says_what_the_admission_classifier_does() {
    use crate::native_update_admission::{
        AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
    };
    let opted_out = |live_ptys: usize| AdmissionFacts {
        staged_verified: true,
        seamless_capable: false,
        native_state_certified: true,
        live_ptys,
        foreground_jobs: 0,
        unknown_foregrounds: 0,
        image_kept: false,
    };
    assert_eq!(
        classify(opted_out(1)),
        AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
        "one open terminal: the install is refused and nothing dies"
    );
    assert_eq!(
        classify(opted_out(0)),
        AdmissionDecision::Apply(ApplyLane::Cold),
        "no terminal at all: the cold re-exec"
    );
    for why in HandoffUnavailable::ALL {
        let line = staged_detail(ApplyPosture::HandoffDisabled {
            why,
            veto: None,
            image_kept: false,
        });
        assert!(
            line.starts_with(why.cause())
                && line.ends_with("installs once every terminal is closed"),
            "{line}"
        );
        let lower = line.to_lowercase();
        assert!(
            !lower.contains("survive")
                && !lower.contains("keep running")
                && !lower.contains("restart")
                && !lower.contains("relaunch")
                && !lower.contains("version menu"),
            "{line}"
        );
    }
    let kept = |seamless_capable: bool| AdmissionFacts {
        seamless_capable,
        image_kept: true,
        ..opted_out(0)
    };
    assert_eq!(
        classify(kept(false)),
        AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff),
        "a kept image the handoff cannot carry installs at a later launch"
    );
    assert_eq!(
        classify(kept(true)),
        AdmissionDecision::Apply(ApplyLane::Seamless),
        "a kept image with no terminal hands over: no tab is waited for"
    );
    for why in HandoffUnavailable::ALL {
        let line = staged_detail(ApplyPosture::HandoffDisabled {
            why,
            veto: None,
            image_kept: true,
        });
        assert!(
            line.starts_with(why.cause()) && line.ends_with("once aterm is quit and opened again"),
            "{line}"
        );
        assert!(!line.contains("terminal tab"), "{line}");
    }
}
