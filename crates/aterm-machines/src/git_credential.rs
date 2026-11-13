// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! git's GitHub credential route (docs/DESIGN-init-system-2026-10-20.md §1.18), never the login
//! keychain. Every network git `aterm init` and the vault's converge
//! pass run carries aterm's route ([`pinned`]): git's helper list reset, then aterm's own helper
//! ([`Helper`], `aterm secrets git-credential`), so no helper a person's or the system's
//! configuration names (git's `osxkeychain`, GitHub CLI's `gh auth git-credential`) runs under
//! them. The explicit release probe is outside it on purpose ([`crate::release_probe`]: it tests
//! the owner's ordinary route to the release remote). The one write is
//! [`Git::replace_keychain_block`]: the block an earlier `aterm init` wrote to git's global
//! settings ([`global_files`]) gives way to a reset and aterm's helper, so the person's own git
//! asks no keychain either.

use crate::model::User;
use crate::port::{Cmd, Secret, System};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The name of git's login-keychain helper (`git-credential-osxkeychain`), which the block an
/// earlier `aterm init` wrote on a Mac names.
const OSXKEYCHAIN: &str = "osxkeychain";

/// git's plain-file helper, which the block an earlier `aterm init` wrote elsewhere (Linux)
/// names.
const STORE: &str = "store";

/// The host git's own GitHub lookups name: clones and fetches over HTTPS.
pub(crate) const GITHUB: &str = "github.com";

/// git's helper list for every host: its empty value, read last, drops every helper a file names
/// whatever URL it is scoped to ([`pinned`]).
const ANY_HELPER: &str = "credential.helper";

/// The github.com settings the block an earlier `aterm init` wrote holds, as `git config
/// --get-regexp` prints their keys (section and name lowercased, the URL as written).
const HELPER: &str = "credential.https://github.com.helper";
const USERNAME: &str = "credential.https://github.com.username";
const USE_PATH: &str = "credential.https://github.com.usehttppath";
/// The gist.github.com helper `gh auth setup-git --hostname github.com` writes beside
/// github.com's, which an `aterm init` before 2026-10-22 ran.
const GIST_HELPER: &str = "credential.https://gist.github.com.helper";

/// The request [`Git::lookups`] hands `git credential fill`: github.com over HTTPS, naming no
/// account or path, as a clone with no account in its URL asks.
const REQUEST: &[u8] = b"protocol=https\nhost=github.com\n\n";

/// The bound on one git run: a helper answers at once, so a hang is what it catches.
const BUDGET: Duration = Duration::from_secs(60);

/// What [`Git::keychain_in_effect`] reads: every credential setting, and every conditional
/// include's file (`git config --get-regexp` prints keys with their section and name
/// lowercased).
const SETTINGS: &str = r"^(credential\..*|includeif\..*\.path)$";

/// git's bound on nested includes (`MAX_INCLUDE_DEPTH`): a deeper chain is git's own error.
const INCLUDE_DEPTH: usize = 10;

/// aterm's own GitHub HTTPS credential helper: `aterm secrets git-credential`, run by the
/// installed command selected for this account, over the vault's token or the token file
/// init.toml's `[github]` names. It reads files and never the login keychain, and answers `get`
/// alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Helper {
    value: String,
}

impl Helper {
    /// This account's declared command, or the ordered legacy locations only when no
    /// declaration exists. A refused or malformed declaration never selects another command.
    pub(crate) fn locations(sys: &dyn System, user: &User) -> std::io::Result<Vec<PathBuf>> {
        use aterm_types::installed_command as command;
        let read = sys.read_owned_file(
            &command::path(Path::new(&user.home)),
            user.uid,
            command::MAX_BYTES,
            false,
        );
        Self::locations_from(user, read.as_deref().map_err(std::io::Error::kind))
    }

