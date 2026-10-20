// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pricing and Codex conversation regressions. All files are composed in
//! scratch directories; no real account home or vendor process is consulted.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::harness::codex_rollout::fixtures::{context, count, meta};
use crate::harness::usage::TranscriptUsage;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn price_table_prices_only_what_the_caller_supplied() {
    let spend = TokenUsage {
        input: 1_000_000,
        output: 500_000,
        cache_write: 200_000,
        cache_write_1h: 0,
        cache_read: 4_000_000,
    };
    let empty = PriceTable::new();
    assert_eq!(
        empty.message_usd("claude-fable-5-1", &spend),
        None,
        "no built-in prices"
    );
    let table = PriceTable::new().with("claude-fable-5-1", sonnet_like());
    let usd = table
        .message_usd("claude-fable-5-1", &spend)
        .expect("priced");
    // 3 + 7.5 + 0.75 + 1.2
    assert!((usd - 12.45).abs() < 1e-9, "{usd}");
    assert_eq!(
        table.message_usd("claude-fable-5", &spend),
        None,
        "exact id only"
    );
    let bad = PriceTable::new().with(
        "m",
        ModelPrice::flat(Rates {
            input: -1.0,
            ..free_cache()
        }),
    );
    assert_eq!(
        bad.message_usd("m", &spend),
        None,
        "a negative price is not a price"
    );
    let nan = PriceTable::new().with(
        "m",
        ModelPrice::flat(Rates {
            output: f64::NAN,
            ..free_cache()
        }),
    );
    assert_eq!(nan.message_usd("m", &spend), None);
    // NEGATIVE CONTROL: the same row with valid prices prices.
    let good = PriceTable::new().with(
        "m",
        ModelPrice::flat(Rates {
            input: 1.0,
            ..free_cache()
        }),
    );
    assert!(
        good.message_usd("m", &spend)
            .is_some_and(|u| (u - 1.0).abs() < 1e-9)
    );
}

/// Every cache class priced at nothing, so a row's validity alone decides.
fn free_cache() -> Rates {
    Rates {
        cache_write: Some(0.0),
        cache_write_1h: Some(0.0),
        cache_read: Some(0.0),
        ..Rates::default()
    }
}

/// $3 / $15 / $3.75 (five-minute writes) / $6 (one-hour writes) / $0.30.
fn sonnet_like() -> ModelPrice {
    ModelPrice::flat(Rates {
        input: 3.0,
        output: 15.0,
        cache_write: Some(3.75),
        cache_write_1h: Some(6.0),
        cache_read: Some(0.3),
    })
}

/// One row's usage with a cache-write split, as Claude Code 2.1.30x
/// writes it (the key names MEASURED on this machine's transcripts).
const SPLIT_ROW: &str = r#"{"type":"assistant","message":{"id":"msg_s","model":"m","usage":{"input_tokens":10,"cache_creation_input_tokens":1000,"cache_read_input_tokens":0,"cache_creation":{"ephemeral_5m_input_tokens":200,"ephemeral_1h_input_tokens":800},"output_tokens":0}}}"#;

/// THE ONE-HOUR WRITES ARE PRICED AT THEIR OWN RATE: 800 of the row's
/// 1000 writes are one-hour ones, so it lists 200 × $3.75 + 800 × $6 (+
/// the input), not 1000 × $3.75. Negative control: the same counts with no
/// split price every write at the five-minute rate.
#[test]
fn one_hour_cache_writes_are_priced_at_their_rate() {
    let mut t = TranscriptUsage::new();
    assert_eq!(t.fold_line(SPLIT_ROW), super::super::usage::Fold::Summed);
    let m = t.per_model()["m"];
    assert_eq!((m.cache_write, m.cache_write_1h), (1000, 800));
    let table = PriceTable::new().with("m", sonnet_like());
    let want = (10.0 * 3.0 + 200.0 * 3.75 + 800.0 * 6.0) / 1e6;
    let one = table.message_usd("m", &m.tokens()).expect("priced");
    assert!((one - want).abs() < 1e-12, "{one} != {want}");
    // NEGATIVE CONTROL: no split, every write at the five-minute rate.
    let unsplit = TokenUsage {
        cache_write_1h: 0,
        ..m.tokens()
    };
    let flat = table.message_usd("m", &unsplit).expect("priced");
    assert!((flat - (10.0 * 3.0 + 1000.0 * 3.75) / 1e6).abs() < 1e-12);
    assert!(flat < one);
}

