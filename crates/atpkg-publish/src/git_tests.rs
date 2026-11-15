// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn credential_preparation_removes_current_and_future_trace_and_ambient_auth_selectors() {
    let names = [
        "GIT_TRACE",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE_REDACT",
        "GIT_TRACE_FUTURE",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "AGH_GIT_GRANT",
        "AGH_GIT_CONFIG_DIR",
        // Inherited TLS overrides, which agh's own runner never passes. NEGATIVE CONTROL:
        // without the `GIT_SSL_` prefix in the removals, these stay.
        "GIT_SSL_NO_VERIFY",
        "GIT_SSL_CAINFO",
        "GIT_SSL_CAPATH",
        // Inherited command-scope config, which git reads as it reads `-c`, so it could carry
        // a URL-scoped `http.*` key or a `url.*.insteadOf` rewrite. NEGATIVE CONTROL: without
        // the `GIT_CONFIG_KEY_`/`GIT_CONFIG_VALUE_` prefixes in the removals, these stay.
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "SSH_AUTH_SOCK",
        "PATH",
    ];
    let remove = git_environment_removals_from(names.map(OsString::from));
    for name in &names[..13] {
        assert!(remove.iter().any(|entry| entry == name), "{name}");
    }
    for name in [
        crate::host::rest::GIT_TOKEN_ENV,
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CURL_VERBOSE",
        "BASH_ENV",
        "ENV",
        "SHELLOPTS",
        "BASHOPTS",
    ] {
        assert!(remove.iter().any(|entry| entry == name));
    }
    assert!(
        !remove
            .iter()
            .any(|entry| entry == "SSH_AUTH_SOCK" || entry == "PATH")
    );
}

/// The scoped pins are for a URL git fetches over HTTP alone, and a URL that would end its
/// `-c` key early refuses rather than pin some other key.
#[test]
fn tls_pins_scope_to_an_http_url_and_refuse_one_no_key_can_name() {
    let url = "https://github.com/fixture/claude-code-alab.git";
    assert_eq!(
        git_tls_pins_at(url).expect("an https URL"),
        [
            "-c".to_string(),
            format!("http.{url}.sslVerify=true"),
            "-c".to_string(),
            format!("http.{url}.followRedirects=false"),
        ]
    );
    for other in [
        "git@github.com:fixture/claude-code-alab.git",
        "file:///r/x.git",
    ] {
        assert_eq!(git_tls_pins_at(other), Ok(Vec::new()), "{other}");
    }
    let why = git_tls_pins_at("https://github.com/a.b=c/r.git").expect_err("an '=' refuses");
    assert!(why.contains("holds '='"), "{why}");
}

/// Only a checkout with no `origin` among its remotes is told it has none. An origin that is
/// there and cannot be read keeps the query's own refusal: a URL that is not UTF-8 is that, not
/// a missing remote. NEGATIVE CONTROL: mapping every refusal of the URL query to "no origin
/// remote", as before, fails the second half.
#[cfg(unix)]
#[test]
fn only_a_checkout_without_an_origin_is_told_it_has_none() {
    const HANG_DETECTOR: Duration = Duration::from_secs(60);
    use std::os::unix::ffi::OsStringExt as _;
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let repo = dir.path().join("trust");
    std::fs::create_dir_all(&repo).expect("mkdir");
    let run = |args: &[std::ffi::OsString]| {
        let mut git = crate::testkit::git();
        git.arg("-C").arg(&repo).args(args);
        // A hang detector, not a budget: each is one local config write.
        let out = aterm_uds::deadline::output_until(&mut git, Instant::now() + HANG_DETECTOR)
            .expect("fixture git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    run(&["init".into(), "-q".into()]);
    let slug = crate::host::Slug::new("fixture-dev", "trust");
    let refusal = |repo: &Path| match source_with_origin(repo, &slug) {
        Ok(_) => panic!("admitted"),
        Err(e) => e.to_string(),
    };
    assert!(
        refusal(&repo).contains("the source has no origin remote; it must name fixture-dev/trust"),
        "{}",
        refusal(&repo)
    );
    run(&[
        "config".into(),
        "remote.origin.url".into(),
        std::ffi::OsString::from_vec(b"https://github.com/fixture-dev/\xfftrust.git".to_vec()),
    ]);
    let unreadable = refusal(&repo);
    assert!(unreadable.contains("not UTF-8"), "{unreadable}");
    assert!(!unreadable.contains("no origin remote"), "{unreadable}");
    run(&[
        "config".into(),
        "remote.origin.url".into(),
        "https://github.com/fixture-dev/trust.git".into(),
    ]);
    assert!(source_with_origin(&repo, &slug).is_ok());
}

/// The index-hint query answers for a repository of any size: it reads the listing to its end
/// an entry at a time and holds only a hinted one. The fixture's index lists more than a whole
/// capture holds (TY's 402,779 files list 38 MB, and the tip lane could not qualify it), with
/// no file written: the query reads the index alone. NEGATIVE CONTROL: the same listing read
/// whole is still refused at the capture limit, as the query was before it read by entry, so
/// the fixture is over the limit and the answer is the entry reading's.
#[cfg(unix)]
#[test]
fn an_index_longer_than_a_whole_capture_is_still_searched_for_hints() {
    const HANG_DETECTOR: Duration = Duration::from_secs(120);
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    let dir = aterm_tempfile::TempDir::new().expect("scratch");
    let repo = dir.path().join("big");
    std::fs::create_dir_all(&repo).expect("mkdir");
    let git = |args: &[&str], stdin: aterm_uds::spawnfd::Stdio| {
        let mut git = crate::testkit::git();
        git.arg("-C").arg(&repo).args(args);
        // A hang detector, not a budget: each is one local index write.
        let out =
            aterm_uds::deadline::output_until_from(&mut git, stdin, Instant::now() + HANG_DETECTOR)
                .expect("fixture git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    let null = aterm_uds::spawnfd::Stdio::null;
    git(&["init", "-q"], null());
    // Index entries name files that are never written: 9,000 paths of some 2,000 bytes.
    let segment = "s".repeat(200);
    let path = |n: usize| format!("{n:05}/{}", [segment.as_str(); 10].join("/"));
    let entries = 9_000;
    let mut info = String::new();
    for n in 0..entries {
        info.push_str(&format!("100644 {EMPTY_BLOB} 0\t{}\n", path(n)));
    }
    assert!(info.len() > MAX_COMPLETE_STDOUT_BYTES, "{}", info.len());
    let listing = dir.path().join("index-info");
    std::fs::write(&listing, &info).expect("write index-info");
    git(
        &["update-index", "--add", "--index-info"],
        std::fs::File::open(&listing)
            .expect("open index-info")
            .into(),
    );

    let whole = run(&repo, &["ls-files", "-v", "-z"]).expect_err("a listing over the limit");
    assert!(
        whole
            .to_string()
            .contains("exceeds the complete-capture limit"),
        "{whole}"
    );
    assert_eq!(
        index_hint(&repo).expect("the listing is read to its end"),
        None
    );

    let last = path(entries - 1);
    for (set, clear, flag) in [
        ("--assume-unchanged", "--no-assume-unchanged", "h"),
        ("--skip-worktree", "--no-skip-worktree", "S"),
    ] {
        git(&["update-index", set, "--", &last], null());
        assert_eq!(
            index_hint(&repo).expect("the listing is read to its end"),
            Some(format!("{flag} {last}")),
            "{set}"
        );
        git(&["update-index", clear, "--", &last], null());
    }
    assert_eq!(
        index_hint(&repo).expect("the listing is read to its end"),
        None
    );
}
