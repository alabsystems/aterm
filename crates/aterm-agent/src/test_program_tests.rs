// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `git <args>` in `dir` from [`kept_git_command`], with no system or global config: its
/// output, whatever its status.
fn kept_git(dir: &Path, args: &[&str]) -> std::process::Output {
    aterm_uds::deadline::output_until(
        kept_git_command()
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap_or_else(|e| panic!("git {args:?} within the hang detector: {e}"))
}

/// A SCRIPTED COMMAND RUNS FROM THE KEPT COPY: `git submodule` is a script in
/// `libexec/git-core`, which Apple's Git finds beside its own executable and its kept copy,
/// elsewhere, only through `GIT_EXEC_PATH`; without it the copy says `'submodule' is not a git
/// command`.
#[test]
fn the_kept_git_runs_its_scripted_commands() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let init = kept_git(tmp.path(), &["init", "-q", "."]);
    assert!(
        init.status.success(),
        "git init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let listed = kept_git(tmp.path(), &["submodule", "status"]);
    assert!(
        listed.status.success(),
        "git submodule status: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
}

/// AN INIT FROM THE KEPT COPY LAYS ITS TEMPLATES: Apple's Git finds `hooks/` and the rest of
/// its init templates by a path relative to its own executable, which its kept copy, elsewhere,
/// finds only through `GIT_TEMPLATE_DIR`; without it the copy's init lays no `hooks/`.
#[test]
fn an_init_from_the_kept_git_lays_its_hooks() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let init = kept_git(tmp.path(), &["init", "-q", "."]);
    assert!(
        init.status.success(),
        "git init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(
        tmp.path().join(".git/hooks").is_dir(),
        "git init laid no hooks/: {}",
        String::from_utf8_lossy(&init.stderr)
    );
}
