// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unit tests for the inline-suggestion engine.
//!
//! Every test is clock-free: `now_ms` is passed explicitly, so the ranker's
//! time-dependent behaviour (recency decay, LRU order) is exercised
//! deterministically and without sleeping.

use super::*;

/// One hour in ms — the unit most of the recency tests are written in.
const HOUR: u64 = 60 * 60 * 1000;

fn on() -> SuggestConfig {
    SuggestConfig {
        mode: SuggestMode::History,
        ..SuggestConfig::default()
    }
}

/// A context that passes every safety gate, so a test that wants to exercise
/// ranking does not have to restate them.
fn live(buffer: &str) -> Context<'_> {
    Context {
        buffer,
        cwd: None,
        at_prompt: true,
        alt_screen: false,
        echoing: true,
    }
}

fn engine_with(cmds: &[(&str, Option<&str>, Option<i32>, u64)]) -> Engine {
    let mut e = Engine::new(on());
    for &(c, cwd, code, at) in cmds {
        e.record(c, cwd, code, at);
    }
    e
}

fn completion(s: &Suggestion) -> &str {
    &s.completion
}

// ───────────────────────────── gates ─────────────────────────────

#[test]
fn off_mode_never_suggests() {
    let mut e = engine_with(&[("cargo build", None, Some(0), 0)]);
    e.set_config(SuggestConfig::default()); // mode: Off
    assert!(e.suggest(&live("car"), HOUR).is_none());
}

#[test]
fn default_engine_is_inert() {
    let e = Engine::default();
    assert_eq!(e.config().mode, SuggestMode::Off);
    assert!(e.suggest(&live("car"), 0).is_none());
}

#[test]
fn refuses_when_not_at_a_prompt() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    let ctx = Context {
        at_prompt: false,
        ..live("car")
    };
    assert!(
        e.suggest(&ctx, HOUR).is_none(),
        "without OSC 133;B the line may be a REPL or a read prompt"
    );
}

#[test]
fn refuses_on_the_alternate_screen() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    let ctx = Context {
        alt_screen: true,
        ..live("car")
    };
    assert!(
        e.suggest(&ctx, HOUR).is_none(),
        "in vim/less every cell belongs to the application"
    );
}

#[test]
fn refuses_on_a_non_echoing_line() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    let ctx = Context {
        echoing: false,
        ..live("car")
    };
    assert!(
        e.suggest(&ctx, HOUR).is_none(),
        "a password prompt must never carry a ghost"
    );
}

#[test]
fn respects_the_minimum_prefix() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    assert!(
        e.suggest(&live("c"), HOUR).is_none(),
        "1 char < min_prefix 2"
    );
    assert!(e.suggest(&live("ca"), HOUR).is_some());
}

#[test]
fn empty_buffer_suggests_nothing() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    assert!(e.suggest(&live(""), HOUR).is_none());
}

#[test]
fn trailing_whitespace_suggests_nothing() {
    let e = engine_with(&[("cargo build --release", None, Some(0), 0)]);
    assert!(
        e.suggest(&live("cargo build "), HOUR).is_none(),
        "a ghost that flickers as you type a space is noise"
    );
}

#[test]
fn multiline_buffer_suggests_nothing() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    assert!(e.suggest(&live("car\ngo"), HOUR).is_none());
}

#[test]
fn an_exact_match_is_not_a_suggestion() {
    let e = engine_with(&[("cargo build", None, Some(0), 0)]);
    assert!(
        e.suggest(&live("cargo build"), HOUR).is_none(),
        "there is nothing left to complete"
    );
}

// ─────────────────────────── the ranker ───────────────────────────

#[test]
fn completes_the_remainder_only() {
    let e = engine_with(&[("cargo build --release", None, Some(0), 0)]);
    let s = e.suggest(&live("cargo bu"), HOUR).expect("a match");
    assert_eq!(completion(&s), "ild --release");
    assert_eq!(s.source, Source::History);
}

#[test]
fn a_command_that_never_succeeded_is_never_suggested() {
    let e = engine_with(&[("cargo buidl", None, Some(101), 0)]);
    assert!(
        e.suggest(&live("cargo bu"), HOUR).is_none(),
        "suggesting a known-broken line costs the user a run to rediscover it fails"
    );
}

#[test]
fn a_command_that_failed_once_but_works_is_still_suggested() {
    let mut e = Engine::new(on());
    e.record("cargo test", None, Some(0), 0);
    e.record("cargo test", None, Some(1), HOUR);
    let s = e.suggest(&live("cargo te"), 2 * HOUR).expect("a match");
    assert_eq!(completion(&s), "st");
}