    /// Decode the same selection from an observer's bounded owned-file reading. The result
    /// names locations only and grants no permission to use or execute them.
    pub(crate) fn locations_from(
        user: &User,
        read: Result<&[u8], std::io::ErrorKind>,
    ) -> std::io::Result<Vec<PathBuf>> {
        use aterm_types::installed_command as command;
        match read {
            Ok(bytes) => command::Selection::decode(bytes)
                .map(|selection| vec![selection.executable().to_path_buf()])
                .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why)),
            Err(std::io::ErrorKind::NotFound) => Ok(command::legacy(Path::new(&user.home)).into()),
            Err(kind) => Err(kind.into()),
        }
    }

    /// The selected executable spelling and its current regular-file target. The declaration
    /// is read afresh, including when a root caller acts for another account.
    pub(crate) fn target(sys: &dyn System, user: &User) -> std::io::Result<(PathBuf, PathBuf)> {
        let mut missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        for path in Self::locations(sys, user)? {
            let target = sys.canonicalize(&path).and_then(|target| {
                let stat = sys.stat(&target)?;
                if stat.dir || stat.symlink || stat.len == 0 || stat.mode & 0o111 == 0 {
                    return Err(std::io::ErrorKind::InvalidInput.into());
                }
                Ok(target)
            });
            match target {
                Ok(target) => return Ok((path, target)),
                Err(why) => missing = why,
            }
        }
        Err(missing)
    }

    /// Why this account currently has no usable installed helper, naming no credential.
    pub(crate) fn unavailable(sys: &dyn System, user: &User) -> String {
        match Self::locations(sys, user) {
            Ok(paths) => format!(
                "aterm's GitHub helper has no nonempty executable installed command at {}; repair its installation",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Err(_) => format!(
                "aterm's installed-command declaration at {} is unreadable or invalid; its helper waits for that account's installation",
                aterm_types::installed_command::path(Path::new(&user.home)).display()
            ),
        }
    }

    /// The helper over the vault's `~/.secrets/gh_access_token`; `None` while `user` has no
    /// nonempty executable installed command.
    pub(crate) fn vault(sys: &dyn System, user: &User) -> Option<Self> {
        Self::run(sys, user, String::new())
    }

    /// The helper over `token_file` (the line of it `token_key` names, when one does), answering
    /// as `account`; `None` while `user` has no usable installed command.
    pub(crate) fn token_file(
        sys: &dyn System,
        user: &User,
        account: &str,
        token_file: &Path,
        token_key: Option<&str>,
    ) -> Option<Self> {
        let word = crate::port::shell_word;
        let mut options = format!(
            " --dev-user {} --token-file {}",
            word(account),
            word(&token_file.to_string_lossy())
        );
        if let Some(key) = token_key {
            options.push_str(&format!(" --token-key {}", word(key)));
        }
        Self::run(sys, user, options)
    }

    fn run(sys: &dyn System, user: &User, options: String) -> Option<Self> {
        let (launcher, _) = Self::target(sys, user).ok()?;
        Some(Self {
            value: format!(
                "!{} secrets git-credential{options}",
                crate::port::shell_word(&launcher.to_string_lossy())
            ),
        })
    }

    /// The `credential.helper` value: the shell command git runs, with the operation after it.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// A test's stand-in for aterm's helper, the shell command `value`.
    #[cfg(test)]
    pub(crate) fn stand_in(value: &str) -> Self {
        Self {
            value: value.to_owned(),
        }
    }
}

/// `git`, a git run before its subcommand, with aterm's own credential route: `-c` settings,
/// which git reads after every file and hands to the git runs under it (a submodule's fetch), so
/// they come last in its list. The generic empty `credential.helper` resets the list for every
/// host, dropping every helper a file names whatever URL it is scoped to (`osxkeychain` among
/// them); the username a file names for github.com is cleared; and `helper` follows, for
/// github.com alone. With no `helper`, or at any other host or a proxy, no helper runs at all
/// and a lookup ends at git's disabled prompt, askpass off in both bases ([`Git::cmd`],
/// [`crate::checkout::git`]). The one builder for every network git `aterm init` and the converge
/// pass run: clones, fetches, `ls-remote`, submodule updates, an `align`'s merge (a partial
/// clone's lazy fetch), and [`Git::lookups`]' read-back.
pub(crate) fn pinned(git: Cmd, helper: Option<&Helper>) -> Cmd {
    let git = git.args([
        "-c",
        &format!("{ANY_HELPER}="),
        "-c",
        &format!("{USERNAME}="),
    ]);
    match helper {
        Some(helper) => git.arg("-c").arg(format!("{HELPER}={}", helper.value)),
        None => git,
    }
}

/// Whether aterm's own route ([`pinned`]) hands back the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookups {
    /// It does.
    Ready,
    /// It does not: why, naming no token.
    Not(String),
}

impl Lookups {
    /// Whether it does.
    pub(crate) const fn ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// What [`Git::replace_keychain_block`] does in one file that holds the block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Replacement {
    /// The file, one of [`global_files`].
    file: PathBuf,
    /// The keys whose every value in `file` is removed, in order: the block's own.
    unset: Vec<&'static str>,
    /// The helper keys that then get the empty reset and aterm's helper: the block's sections,
    /// and those of github.com and gist.github.com `file` holds nothing of.
    write: Vec<&'static str>,
}

/// The keychain block an earlier `aterm init` wrote to git's global settings
/// ([`keychain_block`]), and what [`Git::replace_keychain_block`] makes of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeychainBlock {
    /// One entry per file that holds the block, in git's reading order.
    replacements: Vec<Replacement>,
    /// The shell commands that put those files' settings under those keys back as they were,
    /// from wherever a replacement stopped: file by file, every value of each key either list
    /// names is removed (a key already absent is no stop: the commands are joined with `;`), then
    /// the block's own values are added back in file order.
    revert: String,
}

