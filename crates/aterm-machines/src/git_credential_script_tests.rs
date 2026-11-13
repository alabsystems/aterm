// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{GIST_HELPER, Git, HELPER, Helper, USE_PATH, USERNAME};
use crate::model::User;
use crate::port::{Fake, Ran, System as _};
use std::path::{Path, PathBuf};

/// The Command Line Tools' git, as [`Git::for_user`] finds it without a managed build.
fn clt() -> Git {
    Git {
        program: PathBuf::from(crate::items::github::CLT_GIT),
        env: Vec::new(),
    }
}

/// The git selected for this account, including a ready managed build ([`Git::for_user`]).
fn host(sys: &Fake, user: &User) -> Git {
    Git::for_user(sys, user)
}

/// The helper github.com's section of the block an earlier `aterm init` wrote names on the
/// host `sys` is: `osxkeychain` on a Mac, `store` elsewhere.
pub(crate) fn written_helper(sys: &dyn crate::port::System) -> &'static str {
    if sys.macos() {
        super::OSXKEYCHAIN
    } else {
        super::STORE
    }
}

/// CLT git's own configuration file, which names `osxkeychain` for every host.
pub(crate) const CLT_CONFIG: &str =
    "/Library/Developer/CommandLineTools/usr/share/git-core/gitconfig";

/// The settings read of every file git reads ([`Git::keychain_block`],
/// [`Git::keychain_in_effect`]).
pub(crate) fn helpers(user: &User) -> String {
    clt().read(user, None).display()
}

/// Its read of `file`, one a conditional include names.
pub(crate) fn included(user: &User, file: &str) -> String {
    clt().read(user, Some(std::path::Path::new(file))).display()
}

/// `user`'s launcher in place on `sys`, so aterm's helper ([`Helper::vault`]) is there.
pub(crate) fn launcher(sys: &Fake, user: &User) {
    sys.executable(
        &aterm_types::installed_command::legacy(Path::new(&user.home))[0].to_string_lossy(),
        b"launcher",
    );
}

/// aterm's helper over the vault's token, as it reads for `user` with the launcher there.
pub(crate) fn helper(user: &User) -> Helper {
    let sys = Fake::default();
    launcher(&sys, user);
    Helper::vault(&sys, user).expect("the launcher is there")
}

/// The read-back through aterm's route with the vault's helper ([`Git::lookups`]).
pub(crate) fn fill(user: &User) -> String {
    clt().read_back(user, &helper(user)).display()
}

/// `settings` (file, key, value) as that read prints them.
pub(crate) fn printed(settings: &[(&str, &str, &str)]) -> String {
    settings
        .iter()
        .map(|(file, key, value)| format!("file:{file}\0{key}\n{value}\0"))
        .collect()
}

/// The settings (file, key, value) of the vault's `~/.config/git/config` for github.com,
/// after CLT's own helper on a Mac: a reset, then the vault's helper.
fn vault(sys: &Fake, user: &User) -> Vec<[String; 3]> {
    let xdg = format!("{}/.config/git/config", user.home);
    let mut settings = Vec::new();
    if sys.macos() {
        settings.push([CLT_CONFIG, "credential.helper", "osxkeychain"].map(str::to_owned));
    }
    settings.push([xdg.as_str(), HELPER, ""].map(str::to_owned));
    settings.push([xdg.as_str(), HELPER, "!f() { :; }; f"].map(str::to_owned));
    settings
}

/// Answer the settings read with `settings`.
fn answer(sys: &Fake, user: &User, settings: &[[String; 3]]) {
    let rows: Vec<(&str, &str, &str)> = settings
        .iter()
        .map(|[file, key, value]| (file.as_str(), key.as_str(), value.as_str()))
        .collect();
    sys.on(
        &host(sys, user).read(user, None).display(),
        Ran::ok(&printed(&rows)),
    );
}

/// git's settings as the secrets vault's `~/.config/git/config` leaves them, with no block
/// in either global file. On Linux (`sys.linux`), the distribution's git, with no CLT file.
pub(crate) fn alike(sys: &Fake, user: &User) {
    answer(sys, user, &vault(sys, user));
}

/// git's settings with the keychain block an earlier `aterm init` wrote to `~/.gitconfig`
/// on a Mac for `account`, on top of the vault's helper.
pub(crate) fn keychain_block(sys: &Fake, user: &User, account: &str) {
    let global = format!("{}/.gitconfig", user.home);
    let mut settings = vault(sys, user);
    for (key, value) in [
        (HELPER, ""),
        (HELPER, "osxkeychain"),
        (USERNAME, account),
        (USE_PATH, "false"),
    ] {
        settings.push([global.as_str(), key, value].map(str::to_owned));
    }
    answer(sys, user, &settings);
}

