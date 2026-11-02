// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use super::*;

fn secs(n: u64) -> Option<Duration> {
    Some(Duration::from_secs(n))
}

/// Each program's 429 as its screen draws it: the Responses API's sentence (Muse Code, the lab
/// stub's body), the same sentence wrapped across rows, agy's `RetryInfo` dump wrapped across
/// rows, and a Gemini error's own sentence. (Claude Code's names no number: the Anthropic API
/// puts its wait in a `retry-after` header alone, [`words_with_no_number_name_no_wait`].)
#[test]
fn a_429_names_its_wait_in_each_programs_words() {
    for (message, want) in [
        (
            "model failed: API error 429: Rate limit reached for requests per min. Please try again in 5s. (rate_limit_exceeded)",
            secs(5),
        ),
        (
            "model failed: API error 429: Rate limit reached for requests per min.\nPlease try again in 60s. (rate_limit_exceeded)",
            secs(60),
        ),
        (
            "agent executor error: generating and executing: Error 429, … map[@type:type.googleapis.com/google.rpc.RetryInfo retryDelay:47.2183 63629s]]",
            secs(48),
        ),
        (
            "You exceeded your current quota. Please retry in 33.35s.",
            secs(34),
        ),
        ("Rate limited: try again in 1m30s", secs(90)),
        ("Rate limited: try again in 250ms", secs(1)),
        ("rate limited, retry after 30 seconds", secs(30)),
        ("rate limited, try again in 2 minutes", secs(120)),
        ("Too many requests. Try again in 1h.", secs(3600)),
        (
            "model failed: API error 429: Rate limit reached for requests per min. Please try \
             again in 1 50s. (rate_limit_exceeded)",
            secs(150),
        ),
        ("Please try a gain in 30 s.", secs(30)),
    ] {
        assert_eq!(retry_after(message), want, "{message}");
    }
}

/// Words with no number name no wait: Claude Code's own 429 and 529 (the real API's bodies), the
/// login refresh's `Try again in a minute`, and a unit no span knows.
#[test]
fn words_with_no_number_name_no_wait() {
    for message in [
        "API Error: Request rejected (429) · This request would exceed your organization's rate limit of 50 requests per minute.",
        "API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment.",
        "Could not refresh your login because another Claude Code process is refreshing it · Try again in a minute",
        "API Error: 429 The request was rate limited. Please try again later.",
        "try again in 5 fortnights",
        "retryDelay:soon]",
    ] {
        assert_eq!(retry_after(message), None, "{message}");
    }
}

/// The first phrase with a number wins, so a sentence that names its wait after one that names
/// none still reads it.
#[test]
fn the_first_phrase_followed_by_a_number_names_the_wait() {
    assert_eq!(
        retry_after("try again in a moment; the server says try again in 12s"),
        secs(12)
    );
}

/// A remote wall cannot overflow the parser before its caller applies the retry cap. Both
/// fractional addition and rounding past u128::MAX refuse, while the exact boundary parses.
#[test]
fn an_out_of_range_fraction_or_rounding_refuses_instead_of_overflowing() {
    let whole = u128::MAX / 1000;
    assert_eq!(number(&format!("{whole}.455s")), Some((u128::MAX, "s")));
    for fraction in ["999", "4551", "456"] {
        assert_eq!(number(&format!("{whole}.{fraction}s")), None);
        assert_eq!(
            retry_after(&format!("Please try again in {whole}.{fraction}s")),
            None
        );
    }
    assert_eq!(retry_after("Please try again in 1.0001s"), secs(2));
}