impl KeychainBlock {
    /// The shell commands that put the block back as it was.
    pub(crate) fn revert(&self) -> &str {
        &self.revert
    }
}

/// How aterm runs git for a GitHub credential: the program, and the variables a test adds.
pub(crate) struct Git {
    program: PathBuf,
    env: Vec<(String, String)>,
}

impl Git {
    /// The installed git for `user` ([`crate::items::github::git_program_for_user`]): what
    /// `aterm init`, the vault's converge pass and every clone run as that account.
    pub(crate) fn for_user(sys: &dyn System, user: &User) -> Self {
        Self {
            program: crate::items::github::git_program_for_user(sys, user),
            env: Vec::new(),
        }
    }

    /// For a test against a real git: this host's native git
    /// ([`crate::items::github::need_native_git`]) with no system configuration
    /// (`GIT_CONFIG_NOSYSTEM=1`), so no system file's helper (on a Mac, the Command Line Tools'
    /// own `credential.helper = osxkeychain`) is read anywhere the run looks.
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        Self {
            program: PathBuf::from(crate::items::github::need_native_git()),
            env: vec![("GIT_CONFIG_NOSYSTEM".into(), "1".into())],
        }
    }

    /// Whether the git this runs is installed.
    pub(crate) fn installed(&self, sys: &dyn System) -> bool {
        crate::items::github::git_executable_target(sys, &self.program).is_some()
    }

    /// A git run as `user` (this process's own account for `None`) that can never prompt.
    pub(crate) fn cmd(&self, user: Option<&User>) -> Cmd {
        let mut cmd = Cmd::new(&self.program)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .budget(BUDGET);
        for (key, value) in &self.env {
            cmd = cmd.env(key, value);
        }
        match user {
            Some(user) => cmd.as_user(user),
            None => cmd,
        }
    }

    /// Whether aterm's route ([`pinned`] with `helper`) hands back exactly `token` at github.com
    /// for `user`: a `git credential fill` asked as a clone asks (no account), which runs no
    /// helper but `helper`. With no helper (no launcher), it does not, and nothing runs.
    ///
    /// # Errors
    /// git could not run whole, or answered no single password or one too long.
    pub(crate) fn lookups(
        &self,
        sys: &dyn System,
        user: &User,
        helper: Option<&Helper>,
        token: &Secret,
    ) -> Result<Lookups, String> {
        let Some(helper) = helper else {
            return Ok(Lookups::Not(Helper::unavailable(sys, user)));
        };
        Ok(match self.fill(sys, user, helper)? {
            Some(held) if held.bytes() == token.bytes() => Lookups::Ready,
            Some(_) => Lookups::Not(
                "aterm's GitHub helper hands back another credential than the token".into(),
            ),
            None => Lookups::Not("aterm's GitHub helper hands back no token".into()),
        })
    }

    /// The token aterm's route hands back at github.com, or `None` when it holds none: git then
    /// ends at its disabled prompt (exit 128) without asking. `GCM_INTERACTIVE=never` keeps Git
    /// Credential Manager from asking in a window, as git's own prompt is kept off.
    fn fill(
        &self,
        sys: &dyn System,
        user: &User,
        helper: &Helper,
    ) -> Result<Option<Secret>, String> {
        let mut ran = sys
            .run_complete_stdout_input(&self.read_back(user, helper), REQUEST)
            .map_err(|_| "git credential fill did not complete (output withheld)".to_owned())?;
        let answer = if ran.success() {
            let mut passwords = ran
                .stdout
                .split(|byte| *byte == b'\n')
                .filter_map(|line| line.strip_prefix(b"password="));
            match (passwords.next(), passwords.next()) {
                (Some(password), None) => Secret::new(password)
                    .map(Some)
                    .map_err(|_| "the GitHub credential is too long".to_owned()),
                _ => {
                    Err("git credential fill answered no single password (output withheld)".into())
                }
            }
        } else {
            Ok(None)
        };
        scrub(&mut ran.stdout);
        scrub(&mut ran.stderr);
        answer
    }

    /// The `git credential fill` [`Self::lookups`] runs through aterm's route.
    fn read_back(&self, user: &User, helper: &Helper) -> Cmd {
        pinned(self.cmd(Some(user)), Some(helper))
            .env("GCM_INTERACTIVE", "never")
            .args(["credential", "fill"])
    }

    /// The keychain block an earlier `aterm init` wrote to `user`'s global settings
    /// ([`keychain_block`]), read from git's settings: no helper runs and GitHub is not asked.
    /// `answers` says whether aterm's helper has a token here, which only a section of GitHub
    /// CLI's helper needs.
    ///
    /// # Errors
    /// git could not read its settings whole, or its answer is not configuration.
    pub(crate) fn keychain_block(
        &self,
        sys: &dyn System,
        user: &User,
        answers: &dyn Fn() -> bool,
    ) -> Result<Option<KeychainBlock>, String> {
        let every = self.settings(sys, user, None)?;
        Ok(keychain_block(&user.home, &every, sys.macos(), answers))
    }

    /// What the person's own git may still ask of the login keychain, named by key and file. At
    /// github.com: the keychain helper for every lookup ([`keychain_helper_in_effect`]), one for
    /// a user or a path under the host, or one in a file a conditional include names. At
    /// gist.github.com: one its own section leaves in the list, and nothing else, so a generic
    /// helper at gist.github.com goes unnamed. Where this read cannot tell, why
    /// ([`Self::judged`]). `None` when there is none of these. It only informs a row, so a read
    /// that fails is a reason here, never an error. aterm's own git never asks it ([`pinned`]).
    pub(crate) fn keychain_in_effect(&self, sys: &dyn System, user: &User) -> Option<String> {
        self.judged(sys, user, |every, conditional| {
            let keychain = |setting: &&Setting| {
                relevant(setting)
                    .is_some_and(|(scope, name)| name == "helper" && scope != Scope::Never)
                    && setting.value.as_deref().is_some_and(keychain_helper)
            };
            keychain_helper_in_effect(every)
                .or_else(|| {
                    every
                        .iter()
                        .filter(|setting| {
                            relevant(setting).is_some_and(|(scope, _)| scope == Scope::Part)
                        })
                        .find(keychain)
                        .map(Setting::named)
                })
                .or_else(|| last_keychain_helper(every, |setting| setting.key == GIST_HELPER))
                .or_else(|| {
                    conditional.iter().find(keychain).map(|setting| {
                        format!("{} (a file a conditional include names)", setting.named())
                    })
                })
        })
    }

    /// Replace `block`, the keychain block an earlier `aterm init` wrote, in each of `user`'s
    /// files that holds it: every value of its keys there is removed, then each helper key it
    /// names gets the empty reset and `helper`, so no keychain helper git, the system or an
    /// earlier file names is left in the list of a host whose helper key the block names. A
    /// section of someone else's, in that file or a later one, keeps what it names, and every
    /// other setting (`[user]`, `[url …]`) stays. The caller journals [`KeychainBlock::revert`]
    /// first. Recheck caller admission before every mutating command; an already started
    /// command may finish. `git config --unset-all` exits 5 with empty output when a key is
    /// already absent.
    ///
    /// # Errors
    /// Admission was withdrawn, or a `git config` run did not complete or failed (output
    /// withheld): the files may then hold part of the change, which the revert puts back.
    pub(crate) fn replace_keychain_block(
        &self,
        sys: &dyn System,
        user: &User,
        block: &KeychainBlock,
        helper: &Helper,
        admit: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        for replacement in &block.replacements {
            let config = |args: &[&str], absent_ok: bool| -> Result<(), String> {
                admit()?;
                let ran = sys
                    .run_complete_stdout(
                        &self
                            .cmd(Some(user))
                            .arg("config")
                            .arg("--file")
                            .arg(&replacement.file)
                            .args(args),
                    )
                    .map_err(|_| "replacing git's keychain GitHub settings did not complete")?;
                let absent = absent_ok
                    && ran.status == Some(5)
                    && ran.stdout.is_empty()
                    && ran.stderr.is_empty();
                if ran.success() || absent {
                    Ok(())
                } else {
                    Err("replacing git's keychain GitHub settings failed (output withheld)".into())
                }
            };
            for key in &replacement.unset {
                config(&["--unset-all", key], true)?;
            }
            for key in &replacement.write {
                config(&["--add", key, ""], false)?;
                config(&["--add", key, helper.value()], false)?;
            }
        }
        Ok(())
    }

    /// `judge` of the credential settings of every file git reads as `user`, in its order, and
    /// of each file a conditional include names. A read git refuses, an include this cannot
    /// resolve and one nested too deep are a reason in `judge`'s place: what they hold is
    /// unknown. A file git answers with nothing for holds nothing.
    fn judged(
        &self,
        sys: &dyn System,
        user: &User,
        judge: impl FnOnce(&[Setting], &[Setting]) -> Option<String>,
    ) -> Option<String> {
        let every = match self.settings(sys, user, None) {
            Ok(every) => every,
            Err(why) => return Some(why),
        };
        let mut named = BTreeSet::new();
        let mut pending = Vec::new();
        if let Err(why) = includes(&every, user, 1, &mut named, &mut pending) {
            return Some(why);
        }
        let mut conditional = Vec::new();
        while let Some((file, depth)) = pending.pop() {
            let read = match self.settings(sys, user, Some(&file)) {
                Ok(read) => read,
                Err(why) => return Some(format!("{}: {why}", file.display())),
            };
            if let Err(why) = includes(&read, user, depth + 1, &mut named, &mut pending) {
                return Some(why);
            }
            conditional.extend(read);
        }
        judge(&every, &conditional)
    }

    /// The settings [`SETTINGS`] names, read as `user`, each with the file it is in: from
    /// every file git reads (`None`), or from `file` and what it includes.
    fn settings(
        &self,
        sys: &dyn System,
        user: &User,
        file: Option<&Path>,
    ) -> Result<Vec<Setting>, String> {
        let ran = sys
            .run_complete_stdout(&self.read(user, file))
            .map_err(|_| "cannot completely read git's credential settings")?;
        if ran.status == Some(1) && ran.stdout.is_empty() {
            return Ok(Vec::new());
        }
        if !ran.success() {
            return Err("cannot read git's credential settings (output withheld)".into());
        }
        let text = std::str::from_utf8(&ran.stdout)
            .map_err(|_| "git's credential settings are not UTF-8")?;
        parse(text).ok_or_else(|| "git's credential settings are not configuration".into())
    }

    /// The `git config` run [`Self::settings`] makes.
    fn read(&self, user: &User, file: Option<&Path>) -> Cmd {
        let mut cmd = self
            .cmd(Some(user))
            .args(["config", "--null", "--show-origin"]);
        if let Some(file) = file {
            cmd = cmd.arg("--file").arg(file).arg("--includes");
        }
        cmd.args(["--get-regexp", SETTINGS])
    }
}

