// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`]: price table loading, home-wide transcript folding, dollar spend, and Codex rollout aggregation.
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
/// with `extra` spliced into its usage, and its newline.
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
    Spend::new(home.path(), &table())
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
/// the input), not 1000 × $3.75.
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
    let unsplit = TokenUsage {
        cache_write_1h: 0,
        ..m.tokens()
    };
    let flat = table.message_usd("m", &unsplit).expect("priced");
    assert!((flat - (10.0 * 3.0 + 1000.0 * 3.75) / 1e6).abs() < 1e-12);
    assert!(flat < one);
}

/// NEVER A GUESS: tokens in a class the row has no rate for make the
/// request unpriced, and a model with no row is unpriced.
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
    let read = TokenUsage {
        input: 100,
        cache_read: 1000,
        ..TokenUsage::default()
    };
    let usd = no_writes.message_usd("gpt-x", &read).expect("priced");
    assert!((usd - (100.0 * 5.0 + 1000.0 * 0.5) / 1e6).abs() < 1e-12);
}

/// A final streamed row may reveal the cache-write lifetime without adding
/// another token.
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
    let mut spend = Spend::new(home.path(), &prices);
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

    let mut priced = Spend::new(home.path(), &table());
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
/// `[1m]` spelling is unpriced).
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

/// THE HOME-WIDE FOLD COUNTS SUBAGENTS (the design's F15): the main
/// transcript, a subagent's beside it and a workflow's nested deeper are all
/// summed.
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
    let alone = Home::new("main-only");
    append(
        &alone.transcript("-proj/sess.jsonl"),
        &row("msg_main", "claude-opus-5-5", at, mtok_in(), ""),
    );
    let mut main_only = spend_of(&alone);
    main_only.fold_claude_home(&table(), now, Limits::none());
    assert!(close(everything(&main_only, now).usd, 1.0));
}

/// THE FOLD'S DEADLINE BOUNDS THE WALK TOO (design §4.1: `accounts stats`
/// folds for at most [`super::super::cli::STATS_FOLD_S`]): a fold whose
/// deadline has passed lists nothing more and says its walk was cut, so the
/// discovery of a large home cannot run past the budget.
#[test]
fn the_folds_deadline_stops_its_walk() {
    let home = Home::new("walk-deadline");
    let now = now();
    append(
        &home.transcript("-proj/sess.jsonl"),
        &row("msg_main", "claude-opus-5-5", now - 60, mtok_in(), ""),
    );
    let mut spend = spend_of(&home);
    let passed = Limits {
        bytes: u64::MAX,
        until: Some(std::time::Instant::now()),
    };
    let report = spend.fold_claude_home(&table(), now, passed);
    assert!(report.walk_cut, "{report:?}");
    assert_eq!(report.files, 0, "{report:?}");
    assert!(!report.complete());
    let report = spend.fold_claude_home(&table(), now, Limits::none());
    assert!(!report.walk_cut && report.complete(), "{report:?}");
    assert_eq!((report.files, report.messages), (1, 1));
}

/// ONE MESSAGE, ONE COUNT, AT ITS LARGEST: a streamed message's rows repeat
/// its id with growing output, within one fold and across two (the id is
/// kept in the cursor between them).
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

/// THE COST RULE: a second fold of the same spend reads only what was
/// appended (plus the one boundary byte), and an unchanged home reads
/// nothing.
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
    let mut again = spend_of(&home);
    let first = again.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(first.read, first_rows.len() as u64);
    assert_eq!(
        again.fold_claude_home(&table(), now, Limits::none()).read,
        0
    );
    let more = row("msg_new", "claude-opus-5-5", now - 50, mtok_in(), "");
    append(&path, &more);
    let second = again.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(second.read, more.len() as u64 + 1);
    assert_eq!(second.messages, 1);
    assert!(close(everything(&again, now).usd, 21.0));
    let mut fresh_spend = spend_of(&home);
    let whole = fresh_spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(whole.read, (first_rows.len() + more.len()) as u64);
}

