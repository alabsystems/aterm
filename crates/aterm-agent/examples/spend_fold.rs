// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// THE REVIEWER'S INSTRUMENT for the account stats' dollar fold (kept in the tree, linted with
// `--all-targets`): the REAL home-wide fold (`harness::spend`) of a Claude Code home, twice through a
// spend file, priced by the shipped table. It prints counts, bytes, model ids and ratios only — never a
// dollar sum, a path or a message — so its output can be pasted. `--lab`, for a lab's scratch home whose
// figures are the stub's own (the account-stats design, §9 A5), also prints the tokens by class and the
// list dollars. `--codex` folds a Codex home's rollouts instead (C1: `fold_codex_home`, a fork's copied
// prefix counted for nothing) and prints the tokens counted, every class summed, to set beside a
// fork-aware count; `--at <unix>` sets the clock, so an older home's eight days can be folded.
// `targo --unverified run -q -p aterm-agent --example spend_fold -- <home> <scratch-spend-dir> [--lab]
// [--codex] [--at <unix>]` (the spend file it writes there holds the transcripts' paths: a scratch
// directory, removed after).
use std::path::PathBuf;
use std::time::Instant;

use aterm_agent::harness::spend::{
    Agent, FoldReport, HORIZON_S, Limits, PRICES_TOML, PriceTable, Span, SpendStore,
};

fn line(what: &str, r: &FoldReport, ms: u128) {
    let copied = if r.copied > 0 {
        format!(" copied={}", r.copied)
    } else {
        String::new()
    };
    println!(
        "{what}: files={} read={}B messages={} duplicates={}{copied} refolded={} unread={} untimed={} unparsed={} long={} complete={} in {ms} ms",
        r.files,
        r.read,
        r.messages,
        r.duplicates,
        r.refolded,
        r.unread,
        r.rows_untimed,
        r.rows_unparsed,
        r.rows_long,
        r.complete(),
    );
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(home), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: spend_fold <home> <scratch-spend-dir> [--lab] [--codex] [--at <unix>]");
        std::process::exit(2);
    };
    let (mut lab, mut codex, mut at) = (false, false, None);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--lab") => lab = true,
            Some("--codex") => codex = true,
            Some("--at") => {
                at = args
                    .next()
                    .and_then(|a| a.to_str().and_then(|a| a.parse::<i64>().ok()));
                if at.is_none() {
                    eprintln!("spend_fold: --at takes unix seconds");
                    std::process::exit(2);
                }
            }
            _ => {
                eprintln!("spend_fold: unknown argument {arg:?}");
                std::process::exit(2);
            }
        }
    }
    let (agent, label) = if codex {
        (Agent::Codex, "codex")
    } else {
        (Agent::Claude, "main")
    };
    let home = PathBuf::from(home);
    let store = SpendStore {
        dir: PathBuf::from(dir),
    };
    let prices = PriceTable::from_toml(PRICES_TOML).expect("the shipped table loads");
    let now = || {
        at.unwrap_or_else(|| {
            i64::try_from(aterm_time::unix_secs().expect("a clock")).expect("seconds")
        })
    };
    let fold = |spend: &mut aterm_agent::harness::spend::Spend, now: i64| {
        if codex {
            spend.fold_codex_home(&prices, now, Limits::none())
        } else {
            spend.fold_claude_home(&prices, now, Limits::none())
        }
    };

    let (mut spend, fresh) = store.load(agent, label, &home, &prices);
    println!("load: {}", fresh.map_or("kept", |f| f.as_str()));
    let t = Instant::now();
    let first = fold(&mut spend, now());
    line("fold 1", &first, t.elapsed().as_millis());
    store.save(&spend).expect("save the spend file");

    let (mut again, fresh) = store.load(agent, label, &home, &prices);
    println!("load: {}", fresh.map_or("kept", |f| f.as_str()));
    let t = Instant::now();
    let second = fold(&mut again, now());
    line("fold 2", &second, t.elapsed().as_millis());
    store.save(&again).expect("save the spend file");
    if first.read > 0 {
        println!(
            "fold 2 read {:.4}% of fold 1's bytes",
            second.read as f64 * 100.0 / first.read as f64
        );
    }

    let now = now();
    let sum = again.sum(
        Span {
            from: now - HORIZON_S,
            to: now + 1,
            whole: false,
        },
        None,
    );
    let t = &sum.tokens;
    let all = (t.input + t.output + t.cache_write + t.cache_read).max(1) as f64;
    println!(
        "tokens by class: input {:.2}% output {:.2}% cache writes {:.2}% (of them 1 h {:.2}%) cache reads {:.2}%",
        t.input as f64 * 100.0 / all,
        t.output as f64 * 100.0 / all,
        t.cache_write as f64 * 100.0 / all,
        t.cache_write_1h as f64 * 100.0 / t.cache_write.max(1) as f64,
        t.cache_read as f64 * 100.0 / all,
    );
    if codex {
        println!(
            "tokens counted, every class: {}",
            t.input + t.output + t.cache_write + t.cache_read
        );
    }
    if lab {
        println!(
            "lab sums: input {} output {} cache writes {} (1 h {}) cache reads {} · ≈${:.6} (list)",
            t.input, t.output, t.cache_write, t.cache_write_1h, t.cache_read, sum.usd,
        );
    }
    println!(
        "messages {} · approx {} · unpriced models {:?} · more than 1% of a class unpriced: {}",
        sum.messages,
        sum.approx,
        sum.unpriced.keys().collect::<Vec<_>>(),
        sum.unpriced_over(),
    );
    let mut models: Vec<&str> = again
        .buckets()
        .flat_map(|(_, m)| m.keys().map(String::as_str))
        .collect();
    models.sort_unstable();
    models.dedup();
    println!("models seen: {models:?}");
    let file = store.path(agent, label).expect("a path");
    let size = std::fs::metadata(&file).map_or(0, |m| m.len());
    println!(
        "spend file: {size} bytes, {} buckets, priced by the table of {}",
        again.buckets().count(),
        again.prices_as_of()
    );
}