/// git's settings with what `gh auth setup-git` wrote to `~/.gitconfig` for an `aterm init`
/// before 2026-10-22 (GitHub CLI's helper for github.com and gist.github.com, each after the
/// empty reset), on top of the vault's helper.
pub(crate) fn gh_setup_git_block(sys: &Fake, user: &User) {
    let global = format!("{}/.gitconfig", user.home);
    let gh = "!/opt/homebrew/bin/gh auth git-credential";
    let mut settings = vault(sys, user);
    for (key, value) in [
        (HELPER, ""),
        (HELPER, gh),
        (GIST_HELPER, ""),
        (GIST_HELPER, gh),
    ] {
        settings.push([global.as_str(), key, value].map(str::to_owned));
    }
    answer(sys, user, &settings);
}

/// git's settings before any GitHub setup: CLT's own `credential.helper` alone, the
/// keychain's, and nothing for github.com.
pub(crate) fn unconfigured(sys: &Fake, user: &User) {
    sys.on(
        &helpers(user),
        Ran::ok(&printed(&[(
            CLT_CONFIG,
            "credential.helper",
            "osxkeychain",
        )])),
    );
}

/// A read-back through aterm's route with `helper` that hands back `token`.
pub(crate) fn hands_back(sys: &Fake, user: &User, helper: &Helper, token: &str) {
    sys.on(
        &host(sys, user).read_back(user, helper).display(),
        Ran::ok(&format!(
            "protocol=https\nhost=github.com\nusername=fixture\npassword={token}\n"
        )),
    );
}

/// A read-back through aterm's route with the vault's helper that hands back `token` (for
/// `account` at `host`).
pub(crate) fn lookups_hold(sys: &Fake, user: &User, host: &str, account: &str, token: &str) {
    sys.on(
        &self::host(sys, user)
            .read_back(user, &helper(user))
            .display(),
        Ran::ok(&format!(
            "protocol=https\nhost={host}\nusername={account}\npassword={token}\n"
        )),
    );
}

/// A read-back aterm's helper has nothing for: git's disabled prompt.
pub(crate) fn lookups_hold_nothing(sys: &Fake, user: &User) {
    sys.on(
        &host(sys, user).read_back(user, &helper(user)).display(),
        Ran::exit(
            128,
            "fatal: could not read Password: terminal prompts disabled",
        ),
    );
}

/// The runs [`Git::replace_keychain_block`] makes of the block [`keychain_block`] scripts
/// (github.com's three keys removed; the reset and aterm's helper for github.com and the
/// absent gist.github.com), each answered `ok`.
pub(crate) fn replaces_keychain_block(sys: &Fake, user: &User) -> Vec<String> {
    replaces_keychain_block_with(sys, user, &helper(user))
}

/// [`replaces_keychain_block`] with `helper` in place of the vault's.
pub(crate) fn replaces_keychain_block_with(
    sys: &Fake,
    user: &User,
    helper: &Helper,
) -> Vec<String> {
    replaces(
        sys,
        user,
        helper,
        &[HELPER, USERNAME, USE_PATH],
        &[HELPER, GIST_HELPER],
    )
}

/// The runs [`Git::replace_keychain_block`] makes of the block [`gh_setup_git_block`]
/// scripts, each answered `ok`.
pub(crate) fn replaces_gh_setup_git_block(sys: &Fake, user: &User) -> Vec<String> {
    replaces(
        sys,
        user,
        &helper(user),
        &[HELPER, USERNAME, USE_PATH, GIST_HELPER],
        &[HELPER, GIST_HELPER],
    )
}

/// The `--unset-all` runs of `unset` and the reset and `helper` for each of `write` in
/// `user`'s `~/.gitconfig`, each answered `ok`.
fn replaces(
    sys: &Fake,
    user: &User,
    helper: &Helper,
    unset: &[&str],
    write: &[&str],
) -> Vec<String> {
    let global = Path::new(&user.home).join(".gitconfig");
    let config = |args: &[&str]| {
        host(sys, user)
            .cmd(Some(user))
            .arg("config")
            .arg("--file")
            .arg(&global)
            .args(args.iter().copied())
            .display()
    };
    let mut runs: Vec<String> = unset
        .iter()
        .map(|key| config(&["--unset-all", key]))
        .collect();
    for key in write {
        runs.push(config(&["--add", key, ""]));
        runs.push(config(&["--add", key, helper.value()]));
    }
    for run in &runs {
        sys.on(run, Ran::ok(""));
    }
    runs
}
