// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: the home-wide fold, the spend file and the dollar
//! figures, against composed transcripts in temp homes this test writes
//! (never a real one) and a composed price table read by the real loader.

use std::fs;
use std::io::Write as _;

use super::*;
use crate::harness::usage::TranscriptUsage;

/// A scratch Claude Code home under the system temp dir, removed on drop.
struct Home(aterm_tempfile::TempDir);

impl Home {
    fn new(label: &str) -> Self {
        Self(
            aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-spend-{label}-"))
                .tempdir()
                .expect("create the scratch home"),
        )
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    /// `<home>/projects/<rel>`, its directories made.
    fn transcript(&self, rel: &str) -> PathBuf {
        let path = self.path().join("projects").join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        path
    }
}

fn append(path: &Path, text: &str) {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open for append");
    f.write_all(text.as_bytes()).expect("append");
}

/// The test's clock: real time, so the files this test writes are inside
/// the horizon by their mtime.
fn now() -> i64 {
    i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds")
}

/// Round prices for easy sums: opus $1 in, $10 out, $2 five-minute writes,
/// $4 one-hour writes, $0.10 reads; fable twice that.
const TABLE: &str = r#"
format = 1
as_of = "2026-10-15"

[models."claude-opus-5-5"]
input = 1.0
output = 10.0
cache_write = 2.0
cache_write_1h = 4.0
cache_read = 0.1
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"

[models."claude-fable-5-1"]
input = 2.0
output = 20.0
cache_write = 4.0
cache_write_1h = 8.0
cache_read = 0.2
source = "https://example.com/pricing, 2026-10-15"
id_from = "https://example.com/models, 2026-10-15"
"#;

fn table() -> PriceTable {
    PriceTable::from_toml(TABLE).expect("the test table loads")
}

/// One assistant row as Claude Code writes it (key names as on 2.1.30x),
/// with `extra` spliced into its usage, and its newline. `id` empty: none.
fn row(id: &str, model: &str, at: i64, t: TokenUsage, extra: &str) -> String {
    let id = if id.is_empty() {
        String::new()
    } else {
        format!("\"id\":\"{id}\",")
    };
    format!(
        "{{\"type\":\"assistant\",\"isSidechain\":true,\"timestamp\":\"{}\",\"message\":{{{id}\"model\":\"{model}\",\"stop_reason\":\"end_turn\",\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}],\"usage\":{{\"input_tokens\":{},\"cache_creation_input_tokens\":{},\"cache_read_input_tokens\":{},\"cache_creation\":{{\"ephemeral_5m_input_tokens\":{},\"ephemeral_1h_input_tokens\":{}}},\"output_tokens\":{},\"service_tier\":\"standard\",\"speed\":\"standard\",\"inference_geo\":\"global\"{extra}}}}}}}\n",
        super::super::usage::rfc3339_utc(at),
        t.input,
        t.cache_write,
        t.cache_read,
        t.cache_write - t.cache_write_1h,
        t.cache_write_1h,
        t.output,
    )
}

/// A million input tokens: $1 on opus.
fn mtok_in() -> TokenUsage {
    TokenUsage {
        input: 1_000_000,
        ..TokenUsage::default()
    }
}

fn out(n: u64) -> TokenUsage {
    TokenUsage {
        output: n,
        ..TokenUsage::default()
    }
}

fn spend_of(home: &Home) -> Spend {
    Spend::new(Agent::Claude, "main", home.path(), &table())
}

fn everything(spend: &Spend, now: i64) -> Sum {
    spend.sum(
        Span {
            from: now - HORIZON_S,
            to: now + 1,
            whole: false,
        },
        None,
    )
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// ---------------------------------------------------------------------------
// The price table
// ---------------------------------------------------------------------------

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

/// A final streamed row may reveal the cache-write lifetime without adding
/// another token. If that lifetime has no price, its unpriced delta must
/// suppress capacity even though the aggregate cache-write delta is zero.
#[test]
fn a_late_unpriced_one_hour_split_suppresses_capacity() {
    let home = Home::new("late-unpriced-hour");
    let path = home.transcript("p/session.jsonl");
    let now = now();
    let at = now - HOUR;
    let model = "claude-opus-5-5";
    let prices = PriceTable::from_toml(&TABLE.replace("cache_write_1h = 4.0\n", ""))
        .expect("the table prices five-minute writes only");
    let tokens = TokenUsage {
        cache_write: 1_000_000,
        ..TokenUsage::default()
    };
    let mut first: Value = aterm_json::from_str(&row("msg_cache", model, at, tokens, ""))
        .expect("the first streamed row");
    first
        .as_object_mut()
        .expect("row")
        .get_mut("message")
        .expect("message")
        .as_object_mut()
        .expect("message object")
        .get_mut("usage")
        .expect("usage")
        .as_object_mut()
        .expect("usage object")
        .remove("cache_creation");
    append(&path, &format!("{first}\n"));
    let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
    assert_eq!(
        spend
            .fold_claude_home(&prices, now, Limits::none())
            .messages,
        1
    );
    let reading = ask(now, Some(tap(500, now - 10 * 60)));
    assert!(
        figure(&spend, &reading).capacity.is_ok(),
        "without a one-hour split the five-minute price is sufficient"
    );

    // The first row and cursor may already be durable when the final row arrives.
    let mut spend = Spend::from_json(&spend.to_json()).expect("saved spend");
    append(
        &path,
        &row(
            "msg_cache",
            model,
            at + 1,
            TokenUsage {
                cache_write_1h: tokens.cache_write,
                ..tokens
            },
            "",
        ),
    );
    let folded = spend.fold_claude_home(&prices, now, Limits::none());
    assert_eq!((folded.messages, folded.duplicates), (0, 1));
    let figure = figure(&spend, &reading);
    assert_eq!(figure.used.tokens.cache_write, tokens.cache_write);
    assert_eq!(
        figure.used.unpriced_tokens.cache_write_1h,
        tokens.cache_write
    );
    assert_eq!(figure.capacity, Err(Why::Unpriced));
    assert_eq!(figure.left, Err(Why::Unpriced));

    // The same real transcript is priceable when the supplied table has both rates.
    let mut priced = Spend::new(Agent::Claude, "main", home.path(), &table());
    assert_eq!(
        priced
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        1
    );
    assert!(super::figure(&priced, &reading).capacity.is_ok());
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

// ---------------------------------------------------------------------------
// The fold
// ---------------------------------------------------------------------------

/// THE HOME-WIDE FOLD COUNTS SUBAGENTS (the design's F15): the main
/// transcript, a subagent's beside it and a workflow's nested deeper are all
/// summed. Negative control: the main transcript alone holds a third.
#[test]
fn the_home_fold_counts_every_transcript_subagents_included() {
    let home = Home::new("subagents");
    let now = now();
    let at = now - 60;
    append(
        &home.transcript("-proj/sess.jsonl"),
        &row("msg_main", "claude-opus-5-5", at, mtok_in(), ""),
    );
    append(
        &home.transcript("-proj/sess/subagents/agent-a.jsonl"),
        &row("msg_sub", "claude-opus-5-5", at, mtok_in(), ""),
    );
    append(
        &home.transcript("-proj/sess/subagents/workflows/run-1/agent-b.jsonl"),
        &row("msg_flow", "claude-opus-5-5", at, mtok_in(), ""),
    );
    let mut spend = spend_of(&home);
    let report = spend.fold_claude_home(&table(), now, Limits::none());
    assert!(report.complete(), "{report:?}");
    assert_eq!((report.files, report.messages), (3, 3));
    let all = everything(&spend, now);
    assert!(close(all.usd, 3.0), "{}", all.usd);
    // NEGATIVE CONTROL: a home with only the main transcript.
    let alone = Home::new("main-only");
    append(
        &alone.transcript("-proj/sess.jsonl"),
        &row("msg_main", "claude-opus-5-5", at, mtok_in(), ""),
    );
    let mut main_only = spend_of(&alone);
    main_only.fold_claude_home(&table(), now, Limits::none());
    assert!(close(everything(&main_only, now).usd, 1.0));
}

/// ONE MESSAGE, ONE COUNT, AT ITS LARGEST: a streamed message's rows repeat
/// its id with growing output, within one fold and across two (the id is
/// kept in the cursor between them). Negative control: the same rows with no
/// id count three messages and every row's output.
#[test]
fn a_message_counts_once_at_its_largest_even_across_folds() {
    let home = Home::new("dedupe");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    append(
        &path,
        &row("msg_a", "claude-opus-5-5", now - 30, out(10), ""),
    );
    append(
        &path,
        &row("msg_a", "claude-opus-5-5", now - 30, out(50), ""),
    );
    let mut spend = spend_of(&home);
    let first = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((first.messages, first.duplicates), (1, 1));
    append(
        &path,
        &row("msg_a", "claude-opus-5-5", now - 29, out(80), ""),
    );
    append(
        &path,
        &row("msg_a", "claude-opus-5-5", now - 29, out(80), ""),
    );
    let second = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((second.messages, second.duplicates), (0, 2));
    let all = everything(&spend, now);
    assert_eq!((all.messages, all.tokens.output), (1, 80));
    assert!(close(all.usd, 80.0 * 10.0 / 1e6));
    // NEGATIVE CONTROL: no ids, no dedupe.
    let bare = Home::new("dedupe-control");
    let path = bare.transcript("-p/s.jsonl");
    for n in [10, 50, 80] {
        append(&path, &row("", "claude-opus-5-5", now - 30, out(n), ""));
    }
    let mut control = spend_of(&bare);
    control.fold_claude_home(&table(), now, Limits::none());
    let all = everything(&control, now);
    assert_eq!((all.messages, all.tokens.output), (3, 140));
}

/// THE COST RULE: a second fold reads only what was appended (plus the one
/// boundary byte), through the spend file too, and an unchanged home reads
/// nothing. Negative control: a spend started fresh reads the whole file.
#[test]
fn a_second_fold_reads_only_the_appended_bytes() {
    let home = Home::new("appended");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    let mut first_rows = String::new();
    for i in 0..20 {
        first_rows.push_str(&row(
            &format!("msg_{i}"),
            "claude-opus-5-5",
            now - 100,
            mtok_in(),
            "",
        ));
    }
    append(&path, &first_rows);
    let store = SpendStore {
        dir: home.path().join("accounts").join("spend"),
    };
    let (mut spend, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, Some(Fresh::NoFile));
    let first = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(first.read, first_rows.len() as u64);
    let recording = aterm_durable::seam::Recording::start();
    store.save(&spend).expect("save");
    assert_eq!(
        recording.take(),
        [aterm_durable::Flush::Ordered],
        "a fold is only ordered"
    );
    // Unchanged: not even opened.
    let (mut again, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, None, "the file is this home's, under this table");
    assert_eq!(
        again.fold_claude_home(&table(), now, Limits::none()).read,
        0
    );
    // Appended: those bytes and the boundary byte.
    let more = row("msg_new", "claude-opus-5-5", now - 50, mtok_in(), "");
    append(&path, &more);
    let second = again.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(second.read, more.len() as u64 + 1);
    assert_eq!(second.messages, 1);
    assert!(close(everything(&again, now).usd, 21.0));
    // NEGATIVE CONTROL: no cursor, the whole file.
    let mut fresh_spend = spend_of(&home);
    let whole = fresh_spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(whole.read, (first_rows.len() + more.len()) as u64);
}

/// A failed directory read is not a complete empty home. In particular it
/// cannot discard the per-file streaming state which a later append needs.
#[test]
fn an_incomplete_walk_keeps_the_streaming_cursor_until_the_home_is_readable() {
    let home = Home::new("walk-refused");
    let now = now();
    let mut spend = spend_of(&home);
    assert!(
        spend
            .fold_claude_home(&table(), now, Limits::none())
            .complete(),
        "a genuinely absent projects directory is an empty home"
    );
    let path = home.transcript("p/session.jsonl");
    let initial = TokenUsage {
        input: 100,
        output: 10,
        ..TokenUsage::default()
    };
    append(
        &path,
        &row("msg_stream", "claude-opus-5-5", now - 60, initial, ""),
    );
    assert_eq!(
        spend
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        1
    );

    // Keep the real transcript intact while making its directory unreadable as
    // a directory. This needs no permission tricks or timing-dependent race.
    let projects = home.path().join("projects");
    let parked = home.path().join("projects-held");
    fs::rename(&projects, &parked).expect("park the scratch projects directory");
    fs::write(&projects, "not a directory").expect("damage the scratch directory name");
    let refused = spend.fold_claude_home(&table(), now, Limits::none());
    assert!(
        !refused.complete(),
        "a failed walk claimed the whole home was read"
    );
    fs::remove_file(&projects).expect("remove the scratch obstruction");
    fs::rename(&parked, &projects).expect("restore the same transcript");

    append(
        &path,
        &row(
            "msg_stream",
            "claude-opus-5-5",
            now - 59,
            TokenUsage {
                output: 100,
                ..initial
            },
            "",
        ),
    );
    let recovered = spend.fold_claude_home(&table(), now, Limits::none());
    assert!(recovered.complete());
    let sum = everything(&spend, now);
    assert_eq!(
        (sum.messages, sum.tokens.input, sum.tokens.output),
        (1, 100, 100),
        "the recovered append grows its existing message rather than disappearing as a copy"
    );
}

/// A damaged cache must be refused without waiting for data from a FIFO.
/// The exact child runs under a one-minute hang detector which kills and
/// reaps it on failure; the parent retains and removes the scratch home.
#[cfg(unix)]
#[test]
fn a_fifo_spend_cache_is_refused_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt as _;
    const CHILD: &str = "ATERM_SPEND_FIFO_CACHE_CHILD";
    const TEST: &str =
        "harness::spend::tests::a_fifo_spend_cache_is_refused_without_waiting_for_a_writer";
    if let Some(path) = std::env::var_os(CHILD) {
        let home = PathBuf::from(path);
        let store = SpendStore {
            dir: home.join("spend"),
        };
        assert_eq!(
            store.load(Agent::Claude, "main", &home, &table()).1,
            Some(Fresh::Unreadable)
        );
        fs::write(home.join("refused"), b"refused\n").expect("the exact case completed");
        return;
    }
    let home = Home::new("fifo-cache");
    let store = SpendStore {
        dir: home.path().join("spend"),
    };
    let prices = table();
    let (_, fresh) = store.load(Agent::Claude, "main", home.path(), &prices);
    assert_eq!(fresh, Some(Fresh::NoFile));
    let original = Spend::new(Agent::Claude, "main", home.path(), &prices);
    store.save(&original).expect("write an ordinary cache");
    assert_eq!(
        store.load(Agent::Claude, "main", home.path(), &prices),
        (original, None)
    );
    let path = store.path(Agent::Claude, "main").unwrap();
    fs::remove_file(&path).expect("remove the owned scratch cache");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: cpath is a NUL-terminated path in this test's owned scratch home.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", TEST, "--nocapture"])
        .env_clear()
        .env("HOME", home.path())
        .env(CHILD, home.path());
    let output = aterm_uds::deadline::output_until(
        &mut command,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("the FIFO cache child hung or could not run");
    assert!(
        output.status.success(),
        "cache child: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(home.path().join("refused")).expect("the exact test ran"),
        b"refused\n"
    );
}

/// A TORN LAST LINE is not folded until its newline is written, and then
/// only once.
#[test]
fn a_torn_last_line_waits_for_its_newline() {
    let home = Home::new("torn");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    let whole = row("msg_t", "claude-opus-5-5", now - 10, mtok_in(), "");
    let (head, tail) = whole.split_at(whole.len() / 2);
    append(&path, head);
    let mut spend = spend_of(&home);
    let first = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(first.messages, 0);
    append(&path, tail);
    let second = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(second.messages, 1);
    assert_eq!(
        spend
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        0
    );
    assert!(close(everything(&spend, now).usd, 1.0));
}

/// A BUDGET only spreads the fold: many small folds reach the sums one
/// unbounded fold does, and say they are behind until then — a budget
/// smaller than one line included (a line begun is read to its end).
#[test]
fn a_budgeted_fold_reaches_the_same_sums_as_one_unbounded() {
    let home = Home::new("budget");
    let now = now();
    for f in 0..3 {
        let path = home.transcript(&format!("-p/s{f}.jsonl"));
        for i in 0..15 {
            append(
                &path,
                &row(
                    &format!("msg_{f}_{i}"),
                    "claude-opus-5-5",
                    now - 200 + i,
                    out(1000 + i as u64),
                    "",
                ),
            );
        }
    }
    let mut whole = spend_of(&home);
    assert!(
        whole
            .fold_claude_home(&table(), now, Limits::none())
            .caught_up
    );
    let mut bit = spend_of(&home);
    let mut folds = 0;
    loop {
        folds += 1;
        let r = bit.fold_claude_home(
            &table(),
            now,
            Limits {
                bytes: 100,
                until: None,
            },
        );
        if r.caught_up {
            break;
        }
        assert!(folds < 1000, "never caught up");
    }
    assert_eq!(
        folds, 45,
        "one line per fold: the 100 bytes end inside each"
    );
    assert_eq!(everything(&bit, now), everything(&whole, now));
}

/// A TRANSCRIPT REWRITTEN IN PLACE is folded again from its start: what it
/// had counted is known by its ids and not counted again, and a message new
/// to it is. Negative control: the same refold with the counted ids
/// forgotten (the home's, and the transcript's own) counts the rewritten
/// rows again.
#[test]
fn a_rewritten_transcript_is_folded_again_never_counted_twice() {
    let rewrite = |label: &str, forget: bool| {
        let home = Home::new(label);
        let now = now();
        let path = home.transcript("-p/s.jsonl");
        let a = row("msg_a", "claude-opus-5-5", now - 100, mtok_in(), "");
        append(&path, &a);
        append(
            &path,
            &row("msg_b", "claude-opus-5-5", now - 90, mtok_in(), ""),
        );
        let mut spend = spend_of(&home);
        spend.fold_claude_home(&table(), now, Limits::none());
        if forget {
            spend.seen.clear();
            for cursor in spend.files.values_mut() {
                cursor.ids.clear();
            }
        }
        // Shorter than the offset folded to (a tenth of the tokens, a digit
        // fewer), with a message new to it.
        let tenth = TokenUsage {
            input: 100_000,
            ..TokenUsage::default()
        };
        let c = row("msg_c", "claude-opus-5-5", now - 80, tenth, "");
        fs::write(&path, format!("{a}{c}")).expect("rewrite");
        let report = spend.fold_claude_home(&table(), now, Limits::none());
        (spend, report, now)
    };
    let (mut spend, report, now) = rewrite("rewritten", false);
    assert_eq!(
        (report.refolded, report.messages, report.duplicates),
        (1, 1, 1)
    );
    assert!(report.complete(), "{report:?}");
    assert!(close(everything(&spend, now).usd, 2.1));
    let again = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((again.read, again.refolded, again.messages), (0, 0, 0));
    // NEGATIVE CONTROL.
    let (control, report, now) = rewrite("rewritten-control", true);
    assert_eq!((report.refolded, report.messages), (1, 2));
    assert!(close(everything(&control, now).usd, 3.1));
}

/// A MESSAGE STREAMING WHEN ITS FILE IS FOLDED AGAIN is still raised by its
/// later rows: the ids the transcript held stand through the refold.
/// Negative control: the same refold with the transcript's ids forgotten
/// (the home's still known) drops the message's output.
#[test]
fn a_message_streaming_through_a_refold_is_still_raised() {
    let refold = |label: &str, forget: bool| {
        let home = Home::new(label);
        let now = now();
        let opus = "claude-opus-5-5";
        let main = home.transcript("-p/s.jsonl");
        let first = row("msg_m", opus, now - 90, mtok_in(), "");
        append(&main, &row("msg_a", opus, now - 100, mtok_in(), ""));
        append(&main, &first);
        let mut spend = spend_of(&home);
        spend.fold_claude_home(&table(), now, Limits::none());
        if forget {
            for cursor in spend.files.values_mut() {
                cursor.ids.clear();
            }
        }
        // Shorter than the offset folded to: msg_a's row gone.
        fs::write(&main, &first).expect("rewrite");
        let report = spend.fold_claude_home(&table(), now, Limits::none());
        assert_eq!((report.refolded, report.messages), (1, 0));
        let whole = mtok_in().plus(&out(100_000));
        append(&main, &row("msg_m", opus, now - 90, whole, ""));
        spend.fold_claude_home(&table(), now, Limits::none());
        everything(&spend, now)
    };
    let all = refold("refold-open", false);
    assert_eq!(all.messages, 2);
    assert!(close(all.usd, 3.0), "{}", all.usd);
    // NEGATIVE CONTROL.
    let all = refold("refold-open-control", true);
    assert!(close(all.usd, 2.0), "{}", all.usd);
}

/// ONE MESSAGE, ONE COUNT IN THE HOME: a transcript that repeats messages
/// another one counted (a copy a fork or a resume could write) adds only
/// its own, across two folds through the spend file and in one fold that
/// reads the copy first. Negative control: the home's counted ids forgotten
/// between the folds, the copies count again.
#[test]
fn a_message_another_transcript_repeats_counts_once_in_the_home() {
    let home = Home::new("copies");
    let now = now();
    let a = row("msg_a", "claude-opus-5-5", now - 300, mtok_in(), "");
    let b = row("msg_b", "claude-opus-5-5", now - 200, mtok_in(), "");
    append(&home.transcript("-p/s1.jsonl"), &format!("{a}{b}"));
    let store = SpendStore {
        dir: home.path().join("accounts").join("spend"),
    };
    let (mut spend, _) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(
        spend
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        2
    );
    store.save(&spend).expect("save");
    let c = row("msg_c", "claude-opus-5-5", now - 100, mtok_in(), "");
    append(&home.transcript("-p/s2.jsonl"), &format!("{a}{b}{c}"));
    let (mut later, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, None);
    let report = later.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((report.messages, report.duplicates), (1, 2));
    assert!(close(everything(&later, now).usd, 3.0));
    // In one fold, the copy (the newer file) read first.
    let mut once = spend_of(&home);
    let report = once.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((report.messages, report.duplicates), (3, 2));
    assert_eq!(everything(&once, now), everything(&later, now));
    // NEGATIVE CONTROL.
    let (mut control, _) = store.load(Agent::Claude, "main", home.path(), &table());
    control.seen.clear();
    assert_eq!(
        control
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        3
    );
    assert!(close(everything(&control, now).usd, 5.0));
}

/// BUCKETS are five minutes wide back to the horizon, and nothing past it
/// is kept: time passing drops what leaves the horizon and merges nothing,
/// so a sum over old spend is as fine as one over new. Negative control:
/// past the horizon, the buckets and their ids are gone.
#[test]
fn buckets_are_five_minutes_wide_back_to_the_horizon() {
    let home = Home::new("buckets");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    append(
        &path,
        &row("msg_new", "claude-opus-5-5", now - 600, mtok_in(), ""),
    );
    append(
        &path,
        &row("msg_old", "claude-opus-5-5", now - 8 * 3600, mtok_in(), ""),
    );
    append(
        &path,
        &row(
            "msg_gone",
            "claude-opus-5-5",
            now - 9 * 24 * 3600,
            mtok_in(),
            "",
        ),
    );
    let mut spend = spend_of(&home);
    spend.fold_claude_home(&table(), now, Limits::none());
    let starts: Vec<i64> = spend.buckets().map(|(start, _)| start).collect();
    let five = |at: i64| at.div_euclid(BUCKET_S) * BUCKET_S;
    assert_eq!(starts, [five(now - 8 * 3600), five(now - 600)]);
    let before = everything(&spend, now);
    assert_eq!(before.messages, 2, "nothing past the horizon");
    let later = now + 7 * 24 * 3600;
    spend.expire(later);
    let kept: Vec<i64> = spend.buckets().map(|(start, _)| start).collect();
    assert_eq!(kept, starts, "a week on, still five minutes wide");
    // The old message's own five minutes hold it, a week on.
    let old = five(now - 8 * 3600);
    let alone = spend.sum(
        Span {
            from: old,
            to: old + BUCKET_S,
            whole: true,
        },
        None,
    );
    assert_eq!(alone.messages, 1);
    assert_eq!(spend.seen.len(), 2, "the counted ids, the gone one's not");
    // NEGATIVE CONTROL.
    spend.expire(now + HORIZON_S + 3600);
    assert_eq!(spend.buckets().count(), 0, "all past the horizon");
    assert!(spend.seen.is_empty(), "and their ids");
}

/// NEVER A GUESS: a model with no row is counted and named, its dollars
/// unknown, and more than 1% of a class unpriced says so. Negative control:
/// the same tokens on a priced model are priced and say nothing.
#[test]
fn an_unknown_model_is_unpriced_never_guessed() {
    let home = Home::new("unpriced");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    append(
        &path,
        &row("msg_x", "claude-opus-9", now - 60, mtok_in(), ""),
    );
    append(
        &path,
        &row("msg_y", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let mut spend = spend_of(&home);
    spend.fold_claude_home(&table(), now, Limits::none());
    let all = everything(&spend, now);
    assert_eq!(all.messages, 2);
    assert!(close(all.usd, 1.0), "only the priced model's dollar");
    assert_eq!(all.unpriced["claude-opus-9"].input, 1_000_000);
    assert!(all.unpriced_over());
    // NEGATIVE CONTROL.
    let known = Home::new("priced");
    append(
        &known.transcript("-p/s.jsonl"),
        &row("msg_y", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let mut priced = spend_of(&known);
    priced.fold_claude_home(&table(), now, Limits::none());
    let all = everything(&priced, now);
    assert!(all.unpriced.is_empty() && !all.unpriced_over());
}

/// WHAT THE TABLE DOES NOT PRICE is counted approximate: fast mode, US-only
/// inference, a web search, a tier other than the standard one, an
/// advisor's iterations. Negative control: the standard words, no search,
/// the empty `inference_geo` and `iterations` Claude Code writes where the
/// server sent none (2.1.294's lab rows; its default usage in 2.1.301), and
/// a message's own iteration, are not.
#[test]
fn fast_mode_us_inference_web_search_tiers_and_advisors_are_approximate() {
    let home = Home::new("approx");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    for (i, extra) in [
        r#","speed":"fast""#,
        r#","inference_geo":"us""#,
        r#","server_tool_use":{"web_search_requests":2}"#,
        r#","service_tier":"priority""#,
        r#","iterations":[{"type":"message","input_tokens":3},{"type":"advisor_message","model":"claude-opus-5-5","input_tokens":900}]"#,
        r#","server_tool_use":{"web_search_requests":0}"#,
        r#","inference_geo":"""#,
        r#","iterations":[]"#,
        r#","iterations":[{"type":"message","input_tokens":3}]"#,
        "",
    ]
    .iter()
    .enumerate()
    {
        append(
            &path,
            &row(
                &format!("msg_{i}"),
                "claude-opus-5-5",
                now - 60,
                mtok_in(),
                extra,
            ),
        );
    }
    let mut spend = spend_of(&home);
    spend.fold_claude_home(&table(), now, Limits::none());
    let all = everything(&spend, now);
    assert_eq!((all.messages, all.approx), (10, 5));
    assert!(close(all.usd, 10.0), "the advisor's own tokens not counted");
}

/// The rows that are not a served message's spend: an API-error row, a row
/// with no timestamp (counted), a zero usage, a row of another type.
#[test]
fn only_served_timed_assistant_rows_count() {
    let home = Home::new("served");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    let good = row("msg_ok", "claude-opus-5-5", now - 60, mtok_in(), "");
    append(&path, &good);
    append(
        &path,
        &good
            .replace("msg_ok", "msg_err")
            .replace("\"isSidechain\":true", "\"isApiErrorMessage\":true"),
    );
    let stamp = super::super::usage::rfc3339_utc(now - 60);
    append(
        &path,
        &good
            .replace("msg_ok", "msg_untimed")
            .replace(&format!("\"timestamp\":\"{stamp}\","), ""),
    );
    append(
        &path,
        &good.replace("\"type\":\"assistant\"", "\"type\":\"user\""),
    );
    append(
        &path,
        &row(
            "msg_zero",
            "claude-opus-5-5",
            now - 60,
            TokenUsage::default(),
            "",
        ),
    );
    append(&path, "{not json\n");
    let mut spend = spend_of(&home);
    let report = spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(report.messages, 1);
    assert_eq!(report.rows_untimed, 1);
    assert_eq!(report.rows_unparsed, 1);
}

// ---------------------------------------------------------------------------
// The spend file
// ---------------------------------------------------------------------------

/// THE FILE is private (0700 directory, 0600 file), round-trips whole, and
/// is folded again when it is torn, of another table, of another home, or of
/// the format before; a label that is no roster label names no file.
#[test]
fn the_spend_file_round_trips_private_and_is_started_over_when_not_ours() {
    let home = Home::new("file");
    let now = now();
    let path = home.transcript("-p/s.jsonl");
    append(
        &path,
        &row("msg_a", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let store = SpendStore {
        dir: home.path().join("accounts").join("spend"),
    };
    let (mut spend, _) = store.load(Agent::Claude, "main", home.path(), &table());
    spend.fold_claude_home(&table(), now, Limits::none());
    store.save(&spend).expect("save");
    let file = store.path(Agent::Claude, "main").expect("a path");
    assert!(file.ends_with("spend/claude-main.json"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&store.dir), 0o700);
    }
    let (loaded, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, None);
    assert_eq!(loaded, spend, "the whole state round-trips");
    // Another table's date, another home, a torn file.
    let newer = PriceTable::from_toml(&TABLE.replace("2026-10-15\"\n\n", "2026-11-01\"\n\n"))
        .expect("the newer table");
    assert_eq!(
        store.load(Agent::Claude, "main", home.path(), &newer).1,
        Some(Fresh::OtherTable)
    );
    let other = Home::new("other");
    assert_eq!(
        store.load(Agent::Claude, "main", other.path(), &table()).1,
        Some(Fresh::OtherHome)
    );
    // A file of format 2, whose buckets may be an hour wide.
    let text = fs::read_to_string(&file).expect("read");
    let format = format!("\"v\":{FORMAT}");
    assert_eq!(text.matches(&format).count(), 1);
    let older = text.replacen(&format, "\"v\":2", 1);
    fs::write(&file, &older).expect("write format 2");
    assert_eq!(
        store.load(Agent::Claude, "main", home.path(), &table()).1,
        Some(Fresh::Unreadable)
    );
    // A bucket that does not start on five minutes is no bucket of this
    // format.
    let start = spend.buckets().next().expect("a bucket").0;
    let skewed = text.replacen(
        &format!("\"t\":{start}"),
        &format!("\"t\":{}", start + 60),
        1,
    );
    assert_ne!(skewed, text);
    fs::write(&file, &skewed).expect("write a skewed bucket");
    assert_eq!(
        store.load(Agent::Claude, "main", home.path(), &table()).1,
        Some(Fresh::Unreadable)
    );
    fs::write(&file, &text[..text.len() / 2]).expect("tear");
    assert_eq!(
        store.load(Agent::Claude, "main", home.path(), &table()).1,
        Some(Fresh::Unreadable)
    );
    assert_eq!(store.path(Agent::Claude, "../x"), None);
    assert_eq!(
        store
            .path(Agent::Codex, "codex")
            .expect("a path")
            .file_name()
            .and_then(|n| n.to_str()),
        Some("codex-codex.json")
    );
}

/// A NEWER TABLE folds everything again under its prices: nothing priced by
/// the old one is kept beside the new.
#[test]
fn a_spend_priced_by_another_table_is_folded_again() {
    let home = Home::new("retable");
    let now = now();
    append(
        &home.transcript("-p/s.jsonl"),
        &row("msg_a", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let mut spend = spend_of(&home);
    spend.fold_claude_home(&table(), now, Limits::none());
    assert!(close(everything(&spend, now).usd, 1.0));
    let dearer = PriceTable::from_toml(
        &TABLE
            .replace("2026-10-15\"\n\n", "2026-11-01\"\n\n")
            .replacen("input = 1.0", "input = 3.0", 1),
    )
    .expect("the newer table");
    let report = spend.fold_claude_home(&dearer, now, Limits::none());
    assert_eq!(report.messages, 1, "folded again");
    assert!(close(everything(&spend, now).usd, 3.0));
    assert_eq!(spend.prices_as_of(), "2026-11-01");
}

// ---------------------------------------------------------------------------
// The dollar figures
// ---------------------------------------------------------------------------

/// A spend with `usd` of opus spend placed at each `(at, usd)`.
fn spend_at(rows: &[(i64, f64)], now: i64) -> Spend {
    let home = Home::new("figures");
    let mut spend = spend_of(&home);
    for &(at, usd) in rows {
        // $1 per million input tokens on opus.
        let tokens = TokenUsage {
            input: (usd * 1e6) as u64,
            ..TokenUsage::default()
        };
        spend.add_message(
            &Message {
                at,
                model: "claude-opus-5-5",
                tokens,
                approx: false,
            },
            &table(),
            now,
        );
    }
    spend
}

const HOUR: i64 = 3600;
const FIVE_H: i64 = 5 * HOUR;

fn tap(pct_x10: u32, at: i64) -> Reading {
    Reading {
        pct_x10,
        at,
        kind: ReadingKind::Rounded,
    }
}

fn ask<'a>(now: i64, reading: Option<Reading>) -> Ask<'a> {
    Ask {
        length_s: FIVE_H,
        resets_at: Some(now + HOUR),
        epoch_at: None,
        family: None,
        reading,
        newest: None,
        tenths_seen: false,
        on_credits: false,
        attribution: Attribution::Assumed,
        api_key_seen: false,
        machines: Machines {
            reported: 1,
            used: 1,
            silent: 0,
        },
        calibration: None,
        now,
    }
}

/// `C = $(window start, t_r] ÷ u(t_r)` and `left = C × (1 − u_now)`: spend
/// after the reading never enters `C`, even in the bucket the reading falls
/// in (only buckets that end by it count). A `u_now` from before the window
/// is no `u_now` of it: the reading stands in. Negative control: counting the
/// later spend gives another figure.
#[test]
fn capacity_divides_the_spend_to_the_reading_by_its_use() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    // The reading at :38, $5 more at :39, in the same five-minute bucket.
    let reading_at = now - 22 * 60;
    let spend = spend_at(
        &[
            (now - 2 * HOUR, 6.0),
            (now - 3 * HOUR, 4.0),
            (now - 21 * 60, 5.0),
        ],
        now,
    );
    let mut a = ask(now, Some(tap(250, reading_at)));
    a.newest = Some(tap(500, now - 60));
    let f = figure(&spend, &a);
    assert!(!f.rolling);
    assert!(close(f.used.usd, 15.0), "spent so far: {}", f.used.usd);
    let c = f.capacity.expect("a capacity");
    assert!(close(c.usd, 40.0), "{} != 10 / 0.25", c.usd);
    assert_eq!(c.at, reading_at);
    let left = f.left.expect("a left");
    assert!(close(left.usd, 20.0));
    assert_eq!(left.age_s, 60);
    // A newest reading from the window before: the reading is u_now.
    a.newest = Some(tap(900, now - 6 * HOUR));
    let left = figure(&spend, &a).left.expect("a left");
    assert!(close(left.usd, 30.0) && left.age_s == 22 * 60, "{left:?}");
    // NEGATIVE CONTROL: a numerator that does not stop at the reading takes
    // the later $5, which shares the reading's bucket.
    let to_bucket = spend.sum(
        Span {
            from: now + HOUR - FIVE_H,
            to: reading_at,
            whole: false,
        },
        None,
    );
    assert!(close(to_bucket.usd / 0.25, 60.0), "{}", to_bucket.usd);
}

/// A WEEK'S CAPACITY IS SUMMED FROM ITS FIVE-MINUTE BUCKETS, days after the
/// spend: a reading 35 minutes into an hour three days back holds the $6
/// served at :25 and not the $4 at :50, once time has passed and the spend
/// file has been through `expire`. Negative control: a reading at :26, in
/// the $6's own five minutes, holds neither (only whole buckets count), so
/// it has no capacity: `low`.
#[test]
fn a_weeks_capacity_is_summed_from_its_five_minute_buckets() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let hour = now - 3 * 24 * HOUR;
    let mut spend = spend_at(&[(hour + 25 * 60, 6.0), (hour + 50 * 60, 4.0)], hour + HOUR);
    spend.expire(now);
    let week = |reading_at: i64| {
        let mut a = ask(now, Some(tap(250, reading_at)));
        a.length_s = 7 * 24 * HOUR;
        a.resets_at = Some(now + 2 * 24 * HOUR);
        figure(&spend, &a)
    };
    let f = week(hour + 35 * 60);
    assert!(close(f.used.usd, 10.0), "{}", f.used.usd);
    let c = f.capacity.expect("a capacity");
    assert!(close(c.usd, 24.0), "{} != 6 / 0.25", c.usd);
    assert_eq!(f.left, Err(Why::Stale), "u_now is three days old");
    // NEGATIVE CONTROL.
    assert_eq!(week(hour + 26 * 60).capacity, Err(Why::Low));
}

/// A RESET USED INSIDE THE WINDOW starts the numerator (and the spent figure)
/// at its epoch: the vendor refilled the window there. A reading from before
/// the epoch is no reading of this window. Negative control: without the
/// epoch the earlier spend counts.
#[test]
fn a_reset_used_inside_the_window_starts_at_its_epoch() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let epoch = now - 2 * HOUR;
    let spend = spend_at(&[(now - 3 * HOUR, 50.0), (now - HOUR, 5.0)], now);
    let mut a = ask(now, Some(tap(100, now - 10 * 60)));
    a.epoch_at = Some(epoch);
    let f = figure(&spend, &a);
    assert!(close(f.used.usd, 5.0));
    assert!(close(f.capacity.expect("a capacity").usd, 50.0));
    a.reading = Some(tap(900, epoch - 60));
    assert_eq!(figure(&spend, &a).capacity, Err(Why::NoReading));
    // NEGATIVE CONTROL.
    let mut plain = ask(now, Some(tap(100, now - 10 * 60)));
    plain.epoch_at = None;
    assert!(close(
        figure(&spend, &plain).capacity.expect("a capacity").usd,
        550.0
    ));
}

/// USAGE CREDITS SERVED IN THE WINDOW: `C` and `left` print `on-credits`,
/// whenever in the window they served; what was spent still shows. No share
/// of a bucket is split off as credit spend. Negative control: the same
/// window with no credits has its capacity.
#[test]
fn credits_served_in_the_window_suppress_the_capacity() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let spend = spend_at(
        &[
            (now - 3 * HOUR, 10.0),
            (now - 2 * HOUR, 7.0),
            (now - 30 * 60, 3.0),
        ],
        now,
    );
    let mut a = ask(now, Some(tap(500, now - 10 * 60)));
    a.on_credits = true;
    let f = figure(&spend, &a);
    assert!(close(f.used.usd, 20.0), "{}", f.used.usd);
    assert_eq!(
        (f.capacity.err(), f.left.err()),
        (Some(Why::OnCredits), Some(Why::OnCredits))
    );
    // NEGATIVE CONTROL.
    let plain = figure(&spend, &ask(now, Some(tap(500, now - 10 * 60))));
    assert!(close(plain.capacity.expect("a capacity").usd, 40.0));
}

/// EVERY EMPTY FIGURE SAYS WHY, with one closed word; and a figure that
/// means something says nothing (the negative control).
#[test]
fn each_empty_figure_says_why() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let spend = spend_at(&[(now - 2 * HOUR, 10.0)], now);
    let r = |pct| Some(tap(pct, now - 60));
    let why = |a: &Ask<'_>| {
        let f = figure(&spend, a);
        (f.capacity.err(), f.left.err())
    };
    // NEGATIVE CONTROL: a good reading.
    assert_eq!(why(&ask(now, r(250))), (None, None));
    // Rolling: no reset known.
    let mut rolling = ask(now, r(250));
    rolling.resets_at = None;
    let f = figure(&spend, &rolling);
    assert!(f.rolling && close(f.used.usd, 10.0));
    assert_eq!(why(&rolling), (Some(Why::NoReading), Some(Why::NoReading)));
    assert_eq!(
        why(&ask(now, None)),
        (Some(Why::NoReading), Some(Why::NoReading))
    );
    // Before the window.
    assert_eq!(
        why(&ask(now, Some(tap(250, now - 5 * HOUR)))).0,
        Some(Why::NoReading)
    );
    // Low: under 5 whole, under 1 in tenths.
    assert_eq!(why(&ask(now, r(44))).0, Some(Why::Low));
    let mut tenths = ask(now, r(44));
    tenths.tenths_seen = true;
    assert_eq!(why(&tenths), (None, None));
    tenths.reading = r(9);
    assert_eq!(why(&tenths).0, Some(Why::Low));
    // Low too when no whole bucket of spend comes before the reading, as at
    // a window's start: $8 in its first five minutes, read at 1.2% inside
    // them, would give C = $0 and $0 left. Once that bucket has ended, the
    // same $8 has a capacity.
    let start = now + HOUR - FIVE_H;
    let first = spend_at(&[(start + 20, 8.0)], now);
    let mut early = ask(now, Some(tap(12, start + 250)));
    early.tenths_seen = true;
    early.now = start + 250;
    let f = figure(&first, &early);
    assert!(close(f.used.usd, 8.0), "{}", f.used.usd);
    assert_eq!((f.capacity, f.left), (Err(Why::Low), Err(Why::Low)));
    early.reading = Some(tap(14, start + 310));
    early.now = start + 310;
    let c = figure(&first, &early).capacity.expect("a capacity");
    assert!(close(c.usd, 8.0 / 0.014), "{}", c.usd);
    // At the limit: C stands, nothing is left.
    let mut full = ask(now, r(250));
    full.newest = r(1000);
    assert_eq!(why(&full), (None, Some(Why::AtLimit)));
    // Stale: older than a tenth of the window, or a silent machine.
    let mut old = ask(now, r(250));
    old.newest = Some(tap(300, now - FIVE_H / 10 - 1));
    assert_eq!(why(&old), (None, Some(Why::Stale)));
    old.newest = Some(tap(300, now - FIVE_H / 10));
    assert_eq!(why(&old), (None, None), "at a tenth exactly");
    let mut silent = ask(now, r(250));
    silent.machines.silent = 1;
    assert_eq!(why(&silent), (None, Some(Why::Stale)));
    // Unattributed over 1%; an API key seen.
    let mut shared = ask(now, r(250));
    shared.attribution = Attribution::Unattributed { share: 0.02 };
    assert_eq!(why(&shared).0, Some(Why::Unattributed));
    shared.attribution = Attribution::Unattributed { share: 0.005 };
    assert_eq!(why(&shared).0, None);
    let mut keyed = ask(now, r(250));
    keyed.api_key_seen = true;
    assert_eq!(why(&keyed), (Some(Why::ApiKeySeen), Some(Why::ApiKeySeen)));
    // Unpriced over 1% of a class; unpriced spend alone is `unpriced` too,
    // not `low`.
    let unknown = Message {
        at: now - 2 * HOUR,
        model: "claude-opus-9",
        tokens: TokenUsage {
            input: 1_000_000,
            ..TokenUsage::default()
        },
        approx: false,
    };
    let mut unpriced = spend.clone();
    unpriced.add_message(&unknown, &table(), now);
    let mut alone = spend_at(&[], now);
    alone.add_message(&unknown, &table(), now);
    for s in [&unpriced, &alone] {
        assert_eq!(figure(s, &ask(now, r(250))).capacity, Err(Why::Unpriced));
    }
    for (w, word) in [
        (Why::NoReading, "no-reading"),
        (Why::Low, "low"),
        (Why::AtLimit, "at-limit"),
        (Why::Stale, "stale"),
        (Why::Unpriced, "unpriced"),
        (Why::Unattributed, "unattributed"),
        (Why::ApiKeySeen, "api-key-seen"),
        (Why::OnCredits, "on-credits"),
    ] {
        assert_eq!(w.as_str(), word);
    }
}

/// THE ERROR BAR is the reading's resolution: whole percents until a tenth
/// is seen, a painted panel's floor read as the middle of its percent, a
/// wall exact — then, calibrated, the past windows' spread too.
#[test]
fn the_error_bar_is_the_resolution_then_the_calibrated_spread() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let spend = spend_at(&[(now - 2 * HOUR, 10.0)], now);
    let cap = |a: &Ask<'_>| figure(&spend, a).capacity.expect("a capacity");
    let whole = cap(&ask(now, Some(tap(250, now - 60))));
    assert!(close(whole.pm_pct, 2.0), "0.5 of 25: {}", whole.pm_pct);
    assert_eq!(whole.calibration, None, "uncalibrated");
    let mut tenths = ask(now, Some(tap(250, now - 60)));
    tenths.tenths_seen = true;
    assert!(close(cap(&tenths).pm_pct, 0.2));
    let panel = cap(&ask(
        now,
        Some(Reading {
            pct_x10: 250,
            at: now - 60,
            kind: ReadingKind::Floored,
        }),
    ));
    assert!(close(panel.usd, 10.0 / 0.255) && close(panel.pm_pct, 0.5 / 25.5 * 100.0));
    let wall = cap(&ask(
        now,
        Some(Reading {
            pct_x10: 1000,
            at: now - 60,
            kind: ReadingKind::Wall,
        }),
    ));
    assert!(close(wall.usd, 10.0) && close(wall.pm_pct, 0.0));
    let mut calibrated = ask(now, Some(tap(250, now - 60)));
    calibrated.calibration = Some(Calibration {
        cv_pct: 12.0,
        windows: 5,
    });
    assert!(close(cap(&calibrated).pm_pct, 2.0_f64.hypot(12.0)));
}

/// CALIBRATION needs three closed windows under the current plan and tier,
/// counted back to a change of plan. Negative control: two are not enough.
#[test]
fn calibration_counts_closed_windows_since_the_plan_changed() {
    let c = |at: i64, usd: f64, plan: &str| Close {
        at,
        capacity_usd: usd,
        plan: plan.to_owned(),
    };
    let two = [c(1, 100.0, "max/5x"), c(2, 110.0, "max/5x")];
    assert_eq!(calibrate(&two, "max/5x"), None, "NEGATIVE CONTROL");
    let three = [
        c(1, 100.0, "max/5x"),
        c(2, 110.0, "max/5x"),
        c(3, 90.0, "max/5x"),
    ];
    let cal = calibrate(&three, "max/5x").expect("calibrated");
    assert_eq!(cal.windows, 3);
    assert!(close(cal.cv_pct, 10.0), "{}", cal.cv_pct);
    // A tier change in between starts the count again, in any input order.
    let changed = [
        c(4, 300.0, "max/20x"),
        c(1, 100.0, "max/5x"),
        c(3, 90.0, "max/5x"),
        c(5, 310.0, "max/20x"),
        c(2, 110.0, "max/5x"),
    ];
    assert_eq!(calibrate(&changed, "max/20x"), None);
    assert_eq!(calibrate(&changed, "max/5x"), None, "not the newest run");
}

/// THE FABLE BUCKET's dollars are its models' only. Negative control: the
/// whole window holds both.
#[test]
fn the_fable_bucket_sums_only_fable_models() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let mut spend = spend_at(&[(now - HOUR, 4.0)], now);
    spend.add_message(
        &Message {
            at: now - HOUR,
            model: "claude-fable-5-1",
            tokens: TokenUsage {
                input: 1_000_000,
                ..TokenUsage::default()
            },
            approx: false,
        },
        &table(),
        now,
    );
    let mut a = ask(now, None);
    a.family = Some("fable");
    assert!(close(figure(&spend, &a).used.usd, 2.0));
    assert!(close(figure(&spend, &ask(now, None)).used.usd, 6.0));
}

// ---------------------------------------------------------------------------
// The rows of one message, each message's price, cursors, the seam
// ---------------------------------------------------------------------------

/// A message's first row as Claude Code 2.1.300 writes it while the message
/// streams: no `speed` (nor `server_tool_use` or `iterations`, which [`row`]
/// writes only as `extra`). A later row carries them, with the final usage.
fn first_streamed(row: &str) -> String {
    assert!(row.contains(r#","speed":"standard""#));
    row.replacen(r#","speed":"standard""#, "", 1)
}

/// A MESSAGE IS APPROXIMATE WHEN ANY ROW OF IT SAYS SO, and is counted so
/// once: fast mode said on its last row only, both rows read in one fold; a
/// web search said on the last row a fold and a spend file later; a last
/// row that adds nothing but `speed`; an advisor's iterations on the last
/// row only, after a first row with Claude Code's empty `iterations`.
/// Negative control: a message fast on both rows counts once, its first row
/// folded before a spend file is saved and loaded; a standard one whose
/// rows carry the empty `iterations` and then its own iteration is not
/// approximate.
#[test]
fn a_message_is_approximate_when_any_row_of_it_says_so() {
    let home = Home::new("approx-late");
    let now = now();
    let opus = "claude-opus-5-5";
    let main = home.transcript("-p/s.jsonl");
    let store = SpendStore {
        dir: home.path().join("accounts").join("spend"),
    };
    let counts = |spend: &Spend| {
        let all = everything(spend, now);
        (all.messages, all.approx)
    };
    let (mut spend, _) = store.load(Agent::Claude, "main", home.path(), &table());
    let whole = mtok_in().plus(&out(100_000));
    let fast = r#","speed":"fast""#;
    let empty = r#","iterations":[]"#;
    // Fast mode said on the last row only, both rows read in one fold.
    append(
        &main,
        &first_streamed(&row("msg_f", opus, now - 60, mtok_in(), "")),
    );
    append(&main, &row("msg_f", opus, now - 60, whole, fast));
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (1, 1));
    assert!(close(everything(&spend, now).usd, 2.0), "at the base rates");
    // A web search said on the last row, a fold and a spend file later.
    append(
        &main,
        &first_streamed(&row("msg_w", opus, now - 50, mtok_in(), "")),
    );
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (2, 1));
    store.save(&spend).expect("save");
    let (mut spend, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, None, "the spend file was kept");
    let search = r#","server_tool_use":{"web_search_requests":1}"#;
    append(&main, &row("msg_w", opus, now - 50, whole, search));
    // A last row that adds nothing but `speed`.
    append(
        &main,
        &first_streamed(&row("msg_z", opus, now - 40, mtok_in(), "")),
    );
    append(&main, &row("msg_z", opus, now - 40, mtok_in(), fast));
    // An advisor's iterations on the last row only.
    let advisor = r#","iterations":[{"type":"message"},{"type":"advisor_message","model":"claude-opus-5-5"}]"#;
    append(
        &main,
        &first_streamed(&row("msg_a", opus, now - 35, mtok_in(), empty)),
    );
    append(&main, &row("msg_a", opus, now - 35, whole, advisor));
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (4, 4));
    // NEGATIVE CONTROL.
    append(&main, &row("msg_b", opus, now - 30, mtok_in(), fast));
    spend.fold_claude_home(&table(), now, Limits::none());
    store.save(&spend).expect("save");
    let (mut spend, fresh) = store.load(Agent::Claude, "main", home.path(), &table());
    assert_eq!(fresh, None, "the spend file was kept");
    append(&main, &row("msg_b", opus, now - 30, whole, fast));
    append(
        &main,
        &first_streamed(&row("msg_s", opus, now - 20, mtok_in(), empty)),
    );
    let own = r#","iterations":[{"type":"message"}]"#;
    append(&main, &row("msg_s", opus, now - 20, whole, own));
    // A server-side fallback served this one: two models in one message.
    let fallback = r#","iterations":[{"type":"message","model":"claude-fable-5-1"},{"type":"fallback_message","model":"claude-opus-5-5"}]"#;
    append(
        &main,
        &first_streamed(&row("msg_k", opus, now - 10, mtok_in(), empty)),
    );
    append(&main, &row("msg_k", opus, now - 10, whole, fallback));
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (7, 6));
}

/// EACH MESSAGE IS PRICED ON ITS OWN, a model priced by prompt length too:
/// two requests under the tier, a session's and its subagent's in one
/// bucket, price at the base rates however large their sum. Negative
/// control: the same two requests priced as one pass the tier and price
/// otherwise.
#[test]
fn each_message_is_priced_on_its_own() {
    let tiered = format!(
        "{TABLE}\n[models.\"claude-haiku-5-5\"]\ninput = 1.0\noutput = 1.0\ncache_write = 1.0\ncache_write_1h = 1.0\ncache_read = 1.0\nsource = \"https://example.com/pricing, 2026-10-15\"\nid_from = \"https://example.com/models, 2026-10-15\"\n\n[models.\"claude-haiku-5-5\".long]\nover = 100000\ninput = 5.0\noutput = 5.0\ncache_write = 5.0\ncache_write_1h = 5.0\ncache_read = 5.0\n"
    );
    let prices = PriceTable::from_toml(&tiered).expect("the tiered table");
    let home = Home::new("per-message");
    let now = now();
    let short = TokenUsage {
        input: 60_000,
        ..TokenUsage::default()
    };
    let haiku = "claude-haiku-5-5";
    append(
        &home.transcript("-p/s.jsonl"),
        &row("msg_1", haiku, now - 60, short, ""),
    );
    append(
        &home.transcript("-p/s/subagents/agent-a.jsonl"),
        &row("msg_2", haiku, now - 60, short, ""),
    );
    let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
    spend.fold_claude_home(&prices, now, Limits::none());
    assert_eq!(spend.buckets().count(), 1, "one bucket holds both");
    let all = everything(&spend, now);
    assert_eq!(all.messages, 2);
    assert!(close(all.usd, 0.12), "{}", all.usd);
    // NEGATIVE CONTROL.
    let as_one = prices
        .message_usd(haiku, &short.plus(&short))
        .expect("priced");
    assert!(close(as_one, 0.6), "{as_one}");
}

/// A retained cache-read token is priced below the prompt tier but has no
/// rate above it. Only the prompt's one-token growth reveals that the WHOLE
/// request is unpriced; it must not leave its earlier dollars behind.
fn repricing_table(price_long_cache: bool) -> PriceTable {
    let cache = if price_long_cache {
        "cache_read = 2.0"
    } else {
        ""
    };
    PriceTable::from_toml(&format!(
        "{TABLE}\n[models.\"claude-haiku-5-5\"]\ninput = 1.0\noutput = 1.0\ncache_read = 1.0\nsource = \"https://example.com/pricing\"\nid_from = \"https://example.com/models\"\n[models.\"claude-haiku-5-5\".long]\nover = 100001\ninput = 2.0\noutput = 2.0\n{cache}\n"
    ))
    .expect("the composed table")
}

fn below_repricing_tier() -> TokenUsage {
    TokenUsage {
        input: 100_000,
        output: 1_000_000,
        cache_read: 1,
        ..TokenUsage::default()
    }
}

#[test]
fn a_stream_that_becomes_unpriced_retires_its_whole_prior_price_after_reload() {
    // Positive control first: a complete long row reprices the request normally.
    for priced in [true, false] {
        let home = Home::new("repricing");
        let now = now();
        let at = now - 2 * BUCKET_S;
        let prices = repricing_table(priced);
        let path = home.transcript("-p/s.jsonl");
        let model = "claude-haiku-5-5";
        let first = below_repricing_tier();
        append(&path, &row("msg_stream", model, at, first, ""));
        // Another message shares the bucket; invalidating the stream cannot
        // subtract its dollars, tokens or count.
        append(
            &path,
            &row("msg_other", "claude-opus-5-5", at, mtok_in(), ""),
        );
        let store = SpendStore {
            dir: home.path().join("accounts/spend"),
        };
        let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
        assert!(
            spend
                .fold_claude_home(&prices, now, Limits::none())
                .complete()
        );
        assert!(close(everything(&spend, now).usd, 2.100001));
        assert!(
            figure(&spend, &ask(now, Some(tap(250, now))))
                .capacity
                .is_ok()
        );
        store.save(&spend).unwrap();
        let (mut spend, fresh) = store.load(Agent::Claude, "main", home.path(), &prices);
        assert_eq!(fresh, None);
        let last = TokenUsage {
            input: first.input + 1,
            ..first
        };
        append(&path, &row("msg_stream", model, at + 1, last, ""));
        let report = spend.fold_claude_home(&prices, now, Limits::none());
        assert_eq!((report.messages, report.duplicates), (0, 1));
        let all = everything(&spend, now);
        assert_eq!(
            (all.messages, all.tokens.input, all.tokens.output),
            (2, 1_100_001, 1_000_000)
        );
        if priced {
            assert!(close(
                all.usd,
                1.0 + prices.message_usd(model, &last).unwrap()
            ));
            assert!(all.unpriced.is_empty());
        } else {
            assert!(
                close(all.usd, 1.0),
                "the unpriceable request retained dollars: {all:?}"
            );
            assert_eq!(all.unpriced[model], last);
            assert_eq!(
                figure(&spend, &ask(now, Some(tap(250, now)))).capacity,
                Err(Why::Unpriced)
            );
            store.save(&spend).unwrap();
            let (mut again, fresh) = store.load(Agent::Claude, "main", home.path(), &prices);
            assert_eq!(fresh, None);
            assert_eq!(again.fold_claude_home(&prices, now, Limits::none()).read, 0);
            assert_eq!(everything(&again, now), all);
        }
    }
}

#[test]
fn unpricing_a_stream_corrects_its_old_buckets_without_backdating_later_spend() {
    for priced in [true, false] {
        let home = Home::new("repricing-buckets");
        let now = now();
        let first_at = now.div_euclid(BUCKET_S) * BUCKET_S - 3 * BUCKET_S;
        let later_at = first_at + 2 * BUCKET_S;
        let prices = repricing_table(priced);
        let model = "claude-haiku-5-5";
        let first = below_repricing_tier();
        let path = home.transcript("-p/s.jsonl");
        append(&path, &row("msg_stream", model, first_at, first, ""));
        append(
            &path,
            &row("msg_other", "claude-opus-5-5", first_at, mtok_in(), ""),
        );
        let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
        spend.fold_claude_home(&prices, now, Limits::none());
        let grown = TokenUsage {
            output: first.output + 1000,
            ..first
        };
        append(
            &path,
            &row("msg_stream", model, first_at + BUCKET_S, grown, ""),
        );
        spend.fold_claude_home(&prices, now, Limits::none());
        // Persist after the request already contributed to two different buckets.
        spend = Spend::from_json(&spend.to_json()).expect("the cursor survives save/load");
        let last = TokenUsage {
            input: first.input + 1,
            ..grown
        };
        append(&path, &row("msg_stream", model, later_at, last, ""));
        spend.fold_claude_home(&prices, now, Limits::none());
        let earlier = spend.sum(
            Span {
                from: first_at,
                to: later_at,
                whole: true,
            },
            None,
        );
        if priced {
            // The final tier surcharge happened after this reading. Keeping a
            // stable first-bucket total would falsely inflate its numerator.
            assert!(
                close(earlier.usd, 2.101001),
                "later spend moved before its reading: {earlier:?}"
            );
            assert!(earlier.unpriced.is_empty());
        } else {
            assert!(
                close(earlier.usd, 1.0),
                "old buckets retained the request's price: {earlier:?}"
            );
            assert_eq!(earlier.unpriced[model], grown);
            assert_eq!(everything(&spend, now).unpriced[model], last);
        }
        assert_eq!(earlier.messages, 2);
    }
}

#[test]
fn legacy_spend_without_request_provenance_is_rebuilt_from_transcripts() {
    let home = Home::new("repricing-legacy");
    let now = now();
    let at = now - 2 * BUCKET_S;
    let prices = repricing_table(false);
    let first = below_repricing_tier();
    let path = home.transcript("-p/s.jsonl");
    append(&path, &row("msg_stream", "claude-haiku-5-5", at, first, ""));
    let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
    spend.fold_claude_home(&prices, now, Limits::none());
    let mut old: Value = aterm_json::from_str(&spend.to_json()).unwrap();
    let old_map = old.as_object_mut().unwrap();
    old_map.insert("v".to_owned(), Value::from(3));
    for file in old_map
        .get_mut("files")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        for held in file
            .as_object_mut()
            .unwrap()
            .get_mut("ids")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            held.as_array_mut().unwrap().truncate(3);
        }
    }
    let mut missing = old.clone();
    missing
        .as_object_mut()
        .unwrap()
        .insert("v".to_owned(), Value::from(FORMAT));
    assert!(
        Spend::from_json(&missing.to_string()).is_none(),
        "a claimed current format cannot omit the contribution field"
    );
    // The completed message would have invalidated the legacy cache's USD,
    // but no legacy cursor knows the original bucket contribution.
    let last = TokenUsage {
        input: first.input + 1,
        ..first
    };
    append(
        &path,
        &row("msg_stream", "claude-haiku-5-5", at + 1, last, ""),
    );
    let store = SpendStore {
        dir: home.path().join("accounts/spend"),
    };
    fs::create_dir_all(&store.dir).unwrap();
    fs::write(store.path(Agent::Claude, "main").unwrap(), old.to_string()).unwrap();
    let (mut rebuilt, why) = store.load(Agent::Claude, "main", home.path(), &prices);
    assert_eq!(why, Some(Fresh::Unreadable));
    assert!(
        rebuilt
            .fold_claude_home(&prices, now, Limits::none())
            .complete()
    );
    let all = everything(&rebuilt, now);
    assert_eq!(all.messages, 1);
    assert!(close(all.usd, 0.0));
    assert_eq!(all.unpriced["claude-haiku-5-5"], last);
}

