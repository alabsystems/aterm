// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_models_atpkg::{atpkg_index_probe_cooldown_model, atpkg_index_successor_selection_model};
use aterm_spec::derive::Model;
use std::cell::Cell;
use std::collections::BTreeMap;

struct ModeledCase {
    label: &'static str,
    action: &'static str,
    code: u16,
    location: Option<&'static str>,
    marker: char,
    /// The cooldown in the model's thirty-second ticks ([`INTERVAL`]).
    cooldown_ticks: u32,
    answer: Probe,
}

fn modeled_attempt(
    model: &Model,
    state: &mut BTreeMap<&'static str, i64>,
    layout: &Layout,
    at: SystemTime,
    case: &ModeledCase,
    should_request: bool,
) {
    assert!(model.fire("Acquire", state));
    let admitted = model.action_enabled(case.action, state);
    assert_eq!(admitted, should_request, "{} model guard", case.label);
    if !admitted {
        let buggy = aterm_spec::interp::with_buggy(model, 1);
        assert!(buggy.action_enabled(case.action, state));
    }
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let requests = Cell::new(0usize);
    let mut head = |url: &str| {
        requests.set(requests.get() + 1);
        assert!(!metered(url), "{url}");
        assert!(
            url.ends_with(&format!("/atpkg-index-{}/index.toml", floor + 1))
                || url.ends_with(&format!("/atpkg-index-{}/index.toml", floor + 2)),
            "{url}"
        );
        Ok(HeadAnswer {
            code: case.code,
            location: case.location.map(str::to_string),
        })
    };
    let budget = if admitted { OFFSETS.len() } else { 0 };
    let observed = probe_next(
        layout,
        "alabsystems",
        "aterm",
        floor,
        at,
        &mut head,
        &mut |_| {},
    );
    assert_eq!(requests.get(), budget, "{} wire budget", case.label);
    if admitted {
        assert_eq!(observed, case.answer);
        assert!(model.fire(case.action, state));
        assert_eq!(
            std::fs::read_to_string(layout.prefix.join(NEAR_LOCK)).unwrap(),
            format!("{floor} {}\n", case.marker),
            "the real writer must publish the marker the next reader uses"
        );
        stamped_at(layout, at);
    } else {
        assert_eq!(observed, Probe::Suppressed(Duration::from_secs(1)));
        assert!(model.fire("SkipFresh", state));
    }
    assert!(model.fire("Release", state));
    assert!(model.check_invariant("NoDuplicateRangeInsideCooldown", state));
}

fn layout(name: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-index-probe-{name}-"))
}

/// Pin the stamp's mtime to the synthetic instant `at`: its age is read from the real
/// mtime, so a test stalled before the write would leave it younger than the clock says.
fn stamped_at(layout: &Layout, at: SystemTime) {
    File::options()
        .write(true)
        .open(layout.prefix.join(NEAR_LOCK))
        .and_then(|file| file.set_modified(at))
        .unwrap();
}

/// A second host takes the shared stamp's remaining wait, not another
/// complete local interval.
#[test]
fn a_suppressed_probe_reports_the_exact_shared_cooldown_without_a_head() {
    for (marker, lifetime) in [('M', INTERVAL), ('P', INTERVAL), ('E', RETRY_AFTER_ERROR)] {
        let layout = layout(&format!("shared-handoff-{marker}"));
        std::fs::write(layout.floor(), "43").unwrap();
        std::fs::write(layout.prefix.join(NEAR_LOCK), format!("43 {marker}\n")).unwrap();
        let stamped = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        stamped_at(&layout, stamped);
        let requests = Cell::new(0);
        let mut head = |_: &str| {
            requests.set(requests.get() + 1);
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        };
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                stamped + lifetime - Duration::from_secs(5),
                &mut head,
            ),
            Probe::Suppressed(Duration::from_secs(5))
        );
        assert_eq!(requests.get(), 0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                stamped - Duration::from_secs(5),
                &mut head,
            ),
            Probe::Suppressed(lifetime + Duration::from_secs(5))
        );
        assert_eq!(requests.get(), 0);
        assert_eq!(
            successor_with(
                &layout,
                "alabsystems",
                "aterm",
                stamped + lifetime,
                &mut head,
            ),
            Probe::Missing
        );
        assert_eq!(
            requests.get(),
            2,
            "only the stamp expiry spends a HEAD pair"
        );
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }
}

