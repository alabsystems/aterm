// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update_core::{HeadAnswer, HttpError};

const SLUG: &str = "alabsystems/aterm";
const TAG: &str = "v0.92.0";

fn served() -> std::result::Result<HeadAnswer, HttpError> {
    Ok(HeadAnswer {
        code: 302,
        location: Some(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x".into(),
        ),
    })
}

fn status(code: u16) -> std::result::Result<HeadAnswer, HttpError> {
    Ok(HeadAnswer {
        code,
        location: None,
    })
}

fn names() -> Vec<String> {
    channel::required_asset_names("0.92.0", true, true)
}

fn run(
    names: &[String],
    answer: impl Fn(&str, usize) -> std::result::Result<HeadAnswer, HttpError> + Sync,
) -> (Result<u32>, Vec<String>) {
    let asked = std::sync::Mutex::new(Vec::new());
    let out = prove_assets_download_anonymously(SLUG, TAG, names, &|url| {
        let mut asked = asked.lock().unwrap();
        asked.push(url.to_string());
        let nth = asked.iter().filter(|u| *u == url).count();
        drop(asked);
        answer(url, nth)
    });
    (out, asked.into_inner().unwrap())
}

#[test]
fn the_readability_proof_never_asks_the_api_host() {
    let names = names();
    let (out, mut asked) = run(&names, |_, _| served());
    assert_eq!(out.unwrap(), names.len() as u32);
    let mut want: Vec<String> = names
        .iter()
        .map(|n| format!("https://github.com/{SLUG}/releases/download/{TAG}/{n}"))
        .collect();
    asked.sort();
    want.sort();
    assert_eq!(asked, want, "one HEAD per required asset, nothing else");
    for url in &asked {
        assert!(
            !aterm_update_core::cdn::is_api_host(url) && !url.contains("api.github.com"),
            "the channel's anonymous proof spent the metered API: {url}"
        );
    }
    let retired = format!(
        "{}/repos/{SLUG}/releases/tags/{TAG}",
        aterm_update_core::github_rest::Origins::github().api
    );
    assert!(aterm_update_core::cdn::is_api_host(&retired));
}

#[test]
fn a_refusal_is_asked_once_and_is_not_called_a_rate_limit() {
    for code in [403_u16, 429] {
        let (out, asked) = run(&names()[..1], |_, _| status(code));
        out.expect_err("a refused channel is not proved readable");
        assert_eq!(
            asked.len(),
            1,
            "HTTP {code} was asked {} times",
            asked.len()
        );
    }
}

#[test]
fn a_404_is_a_final_absence_answer_and_is_never_retried() {
    let names = names();
    let (out, asked) = run(&names[..1], |_, _| status(404));
    let refused = out.expect_err("an absent channel is refused").to_string();
    assert_eq!(asked.len(), 1);
    assert!(refused.contains("final absence answer"), "{refused}");
}

/// NEGATIVE CONTROLS: a serial dispatcher cannot open the gate; ignoring
/// any one of the four results accepts the corresponding planted absence.
#[test]
fn concurrent_heads_join_and_refuse_each_missing_asset() {
    use std::sync::{Condvar, Mutex};
    let names: Vec<String> = (0..4).map(|n| format!("asset-{n}")).collect();
    for missing in &names {
        let entered = (Mutex::new(0), Condvar::new());
        let finished = std::sync::atomic::AtomicUsize::new(0);
        let result = prove_assets_download_anonymously(SLUG, TAG, &names, &|url| {
            let mut count = entered.0.lock().unwrap();
            *count += 1;
            entered.1.notify_all();
            let (count, _) = entered
                .1
                .wait_timeout_while(count, Duration::from_secs(60), |count| *count < 4)
                .unwrap();
            assert_eq!(*count, 4, "all four HEADs enter before any returns");
            drop(count);
            finished.fetch_add(1, Ordering::SeqCst);
            if url.ends_with(missing) {
                status(404)
            } else {
                served()
            }
        });
        assert!(result.unwrap_err().to_string().contains(missing));
        assert_eq!(finished.load(Ordering::SeqCst), 4);
    }
}

/// Fixed-service timed model of the production HEAD dispatcher. Both
/// schedules ask the same thirteen URLs once. No latency assertion.
#[test]
#[ignore = "explicit anonymous HEAD scheduling measurement"]
fn anonymous_heads_abba() {
    let mut names = names();
    names.push(crate::service_qualification::REPORT_ASSET.to_string());
    let read = |_: &str| {
        std::thread::sleep(Duration::from_millis(40));
        served()
    };
    for (take, parallel) in [false, true, true, false].into_iter().enumerate() {
        let start = std::time::Instant::now();
        let issued = if parallel {
            prove_assets_download_anonymously(SLUG, TAG, &names, &read).unwrap()
        } else {
            names
                .iter()
                .map(|name| {
                    prove_assets_download_anonymously(SLUG, TAG, std::slice::from_ref(name), &read)
                        .unwrap()
                })
                .sum()
        };
        assert_eq!(issued as usize, names.len());
        println!(
            "anonymous-heads take={take} parallel={parallel} requests={issued} wall_s={:.6}",
            start.elapsed().as_secs_f64()
        );
    }
}

#[test]
fn only_a_redirect_into_the_asset_storage_reads_as_downloadable() {
    let at = |code: u16, location: &str| {
        classify_asset_head(Ok(HeadAnswer {
            code,
            location: Some(location.to_string()),
        }))
    };
    assert_eq!(
        at(302, "https://release-assets.githubusercontent.com/x"),
        AssetHead::Downloads
    );
    assert_eq!(
        at(307, "https://objects.githubusercontent.com/x"),
        AssetHead::Downloads
    );
    assert_eq!(
        at(302, "https://release-objects.githubusercontent.com/x"),
        AssetHead::Downloads
    );
    for (code, location) in [
        (301, "https://release-assets.githubusercontent.com/x"),
        (302, "http://release-assets.githubusercontent.com/x"),
        (
            302,
            "https://release-assets.githubusercontent.com.evil.example/x",
        ),
        (302, "https://user@release-assets.githubusercontent.com/x"),
        (302, "https://githubusercontent.com/x"),
        (302, "https://portal.example/login"),
    ] {
        assert!(
            matches!(at(code, location), AssetHead::Inconclusive(_)),
            "{code} {location} must not read as downloadable"
        );
    }
    assert!(matches!(
        classify_asset_head(status(200)),
        AssetHead::Inconclusive(_)
    ));
    assert_eq!(classify_asset_head(status(404)), AssetHead::NotServed);
    assert_eq!(classify_asset_head(status(403)), AssetHead::Refused(403));
    assert_eq!(classify_asset_head(status(429)), AssetHead::Refused(429));
    assert!(matches!(
        classify_asset_head(Err(HttpError::Transport("curl: (6) DNS".into()))),
        AssetHead::Inconclusive(_)
    ));
}