/// The two files the `git config --global` of an earlier `aterm init` could have written, in
/// git's reading order: `~/.config/git/config`, where that file was there and `~/.gitconfig` was
/// not, and `~/.gitconfig`. `gh auth setup-git` writes the same way. The first path is fixed, not
/// the person's `$XDG_CONFIG_HOME`: init's cleared environment never saw that variable.
fn global_files(home: &str) -> [PathBuf; 2] {
    let home = Path::new(home);
    [home.join(".config/git/config"), home.join(".gitconfig")]
}

/// Whether a helper's value names a helper that reads the login keychain: git's own
/// `osxkeychain` (with or without arguments, which git runs as `git credential-osxkeychain`), a
/// command that runs `git-credential-osxkeychain` (`!git credential-osxkeychain`, its path), or
/// one that runs GitHub CLI's `gh auth git-credential`, which hands back the token gh keeps in
/// the keychain.
fn keychain_helper(value: &str) -> bool {
    value.split_whitespace().next() == Some(OSXKEYCHAIN)
        || value.contains("credential-osxkeychain")
        || command_words(value)
            .collect::<Vec<_>>()
            .windows(3)
            .any(gh_credential)
}

/// The words of a helper's shell command, split at blanks, quotes and the shell's command
/// punctuation: `!"/opt/homebrew/bin/gh" auth git-credential` is `/opt/homebrew/bin/gh`,
/// `auth`, `git-credential`.
fn command_words(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c.is_whitespace() || "\"'!;&|(){}`".contains(c))
        .filter(|word| !word.is_empty())
}

