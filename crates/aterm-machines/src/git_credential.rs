// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! git's GitHub credential route (docs/DESIGN-init-system-2026-10-20.md §1.18), never the login
//! keychain (owner ruling 2026-11-03). Every network git `aterm init` and the vault's converge
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
pub(crate) mod script {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::user;
    use crate::port::spawns::Spawns;
    use crate::port::{Fake, Live, Ran, Scratch};

    const TOKEN: &str = "fixture_credential_token_only";

    /// Where `secret` shows in what `live` spawned for the runs of `record`: the program,
    /// argument, variable or standard input that holds it, or `None`.
    fn exposed(record: &[(Cmd, Option<Vec<u8>>)], live: &Live, secret: &str) -> Option<String> {
        for (cmd, input) in record {
            let (program, args, env) = live.spawned(cmd).unwrap();
            if program.to_string_lossy().contains(secret) {
                return Some(format!("program of {}", cmd.display()));
            }
            if let Some(arg) = args.iter().find(|a| a.to_string_lossy().contains(secret)) {
                return Some(format!("argument {arg:?}"));
            }
            if let Some((key, _)) = env.iter().find(|(_, value)| value.contains(secret)) {
                return Some(format!("variable {key}"));
            }
            if input
                .as_deref()
                .is_some_and(|i| i.windows(secret.len()).any(|w| w == secret.as_bytes()))
            {
                return Some(format!("standard input of {}", cmd.display()));
            }
        }
        None
    }

    fn alice() -> User {
        user("alice", 501)
    }

    /// git's URL matching as [`scope`] reads it;
    /// `the_url_matcher_agrees_with_what_git_runs_for_a_github_lookup` checks the hard spellings
    /// against the lookups a real git runs a helper under each for.
    #[test]
    fn a_setting_applies_to_github_as_git_matches_its_url() {
        for (url, want) in [
            (None, Scope::Every),
            (Some("https://github.com"), Scope::Every),
            (Some("https://github.com/"), Scope::Every),
            (Some("https://GitHub.com"), Scope::Every),
            (Some("HTTPS://github.com"), Scope::Every),
            (Some("https://*.com"), Scope::Every),
            (Some("https://*.*"), Scope::Every),
            (Some("https://github.com:443"), Scope::Every),
            (Some("https://github.com:"), Scope::Every),
            (Some("https://%67ithub.com"), Scope::Every),
            (Some("https://github.com."), Scope::Every),
            (Some("https://GITHUB.COM."), Scope::Every),
            (Some("https://github.com.:443"), Scope::Every),
            (Some("HTTPS://github.com."), Scope::Every),
            (Some("https://github.com.:0443"), Scope::Every),
            (Some("https://github.com.:"), Scope::Every),
            (Some("https://*.*."), Scope::Every),
            (Some("https://%67ithub.com/"), Scope::Every),
            (Some("github.com"), Scope::Every),
            (Some("github.com/"), Scope::Every),
            (Some("github.com//"), Scope::Every),
            (Some("https://%67ithub.com//"), Scope::Every),
            (Some("://github.com"), Scope::Every),
            (Some("https://"), Scope::Every),
            (Some("://"), Scope::Every),
            (Some(""), Scope::Every),
            (Some("https://bot@github.com"), Scope::Part),
            (Some("https://bot:pw@github.com"), Scope::Part),
            (Some("https://github.com/owner"), Scope::Part),
            (Some("https://github.com?x"), Scope::Part),
            (Some("https://github.com./owner"), Scope::Part),
            (Some("https://github.com.:443/owner"), Scope::Part),
            (Some("https://*.com./owner"), Scope::Part),
            (Some("github.com/owner/repo.git"), Scope::Part),
            (Some("bot@github.com"), Scope::Part),
            (Some("https://bot@%67ithub.com"), Scope::Part),
            (Some("/owner"), Scope::Part),
            (Some("https:///owner"), Scope::Part),
            (Some("https://github.com//"), Scope::Part),
            (Some("HTTPS://GITHUB.COM.:443//"), Scope::Part),
            (Some("https://github.com///"), Scope::Part),
            (Some("http://github.com"), Scope::Never),
            (Some("ssh://github.com"), Scope::Never),
            (Some("https://github.com:8443"), Scope::Never),
            (Some("https://github.com:x"), Scope::Never),
            (Some("https://gist.github.com"), Scope::Never),
            (Some("https://github.com.example.invalid"), Scope::Never),
            (Some("https://github.co"), Scope::Never),
            (Some("https://github.com../owner"), Scope::Never),
            (Some("https://github.com%2E"), Scope::Never),
            (Some("https://."), Scope::Never),
            (Some("https://./owner"), Scope::Never),
            (Some("https://*"), Scope::Never),
            (Some("https://[::1]"), Scope::Never),
            (Some("example.invalid"), Scope::Never),
            (Some("github.com."), Scope::Never),
            (Some("github.com./owner"), Scope::Never),
            (Some("bot@github.com."), Scope::Never),
            (Some("GITHUB.COM"), Scope::Never),
            (Some("*.com"), Scope::Never),
            (Some("github.com:443"), Scope::Never),
            (Some("github.com:"), Scope::Never),
            (Some("://github.com."), Scope::Never),
            (Some("://GITHUB.COM"), Scope::Never),
            (Some("https://%47ITHUB.COM"), Scope::Never),
            (Some("HTTPS://%67ithub.com"), Scope::Never),
            (Some("https://%67ithub.com:443"), Scope::Never),
            (Some("https://%67ithub.com."), Scope::Never),
            (Some("https://*.%63om"), Scope::Never),
            (Some("https://%2a.com"), Scope::Never),
            (Some("https://github.com:443."), Scope::Never),
            (Some("HTTPS://"), Scope::Never),
            (Some("http://"), Scope::Never),
        ] {
            assert_eq!(scope(url), want, "{url:?}");
        }
    }

    /// `(key, value)` pairs as settings of the file `/f`.
    fn listed(entries: &[(&str, Option<&str>)]) -> Vec<Setting> {
        entries
            .iter()
            .map(|(key, value)| Setting {
                origin: "file:/f".into(),
                key: (*key).into(),
                value: value.map(str::to_owned),
            })
            .collect()
    }

    #[test]
    fn a_read_is_records_of_a_file_a_key_and_a_value() {
        assert_eq!(
            parse("file:/a\0credential.helper\nosxkeychain\0file:/b\0credential.usehttppath\0"),
            Some(vec![
                Setting {
                    origin: "file:/a".into(),
                    key: "credential.helper".into(),
                    value: Some("osxkeychain".into()),
                },
                Setting {
                    origin: "file:/b".into(),
                    key: "credential.usehttppath".into(),
                    value: None,
                },
            ])
        );
        assert_eq!(parse(""), Some(Vec::new()));
        assert!(parse("file:/a\0").is_none());
        assert!(parse("file:/a").is_none());
    }

    /// A conditional include's file resolves as git resolves it, each once. Negative control:
    /// one this cannot resolve, and one nested past git's bound, are reasons, not skipped.
    #[test]
    fn a_conditional_include_resolves_as_git_resolves_it() {
        let who = alice();
        let include = |origin: &str, value: Option<&str>| Setting {
            origin: origin.into(),
            key: "includeif.gitdir:~/work/.path".into(),
            value: value.map(str::to_owned),
        };
        let (mut named, mut pending) = (BTreeSet::new(), Vec::new());
        let settings = [
            include("file:/Users//alice/.gitconfig", Some("~/work.inc")),
            include("file:/Users//alice/.config/git/config", Some("work.inc")),
            include("file:/etc/gitconfig", Some("/opt/shared.inc")),
            include("file:/Users//alice/.gitconfig", Some("~/work.inc")),
            listed(&[("credential.helper", Some("x"))]).remove(0),
        ];
        includes(&settings, &who, 1, &mut named, &mut pending).unwrap();
        assert_eq!(
            pending,
            [
                (PathBuf::from("/Users//alice/work.inc"), 1),
                (PathBuf::from("/Users//alice/.config/git/work.inc"), 1),
                (PathBuf::from("/opt/shared.inc"), 1),
            ]
        );
        for unresolvable in [
            include("file:/Users//alice/.gitconfig", Some("~other/work.inc")),
            include("file:/Users//alice/.gitconfig", Some("%(prefix)/work.inc")),
            include("command line:", Some("work.inc")),
            include("file:/Users//alice/.gitconfig", None),
        ] {
            let why = includes(&[unresolvable], &who, 1, &mut named, &mut pending).unwrap_err();
            assert!(why.contains("cannot resolve"), "{why}");
        }
        let deep = include("file:/Users//alice/.gitconfig", Some("/deep.inc"));
        assert!(
            includes(
                std::slice::from_ref(&deep),
                &who,
                INCLUDE_DEPTH,
                &mut named,
                &mut pending
            )
            .is_ok()
        );
        let why = includes(&[deep], &who, INCLUDE_DEPTH + 1, &mut named, &mut pending).unwrap_err();
        assert!(why.contains("deeper than git reads"), "{why}");
    }