#[test]
fn real_stamp_writer_reader_and_cooldowns_refine_the_derived_model() {
    let model = atpkg_index_probe_cooldown_model();
    let cases = [
        ModeledCase {
            label: "missing",
            action: "ProbeMissing",
            code: 404,
            location: None,
            marker: 'M',
            cooldown_ticks: 1,
            answer: Probe::Missing,
        },
        ModeledCase {
            label: "error",
            action: "ProbeError",
            code: 503,
            location: None,
            marker: 'E',
            cooldown_ticks: 2,
            answer: Probe::Deferred,
        },
        ModeledCase {
            label: "rate-limited",
            action: "ProbeError",
            code: 429,
            location: None,
            marker: 'E',
            cooldown_ticks: 2,
            answer: Probe::Deferred,
        },
        ModeledCase {
            label: "published",
            action: "ProbePublished",
            code: 302,
            location: Some("https://release-assets.githubusercontent.com/object"),
            marker: 'P',
            cooldown_ticks: 1,
            answer: Probe::Published(45),
        },
    ];
    for case in cases {
        let layout = layout(&format!("modeled-{}", case.label));
        std::fs::write(layout.floor(), "43").unwrap();
        let t0 = SystemTime::now();
        let mut state = model.init_state();
        modeled_attempt(&model, &mut state, &layout, t0, &case, true);
        modeled_attempt(
            &model,
            &mut state,
            &layout,
            t0 + INTERVAL * case.cooldown_ticks - Duration::from_secs(1),
            &case,
            false,
        );
        for _ in 0..case.cooldown_ticks {
            assert!(model.fire("AdvanceTick", &mut state));
        }
        modeled_attempt(
            &model,
            &mut state,
            &layout,
            t0 + INTERVAL * case.cooldown_ticks + Duration::from_secs(10),
            &case,
            true,
        );
        if case.marker == 'M' {
            std::fs::write(layout.floor(), "44").unwrap();
            assert!(model.fire("AdvanceFloor", &mut state));
            modeled_attempt(
                &model,
                &mut state,
                &layout,
                t0 + INTERVAL * case.cooldown_ticks + Duration::from_secs(11),
                &case,
                true,
            );
        }
        std::fs::remove_dir_all(&layout.prefix).unwrap();
    }
}

/// The first HEAD to find a published index reports it before the second returns; the
/// final answer is still the higher of the pair.
#[test]
fn a_published_index_is_reported_before_the_second_head_returns() {
    let layout = layout("near-early");
    std::fs::write(layout.floor(), "43").unwrap();
    let reported = std::cell::RefCell::new(Vec::<u64>::new());
    let order = std::cell::RefCell::new(Vec::<String>::new());
    let mut head = |url: &str| {
        order.borrow_mut().push(format!("head {url}"));
        let found = url.ends_with("/atpkg-index-44/index.toml")
            || url.ends_with("/atpkg-index-45/index.toml");
        Ok(HeadAnswer {
            code: if found { 302 } else { 404 },
            location: found.then(|| "https://release-assets.githubusercontent.com/o".into()),
        })
    };
    let answer = successor_notifying_with(
        &layout,
        "alabsystems",
        "aterm",
        SystemTime::now(),
        &mut head,
        &mut |build| {
            reported.borrow_mut().push(build);
            order.borrow_mut().push(format!("hint {build}"));
        },
    );
    assert_eq!(answer, Probe::Published(45));
    assert_eq!(*reported.borrow(), vec![44, 45]);
    let order = order.into_inner();
    let hint44 = order.iter().position(|e| e == "hint 44").unwrap();
    let head45 = order
        .iter()
        .position(|e| e.ends_with("atpkg-index-45/index.toml"))
        .unwrap();
    assert!(
        hint44 < head45,
        "44 is reported before 45 is asked: {order:?}"
    );
    std::fs::remove_dir_all(&layout.prefix).unwrap();
}