/// Whether `words` are GitHub CLI's helper: a program named `gh` (bare or by path; `agh`, the
/// vault's, is another program), `auth`, `git-credential`.
fn gh_credential(words: &[&str]) -> bool {
    matches!(words, [program, "auth", "git-credential"]
        if Path::new(program).file_name().is_some_and(|name| name == "gh"))
}

/// Whether a helper's value is the one `gh auth setup-git` writes: `!<gh> auth git-credential`,
/// its path bare or quoted, and nothing else.
fn gh_setup_git(value: &str) -> bool {
    value.trim_start().starts_with('!') && gh_credential(&command_words(value).collect::<Vec<_>>())
}

/// Whether `value` of github.com's `key` in a global file is one an earlier `aterm init` wrote
/// there, on a Mac (`macos`) or elsewhere: `Some(true)` for its helper (on a Mac `osxkeychain`,
/// or, where `gh` allows it, GitHub CLI's as `gh auth setup-git` wrote it; elsewhere `store`),
/// `Some(false)` for another value it wrote (the empty reset, an account, `useHttpPath` off), and
/// `None` for anyone else's.
fn github_value(key: &str, value: &str, macos: bool, gh: bool) -> Option<bool> {
    match key {
        HELPER if value.is_empty() => Some(false),
        HELPER if macos => (value == OSXKEYCHAIN || (gh && gh_setup_git(value))).then_some(true),
        HELPER => (value == STORE).then_some(true),
        USE_PATH => (boolean(value) == Some(false)).then_some(false),
        _ => Some(false),
    }
}

/// Whether `value` of gist.github.com's helper in a global file is one `gh auth setup-git`
/// wrote there for an `aterm init` on a Mac: `Some(true)` for GitHub CLI's helper where `gh`
/// allows it, `Some(false)` for the empty reset, `None` for anyone else's (and anything
/// elsewhere, where no `aterm init` ran gh).
fn gist_value(value: &str, macos: bool, gh: bool) -> Option<bool> {
    if !macos {
        None
    } else if value.is_empty() {
        Some(false)
    } else {
        (gh && gh_setup_git(value)).then_some(true)
    }
}

