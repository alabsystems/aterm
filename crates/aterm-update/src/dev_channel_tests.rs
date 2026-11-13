// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn evergreen() -> String {
    pointer::latest_download_url(crate::DEFAULT_OWNER, crate::DEFAULT_REPO, APPCAST_ASSET)
        .expect("the compiled channel is URL-safe")
}

fn channel_at(tag: &'static str) -> impl FnMut(&str) -> Result<HeadAnswer, HttpError> {
    move |url: &str| {
        assert_eq!(
            url.split_once("?aterm_lookup=").unwrap().0,
            evergreen(),
            "only the public evergreen appcast is asked"
        );
        Ok(HeadAnswer {
            code: 302,
            location: Some(format!(
                "https://github.com/{}/{}/releases/download/{tag}/{APPCAST_ASSET}",
                crate::DEFAULT_OWNER,
                crate::DEFAULT_REPO
            )),
        })
    }
}

const DEV: WatchGate = WatchGate {
    automatic: true,
    dev_marked: true,
};

fn run(
    gate: WatchGate,
    running: &str,
    cycles: usize,
    head: &mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>,
) -> (Vec<DevLag>, usize, Vec<Duration>) {
    let mut requests = 0usize;
    let mut told = Vec::new();
    let mut waits = Vec::new();
    let mut counted = |url: &str| {
        requests += 1;
        head(url)
    };
    watch_with(
        gate,
        running,
        &mut counted,
        &mut |lag| told.push(lag),
        &mut |period| {
            waits.push(period);
            waits.len() < cycles
        },
    );
    (told, requests, waits)
}

#[test]
fn a_dev_build_is_told_its_standing_from_one_head() {
    let latest = || "v0.93.0".to_string();
    for (running, want) in [
        (
            "0.91.0",
            DevLag::Behind {
                latest: latest(),
                releases: Some(2),
            },
        ),
        ("0.93.0", DevLag::Current { latest: latest() }),
        ("0.94.0", DevLag::Ahead { latest: latest() }),
    ] {
        let (told, requests, waits) = run(DEV, running, 1, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 1, "{running}: one HEAD per cycle, never a GET");
        assert_eq!(waits, vec![RECHECK_EVERY], "{running}: then it waits a day");
        assert_eq!(told[0].is_behind(), running == "0.91.0", "{running}");
        assert_eq!(told, vec![want], "{running}");
    }
}

#[test]
fn an_unreachable_channel_says_nothing() {
    let answers: [Box<dyn Fn() -> Result<HeadAnswer, HttpError>>; 5] = [
        Box::new(|| {
            Err(HttpError::Transport(
                "curl: (6) Could not resolve host".into(),
            ))
        }),
        Box::new(|| {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        }),
        Box::new(|| {
            Ok(HeadAnswer {
                code: 503,
                location: None,
            })
        }),
        Box::new(|| {
            Ok(HeadAnswer {
                code: 302,
                location: Some("https://evil.example/x/aterm-appcast.toml".into()),
            })
        }),
        Box::new(|| {
            Ok(HeadAnswer {
                code: 302,
                location: Some(format!(
                    "https://github.com/{}/{}/releases/download/atpkg-index-50/{APPCAST_ASSET}",
                    crate::DEFAULT_OWNER,
                    crate::DEFAULT_REPO
                )),
            })
        }),
    ];
    for (i, answer) in answers.iter().enumerate() {
        let (told, requests, waits) = run(DEV, "0.91.0", 3, &mut |_| answer());
        assert!(told.is_empty(), "answer {i}: nothing is told");
        assert_eq!(requests, 3, "answer {i}: one HEAD a day, no retry storm");
        assert_eq!(waits.len(), 3, "answer {i}: the schedule holds");
    }
}

#[test]
fn automatic_checks_off_or_a_release_copy_asks_nothing() {
    for gate in [
        WatchGate {
            automatic: false,
            dev_marked: true,
        },
        WatchGate {
            automatic: true,
            dev_marked: false,
        },
        WatchGate {
            automatic: false,
            dev_marked: false,
        },
    ] {
        let (told, requests, waits) = run(gate, "0.91.0", 3, &mut channel_at("v0.93.0"));
        assert_eq!(requests, 0, "{gate:?}: no request at all");
        assert!(told.is_empty() && waits.is_empty(), "{gate:?}");
        assert!(!should_watch(gate));
    }
    let (_, requests, _) = run(DEV, "0.91.0", 1, &mut channel_at("v0.93.0"));
    assert_eq!(requests, 1, "the control: a dev build with checks on asks");
}

#[test]
fn the_watch_tells_a_change_once() {
    let heads = ["v0.93.0", "v0.93.0", "offline", "v0.93.0", "v0.94.0"];
    let mut day = 0usize;
    let mut head = |_: &str| {
        let tag = heads[day.min(heads.len() - 1)];
        day += 1;
        if tag == "offline" {
            return Err(HttpError::Transport("offline".into()));
        }
        Ok(HeadAnswer {
            code: 302,
            location: Some(format!(
                "https://github.com/{}/{}/releases/download/{tag}/{APPCAST_ASSET}",
                crate::DEFAULT_OWNER,
                crate::DEFAULT_REPO
            )),
        })
    };
    let (told, requests, _) = run(DEV, "0.91.0", heads.len(), &mut head);
    assert_eq!(requests, heads.len());
    assert_eq!(
        told,
        vec![
            DevLag::Behind {
                latest: "v0.93.0".into(),
                releases: Some(2),
            },
            DevLag::Behind {
                latest: "v0.94.0".into(),
                releases: Some(3),
            },
        ]
    );
}

#[test]
fn the_count_is_minors_within_a_major() {
    assert_eq!(
        classify("0.92.0", "v0.93.0"),
        Some(DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(1),
        })
    );
    let patch_lag = classify("0.93.0", "v0.93.1").expect("patch lag");
    assert_eq!(
        patch_lag,
        DevLag::Behind {
            latest: "v0.93.1".into(),
            releases: None,
        }
    );
    assert_eq!(patch_lag.words(), "older than aterm v0.93.1");
    assert_eq!(
        DevLag::Behind {
            latest: "v0.93.1".into(),
            releases: Some(0),
        }
        .words(),
        "older than aterm v0.93.1"
    );
    assert_eq!(
        classify("0.93.0", "v1.0.0"),
        Some(DevLag::Behind {
            latest: "v1.0.0".into(),
            releases: None,
        })
    );
    assert_eq!(
        classify("1.0.0", "v0.99.0").map(|l| l.is_behind()),
        Some(false)
    );
    for (running, tag) in [("0.93", "v0.93.0"), ("0.93.0", "v0.93"), ("x", "v0.93.0")] {
        assert_eq!(classify(running, tag), None, "{running} / {tag}");
    }
}
