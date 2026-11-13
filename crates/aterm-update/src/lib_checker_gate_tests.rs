// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::unreachable_before_the_channel;

#[test]
fn only_this_checks_unreachable_channel_takes_the_quick_rungs() {
    let dns = "curl HEAD https://github.com/alabsystems/aterm/releases/latest/download/\
               aterm-appcast.toml failed (exit status: 6): curl: (6) Could not resolve \
               host: github.com";
    let stalled = "zip download failed: curl download failed (exit status: 28): curl: \
                   (28) Operation too slow. Less than 4096 bytes/sec transferred the last \
                   30 seconds";
    let started = "2026-09-23T12:28:12Z";
    let filed = |kind: &str, at: &str| crate::health::Health {
        kind: kind.to_string(),
        last_failure_at: at.to_string(),
        ..Default::default()
    };
    assert!(unreachable_before_the_channel(
        dns,
        &filed("network", started),
        started
    ));
    assert!(unreachable_before_the_channel(
        "curl HEAD x failed (exit status: 5): curl: (5) Could not resolve proxy: proxy.invalid",
        &filed("network", started),
        started
    ));
    assert!(unreachable_before_the_channel(
        dns,
        &filed("network", "2026-09-23T12:28:15Z"),
        started
    ));
    assert!(
        !unreachable_before_the_channel(stalled, &filed("pipeline", started), started),
        "a stalled container download is not a network that is down"
    );
    assert!(
        !unreachable_before_the_channel(dns, &filed("network", "2026-09-23T12:18:12Z"), started),
        "an earlier check's record says nothing about this one"
    );
    assert!(!unreachable_before_the_channel(
        "parse appcast: EOF while parsing",
        &filed("network", started),
        started
    ));
    assert!(!unreachable_before_the_channel(
        dns,
        &crate::health::Health::default(),
        started
    ));
}