/// NEVER A GUESS: tokens in a class the row has no rate for make the
/// request unpriced, and a model with no row is unpriced. Negative
/// control: the same request without those tokens is priced.
#[test]
fn a_class_without_a_rate_leaves_the_request_unpriced() {
    let no_writes = PriceTable::new().with(
        "gpt-x",
        ModelPrice::flat(Rates {
            input: 5.0,
            output: 30.0,
            cache_read: Some(0.5),
            ..Rates::default()
        }),
    );
    let wrote = TokenUsage {
        input: 100,
        cache_write: 10,
        ..TokenUsage::default()
    };
    assert_eq!(no_writes.message_usd("gpt-x", &wrote), None);
    assert_eq!(no_writes.message_usd("gpt-y", &wrote), None, "no row");
    // NEGATIVE CONTROL.
    let read = TokenUsage {
        input: 100,
        cache_read: 1000,
        ..TokenUsage::default()
    };
    let usd = no_writes.message_usd("gpt-x", &read).expect("priced");
    assert!((usd - (100.0 * 5.0 + 1000.0 * 0.5) / 1e6).abs() < 1e-12);
}

/// THE LONG-PROMPT TIER is decided per request on its whole prompt
/// (input, cache writes and reads): one token over prices every class at
/// the long rates; at the threshold, the base ones.
#[test]
fn a_prompt_over_the_tier_pays_the_long_rates_request_by_request() {
    let base = Rates {
        input: 0.1,
        output: 0.5,
        cache_write: Some(0.125),
        cache_write_1h: Some(0.2),
        cache_read: Some(0.01),
    };
    let long = Rates {
        input: 0.5,
        output: 2.5,
        cache_write: Some(0.625),
        cache_write_1h: Some(1.0),
        cache_read: Some(0.05),
    };
    let table = PriceTable::new().with(
        "h",
        ModelPrice {
            rates: base,
            long: Some((100_000, long)),
        },
    );
    let at = TokenUsage {
        input: 10_000,
        cache_read: 90_000,
        output: 1000,
        ..TokenUsage::default()
    };
    let over = TokenUsage {
        cache_read: 90_001,
        ..at
    };
    let short = table.message_usd("h", &at).expect("priced");
    let long_usd = table.message_usd("h", &over).expect("priced");
    assert!((short - (10_000.0 * 0.1 + 90_000.0 * 0.01 + 1000.0 * 0.5) / 1e6).abs() < 1e-12);
    assert!((long_usd - (10_000.0 * 0.5 + 90_001.0 * 0.05 + 1000.0 * 2.5) / 1e6).abs() < 1e-12);
}

