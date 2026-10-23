// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-host, per-user GitHub SSH key and host-key configuration ([`System::write_user_file`]).

use super::{Ctx, Item};
use crate::model::{Probe, Read, User};
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::path::Path;

/// The `[github]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Make and configure `~/.ssh/aterm_github_ed25519`.
    pub key: Option<bool>,
    /// `git config --global user.name` (`{user}`, `{name}` fill).
    pub git_name: Option<String>,
    /// `git config --global user.email`.
    pub git_email: Option<String>,
    /// Rewrite `https://github.com/` to SSH in git's global config.
    pub ssh_urls: Option<bool>,
}

/// The key, relative to the user's home.
pub const KEY_FILE: &str = ".ssh/aterm_github_ed25519";

/// The first line of every block this family writes.
pub const BLOCK_BEGIN: &str = "# >>> aterm-machines github";

/// The last line of every block this family writes.
pub const BLOCK_END: &str = "# <<< aterm-machines github";

/// GitHub's published SSH host keys (`gh api meta --jq .ssh_keys`, 2026-10-16).
pub const GITHUB_HOST_KEYS: [&str; 3] = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=",
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=",
];

/// SHA-256 fingerprints documented by GitHub for [`GITHUB_HOST_KEYS`].
pub const GITHUB_FINGERPRINTS: [&str; 3] = [
    "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU",
    "SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM",
    "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s",
];

/// git from the Command Line Tools.
pub const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// For a test that runs the real [`CLT_GIT`], as the code it checks does: COULD NOT RUN
/// on a host without it (a Mac before the Command Line Tools; any other unix, where
/// this crate builds but `aterm` never links it), never a spawn error read as a defect.
#[cfg(test)]
pub(crate) fn need_clt_git() {
    if !Path::new(CLT_GIT).is_file() {
        aterm_uds::host::could_not_run(format!(
            "no {CLT_GIT} here (the Command Line Tools' git, which this code runs)"
        ));
    }
}

/// The item git's settings wait for.
pub const CLT_ITEM: &str = "developer.command-line-tools";

/// `ssh-keygen`, a real binary on every macOS (not a shim).
pub const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";

/// Reads a user's file.
const CAT: &str = "/bin/cat";

/// Makes `~/.ssh`.
const MKDIR: &str = "/bin/mkdir";

/// Removes the key, for the revert line.
const RM: &str = "/bin/rm";

/// The git config key that rewrites `https://github.com/` to SSH.
const SSH_URL_KEY: &str = "url.git@github.com:.insteadOf";

/// The prefix rewritten.
const HTTPS_PREFIX: &str = "https://github.com/";

/// `path` inside `user`'s home.
fn in_home(user: &User, rel: &str) -> String {
    format!("{}/{rel}", user.home.trim_end_matches('/'))
}

/// `user`'s private key.
#[must_use]
pub fn key_path(user: &User) -> String {
    in_home(user, KEY_FILE)
}

/// `user`'s public key.
#[must_use]
pub fn pub_path(user: &User) -> String {
    format!("{}.pub", key_path(user))
}

/// The comment the key carries and the title the controller registers it under.
#[must_use]
pub fn key_title(host: &str, user: &str) -> String {
    format!("aterm-machines:{host}:{user}")
}

/// Extracts the key type and base64 body of a valid OpenSSH public key line.
#[must_use]
pub fn key_material(line: &str) -> Option<(String, String)> {
    let mut words = line.split_whitespace();
    let kind = words.next()?;
    let body = words.next()?;
    let blob = aterm_codec::base64::decode(body).ok()?;
    let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    (blob.get(4..4 + len)? == kind.as_bytes()).then(|| (kind.to_owned(), body.to_owned()))
}

/// `path`'s text, read as `user`: `Absent` when it does not exist.
fn read_as(sys: &dyn System, user: &User, path: &str) -> Read<String> {
    match sys.run_complete_stdout(&Cmd::new(CAT).arg(path).as_user(user)) {
        Ok(ran) if ran.success() => match String::from_utf8(ran.stdout) {
            Ok(text) => Read::Value(text),
            Err(_) => Read::Unusable(format!(
                "reading {path}: file text is not UTF-8; left unchanged"
            )),
        },
        Ok(ran) if ran.err().contains("No such file or directory") => Read::Absent,
        Ok(ran) => Read::Unusable(format!("reading {path}: {}", ran.describe())),
        Err(e) => Read::Unusable(format!("reading {path}: {e}")),
    }
}