#[test]
fn failures_derank_relative_to_a_clean_sibling() {
    let mut e = Engine::new(on());
    // Same recency, same run count; one is flaky.
    e.record("deploy staging", None, Some(0), 0);
    for _ in 0..3 {
        e.record("deploy prod", None, Some(1), 0);
    }
    e.record("deploy prod", None, Some(0), 0);
    let s = e.suggest(&live("deploy "), HOUR);
    // Trailing space is refused; use a real prefix.
    assert!(s.is_none());
    let s = e.suggest(&live("deploy s"), HOUR).expect("a match");
    assert_eq!(completion(&s), "taging");
}

#[test]
fn recency_beats_an_older_command() {
    let e = engine_with(&[
        ("git status", None, Some(0), 0),
        ("git stash", None, Some(0), 10 * HOUR),
    ]);
    let s = e.suggest(&live("git st"), 10 * HOUR).expect("a match");
    assert_eq!(completion(&s), "ash", "the newer command wins");
}

#[test]
fn cwd_match_outranks_a_more_recent_command_elsewhere() {
    let e = engine_with(&[
        ("cargo test --all", Some("/repo/aterm"), Some(0), 0),
        (
            "cargo test --lib",
            Some("/somewhere/else"),
            Some(0),
            8 * HOUR,
        ),
    ]);
    let ctx = Context {
        cwd: Some("/repo/aterm"),
        ..live("cargo test --")
    };
    let s = e.suggest(&ctx, 8 * HOUR).expect("a match");
    assert_eq!(
        completion(&s),
        "all",
        "directory is the strongest context signal a terminal has"
    );
}

#[test]
fn frequency_breaks_a_recency_tie() {
    let mut e = Engine::new(on());
    // `make build` run many times, `make bundle` once, both last used now.
    for _ in 0..20 {
        e.record("make build", None, Some(0), 0);
    }
    e.record("make bundle", None, Some(0), 0);
    let s = e.suggest(&live("make bu"), 0).expect("a match");
    assert_eq!(completion(&s), "ild");
}

#[test]
fn suggestion_is_a_pure_function_of_corpus_and_clock() {
    let e = engine_with(&[
        ("git commit", None, Some(0), 0),
        ("git checkout main", None, Some(0), HOUR),
        ("git cherry-pick", None, Some(0), 2 * HOUR),
    ]);
    let first = e.suggest(&live("git c"), 3 * HOUR);
    for _ in 0..64 {
        assert_eq!(
            e.suggest(&live("git c"), 3 * HOUR),
            first,
            "ranking must not depend on iteration luck"
        );
    }
}

// ─────────────────────────── the corpus ───────────────────────────

#[test]
fn rerunning_a_command_dedups_and_bumps_recency() {
    let mut e = Engine::new(on());
    e.record("npm run dev", None, Some(0), 0);
    e.record("npm run dev", None, Some(0), 5 * HOUR);
    assert_eq!(e.len(), 1, "the corpus holds distinct commands");
    let s = e.suggest(&live("npm r"), 5 * HOUR).expect("a match");
    assert_eq!(completion(&s), "un dev");
}

#[test]
fn capacity_evicts_least_recently_used() {
    let mut e = Engine::new(SuggestConfig {
        capacity: 2,
        ..on()
    });
    e.record("aa first", None, Some(0), 0);
    e.record("aa second", None, Some(0), HOUR);
    e.record("aa third", None, Some(0), 2 * HOUR);
    assert_eq!(e.len(), 2);
    // `aa first` is gone; a prefix that only it matched finds nothing.
    let ctx = live("aa f");
    assert!(e.suggest(&ctx, 3 * HOUR).is_none());
    assert!(e.suggest(&live("aa s"), 3 * HOUR).is_some());
}

#[test]
fn shrinking_capacity_trims_immediately() {
    let mut e = engine_with(&[
        ("one cmd", None, Some(0), 0),
        ("two cmd", None, Some(0), HOUR),
        ("three cmd", None, Some(0), 2 * HOUR),
    ]);
    e.set_config(SuggestConfig {
        capacity: 1,
        ..on()
    });
    assert_eq!(e.len(), 1);
}

#[test]
fn blank_and_multiline_commands_are_not_recorded() {
    let mut e = Engine::new(on());
    e.record("   ", None, Some(0), 0);
    e.record("for i in 1 2 3\ndo\ndone", None, Some(0), 0);
    assert!(e.is_empty());
}

