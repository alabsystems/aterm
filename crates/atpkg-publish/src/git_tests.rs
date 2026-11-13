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
        "SSH_AUTH_SOCK",
        "PATH",
    ];
    let remove = git_environment_removals_from(names.map(OsString::from));
    for name in &names[..8] {
        assert!(remove.iter().any(|entry| entry == name));
    }
    for name in [
        crate::host::rest::GIT_TOKEN_ENV,
        "GH_TOKEN",
        "GITHUB_TOKEN",
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