    /// [`keychain_block`] where aterm's helper has a token here.
    fn block_of(home: &str, every: &[Setting], macos: bool) -> Option<KeychainBlock> {
        keychain_block(home, every, macos, &|| true)
    }

    /// The replacement of `block`, which is in one file.
    fn only(block: &KeychainBlock) -> &Replacement {
        let [replacement] = block.replacements.as_slice() else {
            panic!("one file: {block:?}");
        };
        replacement
    }

    /// The keys `block`, which is in one file, removes and writes.
    fn keys(block: &KeychainBlock) -> (Vec<&'static str>, Vec<&'static str>) {
        let replacement = only(block);
        (replacement.unset.clone(), replacement.write.clone())
    }

    /// `(file, key, value)` settings as git prints them.
    fn at(entries: &[(&str, &str, Option<&str>)]) -> Vec<Setting> {
        entries
            .iter()
            .map(|(file, key, value)| Setting {
                origin: format!("file:{file}"),
                key: (*key).into(),
                value: value.map(str::to_owned),
            })
            .collect()
    }

    const GLOBAL: &str = "/Users//alice/.gitconfig";
    const XDG: &str = "/Users//alice/.config/git/config";

    /// The block an earlier `aterm init` wrote, there in `~/.gitconfig`, as git prints it.
    const BLOCK: [(&str, &str, Option<&str>); 4] = [
        (GLOBAL, HELPER, Some("")),
        (GLOBAL, HELPER, Some(OSXKEYCHAIN)),
        (GLOBAL, USERNAME, Some("alice")),
        (GLOBAL, USE_PATH, Some("false")),
    ];

    /// The vault's helper in the XDG file, after CLT's own keychain helper.
    const VAULT: [(&str, &str, Option<&str>); 3] = [
        (script::CLT_CONFIG, "credential.helper", Some(OSXKEYCHAIN)),
        (XDG, HELPER, Some("")),
        (XDG, HELPER, Some("!f() { :; }; f")),
    ];

    /// The keychain block `aterm init` wrote on a Mac is found by its settings alone: its keys
    /// are removed, github.com's helper key gets aterm's route, and the revert removes every
    /// key the replacement touches before it adds the block back in its order. It is found in
    /// either file `git config --global` writes, and replaced there; a block in each goes in one
    /// pass. A value of anyone else's under its keys, or the block in a third file, is not it,
    /// and neither is the vault's own section in the XDG file. A keychain helper
    /// is in effect for github.com when it is still in the list once git's resets apply,
    /// however it is spelled; one a reset drops, or one for another host, is not.
    #[test]
    fn the_keychain_block_and_a_keychain_helper_in_effect_are_read_from_settings_alone() {
        let home = "/Users//alice";
        let mut written = VAULT.to_vec();
        written.extend(BLOCK);
        written.push((GLOBAL, GIST_HELPER, Some("!gh")));
        written.push((GLOBAL, "user.name", Some("Alice")));
        let block = block_of(home, &at(&written), true).expect("the block init wrote");
        assert_eq!(
            only(&block).file,
            Path::new(GLOBAL),
            "the vault's file stays"
        );
        assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
        assert_eq!(
            only(&block).write,
            [HELPER],
            "someone else's gist helper stays"
        );
        assert_eq!(
            block.revert(),
            "git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.helper; \
             git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.username; \
             git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.usehttppath; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper ''; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper osxkeychain; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.username alice; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.usehttppath false"
        );
        assert_eq!(
            keychain_helper_in_effect(&at(&written)).as_deref(),
            Some("credential.https://github.com.helper in /Users//alice/.gitconfig")
        );
        // With no gist.github.com section, its helper key gets aterm's route too.
        let mut bare = VAULT.to_vec();
        bare.extend(BLOCK);
        let block = block_of(home, &at(&bare), true).expect("the block init wrote");
        assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
        assert!(
            block
                .revert()
                .contains("--unset-all credential.https://gist.github.com.helper"),
            "{}",
            block.revert()
        );
        // Negative controls: someone else's value under the block's keys, the block without the
        // keychain, or in another file.
        for (label, extra) in [
            ("another helper", (GLOBAL, HELPER, Some("!other"))),
            ("useHttpPath on", (GLOBAL, USE_PATH, Some("true"))),
            ("a key with no value", (GLOBAL, USERNAME, None)),
        ] {
            let mut theirs = written.clone();
            theirs.push(extra);
            assert_eq!(block_of(home, &at(&theirs), true), None, "{label}");
        }
        let unkeyed: Vec<_> = BLOCK
            .iter()
            .copied()
            .filter(|(_, _, value)| *value != Some(OSXKEYCHAIN))
            .collect();
        assert_eq!(block_of(home, &at(&unkeyed), true), None);
        // The other file `git config --global` writes: the block is found and replaced there,
        // and a section the first file holds nothing of is that file's to fill. A third file is
        // no file of init's, the person's own `$XDG_CONFIG_HOME` among them.
        let moved = |file| -> Vec<_> {
            BLOCK
                .iter()
                .map(|(_, key, value)| (file, *key, *value))
                .collect()
        };
        let mut xdg = moved(XDG);
        xdg.push((GLOBAL, GIST_HELPER, Some("!other")));
        let block = block_of(home, &at(&xdg), true).expect("the block in the XDG file");
        assert_eq!(only(&block).file, Path::new(XDG));
        assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
        assert!(
            block
                .revert()
                .contains(&format!("--file {XDG} --add {HELPER} osxkeychain"))
                && !block.revert().contains(GLOBAL),
            "{}",
            block.revert()
        );
        for file in ["/Users//alice/xdg/git/config", "/Users//alice/work.inc"] {
            assert_eq!(block_of(home, &at(&moved(file)), true), None, "{file}");
        }
        let mut both = moved(XDG);
        both.extend(BLOCK);
        let block = block_of(home, &at(&both), true).expect("a block in each file");
        let files: Vec<&Path> = block
            .replacements
            .iter()
            .map(|replacement| replacement.file.as_path())
            .collect();
        assert_eq!(files, [Path::new(XDG), Path::new(GLOBAL)], "git's order");
        assert!(
            block.revert().contains(&format!("--file {XDG} --add"))
                && block.revert().contains(&format!("--file {GLOBAL} --add")),
            "{}",
            block.revert()
        );
        // The vault's section in the XDG file is someone else's, as the file leaves it for both
        // hosts, and it is no reason to read a block into the other file.
        let agh = "!\"/Users//alice/.local/bin/agh\" auth git-credential";
        let vault = [
            (XDG, HELPER, Some("")),
            (XDG, HELPER, Some(agh)),
            (XDG, GIST_HELPER, Some("")),
            (XDG, GIST_HELPER, Some(agh)),
        ];
        assert_eq!(
            block_of(home, &at(&vault), true),
            None,
            "the vault's section"
        );
        assert_eq!(
            block_of(home, &at(&VAULT), true),
            None,
            "the vault's section"
        );
        let other_case = [(
            GLOBAL,
            "credential.https://GitHub.com.helper",
            Some(OSXKEYCHAIN),
        )];
        assert_eq!(
            block_of(home, &at(&other_case), true),
            None,
            "a key git's --unset-all of the block's own would not remove is not the block"
        );
        // Every generation of init wrote the reset first, so a section without it is a person's:
        // left as it is, whichever helper it holds, and still named.
        for (label, lone) in [
            ("a lone osxkeychain", (GLOBAL, HELPER, Some(OSXKEYCHAIN))),
            (
                "a lone gh helper",
                (
                    GLOBAL,
                    HELPER,
                    Some("!/opt/homebrew/bin/gh auth git-credential"),
                ),
            ),
        ] {
            assert_eq!(block_of(home, &at(&[lone]), true), None, "{label}");
            assert!(keychain_helper_in_effect(&at(&[lone])).is_some(), "{label}");
        }
        let gist = [(GLOBAL, GIST_HELPER, Some("!gh auth git-credential"))];
        assert_eq!(block_of(home, &at(&gist), true), None, "a lone gist helper");
        let reset_last = [
            (GLOBAL, HELPER, Some(OSXKEYCHAIN)),
            (GLOBAL, HELPER, Some("")),
        ];
        assert_eq!(block_of(home, &at(&reset_last), true), None, "reset last");

        assert_eq!(keychain_helper_in_effect(&at(&VAULT)), None);
        for (label, helper) in [
            (
                "a later generic osxkeychain",
                ("credential.helper", "osxkeychain"),
            ),
            (
                "its path",
                (
                    "credential.https://github.com.helper",
                    "/opt/git/libexec/git-core/git-credential-osxkeychain",
                ),
            ),
            (
                "a shell command",
                ("credential.helper", "!git credential-osxkeychain"),
            ),
            (
                "an argument",
                ("credential.helper", "osxkeychain --stand-in"),
            ),
            (
                "another spelling of the host",
                ("credential.https://GitHub.com.helper", " osxkeychain "),
            ),
        ] {
            let mut with = VAULT.to_vec();
            with.push((GLOBAL, helper.0, Some(helper.1)));
            assert_eq!(
                keychain_helper_in_effect(&at(&with)).as_deref(),
                Some(format!("{} in {GLOBAL}", helper.0).as_str()),
                "{label}"
            );
        }
        let mut with = VAULT.to_vec();
        with.push((GLOBAL, GIST_HELPER, Some(OSXKEYCHAIN)));
        assert_eq!(keychain_helper_in_effect(&at(&with)), None, "another host");
        assert_eq!(
            keychain_helper_in_effect(&at(&VAULT[..1])).as_deref(),
            Some(format!("credential.helper in {}", script::CLT_CONFIG).as_str()),
            "CLT's own helper, with no reset after it"
        );
    }