#[test]
fn a_later_run_without_a_cwd_does_not_forget_the_one_we_had() {
    let mut e = Engine::new(on());
    e.record("cargo test", Some("/repo/aterm"), Some(0), 0);
    e.record("cargo test", None, Some(0), HOUR);
    let ctx = Context {
        cwd: Some("/repo/aterm"),
        ..live("cargo te")
    };
    // If the second (cwd-less) run had cleared the slot, this would still match
    // — so pin it against a rival that would win WITHOUT the cwd bonus.
    let mut e2 = Engine::new(on());
    e2.record("cargo test", Some("/repo/aterm"), Some(0), 0);
    e2.record("cargo test", None, Some(0), HOUR);
    e2.record("cargo tes2", None, Some(0), 2 * HOUR);
    let s = e2.suggest(&ctx, 2 * HOUR).expect("a match");
    assert_eq!(
        completion(&s),
        "st",
        "the retained cwd must still outrank the more recent `cargo tes2` \
         (which would win, as \"s2\", had the cwd-less rerun cleared the slot)"
    );
    assert_eq!(e.len(), 1);
}

#[test]
fn a_stale_failure_prone_entry_falls_below_the_score_floor() {
    let mut e = Engine::new(on());
    // Ran once successfully three weeks ago, failed five times since.
    e.record("deploy legacy-thing", None, Some(0), 0);
    for i in 1..=5 {
        e.record("deploy legacy-thing", None, Some(1), i);
    }
    let three_weeks = 21 * 24 * HOUR;
    assert!(
        e.suggest(&live("deploy l"), three_weeks).is_none(),
        "a ghost you have to verify is worth less than no ghost"
    );
}

#[test]
fn a_cwd_match_clears_the_floor_on_its_own() {
    let mut e = Engine::new(on());
    e.record("make deploy", Some("/repo/here"), Some(0), 0);
    let ctx = Context {
        cwd: Some("/repo/here"),
        ..live("make d")
    };
    // Ancient by recency, but it is what you run *in this directory*.
    let three_weeks = 21 * 24 * HOUR;
    assert!(
        e.suggest(&ctx, three_weeks).is_some(),
        "directory is the strongest context signal and must survive the floor"
    );
}

#[test]
fn clear_erases_everything() {
    let mut e = engine_with(&[("cargo build", None, Some(0), 0)]);
    assert_eq!(e.len(), 1);
    e.clear();
    assert!(e.is_empty());
    assert!(e.suggest(&live("car"), HOUR).is_none());
}

#[test]
fn an_unknown_exit_code_counts_as_neither_success_nor_failure() {
    let e = engine_with(&[("ssh box", None, None, 0)]);
    assert_eq!(e.len(), 1, "it is still recorded for recency");
    assert!(
        e.suggest(&live("ssh b"), HOUR).is_none(),
        "but it has never been observed to succeed, so it is not offered"
    );
}

// ────────────────────────── secret hygiene ──────────────────────────

#[test]
fn secret_bearing_commands_are_never_recorded() {
    let mut e = Engine::new(on());
    e.record(concat!("export GITHUB_TOKEN=ghp", "_abcdefghijklmnop"), None, Some(0), 0);
    e.record(
        concat!("curl -H 'Authorization: Bearer sk", "-abc123'"),
        None,
        Some(0),
        0,
    );
    e.record("mysql --password=hunter2", None, Some(0), 0);
    e.record(
        "aws configure set aws_secret_access_key AKIAIOSFODNN7",
        None,
        Some(0),
        0,
    );
    assert!(
        e.is_empty(),
        "a credential must not outlive the session that leaked it"
    );
}

#[test]
fn ordinary_commands_containing_secret_like_words_are_kept() {
    let mut e = Engine::new(on());
    // "auth" appears as a path component, not a flag with a value.
    e.record("cd src/auth", None, Some(0), 0);
    e.record("git commit -m 'fix auth'", None, Some(0), 0);
    e.record("vim tokens.rs", None, Some(0), 0);
    assert_eq!(
        e.len(),
        3,
        "over-suppression silently guts the corpus; the guard must be narrow"
    );
}

#[test]
fn secret_detection_is_case_insensitive_for_words() {
    assert!(looks_secret("PGPASSWORD=abc psql"));
    assert!(looks_secret("--Token=xyz"));
    assert!(!looks_secret("git push origin main"));
}

// ──────────────────────── destructive commands ────────────────────────

#[test]
fn a_destructive_command_is_never_suggested() {
    let e = engine_with(&[("rm -rf build/", None, Some(0), 0)]);
    assert!(
        e.suggest(&live("rm -"), HOUR).is_none(),
        "reflex is the wrong interaction for an irreversible command"
    );
}

#[test]
fn force_push_is_never_completed_into() {
    // The catastrophic case: `main` outranks `my-feature`, and the user meant
    // the latter.
    let e = engine_with(&[
        ("git push --force origin main", None, Some(0), 0),
        ("git push --force origin my-feature", None, Some(0), HOUR),
    ]);
    assert!(
        e.suggest(&live("git push --force origin ma"), 2 * HOUR)
            .is_none()
    );
}

