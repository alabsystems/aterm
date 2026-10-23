// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GitHub credentials through git's own credential protocol, so neither `aterm init` nor the machines controller needs gh (docs/DESIGN-init-system-2026-10-20.md §1.18, §6.1 row 22, S14).

use crate::model::User;
use crate::port::{Cmd, Secret, System};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The helper CLT git ships, and the one every request here names.
pub(crate) const OSXKEYCHAIN: &str = "osxkeychain";

/// The host git's own GitHub lookups name: clones and fetches over HTTPS.
pub(crate) const GITHUB: &str = "github.com";

/// git's per-host settings for [`GITHUB`].
const HELPER: &str = "credential.https://github.com.helper";
const USERNAME: &str = "credential.https://github.com.username";
const USE_PATH: &str = "credential.https://github.com.useHttpPath";

/// The bound on one git run: the keychain answers at once, so a hang is what it catches.
const BUDGET: Duration = Duration::from_secs(60);

/// What [`Git::configured`] reads: every credential setting, and every conditional include's
/// file (`git config --get-regexp` prints keys with their section and name lowercased).
const SETTINGS: &str = r"^(credential\..*|includeif\..*\.path)$";

/// git's bound on nested includes (`MAX_INCLUDE_DEPTH`): a deeper chain is git's own error.
const INCLUDE_DEPTH: usize = 10;

/// What [`Git::configured`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookups {
    /// git's own github.com lookups go to the helper alone, as the account, host-wide.
    Ready,
    /// They do not: why, naming a setting's key and the file it is in, never its value (a helper's command can carry a credential).
    Not(String),
}

impl Lookups {
    /// Whether they do.
    pub(crate) const fn ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// How a credential reaches git: the program, the one helper every request names, and the variables a test adds.
pub(crate) struct Git {
    program: PathBuf,
    helper: String,
    env: Vec<(String, String)>,
}

impl Git {
    /// CLT git and its osxkeychain helper: what `aterm init` and the controller run.
    pub(crate) fn keychain() -> Self {
        Self {
            program: PathBuf::from(crate::items::github::CLT_GIT),
            helper: OSXKEYCHAIN.into(),
            env: Vec::new(),
        }
    }

    /// For a test against a real git: `helper` (git's own `store --file=<scratch>`, which keeps the credential in a file the test reads), and no system configuration read (`GIT_CONFIG_NOSYSTEM=1`), so no keychain helper is configured anywhere the run looks.
    #[cfg(test)]
    pub(crate) fn isolated(helper: &str) -> Self {
        Self {
            program: PathBuf::from(crate::items::github::CLT_GIT),
            helper: helper.into(),
            env: vec![("GIT_CONFIG_NOSYSTEM".into(), "1".into())],
        }
    }