    /// GitHub CLI's helper, which hands back the token gh keeps in the login keychain, is a
    /// keychain helper however a command runs it; the vault's `agh` helper and other gh verbs
    /// are not. What `gh auth setup-git` wrote for an `aterm init` before 2026-10-22 (the reset
    /// and GitHub CLI's helper, for github.com and for gist.github.com) is a keychain block on a
    /// Mac, each section judged on its own. Negative controls: GitHub CLI's helper inside
    /// someone else's command, beside someone else's helper, or off a Mac (where no `aterm init`
    /// ran gh), is not the block.
    #[test]
    fn github_clis_helper_is_a_keychain_helper_and_its_setup_git_block_is_replaced() {
        const GH: &str = "!/opt/homebrew/bin/gh auth git-credential";
        const QUOTED: &str = "!\"/opt/homebrew/bin/gh\" auth git-credential";
        const WRAPPED: &str = "!f() { gh auth git-credential \"$@\"; }; f";
        for (label, value) in [
            ("as gh auth setup-git writes it", GH),
            ("its path quoted", QUOTED),
            ("on PATH", "!gh auth git-credential"),
            ("in a shell function", WRAPPED),
        ] {
            let mut with = VAULT.to_vec();
            with.push((GLOBAL, HELPER, Some(value)));
            assert_eq!(
                keychain_helper_in_effect(&at(&with)).as_deref(),
                Some(format!("{HELPER} in {GLOBAL}").as_str()),
                "{label}"
            );
        }
        for (label, value) in [
            (
                "the vault's agh",
                "!\"/Users//alice/.local/bin/agh\" auth git-credential",
            ),
            ("aterm's agh", "!aterm agh auth git-credential"),
            ("another gh verb", "!gh auth token"),
            (
                "a program named after gh",
                "!gh-stand-in auth git-credential",
            ),
        ] {
            let mut with = VAULT.to_vec();
            with.push((GLOBAL, HELPER, Some(value)));
            assert_eq!(keychain_helper_in_effect(&at(&with)), None, "{label}");
        }

        let home = "/Users//alice";
        let setup_git = [
            (GLOBAL, HELPER, Some("")),
            (GLOBAL, HELPER, Some(GH)),
            (GLOBAL, GIST_HELPER, Some("")),
            (GLOBAL, GIST_HELPER, Some(QUOTED)),
        ];
        let mut written = VAULT.to_vec();
        written.extend(setup_git);
        written.push((GLOBAL, "user.name", Some("Alice")));
        let block = block_of(home, &at(&written), true).expect("the block gh setup-git wrote");
        assert_eq!(
            only(&block).unset,
            [HELPER, USERNAME, USE_PATH, GIST_HELPER]
        );
        assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
        assert_eq!(
            block.revert(),
            "git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.helper; \
             git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.username; \
             git config --file /Users//alice/.gitconfig --unset-all credential.https://github.com.usehttppath; \
             git config --file /Users//alice/.gitconfig --unset-all credential.https://gist.github.com.helper; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper ''; \
             git config --file /Users//alice/.gitconfig --add credential.https://github.com.helper '!/opt/homebrew/bin/gh auth git-credential'; \
             git config --file /Users//alice/.gitconfig --add credential.https://gist.github.com.helper ''; \
             git config --file /Users//alice/.gitconfig --add credential.https://gist.github.com.helper '!\"/opt/homebrew/bin/gh\" auth git-credential'"
        );
        assert_eq!(
            block_of(home, &at(&written), false),
            None,
            "off a Mac no aterm init ran gh"
        );
        // gist.github.com's half alone, where init's later block replaced github.com's: both
        // hosts get aterm's route, github.com's holding nothing in ~/.gitconfig.
        let mut gist = VAULT.to_vec();
        gist.extend(&setup_git[2..]);
        let block = block_of(home, &at(&gist), true).expect("gist.github.com's half");
        assert_eq!(keys(&block), (vec![GIST_HELPER], vec![HELPER, GIST_HELPER]));
        // Negative controls: GitHub CLI's helper in someone else's command is in effect but
        // not the block; beside someone else's helper, its section is someone else's and keeps
        // its helper key.
        let mut wrapped = VAULT.to_vec();
        wrapped.extend([(GLOBAL, HELPER, Some("")), (GLOBAL, HELPER, Some(WRAPPED))]);
        assert_eq!(block_of(home, &at(&wrapped), true), None);
        assert!(keychain_helper_in_effect(&at(&wrapped)).is_some());
        let mut beside = written.clone();
        beside.push((GLOBAL, GIST_HELPER, Some("!other")));
        let block = block_of(home, &at(&beside), true).expect("github.com's section");
        assert_eq!(
            keys(&block),
            (vec![HELPER, USERNAME, USE_PATH], vec![HELPER])
        );
    }