#[test]
fn ordinary_git_and_docker_commands_still_complete() {
    let e = engine_with(&[
        ("git push origin main", None, Some(0), 0),
        ("docker ps -a", None, Some(0), 0),
    ]);
    assert!(e.suggest(&live("git push or"), HOUR).is_some());
    assert!(e.suggest(&live("docker p"), HOUR).is_some());
}

/// The first token is not the whole story. Each of these hid a destructive verb
/// somewhere the original single-token check never looked.
#[test]
fn destructive_detection_sees_past_the_first_token() {
    // A compound line: the destructive part is the SECOND command.
    assert!(is_destructive("make clean && rm -rf target"));
    assert!(is_destructive("cd build; rm -rf *"));
    assert!(is_destructive("yes | rm -rf x"));
    // An environment prefix is not the command.
    assert!(is_destructive("TMPDIR=/x rm -rf /srv"));
    assert!(is_destructive("FOO=1 BAR=2 dd if=/dev/zero of=/dev/disk2"));
    // sudo's own flags precede the command it wraps.
    assert!(is_destructive("sudo -u bob rm -rf /srv"));
    assert!(is_destructive("sudo -E rm -rf /srv"));
    // git's global flags precede the subcommand.
    assert!(is_destructive("git -C /repo push --force origin main"));
    assert!(is_destructive("kubectl -n prod delete pod x"));
    // Irreversible git verbs that are not `push --force`.
    assert!(is_destructive("git reset --hard origin/main"));
    assert!(is_destructive("git clean -fdx"));
    // Real-world docker spellings.
    assert!(is_destructive("docker system prune -af"));
    assert!(is_destructive("docker volume prune"));
    // …and the negatives still pass.
    assert!(!is_destructive("make clean && cargo build"));
    assert!(!is_destructive("FOO=1 cargo test"));
    assert!(!is_destructive("git -C /repo push origin main"));
    assert!(!is_destructive("git reset --soft HEAD~1"));
    assert!(!is_destructive("docker ps -a"));
    assert!(!is_destructive("sudo -u bob systemctl restart nginx"));
}

#[test]
fn a_compound_command_hiding_an_rm_is_never_suggested() {
    let e = engine_with(&[("make clean && rm -rf target", None, Some(0), 0)]);
    assert!(
        e.suggest(&live("make c"), HOUR).is_none(),
        "the destructive tail must veto the whole line"
    );
}

#[test]
fn destructive_detection_sees_through_paths_and_sudo() {
    assert!(is_destructive("rm -rf /"));
    assert!(is_destructive("/bin/rm file"));
    assert!(is_destructive("sudo rm -rf /var/log"));
    assert!(is_destructive("sudo   dd if=/dev/zero of=/dev/disk2"));
    assert!(is_destructive("kubectl delete pod x"));
    assert!(is_destructive("terraform destroy"));
    assert!(is_destructive("docker rmi old"));
    assert!(!is_destructive("sudo systemctl restart nginx"));
    assert!(!is_destructive("git push origin main"));
    assert!(!is_destructive("cargo build"));
    assert!(!is_destructive(""));
}

// ────────────────────────── ghost geometry ──────────────────────────

#[test]
fn a_completion_is_truncated_at_the_first_wide_glyph() {
    // A ghost is painted one cell per char with `wide = false`; a CJK glyph
    // would corrupt the cell to its right.
    let e = engine_with(&[("echo hello 世界", None, Some(0), 0)]);
    let s = e.suggest(&live("echo he"), HOUR).expect("a match");
    assert_eq!(
        &*s.completion, "llo ",
        "stops before the double-width glyph"
    );
}

#[test]
fn a_completion_that_is_entirely_unpaintable_is_refused() {
    let e = engine_with(&[("cd 世界", None, Some(0), 0)]);
    assert!(e.suggest(&live("cd "), HOUR).is_none());
    // (trailing whitespace is refused anyway; the point is no empty ghost)
    assert!(e.suggest(&live("cd"), HOUR).is_none());
}

// ─────────────────────────── config parse ───────────────────────────

#[test]
fn mode_parse_is_fail_safe() {
    assert_eq!(SuggestMode::parse("history"), SuggestMode::History);
    assert_eq!(SuggestMode::parse("  HISTORY "), SuggestMode::History);
    assert_eq!(SuggestMode::parse("on"), SuggestMode::History);
    assert_eq!(SuggestMode::parse("off"), SuggestMode::Off);
    assert_eq!(SuggestMode::parse("banana"), SuggestMode::Off);
    assert_eq!(SuggestMode::parse(""), SuggestMode::Off);
}
