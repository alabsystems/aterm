// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The one test in this binary that installs a logger. A short wait ends. A parked waiter
/// answers true once a logger is installed and the level lets records through, and not
/// before: woken by the level and then by the logger, and woken by the level once the logger
/// is in. Settling the logging with the level `Off` wakes a parked waiter at once, which
/// answers false. This binary cannot settle with no logger once it has installed one.
#[test]
fn a_waiter_wakes_once_a_logger_takes_records() {
    struct Quiet;
    impl Log for Quiet {
        fn enabled(&self, _: &Metadata) -> bool {
            false
        }
        fn log(&self, _: &Record<'_>) {}
        fn flush(&self) {}
    }
    static QUIET: Quiet = Quiet;
    let hang = std::time::Duration::from_secs(60);
    let parked = |times: usize| {
        let since = std::time::Instant::now();
        while PARKS.load(Ordering::SeqCst) < times {
            assert!(since.elapsed() < hang, "the waiter never parked");
            #[expect(
                clippy::disallowed_methods,
                reason = "a test's poll for what must happen, bounded by its hang detector, which no deadline waits on; this crate's tests do not link aterm_uds::deadline::poll_yield"
            )]
            std::thread::yield_now();
        }
    };
    let answer = |waiter: std::thread::JoinHandle<bool>| {
        let since = std::time::Instant::now();
        while !waiter.is_finished() {
            assert!(since.elapsed() < hang, "the wait never ended");
            #[expect(
                clippy::disallowed_methods,
                reason = "a test's poll for what must happen, bounded by its hang detector, which no deadline waits on; this crate's tests do not link aterm_uds::deadline::poll_yield"
            )]
            std::thread::yield_now();
        }
        waiter.join().expect("the waiter")
    };
    let now = std::time::Duration::ZERO;
    assert!(!wait_for_logger(now), "no logger yet");
    let short = std::thread::spawn(|| wait_for_logger(std::time::Duration::from_millis(1)));
    assert!(!answer(short), "no logger within a short wait");
    let parks = PARKS.load(Ordering::SeqCst);
    let timeouts = TIMEOUTS.load(Ordering::SeqCst);

    let waiter = std::thread::spawn(move || wait_for_logger(hang));
    parked(parks + 1);
    set_max_level(LevelFilter::Error);
    assert!(
        !wait_for_logger(now),
        "a level with no logger takes no record"
    );
    parked(parks + 2);
    set_logger(&QUIET).expect("the first logger");
    assert!(answer(waiter), "a logger at Error takes records");
    assert_eq!(
        TIMEOUTS.load(Ordering::SeqCst),
        timeouts,
        "woken, not timed out"
    );

    set_max_level(LevelFilter::Off);
    assert!(
        !wait_for_logger(now),
        "a logger the level lets nothing reach takes none"
    );
    let waiter = std::thread::spawn(move || wait_for_logger(hang));
    parked(parks + 3);
    set_max_level(LevelFilter::Error);
    assert!(answer(waiter), "the level let records reach the logger");
    assert_eq!(
        TIMEOUTS.load(Ordering::SeqCst),
        timeouts,
        "the level woke it"
    );

    set_max_level(LevelFilter::Off);
    let waiter = std::thread::spawn(move || wait_for_logger(hang));
    parked(parks + 4);
    settle_logger();
    assert!(!answer(waiter), "settled with the level Off");
    assert_eq!(
        TIMEOUTS.load(Ordering::SeqCst),
        timeouts,
        "settling woke it"
    );
    set_max_level(LevelFilter::Error);
    assert!(
        wait_for_logger(now),
        "settled, a logger at Error takes records"
    );
}

/// Severity order is what `enabled` compares: a filter admits a level at
/// or below it, and `Off` admits nothing.
#[test]
fn levels_and_filters_order_by_verbosity() {
    assert!(Level::Error < Level::Warn);
    assert!(Level::Warn < Level::Info);
    assert!(Level::Info < Level::Debug);
    assert!(Level::Debug < Level::Trace);
    assert!(LevelFilter::Off < LevelFilter::Error);
    assert!(LevelFilter::Error < LevelFilter::Warn);
    assert!(LevelFilter::Warn < LevelFilter::Info);
    assert!(LevelFilter::Info < LevelFilter::Debug);
    assert!(LevelFilter::Debug < LevelFilter::Trace);
}

#[test]
fn test_level_display() {
    assert_eq!(Level::Error.to_string(), "ERROR");
    assert_eq!(Level::Warn.to_string(), "WARN");
    assert_eq!(Level::Info.to_string(), "INFO");
    assert_eq!(Level::Debug.to_string(), "DEBUG");
    assert_eq!(Level::Trace.to_string(), "TRACE");
}