    /// Off a Mac, the block `aterm init` wrote from 2026-10-26 (the reset, `store`, an account
    /// and `useHttpPath` off) is found whole and replaced like the Mac's; `~/.git-credentials`,
    /// the person's file, is not read or touched. Negative controls: a lone `store`, the block
    /// missing a line, and the block on a Mac (where init never wrote it) are someone else's.
    #[test]
    fn the_store_block_init_wrote_elsewhere_is_found_whole() {
        let home = "/Users//alice";
        let vault = [
            (XDG, HELPER, Some("")),
            (XDG, HELPER, Some("!f() { :; }; f")),
        ];
        let store = [
            (GLOBAL, HELPER, Some("")),
            (GLOBAL, HELPER, Some(STORE)),
            (GLOBAL, USERNAME, Some("alice")),
            (GLOBAL, USE_PATH, Some("false")),
        ];
        let mut written = vault.to_vec();
        written.extend(store);
        let block = block_of(home, &at(&written), false).expect("the block init wrote");
        assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
        assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);
        assert!(
            !block.revert().contains("git-credentials"),
            "{}",
            block.revert()
        );
        assert_eq!(block_of(home, &at(&written), true), None, "on a Mac");
        for (label, drop) in [("no reset", 0), ("no account", 2), ("useHttpPath unset", 3)] {
            let mut part = vault.to_vec();
            part.extend(
                store
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| *at != drop)
                    .map(|(_, setting)| *setting),
            );
            assert_eq!(block_of(home, &at(&part), false), None, "{label}");
        }
        let mut lone = vault.to_vec();
        lone.push((GLOBAL, HELPER, Some(STORE)));
        assert_eq!(block_of(home, &at(&lone), false), None, "a lone store");
        let mut mac = vault.to_vec();
        mac.extend(BLOCK);
        assert_eq!(
            block_of(home, &at(&mac), false),
            None,
            "the Mac's block elsewhere"
        );
    }

    /// The keychain helper the person's own git still asks for github.com is read from the
    /// settings alone, wherever it is: in effect for every lookup, scoped to some, or in a file
    /// a conditional include names. Negative controls: the vault's helper alone, and a scoped
    /// setting that names no keychain helper, are none; no helper runs for any read.
    #[test]
    fn a_keychain_helper_in_effect_is_read_without_running_one() {
        let who = alice();
        let mac = || {
            let sys = Fake::default();
            sys.executable(crate::items::github::CLT_GIT, b"git");
            sys
        };
        let work = "/Users//alice/work.inc";
        let vault = script::printed(&[(XDG, HELPER, ""), (XDG, HELPER, "!f() { :; }; f")]);
        for (label, sys, found, reads) in [
            (
                "GitHub CLI's helper for gist.github.com alone",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[
                        (GLOBAL, GIST_HELPER, ""),
                        (GLOBAL, GIST_HELPER, "!gh auth git-credential"),
                    ]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys
                },
                Some(format!("{GIST_HELPER} in {GLOBAL}")),
                vec![script::helpers(&who)],
            ),
            (
                "aterm's helper for gist.github.com",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[
                        (GLOBAL, GIST_HELPER, ""),
                        (GLOBAL, GIST_HELPER, "!aterm secrets git-credential"),
                    ]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys
                },
                None,
                vec![script::helpers(&who)],
            ),
            (
                "an include git refuses to read is a reason, never an error",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[(
                        GLOBAL,
                        "includeif.gitdir:~/work/.path",
                        "~/work.inc",
                    )]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys.on(
                        &script::included(&who, work),
                        Ran::exit(128, "fatal: bad config line 1"),
                    );
                    sys
                },
                Some(format!(
                    "{work}: cannot read git's credential settings (output withheld)"
                )),
                vec![script::helpers(&who), script::included(&who, work)],
            ),
            (
                "settings git cannot be run to read are a reason too",
                mac(),
                Some("cannot completely read git's credential settings".to_owned()),
                vec![script::helpers(&who)],
            ),
            (
                "the block init wrote",
                {
                    let sys = mac();
                    script::keychain_block(&sys, &who, "alice");
                    sys
                },
                Some(format!("{HELPER} in {GLOBAL}")),
                vec![script::helpers(&who)],
            ),
            (
                "CLT's own helper alone",
                {
                    let sys = mac();
                    script::unconfigured(&sys, &who);
                    sys
                },
                Some(format!("credential.helper in {}", script::CLT_CONFIG)),
                vec![script::helpers(&who)],
            ),
            (
                "a keychain helper in a file a conditional include names",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[(
                        GLOBAL,
                        "includeif.gitdir:~/work/.path",
                        "~/work.inc",
                    )]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys.on(
                        &script::included(&who, work),
                        Ran::ok(&script::printed(&[(
                            work,
                            "credential.helper",
                            "osxkeychain",
                        )])),
                    );
                    sys
                },
                Some(format!(
                    "credential.helper in {work} (a file a conditional include names)"
                )),
                vec![script::helpers(&who), script::included(&who, work)],
            ),
            (
                "a keychain helper for a user",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[(
                        XDG,
                        "credential.https://bot@github.com.helper",
                        "osxkeychain",
                    )]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys
                },
                Some(format!(
                    "credential.https://<user>@github.com.helper in {XDG}"
                )),
                vec![script::helpers(&who)],
            ),
            (
                "control: the vault's helper alone",
                {
                    let sys = mac();
                    script::alike(&sys, &who);
                    sys
                },
                None,
                vec![script::helpers(&who)],
            ),
            (
                "control: a path-scoped account",
                {
                    let sys = mac();
                    let mut text = vault.clone();
                    text.push_str(&script::printed(&[(
                        XDG,
                        "credential.https://github.com/alice.username",
                        "bot",
                    )]));
                    sys.on(&script::helpers(&who), Ran::ok(&text));
                    sys
                },
                None,
                vec![script::helpers(&who)],
            ),
        ] {
            let git = Git::for_user(&sys, &who);
            assert_eq!(git.keychain_in_effect(&sys, &who), found, "{label}");
            assert_eq!(sys.calls(), reads, "{label}: no helper ran");
        }
    }

    /// aterm's route answers when its own helper hands back exactly the token, and says why
    /// otherwise, naming no token; with no launcher there is no route and nothing runs. The
    /// read-back is the one fill, through the route alone.
    #[test]
    fn lookups_ask_atermss_helper_alone_and_hand_back_only_the_token() {
        let who = alice();
        let token = Secret::new(TOKEN.as_bytes()).unwrap();
        let sys = Fake::default();
        let git = Git::for_user(&sys, &who);
        assert_eq!(Helper::vault(&sys, &who), None, "no launcher, no helper");
        let Lookups::Not(why) = git.lookups(&sys, &who, None, &token).unwrap() else {
            panic!("no route is not ready");
        };
        assert!(
            why.contains("/Users//alice/.local/bin/aterm")
                && why.contains("repair its installation"),
            "{why}"
        );
        assert!(sys.calls().is_empty(), "nothing ran");
        for (answer, ready) in [
            (Some(TOKEN), true),
            (Some("fixture_another_token"), false),
            (None, false),
        ] {
            let sys = Fake::default();
            script::launcher(&sys, &who);
            match answer {
                Some(held) => script::lookups_hold(&sys, &who, GITHUB, "alice", held),
                None => script::lookups_hold_nothing(&sys, &who),
            }
            let helper = Helper::vault(&sys, &who).expect("the launcher is there");
            let read = Git::for_user(&sys, &who)
                .lookups(&sys, &who, Some(&helper), &token)
                .unwrap();
            assert_eq!(read.ready(), ready, "{answer:?}: {read:?}");
            assert_eq!(sys.calls(), [script::fill(&who)]);
            if let Lookups::Not(why) = read {
                assert!(!why.contains("fixture"), "{why}");
            }
        }
        let sys = Fake::default();
        script::launcher(&sys, &who);
        sys.on(&script::fill(&who), Ran::ok("password=one\npassword=two\n"));
        let helper = Helper::vault(&sys, &who).unwrap();
        assert!(
            Git::for_user(&sys, &who)
                .lookups(&sys, &who, Some(&helper), &token)
                .is_err(),
            "two passwords are no answer"
        );
        let declared = PathBuf::from("/custom store/aterm");
        let record = aterm_types::installed_command::path(Path::new(&who.home));
        let bytes = aterm_types::installed_command::Selection::new(declared.clone())
            .unwrap()
            .encode()
            .unwrap();
        sys.file(record.to_str().unwrap(), &bytes).stat_of(
            record.to_str().unwrap(),
            crate::port::Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
        sys.executable(declared.to_str().unwrap(), b"declared command");
        let helper = Helper::vault(&sys, &who).unwrap();
        script::hands_back(&sys, &who, &helper, TOKEN);
        assert!(
            git.lookups(&sys, &who, Some(&helper), &token)
                .unwrap()
                .ready()
        );
        assert!(sys.calls().last().unwrap().contains("/custom store/aterm"));
    }

    /// A network git carries aterm's route last on the command line: the helper list
    /// and the account reset, then aterm's helper by the launcher's path, the options a token
    /// file names shell-quoted; with no helper, the resets alone. A helper that would ask in a
    /// window is told not to, and the read-back names no account.
    #[test]
    fn the_route_resets_every_helper_then_names_atermss_own() {
        let who = alice();
        assert_eq!(
            script::fill(&who),
            "alice$ GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= GCM_INTERACTIVE=never \
             /Library/Developer/CommandLineTools/usr/bin/git \
             -c credential.helper= -c credential.https://github.com.username= \
             -c 'credential.https://github.com.helper=!/Users//alice/.local/bin/aterm secrets git-credential' \
             credential fill"
        );
        let sys = Fake::default();
        script::launcher(&sys, &who);
        let helper = Helper::token_file(
            &sys,
            &who,
            "alice",
            Path::new("/Users//alice/my secrets/github.env"),
            Some("GH_TOKEN"),
        )
        .unwrap();
        assert_eq!(
            helper.value(),
            "!/Users//alice/.local/bin/aterm secrets git-credential --dev-user alice \
             --token-file '/Users//alice/my secrets/github.env' --token-key GH_TOKEN"
        );
        let bare = pinned(Cmd::new("/usr/bin/git"), None)
            .arg("fetch")
            .display();
        assert_eq!(
            bare,
            "/usr/bin/git -c credential.helper= \
             -c credential.https://github.com.username= fetch"
        );
        let mut dir = who.clone();
        dir.home = "/Users//alice/a dir".into();
        sys.executable("/Users//alice/a dir/.local/bin/aterm", b"launcher");
        assert_eq!(
            Helper::vault(&sys, &dir).unwrap().value(),
            "!'/Users//alice/a dir/.local/bin/aterm' secrets git-credential"
        );
        let custom = PathBuf::from("/custom store/aterm");
        let custom_bin = "/custom bin/aterm";
        sys.link(custom_bin, custom.to_str().unwrap());
        sys.executable(custom.to_str().unwrap(), b"installed command");
        let record = aterm_types::installed_command::path(Path::new(&who.home));
        let bytes = aterm_types::installed_command::Selection::new(custom)
            .unwrap()
            .encode()
            .unwrap();
        sys.file(record.to_str().unwrap(), &bytes).stat_of(
            record.to_str().unwrap(),
            crate::port::Stat {
                uid: who.uid,
                gid: who.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
        assert_eq!(
            Helper::vault(&sys, &who).unwrap().value(),
            "!'/custom store/aterm' secrets git-credential",
            "the stable declaration wins over an existing legacy launcher and custom BIN link"
        );
    }

    #[test]
    fn helper_requires_a_nonempty_executable_launcher() {
        let who = alice();
        let launcher = aterm_types::installed_command::legacy(Path::new(&who.home))[0].clone();
        let launcher = launcher.to_str().unwrap();
        for executable in [false, true] {
            for bytes in [b"".as_slice(), b"launcher".as_slice()] {
                let sys = Fake::default();
                if executable {
                    sys.executable(launcher, bytes);
                } else {
                    sys.file(launcher, bytes);
                }
                let admitted = executable && !bytes.is_empty();
                assert_eq!(Helper::vault(&sys, &who).is_some(), admitted);
                assert_eq!(
                    Helper::token_file(&sys, &who, "alice", Path::new("/token"), None).is_some(),
                    admitted
                );
            }
        }
        let sys = Fake::default();
        sys.link(launcher, launcher);
        assert!(Helper::vault(&sys, &who).is_none());
        assert!(sys.calls().is_empty(), "admission starts no command");
    }

    /// The launcher is a link into the installed app, so the file behind it decides. Negative
    /// controls: a link to nothing and a link to a folder are no launcher; an executable file
    /// and a link to the app's executable binary are.
    #[test]
    fn a_launcher_link_to_nothing_is_no_launcher() {
        let who = alice();
        let app = "/Applications/aterm.app/Contents/MacOS/aterm";
        let [launcher, store] = aterm_types::installed_command::legacy(Path::new(&who.home));
        let launcher = launcher.to_str().unwrap();
        let sys = Fake::default();
        sys.link(launcher, app);
        assert_eq!(Helper::vault(&sys, &who), None, "the app moved away");
        sys.executable(app, b"aterm");
        assert!(Helper::vault(&sys, &who).is_some(), "the app is there");

        let sys = Fake::default();
        sys.link(launcher, "/Applications");
        sys.stat_of(
            "/Applications",
            crate::port::Stat {
                uid: 0,
                gid: 0,
                mode: 0o755,
                dir: true,
                symlink: false,
                len: 0,
            },
        );
        assert_eq!(Helper::vault(&sys, &who), None, "a folder");

        let sys = Fake::default();
        script::launcher(&sys, &who);
        assert!(Helper::vault(&sys, &who).is_some(), "a plain file");

        let sys = Fake::root();
        sys.executable(store.to_str().unwrap(), b"legacy store command");
        assert!(
            Helper::vault(&sys, &who)
                .unwrap()
                .value()
                .contains(store.to_str().unwrap()),
            "an absent declaration can use the legacy store when BIN is absent"
        );
        script::launcher(&sys, &who);
        let record = aterm_types::installed_command::path(Path::new(&who.home));
        let declared = PathBuf::from("/custom store/aterm");
        let bytes = aterm_types::installed_command::Selection::new(declared.clone())
            .unwrap()
            .encode()
            .unwrap();
        let policy = crate::port::Stat {
            uid: who.uid,
            gid: who.gid,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: bytes.len() as u64,
        };
        sys.file(record.to_str().unwrap(), &bytes)
            .stat_of(record.to_str().unwrap(), policy);
        assert_eq!(
            Helper::vault(&sys, &who),
            None,
            "declared missing target does not fall back"
        );
        sys.executable(declared.to_str().unwrap(), b"repaired command");
        assert!(
            Helper::vault(&sys, &who)
                .unwrap()
                .value()
                .contains("/custom store/aterm")
        );
        sys.stat_of(
            declared.to_str().unwrap(),
            crate::port::Stat {
                dir: true,
                ..policy
            },
        );
        assert_eq!(
            Helper::vault(&sys, &who),
            None,
            "a declared directory does not fall back"
        );
        sys.stat_of(
            declared.to_str().unwrap(),
            crate::port::Stat {
                dir: false,
                mode: 0o755,
                ..policy
            },
        );
        sys.stat_of(
            record.to_str().unwrap(),
            crate::port::Stat { uid: 0, ..policy },
        );
        assert_eq!(
            Helper::vault(&sys, &who),
            None,
            "root cannot borrow a foreign-owned Alice declaration"
        );
        sys.stat_of(record.to_str().unwrap(), policy);
        sys.file(
            "/var/root/.config/aterm/installed-command.json",
            b"unrelated root record",
        );
        assert!(
            Helper::vault(&sys, &who).is_some(),
            "root acting for Alice uses Alice's owned record"
        );
        sys.file(record.to_str().unwrap(), b"malformed");
        assert_eq!(
            Helper::vault(&sys, &who),
            None,
            "malformed never falls back"
        );
        assert!(
            sys.calls().is_empty(),
            "no arbitrary current executable or PATH program is probed"
        );
    }

    /// git tries askpass before it looks at `GIT_TERMINAL_PROMPT`, so a person's `core.askPass`
    /// would answer a lookup the route has no helper for. The run's base keeps it off. Control:
    /// the same run without the empty `GIT_ASKPASS` takes `/bin/echo`'s answer. `/bin/echo` echoes
    /// git's prompt back and reads nothing, and no helper runs in either.
    #[test]
    fn a_persons_askpass_never_answers_a_pinned_lookup() {
        crate::items::github::need_native_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-askpass").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        live.write_file(
            &home.join(".gitconfig"),
            b"[core]\n\taskPass = /bin/echo\n",
            0o600,
        )
        .unwrap();
        let ask = |git: Cmd| {
            let git = git.env("GIT_CONFIG_NOSYSTEM", "1");
            live.run_complete_stdout_input(&pinned(git, None).args(["credential", "fill"]), REQUEST)
                .unwrap()
                .success()
        };
        assert!(!ask(crate::checkout::git(&live, &who, None)));
        let open = Cmd::new(crate::items::github::git_program_for_user(&live, &who))
            .as_user(&who)
            .env("GIT_TERMINAL_PROMPT", "0");
        assert!(ask(open), "control: askpass answers where it is left on");
        scratch.close().unwrap();
    }

    /// `gh auth setup-git` writes its section for anyone, so that shape gives way only where
    /// aterm's helper has a token here; left in place, it is still named. A literal block never
    /// asks: the ruling is about it. Beside a gh half with no token, its own section still goes.
    #[test]
    fn a_gh_section_gives_way_only_where_atermss_helper_has_a_token() {
        const GH: &str = "!/opt/homebrew/bin/gh auth git-credential";
        let home = "/Users//alice";
        let setup_git = [
            (GLOBAL, HELPER, Some("")),
            (GLOBAL, HELPER, Some(GH)),
            (GLOBAL, GIST_HELPER, Some("")),
            (GLOBAL, GIST_HELPER, Some(GH)),
        ];
        let every = at(&setup_git);
        assert_eq!(keychain_block(home, &every, true, &|| false), None);
        assert!(keychain_helper_in_effect(&every).is_some());
        let block = keychain_block(home, &every, true, &|| true).expect("the token is here");
        assert_eq!(only(&block).write, [HELPER, GIST_HELPER]);

        let asked = std::cell::Cell::new(false);
        let unasked = || {
            asked.set(true);
            false
        };
        assert!(keychain_block(home, &at(&BLOCK), true, &unasked).is_some());
        assert!(!asked.get(), "a literal block reads no token");

        let mut beside = BLOCK.to_vec();
        beside.extend(&setup_git[2..]);
        let block = keychain_block(home, &at(&beside), true, &|| false).expect("osxkeychain");
        assert_eq!(only(&block).unset, [HELPER, USERNAME, USE_PATH]);
        assert_eq!(only(&block).write, [HELPER], "the gh half stays");

        // The same word in the other file `gh auth setup-git` writes, and each file on its own:
        // the block in one file does not carry a gh section in the other with it.
        let xdg: Vec<_> = setup_git
            .iter()
            .map(|(_, key, value)| (XDG, *key, *value))
            .collect();
        assert_eq!(keychain_block(home, &at(&xdg), true, &|| false), None);
        let block = keychain_block(home, &at(&xdg), true, &|| true).expect("the token is here");
        assert_eq!(only(&block).file, Path::new(XDG));
        let mut apart = xdg;
        apart.extend(BLOCK);
        let block = keychain_block(home, &at(&apart), true, &|| false).expect("osxkeychain");
        assert_eq!(only(&block).file, Path::new(GLOBAL), "the gh file stays");
    }

    /// A shell stand-in for a keychain helper, by its name alone: `sh` runs the file, which
    /// writes each operation it is asked to `ran` in `home` and answers nothing. No keychain is
    /// read; the name makes [`keychain_helper`] count it.
    fn keychain_stand_in(live: &Live, home: &Path) -> String {
        let dir = home.join("stand-in");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("git-credential-osxkeychain");
        let ran = home.join("ran");
        live.write_file(
            &script,
            format!("echo \"$1\" >> '{}'\n", ran.display()).as_bytes(),
            0o600,
        )
        .unwrap();
        format!("!sh {}", script.display())
    }

    /// Whether the stand-in [`keychain_stand_in`] wrote ran since the last call, which clears
    /// its record.
    fn stand_in_ran(home: &Path) -> bool {
        let ran = home.join("ran");
        let held = std::fs::read_to_string(&ran).is_ok_and(|text| !text.is_empty());
        let _ = std::fs::remove_file(&ran);
        held
    }

    /// Run `revert`, a block's journaled commands, with this host's native git and no system
    /// file.
    fn put_back(live: &Live, who: &User, revert: &str) {
        let commands = revert.replace(
            "git config",
            &format!("{} config", crate::items::github::need_native_git()),
        );
        let ran = live
            .run_complete_stdout(
                &Cmd::new("/bin/sh")
                    .as_user(who)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .args(["-c", &commands]),
            )
            .unwrap();
        assert!(ran.success(), "{ran:?}");
    }

    /// A real native git in a scratch HOME (no system file read): aterm's route keeps every helper
    /// a file names from running, a keychain stand-in among them, and asks its own alone, which
    /// answers. The keychain block an earlier `aterm init` wrote on this host to `~/.gitconfig`
    /// ([`script::written_helper`]: `osxkeychain`'s on a Mac, `store`'s elsewhere), beside the
    /// person's own sections, is found from the settings alone and replaced: its three settings
    /// go, github.com and gist.github.com get the reset and aterm's helper, every other section
    /// stays, and the person's own git then asks aterm's helper and not the generic keychain
    /// stand-in an XDG file names (in CLT's system file's place, which is read before it). The
    /// revert puts the block back, from a replacement cut short too. The token git hands back
    /// is compared here, never handed to git: no program, argument, variable or standard input
    /// the route spawned holds it. Negative controls: the stand-in runs for the person's own git
    /// before the replacement; a helper of someone else's under the block's keys keeps the file
    /// as it is (and on a Mac, `osxkeychain` still in effect is reported; `store` is no
    /// keychain); and a run that does hand git the token is caught. The block's literal helper
    /// is only ever read here, never run: no unrouted lookup runs while it is in the file.
    #[test]
    fn the_keychain_block_gives_way_to_atermss_route_and_no_keychain_helper_runs() {
        crate::items::github::need_native_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-keychain-block").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        std::fs::create_dir_all(home.join(".config/git")).unwrap();
        let stand_in = keychain_stand_in(&live, &home);
        live.write_file(
            &home.join(".config/git/config"),
            format!("[credential]\n\thelper = \"{stand_in}\"\n").as_bytes(),
            0o600,
        )
        .unwrap();
        // aterm's stand-in reads its answer from a file: a value in its `-c` would be a value in
        // the run's arguments, which is what `exposed` looks for.
        let answer = home.join("answer");
        live.write_file(
            &answer,
            b"username=alice\npassword=dummy_vault_value\n",
            0o600,
        )
        .unwrap();
        let ours = Helper::stand_in(&format!(
            "!f() {{ test \"$1\" = get || return 0; cat '{}'; }}; f",
            answer.display()
        ));
        let git = Git::isolated();
        let vault = Secret::new(b"dummy_vault_value").unwrap();
        let gitconfig = home.join(".gitconfig");
        let fill = |input: &[u8]| {
            let ran = live
                .run_complete_stdout_input(
                    &git.cmd(Some(&who))
                        .arg("-C")
                        .arg(&home)
                        .args(["credential", "fill"]),
                    input,
                )
                .unwrap();
            ran.success() && ran.out().contains("password=dummy_vault_value")
        };

        // The Command Line Tools' shape: a generic helper alone, no reset in any file to lean
        // on. The pin clears it, for github.com and for any other host, where no helper answers
        // and the lookup ends at the disabled prompt.
        let spawns = Spawns::new(&live);
        assert_eq!(
            git.lookups(&spawns, &who, Some(&ours), &vault).unwrap(),
            Lookups::Ready
        );
        assert!(!stand_in_ran(&home), "the pin left the generic helper");
        let elsewhere = b"protocol=https\nhost=gitlab.example\n\n";
        for route in [Some(&ours), None] {
            let ran = live
                .run_complete_stdout_input(
                    &pinned(git.cmd(Some(&who)).arg("-C").arg(&home), route)
                        .args(["credential", "fill"]),
                    elsewhere,
                )
                .unwrap();
            assert!(!ran.success(), "{route:?}");
            assert!(!stand_in_ran(&home), "{route:?}: the generic helper ran");
        }
        assert!(!fill(elsewhere));
        assert!(stand_in_ran(&home), "control: the person's own git runs it");

        // The route, before anything is replaced: the stand-in named for github.com after a
        // reset, never run.
        live.write_file(
            &gitconfig,
            format!("[credential \"https://github.com\"]\n\thelper =\n\thelper = \"{stand_in}\"\n")
                .as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(git.keychain_in_effect(&live, &who).is_some());
        let spawns = Spawns::new(&live);
        assert_eq!(
            git.lookups(&spawns, &who, Some(&ours), &vault).unwrap(),
            Lookups::Ready
        );
        assert!(!stand_in_ran(&home), "the route ran a keychain stand-in");
        let record = spawns.record();
        assert_eq!(record.len(), 1, "one fill: {record:?}");
        assert_eq!(exposed(&record, &live, "dummy_vault_value"), None);
        let handed = vec![(
            git.cmd(Some(&who))
                .env("GH_TOKEN", "dummy_vault_value")
                .args(["--version"]),
            None,
        )];
        assert_eq!(
            exposed(&handed, &live, "dummy_vault_value"),
            Some("variable GH_TOKEN".into())
        );
        assert!(!fill(b"protocol=https\nhost=github.com\n\n"));
        assert!(stand_in_ran(&home), "control: the person's own git runs it");

        // The block init wrote on this host, beside the person's own sections.
        let written = format!("helper = {}\n", script::written_helper(&live));
        let block = format!(
            "[credential \"https://github.com\"]\n\thelper = \n\t{written}\tusername = alice\n\tuseHttpPath = false\n"
        );
        let theirs = "[user]\n\tname = Alice\n[url \"https://alabsystems@github.com/alabsystems/\"]\n\tpushInsteadOf = https://github.com/alabsystems/\n";
        live.write_file(&gitconfig, format!("{block}{theirs}").as_bytes(), 0o600)
            .unwrap();
        let found = git
            .keychain_block(&live, &who, &|| true)
            .unwrap()
            .expect("the block init wrote is found");
        assert_eq!(
            keys(&found),
            (vec![HELPER, USERNAME, USE_PATH], vec![HELPER, GIST_HELPER])
        );
        let revert = found.revert().to_owned();
        git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
            .unwrap();
        let after = std::fs::read_to_string(&gitconfig).unwrap();
        assert!(
            !after.contains(&written)
                && !after.contains("useHttpPath")
                && !after.contains("username"),
            "{after}"
        );
        for kept in [
            "name = Alice",
            "pushInsteadOf",
            "[credential \"https://gist.github.com\"]",
        ] {
            assert!(after.contains(kept), "{kept}: {after}");
        }
        assert_eq!(git.keychain_block(&live, &who, &|| true).unwrap(), None);
        assert_eq!(git.keychain_in_effect(&live, &who), None);
        for (label, input) in [
            ("host-wide", &b"protocol=https\nhost=github.com\n\n"[..]),
            ("a clone's", b"url=https://github.com/owner/repo.git\n\n"),
        ] {
            assert!(
                fill(input),
                "{label}: aterm's helper answers the person's own git"
            );
            assert!(
                !stand_in_ran(&home),
                "{label}: the generic stand-in never ran"
            );
        }

        // The revert, run by the same git, puts the block back; so it does from a replacement
        // cut short after its first removal.
        put_back(&live, &who, &revert);
        assert_eq!(
            git.keychain_block(&live, &who, &|| true).unwrap().as_ref(),
            Some(&found)
        );
        let cut = live
            .run_complete_stdout(
                &git.cmd(Some(&who))
                    .args(["config", "--file"])
                    .arg(&gitconfig)
                    .args(["--unset-all", HELPER]),
            )
            .unwrap();
        assert!(cut.success(), "{cut:?}");
        put_back(&live, &who, &revert);
        assert_eq!(
            git.keychain_block(&live, &who, &|| true).unwrap().as_ref(),
            Some(&found)
        );

        // Negative control: someone else's helper beside the block's keeps the file as it is.
        let foreign = format!("{block}\thelper = !other-stand-in\n{theirs}");
        live.write_file(&gitconfig, foreign.as_bytes(), 0o600)
            .unwrap();
        assert_eq!(git.keychain_block(&live, &who, &|| true).unwrap(), None);
        assert_eq!(
            git.keychain_in_effect(&live, &who).is_some(),
            live.macos(),
            "on a Mac the keychain helper is still in effect, and reported"
        );
        assert_eq!(std::fs::read_to_string(&gitconfig).unwrap(), foreign);
        scratch.close().unwrap();
    }

    /// `git config --global` writes `~/.config/git/config` where that file is there and
    /// `~/.gitconfig` is not. A real native git in such a scratch HOME (no system file read) writes
    /// the block that way, as an earlier `aterm init` did on this host
    /// ([`script::written_helper`]). It is found in that file and replaced
    /// there, no `~/.gitconfig` appears, the person's own git then asks aterm's helper, and the
    /// revert puts it back. A block in each file goes in one pass and comes back with one revert.
    /// Negative control: the vault's section in that file is no block, and stays byte for byte
    /// beside a block replaced in `~/.gitconfig`. The block's literal helper is only ever read
    /// here, never run: the one unrouted lookup runs with no file naming it.
    #[test]
    fn a_block_in_the_xdg_file_alone_gives_way_in_that_file() {
        crate::items::github::need_native_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-keychain-xdg").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        std::fs::create_dir_all(home.join(".config/git")).unwrap();
        let xdg = home.join(".config/git/config");
        let gitconfig = home.join(".gitconfig");
        let text = |file: &Path| std::fs::read_to_string(file).unwrap();
        live.write_file(&xdg, b"", 0o600).unwrap();
        let answer = home.join("answer");
        live.write_file(
            &answer,
            b"username=alice\npassword=dummy_vault_value\n",
            0o600,
        )
        .unwrap();
        let ours = Helper::stand_in(&format!(
            "!f() {{ test \"$1\" = get || return 0; cat '{}'; }}; f",
            answer.display()
        ));
        let git = Git::isolated();
        let block = || git.keychain_block(&live, &who, &|| true).unwrap();
        let helper = script::written_helper(&live);
        let written = format!("helper = {helper}\n");

        for (key, value) in [
            (HELPER, ""),
            (HELPER, helper),
            (USERNAME, "alice"),
            (USE_PATH, "false"),
        ] {
            let ran = live
                .run_complete_stdout(
                    &git.cmd(Some(&who))
                        .args(["config", "--global", "--add", key, value]),
                )
                .unwrap();
            assert!(ran.success(), "{ran:?}");
        }
        assert!(text(&xdg).contains(&written), "{}", text(&xdg));
        assert!(!gitconfig.exists(), "--global wrote the other file");

        let found = block().expect("the block in the XDG file");
        assert_eq!(only(&found).file, xdg);
        assert_eq!(
            keys(&found),
            (vec![HELPER, USERNAME, USE_PATH], vec![HELPER, GIST_HELPER])
        );
        git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
            .unwrap();
        let after = text(&xdg);
        assert!(
            !after.contains(&written)
                && !after.contains("usehttppath")
                && !after.contains("username")
                && after.contains("[credential \"https://gist.github.com\"]"),
            "{after}"
        );
        assert!(!gitconfig.exists(), "the replacement made another file");
        assert_eq!(block(), None);
        assert_eq!(git.keychain_in_effect(&live, &who), None);
        let ran = live
            .run_complete_stdout_input(
                &git.cmd(Some(&who))
                    .arg("-C")
                    .arg(&home)
                    .args(["credential", "fill"]),
                REQUEST,
            )
            .unwrap();
        assert!(
            ran.success() && ran.out().contains("password=dummy_vault_value"),
            "aterm's helper answers the person's own git"
        );
        put_back(&live, &who, found.revert());
        assert_eq!(block().as_ref(), Some(&found));
        assert!(!gitconfig.exists(), "the revert made another file");

        live.write_file(&gitconfig, text(&xdg).as_bytes(), 0o600)
            .unwrap();
        let both = block().expect("a block in each file");
        assert_eq!(both.replacements.len(), 2, "{both:?}");
        git.replace_keychain_block(&live, &who, &both, &ours, &|| Ok(()))
            .unwrap();
        assert!(!text(&xdg).contains(&written), "{}", text(&xdg));
        assert!(!text(&gitconfig).contains(&written), "{}", text(&gitconfig));
        assert_eq!(block(), None);
        put_back(&live, &who, both.revert());
        assert_eq!(block().as_ref(), Some(&both));

        let keychain = text(&gitconfig);
        let vault = "[credential \"https://github.com\"]\n\thelper =\n\thelper = \"!\\\"/stand-in/agh\\\" auth git-credential\"\n[credential \"https://gist.github.com\"]\n\thelper =\n\thelper = \"!\\\"/stand-in/agh\\\" auth git-credential\"\n";
        live.write_file(&xdg, vault.as_bytes(), 0o600).unwrap();
        std::fs::remove_file(&gitconfig).unwrap();
        assert_eq!(block(), None, "the vault's section is someone else's");
        live.write_file(&gitconfig, keychain.as_bytes(), 0o600)
            .unwrap();
        let beside = block().expect("the block in ~/.gitconfig");
        assert_eq!(only(&beside).file, gitconfig);
        git.replace_keychain_block(&live, &who, &beside, &ours, &|| Ok(()))
            .unwrap();
        assert_eq!(text(&xdg), vault);
        assert!(!text(&gitconfig).contains(&written), "{}", text(&gitconfig));
        scratch.close().unwrap();
    }

    /// A real native git in a scratch HOME whose `~/.gitconfig` holds what `gh auth setup-git`
    /// wrote for an `aterm init` before 2026-10-22 (GitHub CLI's helper for github.com and
    /// gist.github.com, each after the empty reset), beside the person's `[user]`: the settings
    /// read finds both sections as the block and asks no helper (the spawn record holds that
    /// read alone, and the gh the block names does not exist, so a run of it could only fail);
    /// the replacement leaves aterm's helper for both hosts and the person's `[user]`, and
    /// aterm's helper then answers. Negative control: GitHub CLI's helper inside a command of
    /// someone else's is in effect and not the block, and the file stays as it is. Only on a
    /// Mac: no `aterm init` ran gh elsewhere, so off a Mac those sections are a person's own,
    /// as the settings tests above hold.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_block_gh_setup_git_wrote_gives_way_to_atermss_route() {
        let live = Live::new();
        if !live.macos() {
            aterm_uds::host::could_not_run(
                "not a Mac: only an `aterm init` on a Mac ran `gh auth setup-git`, whose block \
                 this replaces; elsewhere its sections are a person's own",
            );
        }
        crate::items::github::need_native_git();
        let mut scratch = Scratch::new(&live, "git-gh-block").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        let gh = home.join("absent/bin/gh");
        let setup_git = format!(
            "[credential \"https://github.com\"]\n\thelper = \n\thelper = !{gh} auth git-credential\n\
             [credential \"https://gist.github.com\"]\n\thelper = \n\thelper = !\\\"{gh}\\\" auth git-credential\n",
            gh = gh.display()
        );
        let theirs = "[user]\n\tname = Alice\n";
        let gitconfig = home.join(".gitconfig");
        live.write_file(&gitconfig, format!("{setup_git}{theirs}").as_bytes(), 0o600)
            .unwrap();
        let git = Git::isolated();
        let ours = Helper::stand_in(
            "!f() { test \"$1\" = get || return 0; echo username=alice; echo password=dummy_vault_value; }; f",
        );
        let vault = Secret::new(b"dummy_vault_value").unwrap();

        let spawns = Spawns::new(&live);
        let found = git
            .keychain_block(&spawns, &who, &|| true)
            .unwrap()
            .expect("the block gh setup-git wrote is found");
        assert_eq!(
            only(&found).unset,
            [HELPER, USERNAME, USE_PATH, GIST_HELPER]
        );
        assert_eq!(only(&found).write, [HELPER, GIST_HELPER]);
        assert_eq!(
            spawns.record().len(),
            1,
            "the settings read alone: no helper ran"
        );
        git.replace_keychain_block(&live, &who, &found, &ours, &|| Ok(()))
            .unwrap();
        let after = std::fs::read_to_string(&gitconfig).unwrap();
        assert!(!after.contains("auth git-credential"), "{after}");
        assert_eq!(after.matches("dummy_vault_value").count(), 2, "{after}");
        assert!(after.contains("name = Alice"), "{after}");
        assert_eq!(git.keychain_in_effect(&live, &who), None);
        assert_eq!(
            git.lookups(&live, &who, Some(&ours), &vault).unwrap(),
            Lookups::Ready
        );

        // Negative control: GitHub CLI's helper in someone else's command.
        let wrapped = format!(
            "[credential \"https://github.com\"]\n\thelper = \n\thelper = \"!f() {{ {gh} auth git-credential \\\"$@\\\"; }}; f\"\n{theirs}",
            gh = gh.display()
        );
        live.write_file(&gitconfig, wrapped.as_bytes(), 0o600)
            .unwrap();
        let spawns = Spawns::new(&live);
        assert_eq!(git.keychain_block(&spawns, &who, &|| true).unwrap(), None);
        assert!(git.keychain_in_effect(&spawns, &who).is_some());
        assert_eq!(
            spawns.record().len(),
            2,
            "two settings reads: no helper ran"
        );
        assert_eq!(std::fs::read_to_string(&gitconfig).unwrap(), wrapped);
        scratch.close().unwrap();
    }

    /// [`scope`] and [`Git::keychain_in_effect`] agree with a real native git (no system file read,
    /// a scratch HOME) about which lookups a `credential.<url>.helper` reaches: a keychain
    /// stand-in under each hard spelling of the table above runs for both a host-wide lookup and
    /// a clone's that names an account and a path (`Every`), for one of them (`Part`), or for
    /// neither (`Never`), and the stand-in is a keychain helper in effect exactly when it runs
    /// for one. A stand-in in a file a conditional include names (on the repository's
    /// directory, or on its remote) runs for a lookup in that repository and is in effect too.
    /// Negative controls: the spellings that name another host, scheme or port, and an include
    /// that holds no credential setting.
    #[test]
    fn the_url_matcher_agrees_with_what_git_runs_for_a_github_lookup() {
        crate::items::github::need_native_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-url-matcher").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        let stand_in = keychain_stand_in(&live, &home);
        let git = Git::isolated();
        let gitconfig = home.join(".gitconfig");
        let repo = home.join("repo");
        let ask = |dir: &Path, input: &[u8]| {
            live.run_complete_stdout_input(
                &git.cmd(Some(&who))
                    .arg("-C")
                    .arg(dir)
                    .args(["credential", "fill"]),
                input,
            )
            .unwrap();
            stand_in_ran(&home)
        };
        for url in [
            "https://github.com",
            "https://GitHub.com",
            "https://*.com",
            "https://github.com:443",
            "https://github.com.",
            "https://%67ithub.com",
            "github.com",
            "",
            "https://bot@github.com",
            "https://github.com/owner",
            "https://github.com./owner",
            "github.com/owner/repo.git",
            "bot@github.com",
            "https://github.com//",
            "http://github.com",
            "https://github.com:8443",
            "https://gist.github.com",
            "https://github.com../owner",
            "github.com.",
            "GITHUB.COM",
            "github.com:443",
            "https://%67ithub.com.",
            "HTTPS://%67ithub.com",
        ] {
            live.write_file(
                &gitconfig,
                format!("[credential \"{url}\"]\n\thelper = \"{stand_in}\"\n").as_bytes(),
                0o600,
            )
            .unwrap();
            let host_wide = ask(&home, b"protocol=https\nhost=github.com\n\n");
            let clone = ask(&home, b"url=https://bot@github.com/owner/repo.git\n\n");
            let git_runs = match (host_wide, clone) {
                (true, true) => Scope::Every,
                (false, false) => Scope::Never,
                _ => Scope::Part,
            };
            assert_eq!(scope(Some(url)), git_runs, "{url:?}");
            // `scope` speaks of github.com; gist.github.com's section is named for its own host.
            assert_eq!(
                git.keychain_in_effect(&live, &who).is_some(),
                git_runs != Scope::Never || url == "https://gist.github.com",
                "{url:?}"
            );
        }

        let made = live
            .run_complete_stdout(&git.cmd(Some(&who)).arg("init").arg("-q").arg(&repo))
            .unwrap();
        assert!(made.success(), "{made:?}");
        let remote = live
            .run_complete_stdout(&git.cmd(Some(&who)).arg("-C").arg(&repo).args([
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ]))
            .unwrap();
        assert!(remote.success(), "{remote:?}");
        let include = home.join("github.inc");
        let helper = format!("[credential]\n\thelper = \"{stand_in}\"\n");
        for (label, condition, included, runs) in [
            (
                "on the repository's directory",
                "[includeIf \"gitdir:~/repo/\"]\n\tpath = ~/github.inc\n",
                helper.as_str(),
                true,
            ),
            (
                "on its remote",
                "[includeIf \"hasconfig:remote.*.url:https://github.com/**\"]\n\tpath = github.inc\n",
                helper.as_str(),
                true,
            ),
            (
                "control: an include holding no credential setting",
                "[includeIf \"gitdir:~/repo/\"]\n\tpath = ~/github.inc\n",
                "[user]\n\temail = bot@example.invalid\n",
                false,
            ),
        ] {
            live.write_file(&gitconfig, condition.as_bytes(), 0o600)
                .unwrap();
            live.write_file(&include, included.as_bytes(), 0o600)
                .unwrap();
            assert!(
                !ask(&home, b"protocol=https\nhost=github.com\n\n"),
                "{label}: outside the repository"
            );
            assert_eq!(
                ask(&repo, b"url=https://github.com/owner/repo.git\n\n"),
                runs,
                "{label}"
            );
            let found = git.keychain_in_effect(&live, &who);
            assert_eq!(found.is_some(), runs, "{label}: {found:?}");
        }
        scratch.close().unwrap();
    }
}