/// The probe lock is free the moment its probe releases it, whatever copies of its
/// descriptor live on (`copy` stands in for a child mid-spawn on another thread).
#[test]
fn the_probe_lock_is_free_the_moment_its_probe_releases_it() {
    let layout = layout("released");
    std::fs::write(layout.floor(), "46").unwrap();
    let path = layout.prefix.join(NEAR_LOCK);
    let floor = crate::sig::Floor::new(layout.floor()).current();
    let requests = Cell::new(0);
    let mut head = |_: &str| {
        requests.set(requests.get() + 1);
        Ok(HeadAnswer {
            code: 404,
            location: None,
        })
    };
    let probe = |head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>| {
        probe_next(
            &layout,
            "alabsystems",
            "aterm",
            floor,
            SystemTime::now(),
            head,
            &mut |_| {},
        )
    };

    let holder = take_probe_lock(&path).expect("a free probe lock is taken");
    assert!(
        take_probe_lock(&path).is_none(),
        "a held probe lock refuses"
    );
    assert_eq!(probe(&mut head), Probe::Deferred);
    assert_eq!(requests.get(), 0, "a held lock spends no request");

    let copy = holder.try_clone().expect("a copy of the descriptor");
    drop(holder);
    assert!(
        take_probe_lock(&path).is_some(),
        "a released probe lock must be free while a copy of its descriptor lives"
    );
    assert_eq!(probe(&mut head), Probe::Missing);
    assert_eq!(
        requests.get(),
        OFFSETS.len(),
        "the probe after a release goes to the wire"
    );
    drop(copy);
    std::fs::remove_dir_all(&layout.prefix).unwrap();
}

/// The pair's answer refines `AtpkgIndexSuccessorSelection`: for every pair of HEAD
/// answers (missing, a 503, published) the probe answers what the model chooses — the
/// higher published build over a lower one and over uncertainty, `Missing` only when
/// both missed.
#[test]
fn the_real_head_pair_selection_refines_the_derived_model() {
    #[derive(Clone, Copy)]
    enum Answer {
        Missing,
        Deferred,
        Published,
    }
    let model = atpkg_index_successor_selection_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let answers = [Answer::Missing, Answer::Deferred, Answer::Published];
    let mut disagreed = 0;
    for first in answers {
        for second in answers {
            let layout = layout("selection");
            std::fs::write(layout.floor(), "43").unwrap();
            let asked = Cell::new(0usize);
            let mut head = |url: &str| {
                asked.set(asked.get() + 1);
                let answer = if url.ends_with("/atpkg-index-44/index.toml") {
                    first
                } else {
                    second
                };
                Ok(match answer {
                    Answer::Missing => HeadAnswer {
                        code: 404,
                        location: None,
                    },
                    Answer::Deferred => HeadAnswer {
                        code: 503,
                        location: None,
                    },
                    Answer::Published => HeadAnswer {
                        code: 302,
                        location: Some("https://release-assets.githubusercontent.com/o".into()),
                    },
                })
            };
            let real = successor_with(
                &layout,
                "alabsystems",
                "aterm",
                SystemTime::now(),
                &mut head,
            );
            let observe = |answer: Answer, high: bool| match answer {
                Answer::Missing => "ObserveMissing",
                Answer::Deferred => "ObserveDeferred",
                Answer::Published if high => "ObserveHigh",
                Answer::Published => "ObserveLow",
            };
            let chosen = |m: &Model| {
                let mut state = m.init_state();
                for action in [observe(first, false), observe(second, true), "Choose"] {
                    assert!(m.fire(action, &mut state), "{action}");
                }
                match state["chosen"] {
                    1 => Probe::Missing,
                    2 => Probe::Deferred,
                    3 => Probe::Published(44),
                    5 => Probe::Published(45),
                    other => panic!("unmapped choice {other}"),
                }
            };
            assert_eq!(real, chosen(&model), "the model's choice");
            if chosen(&buggy) != real {
                disagreed += 1;
            }
            std::fs::remove_dir_all(&layout.prefix).unwrap();
        }
    }
    assert!(disagreed > 0, "the historical choice must differ somewhere");
}