#[test]
fn parse_level_filter_takes_names_in_any_case_and_refuses_junk() {
    for (text, want) in [
        ("off", Some(LevelFilter::Off)),
        ("error", Some(LevelFilter::Error)),
        ("warn", Some(LevelFilter::Warn)),
        ("info", Some(LevelFilter::Info)),
        ("debug", Some(LevelFilter::Debug)),
        ("trace", Some(LevelFilter::Trace)),
        ("INFO", Some(LevelFilter::Info)),
        ("  Warn\n", Some(LevelFilter::Warn)),
        ("\u{3000}Warn\u{3000}", Some(LevelFilter::Warn)),
        ("OfF", Some(LevelFilter::Off)),
        ("", None),
        ("verbose", None),
        ("info,debug", None),
        ("3", None),
    ] {
        assert_eq!(LevelFilter::parse(text), want, "{text:?}");
    }
}

#[test]
fn record_cap_matches_alert_and_standard_thresholds() {
    assert_eq!(record_cap(Level::Error), MAX_ALERT_RECORD_BYTES);
    assert_eq!(record_cap(Level::Warn), MAX_ALERT_RECORD_BYTES);
    assert_eq!(record_cap(Level::Info), MAX_RECORD_BYTES);
    assert_eq!(record_cap(Level::Debug), MAX_RECORD_BYTES);
    assert_eq!(record_cap(Level::Trace), MAX_RECORD_BYTES);
}

#[test]
fn test_warn_and_error_keep_the_longer_cap() {
    let long = "w".repeat(MAX_ALERT_RECORD_BYTES * 2);
    let warn = sanitize_record_for(Level::Warn, &long);
    assert!(warn.len() > MAX_RECORD_BYTES + '…'.len_utf8());
    assert!(warn.len() <= MAX_ALERT_RECORD_BYTES + '…'.len_utf8());
    assert!(warn.ends_with('…'));
    let info = sanitize_record_for(Level::Info, &long);
    assert_eq!(info, sanitize_record(&long));
    assert!(info.len() <= MAX_RECORD_BYTES + '…'.len_utf8());

    let mid = "m".repeat(MAX_RECORD_BYTES + 100);
    assert!(matches!(
        sanitize_record_for(Level::Warn, &mid),
        Cow::Borrowed(m) if std::ptr::eq(m.as_ptr(), mid.as_ptr()) && m == mid
    ));
    assert!(matches!(
        sanitize_record_for(Level::Info, &mid),
        Cow::Owned(ref s) if s.ends_with('…')
    ));

    let mid_ctl = "m".repeat(MAX_RECORD_BYTES + 97) + "\n";
    let sanitized_ctl = sanitize_record_for(Level::Warn, &mid_ctl);
    assert!(matches!(
        sanitized_ctl,
        Cow::Owned(ref s) if !s.ends_with('…') && !s.chars().any(char::is_control)
    ));
}

#[test]
fn test_sanitize_record_replaces_control_characters() {
    let msg = "path '\x1b]0;evil\x07\nFAKE\u{7f}\u{9b}'";
    let out = sanitize_record(msg);
    assert!(!out.chars().any(char::is_control));
    assert_eq!(
        out,
        "path '\u{fffd}]0;evil\u{fffd}\u{fffd}FAKE\u{fffd}\u{fffd}'"
    );
}

#[test]
fn test_sanitize_record_cap_respects_char_boundaries() {
    let long = "é".repeat(MAX_RECORD_BYTES);
    let out = sanitize_record(&long);
    assert!(out.len() <= MAX_RECORD_BYTES + '…'.len_utf8());
    assert!(out.ends_with('…'));
    assert!(out.trim_end_matches('…').chars().all(|c| c == 'é'));
}

#[test]
fn test_sanitize_record_control_char_at_boundary_stays_within_reservation() {
    for (level, cap) in [
        (Level::Info, MAX_RECORD_BYTES),
        (Level::Error, MAX_ALERT_RECORD_BYTES),
    ] {
        let msg = "a".repeat(cap - 1) + "\u{01}" + "a";
        let out = sanitize_record_for(level, &msg);
        let Cow::Owned(s) = out else {
            panic!("{level}: control char must allocate Owned");
        };
        assert_eq!(
            s.capacity(),
            MAX_ALERT_RECORD_BYTES + 4,
            "{level}: reservation must not reallocate"
        );
        assert!(s.len() <= cap + '…'.len_utf8(), "{level}");
        assert!(s.ends_with('…'), "{level}");
        assert!(!s.chars().any(char::is_control), "{level}");

        // Input with a 1-byte control char that expands to 3-byte U+FFFD and fits cap
        // exactly produces Owned with no ellipsis and no reallocation.
        let fits = "a".repeat(cap - 3) + "\u{01}";
        let exact = sanitize_record_for(level, &fits);
        let Cow::Owned(exact_s) = exact else {
            panic!("{level}: control char within cap must allocate Owned");
        };
        assert_eq!(exact_s.capacity(), MAX_ALERT_RECORD_BYTES + 4, "{level}");
        assert_eq!(exact_s.len(), cap, "{level}");
        assert!(!exact_s.ends_with('…'), "{level}");
        assert!(!exact_s.chars().any(char::is_control), "{level}");
    }
}