/// A failed directory read is not a complete empty home.
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

    let projects = home.path().join("projects");
    let parked = home.path().join("projects-held");
    fs::rename(&projects, &parked).expect("park the scratch projects directory");
    #[cfg(unix)]
    let other = Home::new("linked-projects");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        append(
            &other.transcript("p/foreign.jsonl"),
            &row("msg_foreign", "claude-opus-5-5", now - 60, mtok_in(), ""),
        );
        for target in [other.path().join("projects"), other.path().join("absent")] {
            symlink(&target, &projects).expect("link the scratch projects root");
            let refused = spend.fold_claude_home(&table(), now, Limits::none());
            assert!(
                !refused.complete() && refused.unread == 1 && refused.files == 0,
                "a linked projects root is unread, never another home's spend: {refused:?}"
            );
            let sum = everything(&spend, now);
            assert_eq!((sum.messages, sum.tokens.input), (1, 100));
            fs::remove_file(&projects).expect("remove only the owned projects link");
        }
        symlink(other.path().join("projects"), parked.join("linked"))
            .expect("link inside the scratch projects directory");
    }
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
/// to it is.
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
    let (control, report, now) = rewrite("rewritten-control", true);
    assert_eq!((report.refolded, report.messages), (1, 2));
    assert!(close(everything(&control, now).usd, 3.1));
}

/// A MESSAGE STREAMING WHEN ITS FILE IS FOLDED AGAIN is still raised by its
/// later rows: the ids the transcript held stand through the refold.
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
    let all = refold("refold-open-control", true);
    assert!(close(all.usd, 2.0), "{}", all.usd);
}

/// ONE MESSAGE, ONE COUNT IN THE HOME: a transcript that repeats messages
/// another one counted (a copy a fork or a resume could write) adds only
/// its own, across two folds of one spend and in one fold that reads the
/// copy first.
#[test]
fn a_message_another_transcript_repeats_counts_once_in_the_home() {
    let home = Home::new("copies");
    let now = now();
    let a = row("msg_a", "claude-opus-5-5", now - 300, mtok_in(), "");
    let b = row("msg_b", "claude-opus-5-5", now - 200, mtok_in(), "");
    append(&home.transcript("-p/s1.jsonl"), &format!("{a}{b}"));
    let mut spend = spend_of(&home);
    assert_eq!(
        spend
            .fold_claude_home(&table(), now, Limits::none())
            .messages,
        2
    );
    let c = row("msg_c", "claude-opus-5-5", now - 100, mtok_in(), "");
    append(&home.transcript("-p/s2.jsonl"), &format!("{a}{b}{c}"));
    let mut later = spend.clone();
    let report = later.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((report.messages, report.duplicates), (1, 2));
    assert!(close(everything(&later, now).usd, 3.0));
    let mut once = spend_of(&home);
    let report = once.fold_claude_home(&table(), now, Limits::none());
    assert_eq!((report.messages, report.duplicates), (3, 2));
    assert_eq!(everything(&once, now), everything(&later, now));
    let mut control = spend.clone();
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
/// so a sum over old spend is as fine as one over new.
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
    spend.expire(now + HORIZON_S + 3600);
    assert_eq!(spend.buckets().count(), 0, "all past the horizon");
    assert!(spend.seen.is_empty(), "and their ids");
}

/// NEVER A GUESS: a model with no row is counted and named, its dollars
/// unknown, and more than 1% of a class unpriced says so.
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
/// advisor's iterations.
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

/// A spend with `usd` of opus spend placed at each `(at, usd)`.
fn spend_at(rows: &[(i64, f64)], now: i64) -> Spend {
    let home = Home::new("figures");
    let mut spend = spend_of(&home);
    for &(at, usd) in rows {
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
        now,
    }
}

/// `C = $(window start, t_r] ÷ u(t_r)` and `left = C × (1 − u_now)`: spend
/// after the reading never enters `C`, even in the bucket the reading falls
/// in (only buckets that end by it count).
#[test]
fn capacity_divides_the_spend_to_the_reading_by_its_use() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
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
    a.newest = Some(tap(900, now - 6 * HOUR));
    let left = figure(&spend, &a).left.expect("a left");
    assert!(close(left.usd, 30.0) && left.age_s == 22 * 60, "{left:?}");
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
/// file has been through `expire`.
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
    assert_eq!(week(hour + 26 * 60).capacity, Err(Why::Low));
}

/// A RESET USED INSIDE THE WINDOW starts the numerator (and the spent figure)
/// at its epoch: the vendor refilled the window there.
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
    let mut plain = ask(now, Some(tap(100, now - 10 * 60)));
    plain.epoch_at = None;
    assert!(close(
        figure(&spend, &plain).capacity.expect("a capacity").usd,
        550.0
    ));
}