/// What the doctor reads back: the probe's last answer for the current floor, and
/// nothing for a stamp an older floor wrote.
#[test]
fn the_cached_answer_is_the_stamp_for_the_current_floor() {
    let layout = layout("cached");
    std::fs::write(layout.floor(), "43").unwrap();
    assert_eq!(cached_answer(&layout).near, None, "never probed");
    let head = |_: &str| {
        Ok(HeadAnswer {
            code: 404,
            location: None,
        })
    };
    let t0 = SystemTime::now();
    let asked = Cell::new(0);
    let mut counting = |url: &str| {
        asked.set(asked.get() + 1);
        head(url)
    };
    assert_eq!(
        successor_with(&layout, "alabsystems", "aterm", t0, &mut counting),
        Probe::Missing
    );
    stamped_at(&layout, t0);
    let cached = cached_answer(&layout);
    assert_eq!(cached.floor, 43);
    assert_eq!(cached.near, Some((RangeAnswer::Missing, t0)));
    std::fs::write(layout.floor(), "44").unwrap();
    assert_eq!(cached_answer(&layout).near, None, "an older floor's stamp");
    std::fs::remove_dir_all(&layout.prefix).unwrap();
}

/// A request on GitHub's metered REST API, which the probe must never make.
fn metered(url: &str) -> bool {
    url.starts_with("https://api.github.com/")
}

#[test]
fn only_the_standard_public_source_is_probed() {
    assert!(standard_public_source("alabsystems", "aterm"));
    assert!(!standard_public_source("private-owner", "aterm"));
    assert!(!standard_public_source("alabsystems", "private-repo"));
}

/// Only a 302 or 307 to an https URL under `githubusercontent.com` is a publication,
/// including a host GitHub has not used yet (`release-objects.`); a 404 says "not yet",
/// and every other answer (a 200, a permanent or see-other redirect, a redirect anywhere
/// else, no Location, an error) says nothing.
#[test]
fn only_a_redirect_to_the_asset_storage_is_a_publication() {
    let head = |code, location: Option<&str>| {
        classify(
            45,
            Ok(HeadAnswer {
                code,
                location: location.map(str::to_string),
            }),
        )
    };
    for host in [
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
        "release-objects.githubusercontent.com",
        "eu.release-assets.githubusercontent.com",
    ] {
        for code in [302, 307] {
            assert_eq!(
                head(code, Some(&format!("https://{host}/object?sig=x"))),
                Probe::Published(45),
                "{code} {host}"
            );
        }
    }
    let refused = [
        (200, None),
        (
            301,
            Some("https://github.com/new-owner/aterm/releases/download/atpkg-index-45/index.toml"),
        ),
        (308, Some("https://release-assets.githubusercontent.com/x")),
        (303, Some("https://release-assets.githubusercontent.com/x")),
        (302, Some("https://sso.example.com/login?next=/releases")),
        (302, Some("http://release-assets.githubusercontent.com/x")),
        (
            302,
            Some("https://release-assets.githubusercontent.com.evil.com/x"),
        ),
        (
            302,
            Some("https://user@release-assets.githubusercontent.com/x"),
        ),
        (302, Some("https://githubusercontent.com/x")),
        (302, Some("https://evilgithubusercontent.com/x")),
        (302, Some("https://.githubusercontent.com/x")),
        (302, Some("https://a..githubusercontent.com/x")),
        (
            302,
            Some("https://release-objects.githubusercontent.com.evil.com/x"),
        ),
        (302, Some("http://release-objects.githubusercontent.com/x")),
        (302, None),
        (302, Some("")),
        (503, None),
    ];
    for (code, location) in refused {
        assert_eq!(head(code, location), Probe::Deferred, "{code} {location:?}");
    }
    assert_eq!(head(404, None), Probe::Missing);
    assert_eq!(
        classify(45, Err(HttpError::Transport("offline".into()))),
        Probe::Deferred
    );
}