#[test]
fn a_later_fully_priced_tier_resolves_unknown_tokens_without_backdating_dollars() {
    let home = Home::new("repricing-known");
    let now = now();
    let at = now.div_euclid(BUCKET_S) * BUCKET_S - 3 * BUCKET_S;
    let later = at + 2 * BUCKET_S;
    // The reverse of the missing-long-rate case: only the long tier prices reads.
    let mut prices = repricing_table(true);
    prices
        .rows
        .get_mut("claude-haiku-5-5")
        .unwrap()
        .rates
        .cache_read = None;
    let first = below_repricing_tier();
    let model = "claude-haiku-5-5";
    let path = home.transcript("-p/s.jsonl");
    append(&path, &row("msg_stream", model, at, first, ""));
    let mut spend = Spend::new(Agent::Claude, "main", home.path(), &prices);
    spend.fold_claude_home(&prices, now, Limits::none());
    assert_eq!(everything(&spend, now).unpriced[model], first);
    spend = Spend::from_json(&spend.to_json()).unwrap();
    let last = TokenUsage {
        input: first.input + 1,
        ..first
    };
    append(&path, &row("msg_stream", model, later, last, ""));
    spend.fold_claude_home(&prices, now, Limits::none());
    let all = everything(&spend, now);
    assert!(all.unpriced.is_empty());
    assert!(close(all.usd, prices.message_usd(model, &last).unwrap()));
    let before = spend.sum(
        Span {
            from: at,
            to: later,
            whole: true,
        },
        None,
    );
    assert!(close(before.usd, 0.0), "newly known dollars were backdated");
    assert_eq!(all.tokens, last);
    assert_eq!(all.messages, 1);
}