/// THE SHIPPED TABLE (`prices.toml`) loads, is dated, and every row names
/// the public page it was read from; ids are exact (an alias or a
/// `[1m]` spelling is unpriced). The loader is strict: an unknown key, a
/// missing source or price, a negative price, a bad date each refuse the
/// whole table. Negative control: the shipped text itself is accepted.
#[test]
fn the_shipped_table_loads_dated_and_sourced_and_the_loader_is_strict() {
    let shipped = PRICES_TOML;
    let table = PriceTable::from_toml(shipped).expect("the shipped table loads");
    assert_eq!(table.as_of(), Some("2026-10-15"));
    assert!(table.models().count() >= 20);
    for model in table.models() {
        let source = table.source(model).expect("every row names its page");
        assert!(
            source.starts_with("https://platform.claude.com/")
                || source.starts_with("https://developers.openai.com/"),
            "{model}: {source}"
        );
        assert!(source.ends_with(", 2026-10-15"), "{model}: {source}");
    }
    let opus = table.get("claude-opus-5-5").expect("a row");
    assert_eq!(opus.rates.input, 4.0);
    assert_eq!(opus.rates.cache_write_1h, Some(8.0));
    assert!(table.get("claude-opus-5-5[1m]").is_none());
    assert!(
        table.get("claude-mythos-preview").is_none(),
        "no price on the page"
    );
    assert!(
        table
            .get("claude-haiku-5-5")
            .is_some_and(|p| p.long.is_some())
    );
    assert_eq!(
        table.get("gpt-5.5").map(|p| p.rates.cache_write),
        Some(None),
        "a `-` on the page is no price"
    );
    assert_eq!(
        table
            .get("gpt-5-pro")
            .map(|p| (p.rates.cache_read, p.long.is_some())),
        Some((None, false)),
        "no cached input, no long-context column"
    );
    let row = "format = 1\nas_of = \"2026-10-15\"\n[models.\"m\"]\ninput = 1\noutput = 2\nsource = \"https://example.com/p, 2026-10-15\"\nid_from = \"https://example.com/p, 2026-10-15\"\n";
    assert!(PriceTable::from_toml(row).is_ok(), "NEGATIVE CONTROL");
    for (bad, why) in [
        (
            row.replace("output = 2", "output = 2\ncache_writes = 1"),
            "unknown",
        ),
        (row.replace("output = 2\n", ""), "output"),
        (row.replace("input = 1", "input = -1"), "negative"),
        (
            row.replace("source = \"https://example.com/p, 2026-10-15\"\n", ""),
            "source",
        ),
        (row.replace("2026-10-15\"\n[", "Oct 15\"\n["), "as_of"),
        (row.replace("format = 1", "format = 2"), "format"),
        (
            format!("{row}[models.\"m\".long]\nover = 0\ninput = 1\noutput = 1\n"),
            "over",
        ),
    ] {
        let e = PriceTable::from_toml(&bad).expect_err(why);
        assert!(e.contains(why) || why == "negative", "{why}: {e}");
    }
}

/// Round prices for easy sums: sol $1 in, $0.10 cached, $10 out; luna twice
/// that, with cache writes, and its long rates over 1,000 prompt tokens.
const CODEX_TABLE: &str = r#"
format = 1
as_of = "2026-10-15"

[models."gpt-5.6-sol"]
input = 1.0
cache_read = 0.1
output = 10.0
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/pricing, 2026-10-15"

[models."gpt-6-luna"]
input = 2.0
cache_read = 0.2
cache_write = 2.5
output = 20.0
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/pricing, 2026-10-15"

[models."gpt-6-luna".long]
over = 1000
input = 4.0
cache_read = 0.4
cache_write = 5.0
output = 40.0
"#;

fn codex_table() -> PriceTable {
    PriceTable::from_toml(CODEX_TABLE).expect("the Codex test table loads")
}

/// A rollout in a scratch home; its folder is made for the composed rows.
fn rollout(home: &Path, thread: &str) -> PathBuf {
    let day = home.join("sessions/2026/10/15");
    fs::create_dir_all(&day).expect("mkdir");
    day.join(format!("rollout-2026-10-15T10-00-00-{thread}.jsonl"))
}

const ROOT: &str = "01900000-0000-7000-8000-000000000001";
const SUB: &str = "01900000-0000-7000-8000-000000000002";