/// USAGE CREDITS SERVED IN THE WINDOW: `C` and `left` print `on-credits`,
/// whenever in the window they served; what was spent still shows.
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
    assert_eq!(why(&ask(now, r(250))), (None, None));
    let mut rolling = ask(now, r(250));
    rolling.resets_at = None;
    let f = figure(&spend, &rolling);
    assert!(f.rolling && close(f.used.usd, 10.0));
    assert_eq!(why(&rolling), (Some(Why::NoReading), Some(Why::NoReading)));
    assert_eq!(
        why(&ask(now, None)),
        (Some(Why::NoReading), Some(Why::NoReading))
    );
    assert_eq!(
        why(&ask(now, Some(tap(250, now - 5 * HOUR)))).0,
        Some(Why::NoReading)
    );
    assert_eq!(why(&ask(now, r(44))).0, Some(Why::Low));
    let mut tenths = ask(now, r(44));
    tenths.tenths_seen = true;
    assert_eq!(why(&tenths), (None, None));
    tenths.reading = r(9);
    assert_eq!(why(&tenths).0, Some(Why::Low));
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
    let mut full = ask(now, r(250));
    full.newest = r(1000);
    assert_eq!(why(&full), (None, Some(Why::AtLimit)));
    let mut old = ask(now, r(250));
    old.newest = Some(tap(300, now - FIVE_H / 10 - 1));
    assert_eq!(why(&old), (None, Some(Why::Stale)));
    old.newest = Some(tap(300, now - FIVE_H / 10));
    assert_eq!(why(&old), (None, None), "at a tenth exactly");
    let mut silent = ask(now, r(250));
    silent.machines.silent = 1;
    assert_eq!(why(&silent), (None, Some(Why::Stale)));
    let mut shared = ask(now, r(250));
    shared.attribution = Attribution::Unattributed { share: 0.02 };
    assert_eq!(why(&shared).0, Some(Why::Unattributed));
    shared.attribution = Attribution::Unattributed { share: 0.005 };
    assert_eq!(why(&shared).0, None);
    let mut keyed = ask(now, r(250));
    keyed.api_key_seen = true;
    assert_eq!(why(&keyed), (Some(Why::ApiKeySeen), Some(Why::ApiKeySeen)));
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
/// wall exact.
#[test]
fn the_error_bar_is_the_readings_resolution() {
    let now = 1_792_000_000 - 1_792_000_000 % 3600;
    let spend = spend_at(&[(now - 2 * HOUR, 10.0)], now);
    let cap = |a: &Ask<'_>| figure(&spend, a).capacity.expect("a capacity");
    let whole = cap(&ask(now, Some(tap(250, now - 60))));
    assert!(close(whole.pm_pct, 2.0), "0.5 of 25: {}", whole.pm_pct);
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
}

/// THE FABLE BUCKET's dollars are its models' only.
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