/// Set `path`'s mtime to `at`, unix seconds: a transcript last written then.
fn written_at(path: &Path, at: i64) {
    let when = UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(at).expect("after 1970"));
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open")
        .set_modified(when)
        .expect("set the mtime");
}

/// A TRANSCRIPT PAST THE HORIZON keeps no cursor, though another transcript
/// of its session is live; written again, it is read from its start, and a
/// message the home already counted is not counted again. Negative control:
/// with the home's counted ids forgotten, it is.
#[test]
fn a_transcript_past_the_horizon_lets_its_cursor_go() {
    let run = |label: &str, forget: bool| {
        let home = Home::new(label);
        let now = now();
        let opus = "claude-opus-5-5";
        let main = home.transcript("-p/s.jsonl");
        let rel = "-p/s/subagents/agent-a.jsonl";
        let sub = home.transcript(rel);
        append(&main, &row("msg_1", opus, now - 60, mtok_in(), ""));
        let first = row("msg_2", opus, now - 60, mtok_in(), "");
        append(&sub, &first);
        let mut spend = spend_of(&home);
        spend.fold_claude_home(&table(), now, Limits::none());
        written_at(&sub, now - HORIZON_S - HOUR);
        spend.fold_claude_home(&table(), now, Limits::none());
        assert!(!spend.files.contains_key(rel), "no cursor past the horizon");
        if forget {
            spend.seen.clear();
        }
        let more = row("msg_3", opus, now - 20, mtok_in(), "");
        append(&sub, &more);
        let report = spend.fold_claude_home(&table(), now, Limits::none());
        assert_eq!(
            report.read,
            (first.len() + more.len()) as u64,
            "from its start"
        );
        (report.messages, report.duplicates)
    };
    assert_eq!(run("past", false), (1, 1));
    // NEGATIVE CONTROL.
    assert_eq!(run("past-control", true), (2, 0));
}