/// Make `~/.ssh` (0700) as `user`, when it is missing.
fn ensure_ssh_dir(sys: &dyn System, user: &User) -> Result<(), String> {
    let dir = in_home(user, ".ssh");
    let cmd = Cmd::new(MKDIR)
        .args(["-p", "-m", "700"])
        .arg(&dir)
        .as_user(user);
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(format!("mkdir {dir}: {}", ran.describe())),
        Err(e) => Err(format!("mkdir {dir}: {e}")),
    }
}

/// A file at `path` that is a link.
fn linked(sys: &dyn System, path: &str) -> Option<String> {
    match sys.stat(Path::new(path)) {
        Ok(s) if s.symlink => Some(format!(
            "{path} is a link; this family writes only a plain file (edit its target by hand)"
        )),
        _ => None,
    }
}

/// `~/.ssh/aterm_github_ed25519`, made by `ssh-keygen` as the user when missing.
struct KeyItem {
    user: User,
    host: String,
}

/// What the key's files hold.
enum KeyState {
    /// Both halves, and the public one parses.
    Whole(String),
    /// No private key.
    Missing,
    /// A private key without a usable public half.
    NoPublic,
}

impl KeyItem {
    fn state(&self, sys: &dyn System) -> Result<KeyState, Probe> {
        let key = key_path(&self.user);
        match sys.stat(Path::new(&key)) {
            Ok(s) if s.symlink || s.dir => {
                return Err(Probe::Blocked {
                    why: format!("{key} is not a plain file; refusing to touch it"),
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(KeyState::Missing),
            Err(e) => {
                return Err(Probe::Unusable {
                    why: format!("{key}: {e}"),
                });
            }
        }
        match read_as(sys, &self.user, &pub_path(&self.user)) {
            Read::Value(text) => {
                let line = text.trim();
                Ok(match key_material(line) {
                    Some((kind, _)) if kind == "ssh-ed25519" => KeyState::Whole(line.to_owned()),
                    _ => KeyState::NoPublic,
                })
            }
            Read::Absent => Ok(KeyState::NoPublic),
            Read::Unusable(why) => Err(Probe::Unusable { why }),
        }
    }
}

impl Item for KeyItem {
    fn id(&self) -> String {
        format!("github.key@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        format!("GitHub SSH key for {}", self.user.name)
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.state(sys) {
            Ok(KeyState::Whole(_)) => Probe::Ok {
                now: format!("~/{KEY_FILE}"),
            },
            Ok(KeyState::Missing) => Probe::Change {
                from: "absent".into(),
                to: format!("a new ed25519 key at ~/{KEY_FILE}"),
            },
            Ok(KeyState::NoPublic) => Probe::Change {
                from: "a private key without its .pub".into(),
                to: "its .pub, derived from the private key".into(),
            },
            Err(probe) => probe,
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        ensure_ssh_dir(sys, &self.user)?;
        let key = key_path(&self.user);
        match self.state(sys) {
            Ok(KeyState::Whole(_)) => Ok(()),
            Ok(KeyState::Missing) => {
                let cmd = Cmd::new(SSH_KEYGEN)
                    .args(["-t", "ed25519", "-N", "", "-C"])
                    .arg(key_title(&self.host, &self.user.name))
                    .arg("-f")
                    .arg(&key)
                    .as_user(&self.user);
                match sys.run(&cmd) {
                    Ok(ran) if ran.success() => Ok(()),
                    Ok(ran) => Err(format!("ssh-keygen: {}", ran.describe())),
                    Err(e) => Err(format!("ssh-keygen: {e}")),
                }
            }
            Ok(KeyState::NoPublic) => {
                let cmd = Cmd::new(SSH_KEYGEN)
                    .args(["-y", "-f"])
                    .arg(&key)
                    .as_user(&self.user);
                let line = match sys.run(&cmd) {
                    Ok(ran) if ran.success() => ran.out().trim().to_owned(),
                    Ok(ran) => return Err(format!("ssh-keygen -y: {}", ran.describe())),
                    Err(e) => return Err(format!("ssh-keygen -y: {e}")),
                };
                let Some((kind, body)) = key_material(&line) else {
                    return Err(format!("ssh-keygen -y printed {line:?}, not a public key"));
                };
                let title = key_title(&self.host, &self.user.name);
                let text = format!("{kind} {body} {title}\n");
                sys.write_user_file(
                    &self.user,
                    Path::new(&pub_path(&self.user)),
                    text.as_bytes(),
                    0o644,
                )
                .map_err(|e| format!("{}: {e}", pub_path(&self.user)))
            }
            Err(Probe::Blocked { why } | Probe::Unusable { why }) => Err(why),
            Err(other) => Err(format!("{other:?}")),
        }
    }

    fn revert(&self) -> Option<String> {
        Some(
            Cmd::new(RM)
                .args(["-f", &key_path(&self.user), &pub_path(&self.user)])
                .as_user(&self.user)
                .display(),
        )
    }

    /// The public key line, which the controller registers.
    fn detail(&self, sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        match self.state(sys) {
            Ok(KeyState::Whole(line)) => Some(line),
            _ => None,
        }
    }
}

/// Where a block goes in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// First: `~/.ssh/config` takes the first value it reads for each option.
    Top,
    /// Last: `known_hosts` lines are read in any order.
    Bottom,
}

/// The block in `text`, and the text without it.
fn split_block(text: &str) -> Result<(Option<String>, String), String> {
    let mut lines = text.split_inclusive('\n');
    let mut rest = String::new();
    let mut block: Option<String> = None;
    while let Some(line) = lines.next() {
        if line.trim_end() != BLOCK_BEGIN || block.is_some() {
            rest.push_str(line);
            continue;
        }
        let mut found = line.to_owned();
        let mut closed = false;
        for inner in lines.by_ref() {
            found.push_str(inner);
            if inner.trim_end() == BLOCK_END {
                closed = true;
                break;
            }
        }
        if !closed {
            return Err(format!("`{BLOCK_BEGIN}` has no `{BLOCK_END}` after it"));
        }
        block = Some(found);
    }
    Ok((block, rest))
}

/// `rest` with `block` put back at `place`.
fn join_block(rest: &str, block: &str, place: Place) -> String {
    match place {
        Place::Top => format!("{block}{rest}"),
        Place::Bottom if rest.is_empty() || rest.ends_with('\n') => format!("{rest}{block}"),
        Place::Bottom => format!("{rest}\n{block}"),
    }
}

/// The `~/.ssh/config` block.
#[must_use]
pub fn ssh_config_block() -> String {
    format!(
        "{BLOCK_BEGIN}\n\
         # Written by aterm machines; the lines between these markers are replaced.\n\
         Host github.com\n\
         \x20 HostName github.com\n\
         \x20 User git\n\
         \x20 IdentityFile ~/{KEY_FILE}\n\
         \x20 IdentitiesOnly yes\n\
         Host *\n\
         {BLOCK_END}\n"
    )
}

/// The `~/.ssh/known_hosts` block: GitHub's three host keys.
#[must_use]
pub fn known_hosts_block() -> String {
    let mut out = format!("{BLOCK_BEGIN}\n");
    for key in GITHUB_HOST_KEYS {
        out.push_str("github.com ");
        out.push_str(key);
        out.push('\n');
    }
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

/// A marked block in one of the user's files.
struct BlockItem {
    user: User,
    /// `ssh-config` or `known-hosts`.
    what: &'static str,
    /// Relative to the home.
    rel: &'static str,
    block: String,
    place: Place,
    /// The mode of a file this item creates.
    mode: u32,
}

impl BlockItem {
    fn path(&self) -> String {
        in_home(&self.user, self.rel)
    }

    /// The file's current text (empty when absent), or the probe that stops here.
    fn current(&self, sys: &dyn System) -> Result<(Option<String>, String, bool), Probe> {
        let path = self.path();
        if let Some(why) = linked(sys, &path) {
            return Err(Probe::Blocked { why });
        }
        let (text, exists) = match read_as(sys, &self.user, &path) {
            Read::Value(t) => (t, true),
            Read::Absent => (String::new(), false),
            Read::Unusable(why) => return Err(Probe::Unusable { why }),
        };
        let (block, rest) = split_block(&text).map_err(|e| Probe::Blocked {
            why: format!("{path}: {e}; fix it by hand"),
        })?;
        Ok((block, rest, exists))
    }
}

impl Item for BlockItem {
    fn id(&self) -> String {
        format!("github.{}@{}", self.what, self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        match self.what {
            "ssh-config" => format!("github.com in {}'s ~/{}", self.user.name, self.rel),
            _ => format!("GitHub's host keys in {}'s ~/{}", self.user.name, self.rel),
        }
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.current(sys) {
            Ok((Some(found), _, _)) if found == self.block => Probe::Ok {
                now: "the marked block".into(),
            },
            Ok((Some(_), _, _)) => Probe::Change {
                from: "a different marked block".into(),
                to: "the marked block".into(),
            },
            Ok((None, _, exists)) => Probe::Change {
                from: if exists { "no marked block" } else { "absent" }.into(),
                to: "the marked block".into(),
            },
            Err(probe) => probe,
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        ensure_ssh_dir(sys, &self.user)?;
        let (_, rest, exists) = match self.current(sys) {
            Ok(c) => c,
            Err(Probe::Blocked { why } | Probe::Unusable { why }) => return Err(why),
            Err(other) => return Err(format!("{other:?}")),
        };
        let path = self.path();
        let mode = match sys.stat(Path::new(&path)) {
            Ok(s) if exists && !s.symlink && !s.dir => s.mode & 0o777,
            _ => self.mode,
        };
        let text = join_block(&rest, &self.block, self.place);
        sys.write_user_file(&self.user, Path::new(&path), text.as_bytes(), mode)
            .map_err(|e| format!("{path}: {e}"))
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "delete the lines from `{BLOCK_BEGIN}` to `{BLOCK_END}` in {}",
            self.path()
        ))
    }
}

/// One `git config --global` value.
struct GitItem {
    user: User,
    /// `git-name`, `git-email`, `ssh-urls`.
    what: &'static str,
    key: &'static str,
    value: String,
    /// `--add` to a multi-valued key (`insteadOf`) instead of replacing it.
    add: bool,
}

impl GitItem {
    fn git(&self) -> Cmd {
        Cmd::new(CLT_GIT)
            .args(["config", "--global"])
            .as_user(&self.user)
    }

    fn read(&self, sys: &dyn System) -> Read<Vec<String>> {
        let get = if self.add { "--get-all" } else { "--get" };
        match sys.run(&self.git().args([get, self.key])) {
            Ok(ran) if ran.success() => Read::Value(ran.out().lines().map(str::to_owned).collect()),
            Ok(ran) if ran.status == Some(1) && ran.stdout.is_empty() && ran.stderr.is_empty() => {
                Read::Absent
            }
            Ok(ran) => Read::Unusable(format!("git config {get} {}: {}", self.key, ran.describe())),
            Err(e) => Read::Unusable(format!("git config {get} {}: {e}", self.key)),
        }
    }
}

impl Item for GitItem {
    fn id(&self) -> String {
        format!("github.{}@{}", self.what, self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        format!("git {} for {}", self.key, self.user.name)
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match sys.stat(Path::new(CLT_GIT)) {
            Ok(s) if !s.dir => {}
            _ => {
                return Probe::Waits {
                    on: CLT_ITEM.to_owned(),
                    why: format!(
                        "the Command Line Tools are not installed ({CLT_GIT} is missing), and \
                         /usr/bin/git would ask a person to install them: set [developer] \
                         command_line_tools = true"
                    ),
                };
            }
        }
        match self.read(sys) {
            Read::Value(values)
                if values.contains(&self.value) && (self.add || values.len() == 1) =>
            {
                Probe::Ok {
                    now: self.value.clone(),
                }
            }
            Read::Value(values) => Probe::Change {
                from: values.join(", "),
                to: self.value.clone(),
            },
            Read::Absent => Probe::Change {
                from: "absent".into(),
                to: self.value.clone(),
            },
            Read::Unusable(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = if self.add {
            self.git().args(["--add", self.key]).arg(&self.value)
        } else {
            self.git().arg(self.key).arg(&self.value)
        };
        match sys.run(&cmd) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!("git config {}: {}", self.key, ran.describe())),
            Err(e) => Err(format!("git config {}: {e}", self.key)),
        }
    }

    fn revert(&self) -> Option<String> {
        let unset = if self.add {
            self.git().args(["--unset", self.key]).arg(&self.value)
        } else {
            self.git().args(["--unset", self.key])
        };
        Some(unset.display())
    }

    fn after(&self) -> Vec<String> {
        vec![CLT_ITEM.to_owned()]
    }
}

/// The items `table` asks for, per user in `ctx.users`.
///
/// # Errors
/// One line naming what cannot be expanded.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for user in &ctx.users {
        let fill = |s: &str| {
            crate::baseline::fill_str(
                s,
                &[
                    ("user", user.name.as_str()),
                    ("name", ctx.host_name.as_str()),
                ],
            )
        };
        if table.key == Some(true) {
            items.push(Box::new(KeyItem {
                user: user.clone(),
                host: ctx.host_name.clone(),
            }));
            items.push(Box::new(BlockItem {
                user: user.clone(),
                what: "ssh-config",
                rel: ".ssh/config",
                block: ssh_config_block(),
                place: Place::Top,
                mode: 0o600,
            }));
            items.push(Box::new(BlockItem {
                user: user.clone(),
                what: "known-hosts",
                rel: ".ssh/known_hosts",
                block: known_hosts_block(),
                place: Place::Bottom,
                mode: 0o644,
            }));
        }
        for (what, key, value) in [
            ("git-name", "user.name", &table.git_name),
            ("git-email", "user.email", &table.git_email),
        ] {
            let Some(value) = value else { continue };
            let value = fill(value);
            if value.trim().is_empty() || value.contains('\n') {
                return Err(format!("[github] {key} is {value:?} for {}", user.name));
            }
            items.push(Box::new(GitItem {
                user: user.clone(),
                what,
                key,
                value,
                add: false,
            }));
        }
        if table.ssh_urls == Some(true) {
            items.push(Box::new(GitItem {
                user: user.clone(),
                what: "ssh-urls",
                key: SSH_URL_KEY,
                value: HTTPS_PREFIX.to_owned(),
                add: true,
            }));
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HostFacts;
    use crate::port::{Fake, Ran, Stat};

    fn alice() -> User {
        User {
            name: "alice".into(),
            uid: 502,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        }
    }

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: vec![alice()],
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    fn table() -> Table {
        Table {
            key: Some(true),
            git_name: Some("{user} on {name}".into()),
            git_email: Some("{user}@lab.example".into()),
            ssh_urls: Some(true),
        }
    }

    fn items() -> Vec<Box<dyn Item>> {
        expand(&table(), &ctx()).unwrap()
    }

    fn by_id(id: &str) -> Box<dyn Item> {
        items().into_iter().find(|i| i.id() == id).unwrap()
    }

    const FILE: Stat = Stat {
        uid: 502,
        gid: 20,
        mode: 0o600,
        dir: false,
        symlink: false,
        len: 419,
    };

    /// Public key fixture generated by `ssh-keygen -t ed25519` on the VM.
    const PUB_VM: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines:vm:admin\n";

    /// `/bin/cat` of a missing file, as the VM's admin: exit 1.
    fn missing(path: &str) -> Ran {
        Ran::exit(1, &format!("cat: {path}: No such file or directory\n"))
    }

    #[test]
    fn the_pinned_host_keys_hash_to_the_fingerprints_github_documents() {
        for (key, fingerprint) in GITHUB_HOST_KEYS.iter().zip(GITHUB_FINGERPRINTS) {
            let (_, body) = key_material(key).unwrap();
            let blob = aterm_codec::base64::decode(&body).unwrap();
            let mut h = aterm_digest::Sha256::new();
            h.update(&blob);
            let b64 = aterm_codec::base64::encode(&h.finalize()).unwrap();
            assert_eq!(format!("SHA256:{}", b64.trim_end_matches('=')), fingerprint);
        }
    }

    #[test]
    fn a_public_key_line_parses_only_when_its_body_is_a_key_of_its_type() {
        let (kind, body) = key_material(PUB_VM).unwrap();
        assert_eq!(kind, "ssh-ed25519");
        assert!(body.starts_with("AAAAC3Nza"));
        assert!(key_material("ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIJpF").is_none());
        assert!(key_material("ssh-ed25519 not-base64!").is_none());
        assert!(key_material("").is_none());
    }

    #[test]
    fn the_items_are_per_user_with_stable_ids() {
        let ids: Vec<String> = items().iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "github.key@alice",
                "github.ssh-config@alice",
                "github.known-hosts@alice",
                "github.git-name@alice",
                "github.git-email@alice",
                "github.ssh-urls@alice",
            ]
        );
    }

    #[test]
    fn a_missing_key_is_made_by_ssh_keygen_as_the_user_and_its_line_is_the_detail() {
        let sys = Fake::root();
        let pub_key = "/Users//alice/.ssh/aterm_github_ed25519.pub";
        let item = by_id("github.key@alice");
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "absent".into(),
                to: "a new ed25519 key at ~/.ssh/aterm_github_ed25519".into()
            }
        );
        sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
        let keygen = "alice$ /usr/bin/ssh-keygen -t ed25519 -N '' -C aterm-machines:lab-07:alice \
                      -f /Users//alice/.ssh/aterm_github_ed25519";
        sys.on(
            keygen,
            Ran::ok("Generating public/private ed25519 key pair.\n"),
        );
        item.apply(&sys, &ctx()).unwrap();
        assert!(sys.calls().iter().any(|c| c == keygen), "{:?}", sys.calls());
        sys.stat_of("/Users//alice/.ssh/aterm_github_ed25519", FILE);
        sys.on(&format!("alice$ /bin/cat {pub_key}"), Ran::ok(PUB_VM));
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Ok { .. }));
        assert_eq!(item.detail(&sys, &ctx()).as_deref(), Some(PUB_VM.trim()));
    }