/// The keychain helper among the helpers git runs for every github.com lookup, once the empty
/// entries in `every` have reset the list as git resets it, named by key and file; `None` when
/// none of them is one.
fn keychain_helper_in_effect(every: &[Setting]) -> Option<String> {
    last_keychain_helper(every, |setting| {
        matches!(relevant(setting), Some((Scope::Every, "helper")))
    })
}

/// The keychain helper left among the `helper` settings of `every`, once their empty entries
/// have reset the list as git resets it, named by key and file.
fn last_keychain_helper(every: &[Setting], helper: impl Fn(&Setting) -> bool) -> Option<String> {
    let mut helpers: Vec<&Setting> = Vec::new();
    for setting in every.iter().filter(|setting| helper(setting)) {
        match setting.value.as_deref() {
            Some("") => helpers.clear(),
            Some(_) => helpers.push(setting),
            None => {}
        }
    }
    helpers
        .into_iter()
        .find(|setting| setting.value.as_deref().is_some_and(keychain_helper))
        .map(Setting::named)
}

/// How a section of one global file stands ([`keychain_block`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// That file holds none of the keys.
    Absent,
    /// It is the block an earlier `aterm init` wrote.
    Written,
    /// It is someone else's.
    Theirs,
}

/// How `keys` stand in one file (`in_file`'s settings among `every`), `judge` reading each
/// value: [`Section::Written`] when every value is one init wrote and its helper is among
/// them, its `helper_key` opens with the empty reset (every generation of init wrote that first,
/// so a section that does not is not init's), and, where `whole`, it holds an account and
/// `useHttpPath` too (the `store` block: a helper and a reset alone are too plain to be init's).
fn section(
    every: &[Setting],
    in_file: &dyn Fn(&Setting) -> bool,
    keys: &[&str],
    helper_key: &str,
    judge: impl Fn(&str, &str) -> Option<bool>,
    whole: bool,
) -> Section {
    let held: Vec<&Setting> = every
        .iter()
        .filter(|setting| in_file(setting) && keys.contains(&setting.key.as_str()))
        .collect();
    if held.is_empty() {
        return Section::Absent;
    }
    let mut helper = false;
    for setting in &held {
        match setting
            .value
            .as_deref()
            .and_then(|value| judge(&setting.key, value))
        {
            Some(found) => helper |= found,
            None => return Section::Theirs,
        }
    }
    let opens_with_reset = held
        .iter()
        .find(|setting| setting.key == helper_key)
        .is_some_and(|setting| setting.value.as_deref() == Some(""));
    let has = |key: &str| held.iter().any(|setting| setting.key == key);
    let complete = !whole || (has(USERNAME) && has(USE_PATH));
    if helper && opens_with_reset && complete {
        Section::Written
    } else {
        Section::Theirs
    }
}

/// When either of `home`'s global files ([`global_files`]) holds a keychain block an earlier
/// `aterm init` wrote (on a Mac when `macos`), what [`Git::replace_keychain_block`] removes and
/// writes there, and the shell commands that put it back. Each file is judged on its own, and in
/// it each section ([`section`]): github.com's three settings ([`HELPER`], [`USERNAME`],
/// [`USE_PATH`], [`github_value`]: on a Mac the empty reset and `osxkeychain` with an account and
/// `useHttpPath` off, as init wrote from 2026-10-22, or the reset and GitHub CLI's helper, as the
/// `gh auth setup-git` init ran before then wrote; elsewhere the reset and `store` with an
/// account and `useHttpPath` off, as init wrote from 2026-10-26) and gist.github.com's helper
/// ([`GIST_HELPER`], [`gist_value`]: the reset and GitHub CLI's helper, on a Mac). A value of
/// anyone else's under a section's keys, or a section with no reset, makes that section someone
/// else's, left as it is: the vault's own section in `~/.config/git/config` is one. With a block
/// found in a file, each of the two hosts whose section there is the block's, or holds nothing in
/// that file, gets the reset and aterm's helper there.
///
/// `gh auth setup-git` writes GitHub CLI's section for anyone, so its shape cannot prove it was
/// init's, and replacing a person's working gh route with a helper that has no token to give
/// would break their `git push`. Such a section counts only where `answers` says aterm's helper
/// has a token here; `answers` is asked only then. `osxkeychain` and `store` need no such word:
/// the ruling is about them.
fn keychain_block(
    home: &str,
    every: &[Setting],
    macos: bool,
    answers: &dyn Fn() -> bool,
) -> Option<KeychainBlock> {
    let mut replacements = Vec::new();
    let mut steps = Vec::new();
    for file in global_files(home) {
        if let Some((replacement, revert)) = block_in(file, every, macos, answers) {
            replacements.push(replacement);
            steps.extend(revert);
        }
    }
    (!replacements.is_empty()).then(|| KeychainBlock {
        replacements,
        revert: steps.join("; "),
    })
}