/// A CODEX TURN AS A MESSAGE: its cached input is a cache read and out of
/// its input, so no cached token is priced twice, and the prompt a long row
/// is decided on is OpenAI's `input_tokens`; a cache write, a service tier,
/// or parts past the input make it approximate. Negative control: a turn
/// that wrote nothing to the cache, named no tier and whose parts fit is
/// exact.
#[test]
fn a_codex_turn_maps_into_one_message() {
    use crate::harness::codex_rollout::{Turn, TurnTokens};
    let turn = |input, cached, cache_write, tier| Turn {
        at: 1_790_000_000,
        model: Some("gpt-6-luna".to_owned()),
        tokens: TurnTokens {
            input,
            cached,
            cache_write,
            output: 10,
            reasoning: 4,
        },
        tier,
        key: 0,
    };
    let t = turn(900, 600, 0, false);
    let m = codex_message(&t);
    assert_eq!(
        m.tokens,
        TokenUsage {
            input: 300,
            output: 10,
            cache_read: 600,
            ..TokenUsage::default()
        }
    );
    assert!(!m.approx, "NEGATIVE CONTROL: exact");
    assert_eq!(m.tokens.prompt(), 900);
    // Under the long threshold the base rates, over it the long ones.
    let prices = codex_table();
    let base = (300.0 * 2.0 + 600.0 * 0.2 + 10.0 * 20.0) / 1e6;
    assert!(close(
        prices.message_usd(m.model, &m.tokens).expect("priced"),
        base
    ));
    let t = turn(1200, 600, 0, false);
    let m = codex_message(&t);
    let long = (600.0 * 4.0 + 600.0 * 0.4 + 10.0 * 40.0) / 1e6;
    assert!(close(
        prices.message_usd(m.model, &m.tokens).expect("priced"),
        long
    ));
    // A cache write is a part of the input, and approximate.
    let t = turn(900, 600, 100, false);
    let m = codex_message(&t);
    assert_eq!((m.tokens.input, m.tokens.cache_write), (200, 100));
    assert!(m.approx);
    assert!(codex_message(&turn(900, 600, 0, true)).approx, "a tier");
    assert!(
        codex_message(&turn(500, 600, 0, false)).approx,
        "parts past input"
    );
    assert_eq!(codex_message(&turn(500, 600, 0, false)).tokens.input, 0);
    let unnamed = Turn {
        model: None,
        ..turn(900, 600, 0, false)
    };
    assert_eq!(codex_message(&unnamed).model, UNKNOWN_MODEL);
}

/// TWO THREADS SERVED THE SAME COUNTS are two turns (one prompt run twice,
/// as a script runs `codex exec`, or the lab's stub): their fingerprints
/// match, and the conversation does not deduplicate across rollouts by it.
/// Negative control: the two turns' fingerprints are one, so a fold keyed by
/// that fingerprint alone would count one.
#[test]
fn two_threads_served_the_same_counts_each_count() {
    use crate::harness::codex_rollout::{Row, TurnReader};
    let home = aterm_tempfile::tempdir().expect("a scratch home");
    let b = 1_790_000_000_000;
    let run = |thread: &str, at: i64| {
        [
            meta(at, thread, thread, None),
            context(at + 1000, "gpt-5.6-sol", None),
            count(at + 5000, (1000, 800, 50), (1000, 800, 50)),
        ]
        .concat()
    };
    let (one, two) = (run(ROOT, b), run(SUB, b + 60_000));
    fs::write(rollout(home.path(), ROOT), &one).expect("write");
    fs::write(rollout(home.path(), SUB), &two).expect("write");
    let conversation = fold_conversation(
        &[rollout(home.path(), ROOT), rollout(home.path(), SUB)],
        None,
        &codex_table(),
        u64::MAX,
    );
    assert_eq!(conversation.turns, 2);
    // NEGATIVE CONTROL.
    let key = |text: &str| {
        let mut reader = TurnReader::default();
        text.lines()
            .find_map(|l| match reader.line(l) {
                Row::Turn(t) => Some(t.key),
                _ => None,
            })
            .expect("a turn")
    };
    assert_eq!(key(&one), key(&two), "one fingerprint");
}
