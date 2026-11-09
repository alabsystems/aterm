// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source refresh before the launcher's fix admission, on a fixture origin.

use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    crate::ledger::git_ok(&GitCli::new(repo), args)
        .unwrap()
        .stdout_utf8()
        .trim()
        .to_owned()
}

#[test]
fn an_unfetched_fix_is_fetched_before_admission_and_other_fixes_still_refuse() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let root = temp.path();
    // A fix is admitted onto a release line, which needs a git >= `line::MIN_GIT`: on an older
    // one the test is COULD NOT RUN, as the line's own tests are, not a failed unwrap.
    if let Err(stop) = crate::line::git_ready(&GitCli::new(root)) {
        aterm_spec::host::could_not_run(stop.why);
    }
    let seed = root.join("seed");
    // The fixture commits under its own identity: a test's scratch HOME has none.
    crate::ledger::testing::init(
        &seed,
        &[
            ("user.name", "Test"),
            ("user.email", "test@example.invalid"),
        ],
    );
    std::fs::write(seed.join("README"), "base\n").unwrap();
    git(&seed, &["add", "README"]);
    git(&seed, &["commit", "-qm", "base"]);
    let base = git(&seed, &["rev-parse", "HEAD"]);
    let bare = root.join("origin.git");
    git(
        &seed,
        &[
            "clone",
            "--bare",
            "-q",
            seed.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let work = root.join("work");
    git(
        &seed,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    git(
        &work,
        &["config", "remote.fixture.url", bare.to_str().unwrap()],
    );
    git(
        &work,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/fixture/app.git",
        ],
    );
    git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    std::fs::write(seed.join("README"), "base\nlanded fix\n").unwrap();
    git(&seed, &["commit", "-qam", "landed fix"]);
    let fix = git(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["checkout", "-qb", "side"]);
    std::fs::write(seed.join("side"), "not landed\n").unwrap();
    git(&seed, &["add", "side"]);
    git(&seed, &["commit", "-qm", "side only"]);
    let off_main = git(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["checkout", "-q", "main"]);
    git(
        &seed,
        &[
            "merge",
            "--no-ff",
            "-s",
            "ours",
            "-qm",
            "record side without its tree",
            "side",
        ],
    );
    let merge = git(&seed, &["rev-parse", "HEAD"]);
    // Keep an off-main commit available to the clone without landing it on main.
    git(&seed, &["checkout", "-qb", "unlanded"]);
    std::fs::write(seed.join("unlanded"), "unlanded\n").unwrap();
    git(&seed, &["add", "unlanded"]);
    git(&seed, &["commit", "-qm", "unlanded"]);
    let unlanded = git(&seed, &["rev-parse", "HEAD"]);
    git(&seed, &["push", "-q", "origin", "main", "unlanded"]);
    let line = Line::start("0.1.0", &base, 1);
    let runner = GitCli::new(&work);
    // Before: the launcher sees no object. This is also the negative control for
    // removing the refresh from Real::fix_of.
    assert_eq!(
        crate::line::admit(&runner, &line, &fix).unwrap_err().reason,
        crate::line::Reason::NoCommit
    );
    let mut real = Real::new(&work, Some(&root.join("absent-engine")), true, false).unwrap();
    real.source_origin = |repo| {
        let bare = git(repo, &["config", "--get", "remote.fixture.url"]);
        Ok(crate::github_remote::claim_fixture(repo, Path::new(&bare)))
    };
    assert!(
        matches!(real.fix_of(&line, &fix).unwrap(), crate::line::Admitted::Fix(found) if found.commit == fix)
    );
    assert!(real.origin_main.is_none(), "source refresh reads no engine");
    assert_eq!(
        git(&work, &["rev-parse", "HEAD"]),
        base,
        "fetch does not move the pinned line"
    );
    git(&work, &["fetch", "-q", bare.to_str().unwrap(), "unlanded"]);
    for (rev, reason) in [
        ("0".repeat(40), crate::line::Reason::NoCommit),
        (unlanded, crate::line::Reason::OffMain),
        (merge.clone(), crate::line::Reason::Merge),
        (off_main, crate::line::Reason::OffMain),
    ] {
        assert_eq!(
            crate::line::admit(&runner, &line, &rev).unwrap_err().reason,
            reason
        );
        assert!(real.fix_of(&line, &rev).is_err(), "{rev} must still refuse");
    }
    // The step-1 fetch consumes the source snapshot without a second network read.
    real.source_origin = |_| Err("a redundant source fetch".into());
    assert_eq!(real.fetch().unwrap(), merge);
    assert!(real.prefetched_origin.is_none());
}