/// The block in `file` alone ([`keychain_block`]): the replacement there, and the commands that
/// put `file`'s settings back.
fn block_in(
    file: PathBuf,
    every: &[Setting],
    macos: bool,
    answers: &dyn Fn() -> bool,
) -> Option<(Replacement, Vec<String>)> {
    let in_file = |setting: &Setting| {
        setting
            .origin
            .strip_prefix("file:")
            .is_some_and(|origin| Path::new(origin) == file)
    };
    let gh = macos
        && every
            .iter()
            .filter(|setting| in_file(setting))
            .any(|setting| setting.value.as_deref().is_some_and(gh_setup_git))
        && answers();
    let github_keys = [HELPER, USERNAME, USE_PATH];
    let github = section(
        every,
        &in_file,
        &github_keys,
        HELPER,
        |key, value| github_value(key, value, macos, gh),
        !macos,
    );
    let gist = section(
        every,
        &in_file,
        &[GIST_HELPER],
        GIST_HELPER,
        |_, value| gist_value(value, macos, gh),
        false,
    );
    if github != Section::Written && gist != Section::Written {
        return None;
    }
    let mut unset: Vec<&'static str> = Vec::new();
    let mut write: Vec<&'static str> = Vec::new();
    for (stands, keys, helper) in [
        (github, &github_keys[..], HELPER),
        (gist, &[GIST_HELPER][..], GIST_HELPER),
    ] {
        if stands == Section::Written {
            unset.extend(keys);
        }
        if stands != Section::Theirs {
            write.push(helper);
        }
    }
    let named = crate::port::shell_word(&file.to_string_lossy());
    let mut touched = unset.clone();
    touched.extend(write.iter().filter(|key| !unset.contains(key)));
    let mut steps: Vec<String> = touched
        .iter()
        .map(|key| format!("git config --file {named} --unset-all {key}"))
        .collect();
    steps.extend(
        every
            .iter()
            .filter(|setting| in_file(setting) && unset.contains(&setting.key.as_str()))
            .map(|setting| {
                format!(
                    "git config --file {named} --add {} {}",
                    setting.key,
                    crate::port::shell_word(setting.value.as_deref().unwrap_or_default())
                )
            }),
    );
    Some((Replacement { file, unset, write }, steps))
}

/// One setting a read printed: the file it is in (`--show-origin`: `file:<path>`), its key
/// (section and name lowercased, a URL as written), and its value (`None` for a key written
/// with no `=`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Setting {
    origin: String,
    key: String,
    value: Option<String>,
}

impl Setting {
    /// Its key, a URL's user and password withheld, and the file it is in: what a reason
    /// names.
    fn named(&self) -> String {
        let file = self.origin.strip_prefix("file:").unwrap_or(&self.origin);
        let key = match (self.key.strip_prefix("credential."), self.key.rfind('@')) {
            (Some(url), Some(at)) => {
                let start = "credential.".len() + url.find("://").map_or(0, |end| end + 3);
                if start < at {
                    format!("{}<user>{}", &self.key[..start], &self.key[at..])
                } else {
                    self.key.clone()
                }
            }
            _ => self.key.clone(),
        };
        format!("{key} in {file}")
    }
}

/// The settings of `git config --null --show-origin --get-regexp` output (`origin\0key\nvalue\0`
/// per entry, `origin\0key\0` for a key with no value), in git's reading order.
fn parse(text: &str) -> Option<Vec<Setting>> {
    let mut fields = text.split_terminator('\0');
    let mut out = Vec::new();
    while let Some(origin) = fields.next() {
        let entry = fields.next()?;
        let (key, value) = entry
            .split_once('\n')
            .map_or((entry, None), |(key, value)| (key, Some(value)));
        out.push(Setting {
            origin: origin.to_owned(),
            key: key.to_owned(),
            value: value.map(str::to_owned),
        });
    }
    Some(out)
}