    /// Whether the git this runs is installed.
    pub(crate) fn installed(&self, sys: &dyn System) -> bool {
        sys.stat(&self.program).is_ok_and(|stat| !stat.dir)
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

    /// `git credential <verb>` with only this one's helper configured.
    fn credential(&self, user: Option<&User>, verb: &str) -> Cmd {
        self.cmd(user).args([
            "-c".to_owned(),
            "credential.helper=".to_owned(),
            "-c".to_owned(),
            format!("credential.helper={}", self.helper),
            "credential".to_owned(),
            verb.to_owned(),
        ])
    }

    /// Store `token` for `account` at `host` (`git credential approve`), then read it back ([`Self::fill`]): git reports no helper's failure to store, so the read is the proof.
    /// # Errors
    /// The account or host cannot be a credential field, git could not run whole, or the helper does not hand back exactly this token.
    pub(crate) fn store(
        &self,
        sys: &dyn System,
        user: Option<&User>,
        host: &str,
        account: &str,
        token: &Secret,
    ) -> Result<(), String> {
        let mut input = request(host, account)?;
        input.extend_from_slice(b"password=");
        input.extend_from_slice(token.bytes());
        input.extend_from_slice(b"\n\n");
        let result = sys.run_complete_stdout_input(&self.credential(user, "approve"), &input);
        scrub(&mut input);
        let mut ran = result
            .map_err(|_| "git credential approve did not complete (output withheld)".to_owned())?;
        let stored = ran.success();
        scrub(&mut ran.stdout);
        scrub(&mut ran.stderr);
        if !stored {
            return Err("git credential approve failed (output withheld)".into());
        }
        match self.fill(sys, user, host, account)? {
            Some(back) if back.bytes() == token.bytes() => Ok(()),
            _ => Err(format!(
                "the {} credential helper did not hand back the {host} credential it was given \
                 (output withheld)",
                self.helper
            )),
        }
    }

    /// The token the helper holds for `account` at `host` (`git credential fill`), or `None` when it holds none: git then ends at its disabled prompt (exit 128) without asking.
    /// # Errors
    /// The account or host cannot be a credential field, git could not run whole, or its answer is not one password.
    pub(crate) fn fill(
        &self,
        sys: &dyn System,
        user: Option<&User>,
        host: &str,
        account: &str,
    ) -> Result<Option<Secret>, String> {
        let mut input = request(host, account)?;
        input.push(b'\n');
        let result = sys.run_complete_stdout_input(&self.credential(user, "fill"), &input);
        scrub(&mut input);
        let mut ran = result
            .map_err(|_| "git credential fill did not complete (output withheld)".to_owned())?;
        let answer = if ran.success() {
            let mut passwords = ran
                .stdout
                .split(|byte| *byte == b'\n')
                .filter_map(|line| line.strip_prefix(b"password="));
            match (passwords.next(), passwords.next()) {
                (Some(password), None) => Secret::new(password)
                    .map(Some)
                    .map_err(|_| "the stored GitHub credential is too long".to_owned()),
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

    /// Whether git's own github.com lookups go to this one's helper alone, as `account`, host-wide (`useHttpPath` false, so a clone never asks for a per-repository entry the host-wide token is not filed under), or the setting that keeps them from it.
    /// # Errors
    /// git could not run whole or its answer is not configuration.
    pub(crate) fn configured(
        &self,
        sys: &dyn System,
        user: &User,
        account: &str,
    ) -> Result<Lookups, String> {
        let every = self.settings(sys, user, None)?;
        let mut named = BTreeSet::new();
        let mut pending = Vec::new();
        if let Err(why) = includes(&every, user, 1, &mut named, &mut pending) {
            return Ok(Lookups::Not(why));
        }
        let mut conditional = Vec::new();
        while let Some((file, depth)) = pending.pop() {
            let read = self.settings(sys, user, Some(&file))?;
            if let Err(why) = includes(&read, user, depth + 1, &mut named, &mut pending) {
                return Ok(Lookups::Not(why));
            }
            conditional.extend(read);
        }
        Ok(judge(&self.helper, account, &every, &conditional))
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

    /// Point git's github.com lookups at this one's helper alone, as `account`, host-wide, in the account's global configuration: the empty entry first resets the helpers earlier files name (CLT's own `osxkeychain` among them).
    /// # Errors
    /// A `git config` run failed (output withheld).
    pub(crate) fn configure(
        &self,
        sys: &dyn System,
        user: &User,
        account: &str,
    ) -> Result<(), String> {
        for args in [
            ["config", "--global", "--replace-all", HELPER, ""],
            ["config", "--global", "--add", HELPER, &self.helper],
            ["config", "--global", "--replace-all", USERNAME, account],
            ["config", "--global", "--replace-all", USE_PATH, "false"],
        ] {
            let ran = sys
                .run_complete_stdout(&self.cmd(Some(user)).args(args))
                .map_err(|_| "git's GitHub credential configuration did not complete")?;
            if !ran.success() {
                return Err(
                    "git's GitHub credential configuration failed (output withheld)".into(),
                );
            }
        }
        Ok(())
    }
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
    let rest = match url.split_once("://") {
        Some((scheme, _)) if !scheme.is_empty() && !scheme.eq_ignore_ascii_case("https") => {
            return Scope::Never;
        }
        Some((_, rest)) => rest,
        None => url,
    };
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (user, address) = match authority.rsplit_once('@') {
        Some((user, address)) => (Some(user), address),
        None => (None, authority),
    };
    let host = match address.rsplit_once(':') {
        Some((_, port)) if !port.is_empty() && port.trim_start_matches('0') != "443" => {
            return Scope::Never;
        }
        Some((host, _)) => host,
        None => address,
    };
    let host = percent_decoded(host);
    let labels = GITHUB.split('.');
    if !host.is_empty()
        && (host.split('.').count() != labels.clone().count()
            || !host
                .split('.')
                .zip(labels)
                .all(|(have, want)| have == "*" || have.eq_ignore_ascii_case(want)))
    {
        return Scope::Never;
    }
    if user.is_some() || !path.trim_matches('/').is_empty() {
        Scope::Part
    } else {
        Scope::Every
    }
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

/// Judge what git's github.com lookups resolve to. `every` holds the settings of every file git
/// reads, in its order: those that apply to every lookup are applied as git applies them (an
/// empty helper resets the list; the last account and `useHttpPath` win).
fn judge(helper: &str, account: &str, every: &[Setting], conditional: &[Setting]) -> Lookups {
    let agrees = |name: &str, value: Option<&str>| match (name, value) {
        ("helper", Some(value)) => value == helper,
        ("username", Some(value)) => value.eq_ignore_ascii_case(account),
        ("usehttppath", Some(value)) => boolean(value) == Some(false),
        _ => false,
    };
    let mut helpers: Vec<&Setting> = Vec::new();
    let (mut username, mut use_path) = (None, None);
    let mut strict = Vec::new();
    for setting in every {
        match relevant(setting) {
            None | Some((Scope::Never, _)) => {}
            Some((Scope::Part, name)) => {
                strict.push((setting, name, "for some github.com lookups"))
            }
            Some((Scope::Every, name)) => match (name, setting.value.as_deref()) {
                (_, None) => return Lookups::Not(format!("{} has no value", setting.named())),
                ("helper", Some("")) => helpers.clear(),
                ("helper", Some(_)) => helpers.push(setting),
                ("username", Some(_)) => username = Some(setting),
                _ => use_path = Some(setting),
            },
        }
    }
    for setting in conditional {
        if let Some((scope, name)) = relevant(setting)
            && scope != Scope::Never
        {
            strict.push((setting, name, "in a file a conditional include names"));
        }
    }
    if helpers.is_empty() {
        return Lookups::Not(format!(
            "no credential.helper names {helper} for github.com"
        ));
    }
    if let Some(other) = helpers
        .iter()
        .find(|setting| setting.value.as_deref() != Some(helper))
    {
        return Lookups::Not(format!("{} also runs for github.com", other.named()));
    }
    match username {
        None => {
            return Lookups::Not(format!(
                "no credential.username names {account} for github.com"
            ));
        }
        Some(setting) if !agrees("username", setting.value.as_deref()) => {
            return Lookups::Not(format!("{} names another account", setting.named()));
        }
        Some(_) => {}
    }
    if let Some(setting) = use_path
        && !agrees("usehttppath", setting.value.as_deref())
    {
        return Lookups::Not(format!(
            "{} asks for a credential per repository",
            setting.named()
        ));
    }
    if let Some((setting, _, place)) = strict
        .iter()
        .find(|(setting, name, _)| !agrees(name, setting.value.as_deref()))
    {
        return Lookups::Not(format!("{} disagrees {place}", setting.named()));
    }
    Lookups::Ready
}

/// The fields a credential request names. An account or host that could add a field (a
/// newline, `=`) is refused before git runs.
fn request(host: &str, account: &str) -> Result<Vec<u8>, String> {
    let field = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
    };
    if !field(host) || !field(account) {
        return Err("a GitHub credential names a host and a login".into());
    }
    Ok(format!("protocol=https\nhost={host}\nusername={account}\n").into_bytes())
}

fn scrub(bytes: &mut [u8]) {
    aterm_uds::secret::scrub(bytes);
}

/// The git runs above as the test arm keys them, for the tests of `aterm init`'s and the controller's GitHub steps: each scripts a [`crate::port::Fake`] answer for one of them.
#[cfg(test)]
pub(crate) mod script {
    use super::{Git, HELPER, USE_PATH, USERNAME};
    use crate::model::User;
    use crate::port::{Fake, Ran};

    /// The key of `git credential <verb>`, as `user` or this process.
    pub(crate) fn credential(user: Option<&User>, verb: &str) -> String {
        Git::keychain().credential(user, verb).display()
    }

    /// The key of a `git config` run as `user`.
    pub(crate) fn config(user: &User, args: &[&str]) -> String {
        Git::keychain()
            .cmd(Some(user))
            .args(args.iter().copied())
            .display()
    }

    /// CLT git's own configuration file, which names `osxkeychain` for every host.
    pub(crate) const CLT_CONFIG: &str =
        "/Library/Developer/CommandLineTools/usr/share/git-core/gitconfig";

    /// The settings read [`Git::configured`] makes of every file git reads.
    pub(crate) fn helpers(user: &User) -> String {
        Git::keychain().read(user, None).display()
    }

    /// Its read of `file`, one a conditional include names.
    pub(crate) fn included(user: &User, file: &str) -> String {
        Git::keychain()
            .read(user, Some(std::path::Path::new(file)))
            .display()
    }

    /// `settings` (file, key, value) as that read prints them.
    pub(crate) fn printed(settings: &[(&str, &str, &str)]) -> String {
        settings
            .iter()
            .map(|(file, key, value)| format!("file:{file}\0{key}\n{value}\0"))
            .collect()
    }

    /// The settings of every file git reads once [`Git::configure`] ran for `account`: CLT's
    /// own helper, then what configure wrote to `user`'s global file, as git prints them.
    pub(crate) fn configured_settings(user: &User, account: &str) -> String {
        let global = format!("{}/.gitconfig", user.home);
        let [helper, username, use_path] =
            [HELPER, USERNAME, USE_PATH].map(str::to_ascii_lowercase);
        printed(&[
            (CLT_CONFIG, "credential.helper", "osxkeychain"),
            (&global, &helper, ""),
            (&global, &helper, "osxkeychain"),
            (&global, &username, account),
            (&global, &use_path, "false"),
        ])
    }

    /// git's settings for github.com as [`Git::configure`] leaves them for `account`.
    pub(crate) fn configured(sys: &Fake, user: &User, account: &str) {
        sys.on(&helpers(user), Ran::ok(&configured_settings(user, account)));
    }

    /// git's settings before any GitHub setup: CLT's own `credential.helper` alone, which
    /// names no account.
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

    /// The four writes [`Git::configure`] makes for `account`, each answered `ok`.
    pub(crate) fn configure(sys: &Fake, user: &User, account: &str) -> Vec<String> {
        let keys = [
            ["config", "--global", "--replace-all", HELPER, ""],
            ["config", "--global", "--add", HELPER, "osxkeychain"],
            ["config", "--global", "--replace-all", USERNAME, account],
            ["config", "--global", "--replace-all", USE_PATH, "false"],
        ]
        .map(|args| config(user, &args));
        for key in &keys {
            sys.on(key, Ran::ok(""));
        }
        keys.to_vec()
    }

    /// A `fill` that hands back `token` for `account` at `host`.
    pub(crate) fn holds(sys: &Fake, user: Option<&User>, host: &str, account: &str, token: &str) {
        sys.on(
            &credential(user, "fill"),
            Ran::ok(&format!(
                "protocol=https\nhost={host}\nusername={account}\npassword={token}\n"
            )),
        );
    }

    /// A `fill` the helper has nothing for: git's disabled prompt.
    pub(crate) fn holds_nothing(sys: &Fake, user: Option<&User>) {
        sys.on(
            &credential(user, "fill"),
            Ran::exit(
                128,
                "fatal: could not read Password: terminal prompts disabled",
            ),
        );
    }

    /// An `approve` git accepts.
    pub(crate) fn approves(sys: &Fake, user: Option<&User>) -> String {
        let key = credential(user, "approve");
        sys.on(&key, Ran::ok(""));
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::defaults::tests::user;
    use crate::port::spawns::Spawns;
    use crate::port::{Fake, Live, Ran, Scratch};

    const TOKEN: &str = "fixture_credential_token_only";

    fn alice() -> User {
        user("alice", 501)
    }

    /// git's URL matching as [`scope`] reads it. The real-git test below holds the spellings
    /// the review found to what git itself hands a github.com credential to.
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
            (Some("github.com"), Scope::Every),
            (Some("://github.com"), Scope::Every),
            (Some("https://"), Scope::Every),
            (Some(""), Scope::Every),
            (Some("https://bot@github.com"), Scope::Part),
            (Some("https://bot:pw@github.com"), Scope::Part),
            (Some("https://github.com/owner"), Scope::Part),
            (Some("https://github.com?x"), Scope::Part),
            (Some("github.com/owner/repo.git"), Scope::Part),
            (Some("bot@github.com"), Scope::Part),
            (Some("/owner"), Scope::Part),
            (Some("http://github.com"), Scope::Never),
            (Some("ssh://github.com"), Scope::Never),
            (Some("https://github.com:8443"), Scope::Never),
            (Some("https://github.com:x"), Scope::Never),
            (Some("https://gist.github.com"), Scope::Never),
            (Some("https://github.com.example.invalid"), Scope::Never),
            (Some("https://github.co"), Scope::Never),
            (Some("https://*"), Scope::Never),
            (Some("https://[::1]"), Scope::Never),
            (Some("example.invalid"), Scope::Never),
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

    /// What [`Git::configure`] leaves, as [`judge`] reads it.
    const CONFIGURED: [(&str, Option<&str>); 5] = [
        ("credential.helper", Some("osxkeychain")),
        ("credential.https://github.com.helper", Some("")),
        ("credential.https://github.com.helper", Some(OSXKEYCHAIN)),
        ("credential.https://github.com.username", Some("bot")),
        ("credential.https://github.com.usehttppath", Some("false")),
    ];

    /// Settings applied as git applies them, each reason naming the setting and never its
    /// value.
    #[test]
    fn settings_apply_as_git_applies_them_and_a_reason_names_the_setting() {
        let with = |extra: &[(&str, Option<&str>)], conditional: &[(&str, Option<&str>)]| {
            let mut every = CONFIGURED.to_vec();
            every.extend_from_slice(extra);
            judge(OSXKEYCHAIN, "bot", &listed(&every), &listed(conditional))
        };
        assert_eq!(with(&[], &[]), Lookups::Ready);
        for extra in [
            ("credential.https://example.invalid.helper", Some("!other")),
            ("credential.http://github.com.helper", Some("!other")),
            (
                "credential.https://bot@github.com.helper",
                Some(OSXKEYCHAIN),
            ),
            ("credential.https://github.com/owner.username", Some("BOT")),
            (
                "credential.https://github.com/owner.usehttppath",
                Some("no"),
            ),
            ("credential.interactive", Some("never")),
            ("credential.username", Some("Bot")),
        ] {
            assert_eq!(with(&[extra], &[]), Lookups::Ready, "{extra:?}");
        }
        let mut overridden = listed(&[
            ("credential.helper", Some("!other")),
            ("credential.username", Some("other")),
            ("credential.usehttppath", Some("true")),
        ]);
        overridden.extend(listed(&CONFIGURED));
        assert_eq!(judge(OSXKEYCHAIN, "bot", &overridden, &[]), Lookups::Ready);
        for (extra, reason) in [
            (
                ("credential.helper", Some("!other")),
                "credential.helper in /f also runs for github.com",
            ),
            (
                ("credential.https://GitHub.com.helper", Some("!other")),
                "credential.https://GitHub.com.helper in /f also runs",
            ),
            (
                ("credential.github.com.helper", Some("!other")),
                "credential.github.com.helper in /f also runs",
            ),
            (
                ("credential.https://*.com.helper", Some("!other")),
                "credential.https://*.com.helper in /f also runs",
            ),
            (
                ("credential.https://github.com:443.helper", Some("!other")),
                "credential.https://github.com:443.helper in /f also runs",
            ),
            (
                (
                    "credential.https://bot:pw@github.com.helper",
                    Some("!other"),
                ),
                "credential.https://<user>@github.com.helper in /f disagrees for some",
            ),
            (
                ("credential.github.com/owner.helper", Some("")),
                "credential.github.com/owner.helper in /f disagrees for some",
            ),
            (
                ("credential.https://github.com.helper", Some("")),
                "no credential.helper names osxkeychain",
            ),
            (
                ("credential.username", Some("other")),
                "credential.username in /f names another account",
            ),
            (
                (
                    "credential.https://github.com/owner.username",
                    Some("other"),
                ),
                "disagrees for some github.com lookups",
            ),
            (
                ("credential.usehttppath", Some("true")),
                "credential.usehttppath in /f asks for a credential per repository",
            ),
            (
                ("credential.https://github.com.usehttppath", Some("1")),
                "asks for a credential per repository",
            ),
            (
                ("credential.usehttppath", None),
                "credential.usehttppath in /f has no value",
            ),
        ] {
            let Lookups::Not(why) = with(&[extra], &[]) else {
                panic!("{extra:?} is not ready");
            };
            assert!(why.contains(reason), "{extra:?}: {why}");
            assert!(!why.contains("!other") && !why.contains("pw"), "{why}");
        }
        assert_eq!(
            judge(OSXKEYCHAIN, "bot", &[], &[]),
            Lookups::Not("no credential.helper names osxkeychain for github.com".into())
        );
        for agreeing in [
            ("credential.helper", Some(OSXKEYCHAIN)),
            ("credential.https://example.invalid.helper", Some("!other")),
            ("credential.username", Some("bot")),
        ] {
            assert_eq!(with(&[], &[agreeing]), Lookups::Ready, "{agreeing:?}");
        }
        for disagreeing in [
            ("credential.helper", Some("!other")),
            ("credential.https://github.com.helper", Some("")),
            ("credential.username", Some("other")),
            ("credential.usehttppath", Some("true")),
        ] {
            let Lookups::Not(why) = with(&[], &[disagreeing]) else {
                panic!("{disagreeing:?} is not ready");
            };
            assert!(
                why.contains("in a file a conditional include names"),
                "{why}"
            );
        }
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

    /// The reads [`Git::configured`] makes through the test arm: every file git reads, then each
    /// file a conditional include names.
    #[test]
    fn configured_reads_every_file_then_each_conditional_include() {
        let who = alice();
        let mut every = script::configured_settings(&who, "bot");
        every.push_str(&script::printed(&[(
            "/Users//alice/.gitconfig",
            "includeif.hasconfig:remote.*.url:https://github.com/**.path",
            "~/github.inc",
        )]));
        for (file, ready) in [
            ("credential.helper\nosxkeychain", true),
            ("user.email\nalice@example.invalid", true),
            ("credential.helper\n!other", false),
        ] {
            let sys = Fake::default();
            sys.on(&script::helpers(&who), Ran::ok(&every));
            let inc = "/Users//alice/github.inc";
            sys.on(
                &script::included(&who, inc),
                Ran::ok(&format!("file:{inc}\0{file}\0")),
            );
            let read = Git::keychain().configured(&sys, &who, "bot").unwrap();
            assert_eq!(read.ready(), ready, "{file}: {read:?}");
            assert_eq!(
                sys.calls(),
                [script::helpers(&who), script::included(&who, inc)]
            );
        }
        let sys = Fake::default();
        sys.on(
            &script::helpers(&who),
            Ran::exit(128, "fatal: bad config line"),
        );
        assert!(Git::keychain().configured(&sys, &who, "bot").is_err());
        let sys = Fake::default();
        sys.on(&script::helpers(&who), Ran::exit(1, ""));
        assert_eq!(
            Git::keychain().configured(&sys, &who, "bot").unwrap(),
            Lookups::Not("no credential.helper names osxkeychain for github.com".into())
        );
    }

    #[test]
    fn a_field_that_could_add_another_is_refused_before_git_runs() {
        let sys = Fake::default();
        for (host, account) in [
            ("github.com", "bot\npassword=x"),
            ("github.com", "bot=x"),
            ("github.com\nhost=evil", "bot"),
            ("", "bot"),
        ] {
            assert!(Git::keychain().fill(&sys, None, host, account).is_err());
        }
        assert!(sys.calls().is_empty());
    }

    /// Where `secret` shows in what `live` spawned for the runs of `record`: the program,
    /// argument or variable that holds it, or `None`.
    fn exposed(record: &[(Cmd, Option<Vec<u8>>)], live: &Live, secret: &str) -> Option<String> {
        for (cmd, _) in record {
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
        }
        None
    }

    /// The real CLT git stores and reads back a token through git's own `store` helper in a
    /// scratch HOME (no keychain is configured: `GIT_CONFIG_NOSYSTEM=1`), and the spawn record
    /// shows the token on standard input alone.
    #[test]
    fn the_token_reaches_git_on_stdin_alone_and_is_read_back() {
        crate::items::github::need_clt_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-credential-test").unwrap();
        let file = scratch.path().join("credentials");
        let mut who = user("fixture", live.euid());
        who.home = scratch.path().to_string_lossy().into_owned();
        let sys = Spawns::new(&live);
        let git = Git::isolated(&format!("store --file={}", file.display()));
        let token = Secret::new(TOKEN.as_bytes()).unwrap();
        assert_eq!(
            git.fill(&sys, Some(&who), GITHUB, "bot")
                .unwrap()
                .map(|t| t.bytes().to_vec()),
            None,
            "nothing stored yet: fill ends at the disabled prompt"
        );
        git.store(&sys, Some(&who), GITHUB, "bot", &token).unwrap();
        assert_eq!(
            git.fill(&sys, Some(&who), GITHUB, "bot")
                .unwrap()
                .unwrap()
                .bytes(),
            TOKEN.as_bytes()
        );
        assert!(
            git.fill(&sys, Some(&who), GITHUB, "other")
                .unwrap()
                .is_none()
        );
        let stored = std::fs::read_to_string(&file).unwrap();
        assert!(stored.contains(TOKEN), "the helper received the token");
        let record = sys.record();
        assert!(record.len() >= 4, "{record:?}");
        assert_eq!(exposed(&record, &live, TOKEN), None);
        assert!(
            record.iter().any(|(_, input)| input
                .as_ref()
                .is_some_and(|i| i.windows(TOKEN.len()).any(|w| w == TOKEN.as_bytes()))),
            "the token went to approve's standard input"
        );
        let leaked = vec![(
            git.cmd(Some(&who))
                .env("GH_TOKEN", TOKEN)
                .args(["--version"]),
            None,
        )];
        assert_eq!(
            exposed(&leaked, &live, TOKEN),
            Some("variable GH_TOKEN".into())
        );
        let argv = vec![(
            git.cmd(Some(&who)).args(["-c", &format!("x.y={TOKEN}")]),
            None,
        )];
        assert!(exposed(&argv, &live, TOKEN).is_some());
        scratch.close().unwrap();
    }

    /// The read-back of a real git agrees with what that git does with a github.com
    /// credential.
    #[test]
    fn the_read_back_agrees_with_what_git_does_with_a_github_credential() {
        crate::items::github::need_clt_git();
        let live = Live::new();
        let mut scratch = Scratch::new(&live, "git-helper-test").unwrap();
        let home = scratch.path().to_owned();
        let mut who = user("fixture", live.euid());
        who.home = home.to_string_lossy().into_owned();
        let ours = home.join("credentials");
        let other = home.join("other");
        let git = Git::isolated(&format!("store --file={}", ours.display()));
        let Lookups::Not(why) = git.configured(&live, &who, "bot").unwrap() else {
            panic!("nothing configured yet is not ready");
        };
        assert!(why.starts_with("no credential.helper names store"), "{why}");
        git.configure(&live, &who, "bot").unwrap();
        assert_eq!(git.configured(&live, &who, "bot").unwrap(), Lookups::Ready);
        assert!(
            git.configured(&live, &who, "BOT").unwrap().ready(),
            "logins compare caseless"
        );
        assert!(!git.configured(&live, &who, "other").unwrap().ready());
        assert!(
            !Git::isolated(OSXKEYCHAIN)
                .configured(&live, &who, "bot")
                .unwrap()
                .ready()
        );
        let token = Secret::new(b"dummy_not_a_token").unwrap();
        git.store(&live, Some(&who), GITHUB, "bot", &token).unwrap();
        let stored = std::fs::read(&ours).unwrap();
        let config = home.join(".gitconfig");
        let configured = std::fs::read(&config).unwrap();
        let repo = home.join("repo");
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
        let unpinned = |verb: &str, input: &str| {
            let ran = live
                .run_complete_stdout_input(
                    &git.cmd(Some(&who))
                        .arg("-C")
                        .arg(&repo)
                        .args(["credential", verb]),
                    input.as_bytes(),
                )
                .unwrap();
            (ran.success(), ran.out())
        };
        let git_does = || {
            live.write_file(&ours, &stored, 0o600).unwrap();
            let _ = std::fs::remove_file(&other);
            let (filled, answer) = unpinned("fill", "url=https://github.com/owner/repo.git\n\n");
            let (_, _) = unpinned(
                "approve",
                "protocol=https\nhost=github.com\npath=owner/repo.git\nusername=bot\npassword=dummy_not_a_token\n\n",
            );
            let handed = std::fs::read_to_string(&other)
                .is_ok_and(|text| text.contains("dummy_not_a_token"));
            (
                filled && answer.contains("password=dummy_not_a_token"),
                handed,
            )
        };
        assert_eq!(
            git_does(),
            (true, false),
            "configured: git serves the account alone"
        );
        let foreign = format!("\thelper = store --file={}\n", other.display());
        let section = |name: &str| format!("[credential{name}]\n{foreign}");
        let include = home.join("github.inc");
        for (label, extra, included) in [
            ("a later helper for every host", section(""), None),
            ("another host case", section(" \"https://GitHub.com\""), None),
            ("a partial URL", section(" \"github.com\""), None),
            ("a * host label", section(" \"https://*.com\""), None),
            ("the default port", section(" \"https://github.com:443\""), None),
            ("a user", section(" \"https://bot@github.com\""), None),
            ("a path", section(" \"https://github.com/owner\""), None),
            (
                "a later account",
                "[credential]\n\tusername = other\n".to_owned(),
                None,
            ),
            (
                "a conditional include on the remote",
                "[includeIf \"hasconfig:remote.*.url:https://github.com/**\"]\n\tpath = github.inc\n"
                    .to_owned(),
                Some(format!("[credential]\n{foreign}")),
            ),
            (
                "a conditional include on the directory",
                "[includeIf \"gitdir:~/repo/\"]\n\tpath = ~/github.inc\n".to_owned(),
                Some(format!("[credential]\n{foreign}")),
            ),
            ("control: another scheme", section(" \"http://github.com\""), None),
            ("control: another port", section(" \"https://github.com:8443\""), None),
            ("control: another host", section(" \"https://gist.github.com\""), None),
            (
                "control: an include holding no credential setting",
                "[includeIf \"hasconfig:remote.*.url:https://github.com/**\"]\n\tpath = github.inc\n"
                    .to_owned(),
                Some("[user]\n\temail = bot@example.invalid\n".to_owned()),
            ),
        ] {
            let mut bytes = configured.clone();
            bytes.extend_from_slice(extra.as_bytes());
            live.write_file(&config, &bytes, 0o600).unwrap();
            match &included {
                Some(text) => live.write_file(&include, text.as_bytes(), 0o600).unwrap(),
                None => {
                    let _ = std::fs::remove_file(&include);
                }
            }
            let (filled, handed) = git_does();
            let read = git.configured(&live, &who, "bot").unwrap();
            assert_eq!(
                read.ready(),
                filled && !handed,
                "{label}: git filled {filled}, handed another helper {handed}; read {read:?}"
            );
            assert_eq!(
                label.starts_with("control: "),
                read.ready(),
                "{label}: {read:?}"
            );
        }
        let mut path_scoped = configured.clone();
        path_scoped.extend_from_slice(b"[credential]\n\tuseHttpPath = true\n");
        live.write_file(&config, &path_scoped, 0o600).unwrap();
        assert!(!git.configured(&live, &who, "bot").unwrap().ready());
        live.write_file(&config, &configured, 0o600).unwrap();
        let _ = std::fs::remove_file(&include);
        assert!(git.configured(&live, &who, "bot").unwrap().ready());
        scratch.close().unwrap();
    }

    #[test]
    fn requests_are_pinned_to_the_helper_and_never_prompt() {
        let cmd = Git::keychain().credential(Some(&alice()), "fill");
        assert_eq!(
            cmd.display(),
            "alice$ GIT_TERMINAL_PROMPT=0 GIT_ASKPASS= /Library/Developer/CommandLineTools/usr/bin/git -c credential.helper= -c credential.helper=osxkeychain credential fill"
        );
        let sys = Fake::default();
        sys.on(&cmd.display(), Ran::exit(128, "terminal prompts disabled"));
        let fill = |sys: &Fake| Git::keychain().fill(sys, Some(&alice()), GITHUB, "alice");
        assert!(fill(&sys).unwrap().is_none());
        let sys = Fake::default();
        sys.on(&cmd.display(), Ran::ok("password=one\npassword=two\n"));
        assert!(fill(&sys).is_err(), "two passwords are no answer");
        let sys = Fake::default();
        sys.on(&cmd.display(), Ran::ok("username=alice\npassword=one\n"));
        assert_eq!(fill(&sys).unwrap().unwrap().bytes(), b"one");
    }
}
