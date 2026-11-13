// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retirement reads must not hydrate a partial clone while judging its contents.

use super::super::Git as _;
use super::{GitCli, for_git, git};

#[test]
fn cleanup_git_never_fetches_promised_objects() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("retired-git-promisor-")
        .tempdir()
        .expect("scratch");
    let provider = scratch.path().join("provider.git");
    let partial = scratch.path().join("partial.git");
    for repository in [&provider, &partial] {
        git(
            scratch.path(),
            &["init", "--bare", "--template=", "-q", &for_git(repository)],
        );
    }
    let hashed = GitCli.run(&provider, &["hash-object", "-w", "--no-filters", "HEAD"]);
    assert_eq!(hashed.code, Some(0), "{}", hashed.err);
    let object = hashed.out.trim();
    assert!(
        !object.is_empty(),
        "the provider must hold an actual object"
    );
    git(&partial, &["config", "remote.origin.promisor", "true"]);
    git(
        &partial,
        &["config", "remote.origin.url", &for_git(&provider)],
    );
    // Even a person's URL rewrite cannot make the control contact another transport.
    let read = [
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
        "cat-file",
        "-t",
        object,
    ];
    let guarded = GitCli.run(&partial, &read);
    assert!(guarded.code.is_some(), "Git must have run: {}", guarded.err);
    assert_ne!(
        guarded.code,
        Some(0),
        "the promised object must stay missing"
    );
    assert_eq!(
        std::fs::read_dir(partial.join("objects/pack"))
            .expect("partial pack directory")
            .count(),
        0,
        "inspection must not install a promisor pack"
    );

    // The same local provider must actually supply this object with the guard disabled.
    // Run it second so the control cannot hydrate the guarded read's input in advance.
    let control = aterm_uds::spawnfd::command("git")
        .arg("-C")
        .arg(for_git(&partial))
        .args(read)
        .env("GIT_NO_LAZY_FETCH", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .expect("local-fetch control ran");
    assert!(
        control.status.success(),
        "the local provider must work: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert_eq!(control.stdout, b"blob\n");
    let hydrated = GitCli.run(&partial, &read);
    assert_eq!(hydrated.code, Some(0), "{}", hydrated.err);
    assert_eq!(hydrated.out.trim(), "blob");
}