/// A message's first row as Claude Code 2.1.300 writes it while the message
/// streams: no `speed` (nor `server_tool_use` or `iterations`, which [`row`]
/// writes only as `extra`).
fn first_streamed(row: &str) -> String {
    assert!(row.contains(r#","speed":"standard""#));
    row.replacen(r#","speed":"standard""#, "", 1)
}

/// A MESSAGE IS APPROXIMATE WHEN ANY ROW OF IT SAYS SO, and is counted so
/// once: fast mode said on its last row only, both rows read in one fold; a
/// web search said on the last row a fold later; a last row that adds
/// nothing but `speed`; an advisor's iterations on the last row only, after
/// a first row with Claude Code's empty `iterations`.
#[test]
fn a_message_is_approximate_when_any_row_of_it_says_so() {
    let home = Home::new("approx-late");
    let now = now();
    let opus = "claude-opus-5-5";
    let main = home.transcript("-p/s.jsonl");
    let counts = |spend: &Spend| {
        let all = everything(spend, now);
        (all.messages, all.approx)
    };
    let mut spend = spend_of(&home);
    let whole = mtok_in().plus(&out(100_000));
    let fast = r#","speed":"fast""#;
    let empty = r#","iterations":[]"#;
    append(
        &main,
        &first_streamed(&row("msg_f", opus, now - 60, mtok_in(), "")),
    );
    append(&main, &row("msg_f", opus, now - 60, whole, fast));
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (1, 1));
    assert!(close(everything(&spend, now).usd, 2.0), "at the base rates");
    append(
        &main,
        &first_streamed(&row("msg_w", opus, now - 50, mtok_in(), "")),
    );
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (2, 1));
    let search = r#","server_tool_use":{"web_search_requests":1}"#;
    append(&main, &row("msg_w", opus, now - 50, whole, search));
    append(
        &main,
        &first_streamed(&row("msg_z", opus, now - 40, mtok_in(), "")),
    );
    append(&main, &row("msg_z", opus, now - 40, mtok_in(), fast));
    let advisor = r#","iterations":[{"type":"message"},{"type":"advisor_message","model":"claude-opus-5-5"}]"#;
    append(
        &main,
        &first_streamed(&row("msg_a", opus, now - 35, mtok_in(), empty)),
    );
    append(&main, &row("msg_a", opus, now - 35, whole, advisor));
    spend.fold_claude_home(&table(), now, Limits::none());
    assert_eq!(counts(&spend), (4, 4));
    append(&main, &row("msg_b", opus, now - 30, mtok_in(), fast));
    spend.fold_claude_home(&table(), now, Limits::none());
    append(&main, &row("msg_b", opus, now - 30, whole, fast));
    append(
        &main,
        &first_streamed(&row("msg_s", opus, now - 20, mtok_in(), empty)),
    );
    let own = r#","iterations":[{"type":"message"}]"#;
    append(&main, &row("msg_s", opus, now - 20, whole, own));
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
/// bucket, price at the base rates however large their sum.
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
    let mut spend = Spend::new(home.path(), &prices);
    spend.fold_claude_home(&prices, now, Limits::none());
    assert_eq!(spend.buckets().count(), 1, "one bucket holds both");
    let all = everything(&spend, now);
    assert_eq!(all.messages, 2);
    assert!(close(all.usd, 0.12), "{}", all.usd);
    let as_one = prices
        .message_usd(haiku, &short.plus(&short))
        .expect("priced");
    assert!(close(as_one, 0.6), "{as_one}");
}

/// A retained cache-read token is priced below the prompt tier but has no
/// rate above it.
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
fn a_stream_that_becomes_unpriced_retires_its_whole_prior_price() {
    for priced in [true, false] {
        let home = Home::new("repricing");
        let now = now();
        let at = now - 2 * BUCKET_S;
        let prices = repricing_table(priced);
        let path = home.transcript("-p/s.jsonl");
        let model = "claude-haiku-5-5";
        let first = below_repricing_tier();
        append(&path, &row("msg_stream", model, at, first, ""));
        append(
            &path,
            &row("msg_other", "claude-opus-5-5", at, mtok_in(), ""),
        );
        let mut spend = Spend::new(home.path(), &prices);
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
            assert_eq!(spend.fold_claude_home(&prices, now, Limits::none()).read, 0);
            assert_eq!(everything(&spend, now), all);
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
        let mut spend = Spend::new(home.path(), &prices);
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
fn a_later_fully_priced_tier_resolves_unknown_tokens_without_backdating_dollars() {
    let home = Home::new("repricing-known");
    let now = now();
    let at = now.div_euclid(BUCKET_S) * BUCKET_S - 3 * BUCKET_S;
    let later = at + 2 * BUCKET_S;
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
    let mut spend = Spend::new(home.path(), &prices);
    spend.fold_claude_home(&prices, now, Limits::none());
    assert_eq!(everything(&spend, now).unpriced[model], first);
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
/// message the home already counted is not counted again.
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
    assert_eq!(run("past-control", true), (2, 0));
}

use crate::harness::codex_rollout::fixtures::{context, count, meta};

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

/// A CODEX TURN AS A MESSAGE: its cached input is a cache read and out of
/// its input, so no cached token is priced twice, and the prompt a long row
/// is decided on is OpenAI's `input_tokens`; a cache write, a service tier,
/// or parts past the input make it approximate.
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
#[test]
fn two_threads_served_the_same_counts_each_count() {
    use crate::harness::codex_rollout::{Row, TurnReader};
    let home = Home::new("codex-twice");
    let b = (now() - HOUR) * 1000;
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
    let conversation = fold_conversation(
        &[rollout(&home, ROOT), rollout(&home, SUB)],
        None,
        &codex_table(),
        u64::MAX,
    );
    assert_eq!(conversation.turns, 2);
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