    #[test]
    fn a_private_key_without_its_public_half_gets_the_half_back_and_is_never_regenerated() {
        let sys = Fake::root();
        let pub_key = "/Users//alice/.ssh/aterm_github_ed25519.pub";
        sys.stat_of("/Users//alice/.ssh/aterm_github_ed25519", FILE);
        sys.on(&format!("alice$ /bin/cat {pub_key}"), missing(pub_key));
        sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
        sys.on(
            "alice$ /usr/bin/ssh-keygen -y -f /Users//alice/.ssh/aterm_github_ed25519",
            Ran::ok("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines:vm:admin\n"),
        );
        let item = by_id("github.key@alice");
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Change { .. }));
        assert_eq!(item.detail(&sys, &ctx()), None);
        item.apply(&sys, &ctx()).unwrap();
        let written = String::from_utf8(sys.written(pub_key).unwrap()).unwrap();
        assert_eq!(
            written,
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao \
             aterm-machines:lab-07:alice\n"
        );
        assert!(
            !sys.calls().iter().any(|c| c.contains("-t ed25519")),
            "an existing private key is never replaced: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn the_ssh_config_block_goes_first_and_keeps_the_rest_of_the_file() {
        let sys = Fake::root();
        let path = "/Users//alice/.ssh/config";
        let mine = "AddKeysToAgent yes\nHost lab\n  User alice\n";
        sys.on(&format!("alice$ /bin/cat {path}"), Ran::ok(mine));
        sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
        sys.file(path, mine.as_bytes());
        let item = by_id("github.ssh-config@alice");
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "no marked block".into(),
                to: "the marked block".into()
            }
        );
        item.apply(&sys, &ctx()).unwrap();
        let written = String::from_utf8(sys.written(path).unwrap()).unwrap();
        assert_eq!(written, format!("{}{mine}", ssh_config_block()));
        assert!(
            written.contains("IdentitiesOnly yes\nHost *\n"),
            "{written}"
        );
        assert!(
            sys.calls()
                .iter()
                .any(|c| c == "alice$ write /Users//alice/.ssh/config 644"),
            "the file keeps its mode: {:?}",
            sys.calls()
        );
        let sys2 = Fake::root();
        sys2.on(&format!("alice$ /bin/cat {path}"), Ran::ok(&written));
        assert!(matches!(item.probe(&sys2, &ctx()), Probe::Ok { .. }));
    }

    #[test]
    fn a_stale_block_is_replaced_in_one_piece_and_a_new_file_is_private() {
        let sys = Fake::root();
        let path = "/Users//alice/.ssh/config";
        let stale = format!(
            "Host lab\n  User alice\n{BLOCK_BEGIN}\nHost github.com\n  IdentityFile ~/.ssh/old\n{BLOCK_END}\nHost other\n"
        );
        sys.on(&format!("alice$ /bin/cat {path}"), Ran::ok(&stale));
        sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
        let item = by_id("github.ssh-config@alice");
        assert!(matches!(
            item.probe(&sys, &ctx()),
            Probe::Change { from, .. } if from == "a different marked block"
        ));
        item.apply(&sys, &ctx()).unwrap();
        let written = String::from_utf8(sys.written(path).unwrap()).unwrap();
        assert_eq!(
            written,
            format!("{}Host lab\n  User alice\nHost other\n", ssh_config_block())
        );
        assert!(!written.contains("~/.ssh/old"));
        assert!(
            sys.calls().iter().any(|c| c.ends_with("config 600")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn an_unclosed_block_or_a_linked_file_is_left_for_a_person() {
        let sys = Fake::root();
        let path = "/Users//alice/.ssh/config";
        sys.on(
            &format!("alice$ /bin/cat {path}"),
            Ran::ok(&format!("{BLOCK_BEGIN}\nHost mine\n")),
        );
        let item = by_id("github.ssh-config@alice");
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Blocked { .. }));
        assert!(item.apply(&sys, &ctx()).is_err());
        assert!(sys.written(path).is_none());

        let sys = Fake::root();
        sys.stat_of(
            path,
            Stat {
                symlink: true,
                ..FILE
            },
        );
        assert!(
            matches!(item.probe(&sys, &ctx()), Probe::Blocked { why } if why.contains("is a link"))
        );
    }

    #[test]
    fn known_hosts_gains_githubs_keys_at_the_end() {
        let sys = Fake::root();
        let path = "/Users//alice/.ssh/known_hosts";
        let mine = "lab-07.local ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao";
        sys.on(&format!("alice$ /bin/cat {path}"), Ran::ok(mine));
        sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
        let item = by_id("github.known-hosts@alice");
        item.apply(&sys, &ctx()).unwrap();
        let written = String::from_utf8(sys.written(path).unwrap()).unwrap();
        assert_eq!(written, format!("{mine}\n{}", known_hosts_block()));
        for key in GITHUB_HOST_KEYS {
            assert!(written.contains(&format!("github.com {key}\n")));
        }
        let absent = Fake::root();
        absent.on(&format!("alice$ /bin/cat {path}"), missing(path));
        assert!(matches!(
            item.probe(&absent, &ctx()),
            Probe::Change { from, .. } if from == "absent"
        ));
    }

    #[test]
    fn git_settings_wait_for_the_command_line_tools_and_never_touch_the_shim() {
        let sys = Fake::root();
        for id in [
            "github.git-name@alice",
            "github.git-email@alice",
            "github.ssh-urls@alice",
        ] {
            let item = by_id(id);
            assert!(
                matches!(item.probe(&sys, &ctx()), Probe::Waits { on, .. } if on == CLT_ITEM),
                "{id}"
            );
            assert_eq!(item.after(), ["developer.command-line-tools"]);
        }
        assert!(
            sys.calls().iter().all(|c| !c.contains("/usr/bin/git")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn git_identity_fills_user_and_name_and_reads_back_through_git_config() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, FILE);
        let get = format!("alice$ {CLT_GIT} config --global --get user.name");
        sys.on(&get, Ran::exit(1, ""));
        sys.on(&get, Ran::ok("alice on lab-07\n"));
        let item = by_id("github.git-name@alice");
        assert_eq!(
            item.probe(&sys, &ctx()),
            Probe::Change {
                from: "absent".into(),
                to: "alice on lab-07".into()
            }
        );
        let set = format!("alice$ {CLT_GIT} config --global user.name 'alice on lab-07'");
        sys.on(&set, Ran::ok(""));
        item.apply(&sys, &ctx()).unwrap();
        assert!(sys.calls().contains(&set), "{:?}", sys.calls());
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Ok { .. }));
        assert_eq!(
            item.revert().as_deref(),
            Some(format!("alice$ {CLT_GIT} config --global --unset user.name").as_str())
        );
        let email = by_id("github.git-email@alice");
        sys.on(
            &format!("alice$ {CLT_GIT} config --global --get user.email"),
            Ran::ok("alice@lab.example\n"),
        );
        assert!(
            matches!(email.probe(&sys, &ctx()), Probe::Ok { now } if now == "alice@lab.example")
        );
    }

    #[test]
    fn an_unreadable_git_config_is_unusable_not_absent() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, FILE);
        sys.on(
            &format!("alice$ {CLT_GIT} config --global --get user.name"),
            Ran::exit(
                128,
                "fatal: bad config line 1 in file /Users//alice/.gitconfig",
            ),
        );
        assert!(matches!(
            by_id("github.git-name@alice").probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, FILE);
        sys.on(
            &format!("alice$ {CLT_GIT} config --global --get user.name"),
            Ran::exit(
                1,
                "warning: unable to access '/Users//alice/.gitconfig': Permission denied\n\
                 warning: unable to access '/Users//alice/.gitconfig': Permission denied\n",
            ),
        );
        assert!(matches!(
            by_id("github.git-name@alice").probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
    }

    #[test]
    fn the_ssh_rewrite_is_added_beside_other_values_rather_than_replacing_them() {
        let sys = Fake::root();
        sys.stat_of(CLT_GIT, FILE);
        let get =
            format!("alice$ {CLT_GIT} config --global --get-all url.git@github.com:.insteadOf");
        sys.on(&get, Ran::ok("gh:\n"));
        sys.on(&get, Ran::ok("gh:\nhttps://github.com/\n"));
        let item = by_id("github.ssh-urls@alice");
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Change { .. }));
        let add = format!(
            "alice$ {CLT_GIT} config --global --add url.git@github.com:.insteadOf https://github.com/"
        );
        sys.on(&add, Ran::ok(""));
        item.apply(&sys, &ctx()).unwrap();
        assert!(sys.calls().contains(&add));
        assert!(matches!(item.probe(&sys, &ctx()), Probe::Ok { .. }));
    }

    #[test]
    fn a_key_left_out_of_the_table_makes_no_ssh_items() {
        let only_git = Table {
            git_name: Some("Lab".into()),
            ..Table::default()
        };
        let ids: Vec<String> = expand(&only_git, &ctx())
            .unwrap()
            .iter()
            .map(|i| i.id())
            .collect();
        assert_eq!(ids, ["github.git-name@alice"]);
        let empty = Table {
            git_email: Some("  ".into()),
            ..Table::default()
        };
        assert!(expand(&empty, &ctx()).is_err());
    }
    #[test]
    fn a_managed_block_never_reconstructs_a_file_from_an_incomplete_or_non_utf8_read() {
        let paths = [
            ("github.ssh-config@alice", "/Users//alice/.ssh/config"),
            ("github.known-hosts@alice", "/Users//alice/.ssh/known_hosts"),
        ];
        let apply =
            |id: &str, path: &str, original: &[u8], captured: &[u8], refusal: Option<&str>| {
                let sys = Fake::root();
                sys.file(path, original);
                let cat = format!("alice$ /bin/cat {path}");
                sys.on(
                    &cat,
                    Ran {
                        status: Some(0),
                        stdout: captured.to_vec(),
                        stderr: Vec::new(),
                    },
                );
                if let Some(why) = refusal {
                    sys.refuse_complete_stdout(&cat, why);
                }
                sys.on("alice$ /bin/mkdir -p -m 700 /Users//alice/.ssh", Ran::ok(""));
                let result = by_id(id).apply(&sys, &ctx());
                assert!(
                    sys.calls().iter().any(|call| call == &cat),
                    "actual file-read route reached"
                );
                (result, sys.written(path).unwrap())
            };

        let ordinary = b"# owner sentinel\nHost private\n  User alice\n";
        for (id, path) in paths {
            let (result, written) = apply(id, path, ordinary, ordinary, None);
            assert!(result.is_ok(), "ordinary admitted control: {result:?}");
            assert!(written.windows(ordinary.len()).any(|w| w == ordinary));
            assert!(
                written
                    .windows(BLOCK_BEGIN.len())
                    .any(|w| w == BLOCK_BEGIN.as_bytes())
            );
        }

        let tail = "#\n"
            .repeat(aterm_uds::deadline::OUTPUT_KEEP / 2)
            .into_bytes();
        assert_eq!(tail.len(), aterm_uds::deadline::OUTPUT_KEEP);
        let mut full = b"# irreplaceable prefix sentinel\n".to_vec();
        full.extend_from_slice(&tail);
        let invalid = b"# owner bytes\n\xff\n";
        let mut preserved = Vec::new();
        for (id, path) in paths {
            for (original, captured, refusal) in [
                (
                    full.as_slice(),
                    tail.as_slice(),
                    Some("stdout was truncated before the retained tail"),
                ),
                (invalid.as_slice(), invalid.as_slice(), None),
            ] {
                let (result, written) = apply(id, path, original, captured, refusal);
                preserved.push((result.is_err(), written.as_slice() == original));
            }
        }
        assert_eq!(
            preserved,
            [(true, true); 4],
            "all unknown/invalid reads refuse and retain every original byte"
        );
    }
}