/// Queue, once each, the files the conditional includes among `settings` name, resolved as
/// git resolves an include's path: `~/` from the account's home, a relative path from the
/// directory of the file that names it.
///
/// # Errors
/// An include this cannot resolve, or one nested deeper than git reads: why, as a reason.
fn includes(
    settings: &[Setting],
    user: &User,
    depth: usize,
    named: &mut BTreeSet<PathBuf>,
    pending: &mut Vec<(PathBuf, usize)>,
) -> Result<(), String> {
    for setting in settings
        .iter()
        .filter(|s| s.key.starts_with("includeif.") && s.key.ends_with(".path"))
    {
        let unresolved = || format!("{} names a file this check cannot resolve", setting.named());
        let value = setting
            .value
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(unresolved)?;
        let file = if let Some(rest) = value.strip_prefix("~/") {
            Path::new(&user.home).join(rest)
        } else if value.starts_with('/') {
            PathBuf::from(value)
        } else if value.starts_with('~') || value.starts_with("%(") {
            return Err(unresolved());
        } else {
            setting
                .origin
                .strip_prefix("file:")
                .map(Path::new)
                .and_then(Path::parent)
                .filter(|dir| dir.is_absolute())
                .ok_or_else(unresolved)?
                .join(value)
        };
        if depth > INCLUDE_DEPTH {
            return Err(format!(
                "{} nests includes deeper than git reads",
                setting.named()
            ));
        }
        if named.insert(file.clone()) {
            pending.push((file, depth));
        }
    }
    Ok(())
}

/// How a `credential.<url>.*` setting applies to git's github.com lookups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// To every one: the generic setting, or a URL naming the host alone.
    Every,
    /// To some: a URL naming a user or a path under the host.
    Part,
    /// To none: another scheme, host or port.
    Never,
}

/// How the setting whose key holds `url` (`None` for the generic `credential.<name>`)
/// applies to git's lookups for `https://[<user>@]github.com[/<path>]`.
fn scope(url: Option<&str>) -> Scope {
    let Some(url) = url else {
        return Scope::Every;
    };
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (user, address) = match authority.rsplit_once('@') {
        Some((user, address)) => (Some(user), address),
        None => (None, authority),
    };
    // git normalizes a key with a scheme and a host it can spell (no `%` escape), and
    // compares any other key as a partial URL. A normalized key's path keeps its slashes and
    // is matched as a prefix of the lookup's (`//` matches only a lookup with no path); a
    // partial key's path loses its leading and trailing ones.
    let normalized = !scheme.is_empty() && !address.is_empty() && !address.contains('%');
    let (github, whole_host) = if normalized {
        (normalized_github(scheme, address), matches!(path, "" | "/"))
    } else {
        (
            partial_github(scheme, address),
            path.trim_matches('/').is_empty(),
        )
    };
    if !github {
        Scope::Never
    } else if user.is_some() || !whole_host {
        Scope::Part
    } else {
        Scope::Every
    }
}

/// Whether a key git normalizes names github.com over https: the scheme and host in any
/// case, a `*` label for any label, the default port, and one trailing dot dropped from the
/// host unless nothing would be left.
fn normalized_github(scheme: &str, address: &str) -> bool {
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let host = match address.rsplit_once(':') {
        Some((_, port)) if !port.is_empty() && port.trim_start_matches('0') != "443" => {
            return false;
        }
        Some((host, _)) => host,
        None => address,
    };
    let host = match host.strip_suffix('.') {
        Some(rest) if !rest.is_empty() => rest,
        _ => host,
    };
    let labels = GITHUB.split('.');
    host.split('.').count() == labels.clone().count()
        && host
            .split('.')
            .zip(labels)
            .all(|(have, want)| have == "*" || have.eq_ignore_ascii_case(want))
}

/// Whether a partial key names github.com over https as git compares one, byte for byte: a
/// scheme, if any, spelled `https`, and a host, if any, that decodes to `github.com` (a port
/// is part of the host).
fn partial_github(scheme: &str, address: &str) -> bool {
    (scheme.is_empty() || scheme == "https")
        && (address.is_empty() || percent_decoded(address) == GITHUB)
}

/// `text` with each `%XX` escape decoded (git decodes a URL's host before matching it); an
/// escape that is not two hex digits stays as written.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A git boolean (`git_config_bool`): `None` for a value git refuses.
fn boolean(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" | "" => Some(false),
        number => number.parse::<i64>().ok().map(|number| number != 0),
    }
}

/// How a setting git reads for a credential lookup applies to github.com's, and its name:
/// `None` for one no lookup reads (another section, `credential.interactive`).
fn relevant(setting: &Setting) -> Option<(Scope, &str)> {
    let rest = setting.key.strip_prefix("credential.")?;
    let (url, name) = rest
        .rsplit_once('.')
        .map_or((None, rest), |(url, name)| (Some(url), name));
    matches!(name, "helper" | "username" | "usehttppath").then(|| (scope(url), name))
}

fn scrub(bytes: &mut [u8]) {
    aterm_uds::secret::scrub(bytes);
}

/// The git runs above as the test arm keys them, for the tests of `aterm init`'s GitHub steps
/// and the vault's converge pass: each scripts a [`crate::port::Fake`] answer for one of them.
#[cfg(test)]
#[path = "git_credential_script_tests.rs"]
pub(crate) mod script;

#[cfg(test)]
#[path = "git_credential_tests.rs"]
mod tests;