/// THE SEAM another agent's fold feeds: a Codex spend takes messages through
/// `add_message` and folds nothing from a home itself.
#[test]
fn add_message_is_the_seam_and_a_codex_spend_folds_no_home() {
    let home = Home::new("codex");
    let now = now();
    append(
        &home.transcript("-p/s.jsonl"),
        &row("msg_a", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let mut codex = Spend::new(Agent::Codex, "codex", home.path(), &table());
    assert_eq!(
        codex
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        0
    );
    codex.add_message(
        &Message {
            at: now - 60,
            model: "claude-opus-5-5",
            tokens: mtok_in(),
            approx: true,
        },
        &table(),
        now,
    );
    codex.add_message(
        &Message {
            at: now - HORIZON_S - 1,
            model: "claude-opus-5-5",
            tokens: mtok_in(),
            approx: false,
        },
        &table(),
        now,
    );
    let all = everything(&codex, now);
    assert_eq!((all.messages, all.approx), (1, 1));
    assert!(close(all.usd, 1.0));
    // A model that is no model id is filed unpriced under `unknown`, never
    // kept as written. Negative control: the id above was kept.
    codex.add_message(
        &Message {
            at: now - 60,
            model: "gpt\u{1b}[31m x",
            tokens: mtok_in(),
            approx: false,
        },
        &table(),
        now,
    );
    let all = everything(&codex, now);
    assert_eq!(all.unpriced.keys().collect::<Vec<_>>(), [UNKNOWN_MODEL]);
    assert!(codex.buckets().all(|(_, m)| {
        m.keys()
            .all(|k| k == "claude-opus-5-5" || k == UNKNOWN_MODEL)
    }));
}

// ---------------------------------------------------------------------------
// Codex rollouts (C1)
// ---------------------------------------------------------------------------

use crate::harness::codex_rollout::fixtures::{context, copied, count, message, meta};

/// Thread ids in UUIDv7 form: a conversation's own thread and a subagent
/// forked from it.
const ROOT: &str = "01900000-0000-7000-8000-000000000001";
const SUB: &str = "01900000-0000-7000-8000-000000000002";

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

/// `<home>/sessions/2026/10/15/rollout-…-<thread>.jsonl`, its folder made.
fn rollout(home: &Home, thread: &str) -> PathBuf {
    let day = home.path().join("sessions/2026/10/15");
    fs::create_dir_all(&day).expect("mkdir");
    day.join(format!("rollout-2026-10-15T10-00-00-{thread}.jsonl"))
}

/// The conversation's own thread: two turns on sol, an hour before `now`.
fn parent_lines(now: i64) -> String {
    let b = (now - HOUR) * 1000;
    [
        meta(b, ROOT, ROOT, None),
        context(b + 1000, "gpt-5.6-sol", None),
        count(b + 5000, (1000, 800, 50), (1000, 800, 50)),
        count(b + 9000, (3000, 2600, 150), (2000, 1800, 100)),
    ]
    .concat()
}

/// A subagent forked from it a minute later: the parent's lines copied in
/// one burst, then a turn of its own on luna.
fn fork_lines(now: i64) -> (String, String) {
    let at = (now - HOUR) * 1000 + 60_000;
    let copy = [
        meta(at, SUB, ROOT, Some(ROOT)),
        copied(&parent_lines(now), at + 1),
    ]
    .concat();
    let own = [
        message(at + 2500),
        context(at + 2501, "gpt-6-luna", None),
        count(at + 7000, (3500, 2600, 250), (500, 0, 100)),
    ]
    .concat();
    (copy, own)
}

fn codex_spend(home: &Home) -> Spend {
    Spend::new(Agent::Codex, "codex", home.path(), &codex_table())
}

/// THE CODEX FOLD (the design's §4.3, §8.4): every rollout of the home,
/// each served turn once into the spend at the table's price — the
/// parent's two turns on sol, the fork's own on luna — and nothing of the
/// fork's copied prefix. Negative controls: the fork with its parent past
/// the horizon still counts only its own turn; read without its
/// `forked_from_id`, as the per-file rule reads it, it counts the parent's
/// two again.
#[test]
fn a_codex_home_counts_each_turn_once_into_its_spend() {
    let home = Home::new("codex-fold");
    let now = now();
    let parent = rollout(&home, ROOT);
    let fork = rollout(&home, SUB);
    fs::write(&parent, parent_lines(now)).expect("write");
    let (copy, own) = fork_lines(now);
    fs::write(&fork, format!("{copy}{own}")).expect("write");
    let mut spend = codex_spend(&home);
    let report = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!(
        (report.files, report.messages, report.copied),
        (2, 3, 2),
        "{report:?}"
    );
    assert!(report.complete(), "{report:?}");
    let all = everything(&spend, now);
    assert_eq!(all.messages, 3);
    // sol: 400 uncached in, 2,600 cached, 150 out; luna: 500 in, 100 out.
    assert_eq!(
        all.tokens,
        TokenUsage {
            input: 900,
            output: 250,
            cache_read: 2600,
            ..TokenUsage::default()
        }
    );
    let sol = 400.0 * 1.0 + 2600.0 * 0.1 + 150.0 * 10.0;
    let luna = 500.0 * 2.0 + 100.0 * 20.0;
    assert!(close(all.usd, (sol + luna) / 1e6), "{}", all.usd);
    assert!(all.unpriced.is_empty());
    // Placed in the buckets of their own turns.
    let fork_turn = (now - HOUR) + 67;
    let luna_bucket = fork_turn.div_euclid(BUCKET_S) * BUCKET_S;
    assert!(
        spend
            .buckets()
            .any(|(start, m)| start == luna_bucket && m.contains_key("gpt-6-luna"))
    );

    // NEGATIVE CONTROL 1: the parent past the horizon, the fork alone.
    written_at(&parent, now - HORIZON_S - HOUR);
    let mut alone = codex_spend(&home);
    let report = alone.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((report.files, report.messages, report.copied), (1, 1, 2));
    assert!(close(everything(&alone, now).usd, luna / 1e6));
    // NEGATIVE CONTROL 2: no fork named, the copies count again.
    fs::write(
        &fork,
        format!("{copy}{own}").replace(&format!(",\"forked_from_id\":\"{ROOT}\""), ""),
    )
    .expect("write");
    let mut per_file = codex_spend(&home);
    let report = per_file.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((report.messages, report.copied), (3, 0));
    assert!(close(everything(&per_file, now).usd, (sol + luna) / 1e6));
}

/// A SECOND FOLD reads only what was appended, and a fork folded while its
/// copy was all it held keeps its place in the copy through the spend file:
/// its own turn, appended after, is counted and the copy is not. Negative
/// control: a spend file whose rollout cursor lost its reader is refused,
/// and folded again from the rollouts.
#[test]
fn a_second_codex_fold_reads_only_the_appended_bytes_through_its_spend_file() {
    let home = Home::new("codex-again");
    let now = now();
    let fork = rollout(&home, SUB);
    let (copy, own) = fork_lines(now);
    fs::write(&fork, &copy).expect("write");
    let store = SpendStore {
        dir: home.path().join("spend"),
    };
    let (mut spend, fresh) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    assert_eq!(fresh, Some(Fresh::NoFile));
    let first = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((first.messages, first.copied), (0, 2));
    store.save(&spend).expect("save");
    let (mut again, fresh) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    assert_eq!(fresh, None, "kept");
    assert_eq!(again, spend, "the reader's place in the copy came back");
    append(&fork, &own);
    let second = again.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((second.messages, second.copied), (1, 0));
    assert_eq!(
        second.read,
        own.len() as u64 + 1,
        "the appended bytes and the byte before"
    );
    let all = everything(&again, now);
    assert_eq!(all.tokens.input, 500, "the fork's own turn alone");

    // NEGATIVE CONTROL.
    let path = store.path(Agent::Codex, "codex").expect("a path");
    let text = fs::read_to_string(&path).expect("read");
    let at = text.find(",\"x\":[").expect("the reader is kept");
    let end = at + text[at..].find(']').expect("its end") + 1;
    fs::write(&path, format!("{}{}", &text[..at], &text[end..])).expect("write");
    let (_, fresh) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    assert_eq!(fresh, Some(Fresh::Unreadable));
}

/// A LEGACY FORK FOLDED INSIDE ITS COPY keeps its own thread through the
/// spend file (Codex 0.163.0, `harness::codex_rollout`'s module header): its
/// own `thread_settings_applied`, appended 17 ms after the copy with a
/// prompt's turn behind it, ends the copy, and the turn counts on its own
/// model. A reader kept before the thread was (five entries) still loads.
/// Negative control: that older reader, with no thread to end the copy by,
/// takes the turn, inside the burst, for the copy's.
#[test]
fn a_legacy_fork_folded_inside_its_copy_keeps_its_thread_through_the_spend_file() {
    use crate::harness::codex_rollout::fixtures::{meta_as, settings};
    let home = Home::new("codex-prompted");
    let now = now();
    let fork = rollout(&home, SUB);
    let at = (now - HOUR) * 1000;
    let copy = [
        meta_as(at, SUB, SUB, Some(ROOT), Some("legacy"), None),
        copied(&parent_lines(now), at),
    ]
    .concat();
    let own = [
        settings(at + 20, SUB),
        context(at + 30, "gpt-6-luna", None),
        count(at + 45, (3500, 2600, 250), (500, 0, 100)),
    ]
    .concat();
    fs::write(&fork, &copy).expect("write");
    let store = SpendStore {
        dir: home.path().join("spend"),
    };
    let (mut spend, _) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    let first = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((first.messages, first.copied), (0, 2));
    store.save(&spend).expect("save");
    let path = store.path(Agent::Codex, "codex").expect("a path");
    let kept = fs::read_to_string(&path).expect("read");
    let (mut again, fresh) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    assert_eq!(fresh, None, "kept");
    assert_eq!(again, spend, "the reader's thread came back");
    append(&fork, &own);
    let second = again.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((second.messages, second.copied), (1, 0), "{second:?}");
    assert_eq!(everything(&again, now).tokens.input, 500);
    assert!(again.buckets().any(|(_, m)| m.contains_key("gpt-6-luna")));

    // NEGATIVE CONTROL.
    let thread = format!(",\"{SUB}\"]");
    assert!(kept.contains(&thread), "the thread is kept");
    fs::write(&path, kept.replace(&thread, "]")).expect("write");
    let (mut older, fresh) = store.load(Agent::Codex, "codex", home.path(), &codex_table());
    assert_eq!(fresh, None, "a five-entry reader loads");
    let third = older.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((third.messages, third.copied), (0, 1), "{third:?}");
}

/// A ROLLOUT READ AGAIN FROM ITS START (another file at its path) counts
/// none of its turns twice: the home knows them by their fingerprints.
/// Negative control: with the counted fingerprints forgotten, its turns
/// count again.
#[test]
fn a_rollout_folded_again_counts_its_turns_once() {
    let home = Home::new("codex-refold");
    let now = now();
    let parent = rollout(&home, ROOT);
    let replace = |path: &Path| {
        let next = path.with_extension("next");
        fs::write(&next, parent_lines(now)).expect("write");
        fs::rename(&next, path).expect("rename over");
    };
    fs::write(&parent, parent_lines(now)).expect("write");
    let mut spend = codex_spend(&home);
    assert_eq!(
        spend
            .fold_codex_home(&codex_table(), now, Limits::none())
            .messages,
        2
    );
    let once = everything(&spend, now);
    replace(&parent);
    let report = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!(
        (report.refolded, report.messages, report.duplicates),
        (1, 0, 2)
    );
    assert_eq!(everything(&spend, now), once);
    // NEGATIVE CONTROL.
    spend.seen.clear();
    replace(&parent);
    let report = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((report.refolded, report.messages), (1, 2));
    assert_eq!(everything(&spend, now).messages, 4);
}

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

/// EACH FOLD FOLDS ITS OWN AGENT'S FILES ONLY, and a Claude Code spend's
/// file holds what it always held: no transcript cursor carries a rollout
/// reader. Negative control: a Codex spend's rollout cursor does.
#[test]
fn each_fold_folds_its_own_agents_files_and_a_claude_spend_file_is_unchanged() {
    let home = Home::new("codex-own");
    let now = now();
    append(
        &home.transcript("-p/s.jsonl"),
        &row("msg_a", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    fs::write(rollout(&home, ROOT), parent_lines(now)).expect("write");
    let mut claude = spend_of(&home);
    let before = claude.clone();
    assert_eq!(
        claude.fold_codex_home(&codex_table(), now, Limits::none()),
        FoldReport::default()
    );
    assert_eq!(claude, before, "a Claude spend folds no rollout");
    assert_eq!(
        claude
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        1
    );
    let mut codex = codex_spend(&home);
    assert_eq!(
        codex.fold_claude_home(&table(), now, Limits::none()),
        FoldReport::default()
    );
    assert_eq!(
        codex
            .fold_codex_home(&codex_table(), now, Limits::none())
            .messages,
        2
    );
    let keys = |spend: &Spend| -> Vec<Vec<String>> {
        let v: Value = aterm_json::from_str(&spend.to_json()).expect("json");
        v["files"]
            .as_object()
            .expect("files")
            .values()
            .map(|f| f.as_object().expect("a file").keys().cloned().collect())
            .collect()
    };
    let transcript = ["b", "end", "id", "ids", "mtime", "off", "size", "skip"];
    assert_eq!(keys(&claude), [transcript.map(str::to_owned).to_vec()]);
    // NEGATIVE CONTROL.
    let mut with_reader = transcript.map(str::to_owned).to_vec();
    with_reader.push("x".to_owned());
    with_reader.sort();
    assert_eq!(keys(&codex), [with_reader]);
}

/// TWO THREADS SERVED THE SAME COUNTS are two turns (one prompt run twice,
/// as a script runs `codex exec`, or the lab's stub): their fingerprints
/// match, and neither the home's fold nor a conversation's dedupes across
/// rollouts by it. Negative control: the two turns' fingerprints are one, so
/// a fold that keyed its counted ids by the fingerprint alone would count
/// one.
#[test]
fn two_threads_served_the_same_counts_each_count() {
    use crate::harness::codex_rollout::{Row, TurnReader};
    let home = Home::new("codex-twice");
    let now = now();
    let b = (now - HOUR) * 1000;
    let run = |thread: &str, at: i64| {
        [
            meta(at, thread, thread, None),
            context(at + 1000, "gpt-5.6-sol", None),
            count(at + 5000, (1000, 800, 50), (1000, 800, 50)),
        ]
        .concat()
    };
    let (one, two) = (run(ROOT, b), run(SUB, b + 60_000));
    fs::write(rollout(&home, ROOT), &one).expect("write");
    fs::write(rollout(&home, SUB), &two).expect("write");
    let mut spend = codex_spend(&home);
    let report = spend.fold_codex_home(&codex_table(), now, Limits::none());
    assert_eq!((report.messages, report.duplicates), (2, 0), "{report:?}");
    assert_eq!(everything(&spend, now).tokens.cache_read, 1600);
    let conversation = fold_conversation(
        &[rollout(&home, ROOT), rollout(&home, SUB)],
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
