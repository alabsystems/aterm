// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn slug_validation_accepts_real_names_rejects_url_metacharacters() {
    for ok in [
        "alabsystems",
        "aterm",
        "a",
        "My-Org_1",
        "repo.name",
        "x.y.z",
    ] {
        assert!(is_valid_slug(ok), "{ok:?} should be valid");
    }
    for bad in [
        "",           // empty
        "owner/repo", // path separator
        "..",         // traversal
        "a b",        // space
        "evil.com/x", // host injection
        "x?y",        // query
        "x#y",        // fragment
        "x@y",        // userinfo
        "x\ny",       // newline
        "%2e%2e",     // percent-encoding
        "café",       // non-ASCII
    ] {
        assert!(!is_valid_slug(bad), "{bad:?} should be rejected");
    }
    assert!(!is_valid_slug(&"a".repeat(101)));
    assert!(is_valid_slug(&"a".repeat(100)));
}

#[test]
fn pick_slug_takes_a_valid_value_else_the_default() {
    assert_eq!(
        pick_slug("[update] owner", Some("cfgowner"), "default"),
        "cfgowner"
    );
    assert_eq!(pick_slug("[update] owner", None, "default"), "default");
    assert_eq!(
        pick_slug("[update] owner", Some("   "), "default"),
        "default"
    );
    assert_eq!(
        pick_slug("[update] owner", Some("  cfg \n"), "default"),
        "cfg"
    );
    assert_eq!(
        pick_slug("[update] owner", Some("c/d"), "default"),
        "default"
    );
    assert_eq!(
        pick_slug("[update] owner", Some("a b"), "default"),
        "default"
    );
}

/// The `aterm_log` records (level and message) written on this thread while `body` runs.
fn logged_on_this_thread(body: impl FnOnce()) -> Vec<String> {
    use std::cell::RefCell;
    thread_local! {
        static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }
    struct Capture;
    impl aterm_log::Log for Capture {
        fn enabled(&self, _: &aterm_log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &aterm_log::Record<'_>) {
            LINES.with(|lines| {
                if let Some(lines) = lines.borrow_mut().as_mut() {
                    lines.push(format!("{} {}", record.level(), record.args()));
                }
            });
        }
        fn flush(&self) {}
    }
    static CAPTURE: Capture = Capture;
    let _ = aterm_log::set_logger(&CAPTURE);
    aterm_log::set_max_level(aterm_log::LevelFilter::Trace);
    LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
    body();
    LINES.with(|lines| lines.borrow_mut().take().unwrap_or_default())
}

#[test]
fn an_invalid_slug_reaches_the_log_by_its_length() {
    let logged = logged_on_this_thread(|| {
        assert_eq!(
            pick_slug("[update] owner", Some("QZP7E7K/x"), "default"),
            "default"
        );
    });
    assert!(
        logged.iter().any(|line| line
            == "WARN ignoring [update] owner (9B): not a valid GitHub name; using default"),
        "the line is logged with the value's length: {} lines",
        logged.len()
    );
    assert!(
        !logged.iter().any(|line| line.contains("QZP7E7K")),
        "the log keeps the configured value"
    );
}

#[test]
fn a_config_repoint_is_honoured_only_by_a_development_build() {
    let dev = Source::resolve_with(true, Some("fork-owner"), Some("fork-repo"));
    assert_eq!(
        (dev.owner.as_str(), dev.repo.as_str()),
        ("fork-owner", "fork-repo")
    );
    let shipped = Source::resolve_with(false, Some("fork-owner"), Some("fork-repo"));
    assert_eq!(
        (shipped.owner.as_str(), shipped.repo.as_str()),
        (DEFAULT_OWNER, DEFAULT_REPO),
        "a shipped binary is never repointed by a config file"
    );
    let half = Source::resolve_with(true, Some("fork-owner"), None);
    assert_eq!(half.repo, DEFAULT_REPO);

    assert!(has_nonempty_repoint(Some(""), Some("fork-repo")));
    assert!(has_nonempty_repoint(Some("   "), Some("fork-repo")));
    assert!(has_nonempty_repoint(Some("fork-owner"), Some("")));
    assert!(!has_nonempty_repoint(Some(""), Some("   ")));
    assert!(!has_nonempty_repoint(None, None));
}
