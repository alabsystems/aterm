// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a Claude Code agent's OWN settings live, and the two things the
//! supervisor reads from them: the `env` its Bash tool gets
//! ([`super::git_config::WorkerEnv::settings_env`]) and its BARE-TOOL ask
//! rules ([`bare_asks`]), which send every call of a tool to a person.
//!
//! **The chain** ([`sources`]), as Claude Code reads it (its bundle's
//! settings-path switch, read 2026-10-09 on 2.1.295 and 2026-10-14 on
//! 2.1.299 and 2.1.300): the managed file and its `managed-settings.d`
//! drop-ins in [`MANAGED_DIR`]; a `--settings` the agent was launched with
//! (inline JSON, or a file relative to where it was launched); the
//! project's `.claude/settings.local.json` in the launch directory AND in
//! the repository's main worktree, where Claude Code keeps it for every
//! subdirectory and linked worktree of a repository the user owns (its
//! `localSettings` "consent store": both are read, a superset);
//! the project's `.claude/settings.json` in the launch directory — both
//! project files under the root a `--project-config-root` names instead,
//! where the agent was launched with one (2.1.300 refuses to start on a
//! relative one; it is read against the launch directory here); and the
//! user's `settings.json` (`cowork_settings.json` under a true
//! `CLAUDE_CODE_USE_COWORK_PLUGINS`) in the agent's Claude Code directory,
//! resolved as Claude Code resolves it ([`WorkerEnv::claude_dir`]). A file
//! that is not there sets nothing, and one that is empty or only white
//! space sets nothing too (Claude Code reads it as `{}`); a leading
//! byte-order mark is dropped before parsing, as Claude Code drops it. One
//! that is there but cannot be read whole or does not parse, and a user
//! directory that cannot be named, is reported, never skipped, so its
//! readers fail closed — and so is every document named from the agent's
//! directory where that directory is not known (a relative `launch`), and,
//! for the reader that fences them ("The file-provider fence" below), one
//! that is there inside a file-provider domain of this process's home,
//! which is never opened (nor is a linked worktree's git file there, read to
//! find its main worktree: that worktree's local file is then reported).
//! Read as a superset:
//! a `--setting-sources` that leaves the project's or the user's files out
//! (they are read all the same). Not read: a `--managed-settings`, the SDK
//! parent's policy JSON (the real-Claude lab, 2026-10-14: an interactive
//! 2.1.300 session launched with one naming a base URL did not take it);
//! an organisation's settings helper, its server-delivered settings, a WSL
//! session's Windows chain, and its MDM profile, which
//! [`managed_preferences`] reports, unparsed, where it is there.
//!
//! **Its readers** — the readers of the `env` block and the ask rules,
//! which read this chain through its one parse ([`parse`]) and no list of
//! their own: the approval lane's ([`bare_asks`], and the Bash tool's `env`
//! through [`WorkerEnv::settings_env`]), and the API route's and the stored
//! login's (`crate::harness::netwatch::RouteFacts`, gathered by aterm-gui's
//! `harness_netprobe` from [`sources_under`] and [`managed_preferences`],
//! which those two take as a document they cannot see), and the upgrade's
//! model half (`crate::harness::upgrade_drive`'s `agent_settings`: the saved
//! `model` and `availableModels`, each from the highest document that sets
//! it, through [`sources_under`] fenced at the user's home; until round
//! eighteen it read a Claude home's `settings.json` alone, so a managed or
//! project `availableModels` or `model` was not its). One other reader of
//! Claude Code's settings is NOT this chain's: aterm-primer edits the
//! user's `settings.json` with an order-keeping parser of its own (a
//! writer, not a reader of what Claude Code takes).
//!
//! **The file-provider fence is the route's** (ruling 2026-10-14, the review
//! of f71bc6de9). A document that is NOT there sets nothing for every reader:
//! whether it is there is a `stat`, which no consent gates
//! (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.5). One that IS there
//! inside a file-provider domain of this process's home (iCloud Drive, a sync
//! provider's folder) is reported, never opened, by the route's and the
//! stored login's gather ([`sources_under`] given that home): one thread
//! every session shares, which an unanswered consent dialog — it has no
//! timeout — would park for all of them. The approval lane ([`sources`])
//! opens it. It reads for one session, on that session's own thread, and
//! only once the agent shows a box: by then the agent, which macOS
//! attributes to this aterm (the same consent subject), has read the same
//! documents itself at its start, so the domain's answer is given (a
//! FileProviderDomain answer persists, allowed or denied: the TCC.db survey,
//! §1.6a there), and a denial reads as a document that cannot be read, which
//! escalates. Fenced there, every read-only Bash box and git read of a
//! project in such a folder escalated wherever a `.claude/settings.local.json`
//! was there, and a git read's own probe runs `git` in that folder anyway.
//! Live (the real-Claude lab, Claude Code 2.1.300, `approve = "safe"`, a
//! project under the instance home's `Library/CloudStorage/<provider>` folder): on
//! f71bc6de9 a read-only `wc -l /etc/hosts` box escalated, "a command whose
//! Bash tool's settings (…/proj/.claude/settings.local.json) lie in a
//! file-provider domain …, never opened", with no `.claude` there at all, and
//! so did one with a quiet local file; on the fix both were pressed
//! (`read-only@classify-v3`), and one whose local file names `BASH_ENV`
//! escalated naming it (read, not unknown).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::git_config::WorkerEnv;

/// Claude Code's managed (system-wide) settings directory on this platform
/// (2.1.300's `tb()`): its `managed-settings.json`, and the
/// `managed-settings.d` drop-ins beside it. On WSL Claude Code can also be
/// told to read the Windows machine's; that is not read here.
#[cfg(target_os = "macos")]
pub const MANAGED_DIR: Option<&str> = Some("/Library/Application Support/ClaudeCode");
/// Claude Code's managed (system-wide) settings directory on this platform.
#[cfg(windows)]
pub const MANAGED_DIR: Option<&str> = Some(r"C:\Program Files\ClaudeCode");
/// Claude Code's managed (system-wide) settings directory on this platform.
#[cfg(all(unix, not(target_os = "macos")))]
pub const MANAGED_DIR: Option<&str> = Some("/etc/claude-code");
/// Claude Code's managed (system-wide) settings directory on this platform.
#[cfg(not(any(unix, windows)))]
pub const MANAGED_DIR: Option<&str> = None;

/// The managed settings file in [`MANAGED_DIR`].
const MANAGED_FILE: &str = "managed-settings.json";

/// The managed settings' drop-in directory beside the managed file: every
/// `*.json` in it is a managed settings document too.
const DROP_INS: &str = "managed-settings.d";

/// Where macOS keeps managed preference domains: Claude Code (2.1.282 on;
/// 2.1.300's `cBs`) reads its `com.anthropic.claudecode` domain there, the
/// account's and the device's ([`managed_preferences`]).
pub const MANAGED_PREFERENCES_DIR: &str = "/Library/Managed Preferences";

/// The managed preference domain's file name.
const MANAGED_PREFERENCES_FILE: &str = "com.anthropic.claudecode.plist";

/// The most of a settings file this module reads; a larger one is not read.
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// The most drop-ins one managed directory is read for; past it the directory
/// is reported unread.
const MAX_DROP_INS: usize = 64;

/// One document of the chain: where it is (`--settings` for an inline one),
/// as a reason names it, and its text — `Ok(None)` where there is none, `Err`
/// naming why where one is there but cannot be read whole (or where it is
/// cannot be told).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Where it is, for a reason or a log line.
    pub name: String,
    /// Its text, none, or why it cannot be read.
    pub text: Result<Option<String>, String>,
}

/// The agent's settings documents, for an agent launched in `launch`, in
/// Claude Code's order (module header, "The chain"): THE CHAIN, with the
/// managed documents of this platform's [`MANAGED_DIR`], every document that
/// is there opened (the approval lane's: module header, "The file-provider
/// fence is the route's").
#[must_use]
pub fn sources(worker: &WorkerEnv, launch: &Path) -> Vec<Source> {
    sources_under(worker, launch, MANAGED_DIR.map(Path::new), None)
}

/// [`sources`] with the managed documents read from `managed` (`None`: no
/// managed directory): Claude Code's own for [`sources`], a scratch tree for
/// a test. Where `fence` names a home (the route's gather passes this
/// process's), a document that is there inside a file-provider domain of it
/// is reported, never opened ([`read_settings_in`]); `None` opens every one.
/// A `launch` that is not absolute is a directory not known: every document
/// named from it is reported as one that cannot be found.
#[must_use]
pub fn sources_under(
    worker: &WorkerEnv,
    launch: &Path,
    managed: Option<&Path>,
    fence: Option<&Path>,
) -> Vec<Source> {
    let mut out = Vec::new();
    let file = |path: PathBuf, out: &mut Vec<Source>| {
        let name = path.display().to_string();
        if !out.iter().any(|s: &Source| s.name == name) {
            out.push(Source {
                text: read_settings_in(fence, &path),
                name,
            });
        }
    };
    let launch = launch.is_absolute().then_some(launch);
    let unplaced = |name: String, out: &mut Vec<Source>| {
        out.push(Source {
            name,
            text: Err("cannot be found: the agent's directory is not known".to_string()),
        });
    };
    // A path as the agent names it: relative ones against where it was
    // launched (`path.resolve`), where that is known.
    let placed = |path: &str| {
        let path = Path::new(path);
        if path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            launch.map(|launch| launch.join(path))
        }
    };
    if let Some(managed) = managed {
        file(managed.join(MANAGED_FILE), &mut out);
        match drop_ins(&managed.join(DROP_INS)) {
            Ok(paths) => paths.into_iter().for_each(|p| file(p, &mut out)),
            Err(why) => out.push(Source {
                name: managed.join(DROP_INS).display().to_string(),
                text: Err(why),
            }),
        }
    }
    let argv = worker.argv();
    for value in flag_values(argv, "--settings") {
        if value.trim_start().starts_with('{') {
            out.push(Source {
                name: "--settings".to_string(),
                text: Ok(Some(value.to_string())),
            });
        } else {
            match placed(value) {
                Some(path) => file(path, &mut out),
                None => unplaced(format!("--settings {value}"), &mut out),
            }
        }
    }
    match flag_values(argv, "--project-config-root").last() {
        Some(root) => match placed(root) {
            Some(root) => {
                let root = super::git_config::normalise(&root);
                file(root.join(".claude/settings.local.json"), &mut out);
                file(root.join(".claude/settings.json"), &mut out);
            }
            None => unplaced(format!("the project's settings under {root}"), &mut out),
        },
        None => match launch {
            Some(launch) => {
                file(launch.join(".claude/settings.local.json"), &mut out);
                match main_worktree(launch, fence) {
                    Ok(Some(root)) => file(root.join(".claude/settings.local.json"), &mut out),
                    Ok(None) => {}
                    Err(why) => out.push(Source {
                        name: "the main worktree's .claude/settings.local.json".to_string(),
                        text: Err(why),
                    }),
                }
                file(launch.join(".claude/settings.json"), &mut out);
            }
            None => unplaced("the project's settings".to_string(), &mut out),
        },
    }
    let user = if cowork(worker) {
        "cowork_settings.json"
    } else {
        "settings.json"
    };
    match worker.claude_dir(launch.unwrap_or(Path::new(""))) {
        Ok(dir) if dir.is_absolute() => file(dir.join(user), &mut out),
        Ok(_) => unplaced(format!("the user's {user}"), &mut out),
        Err(why) => out.push(Source {
            name: format!("the user's {user}"),
            text: Err(format!("cannot be found: {why}")),
        }),
    }
    out
}

/// Claude Code's managed preference domains for `worker`, in `dir`
/// ([`MANAGED_PREFERENCES_DIR`]; a scratch tree for a test): the account's,
/// under its name, and the device's. Claude Code names the account by
/// `os.userInfo().username`; it is read here from the worker's `USER`, else
/// its `LOGNAME` (the worker is this process's uid). Each is a plist this
/// module does not parse, and none is opened: one that is there is reported
/// as a document whose contents are unknown, one that is not is left out.
/// Not part of [`sources`] (module header, "Its readers").
#[must_use]
pub fn managed_preferences(worker: &WorkerEnv, dir: &Path) -> Vec<Source> {
    let account = ["USER", "LOGNAME"]
        .iter()
        .find_map(|key| worker.var(key).filter(|name| !name.is_empty()));
    account
        .map(|name| dir.join(name).join(MANAGED_PREFERENCES_FILE))
        .into_iter()
        .chain([dir.join(MANAGED_PREFERENCES_FILE)])
        .filter(|domain| std::fs::symlink_metadata(domain).is_ok())
        .map(|domain| Source {
            name: domain.display().to_string(),
            text: Err("are a managed preference domain this reader does not parse".to_string()),
        })
        .collect()
}

/// Each value `argv` gives `flag`, as `flag value` or `flag=value`, in order.
fn flag_values<'a>(argv: &'a [String], flag: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    argv.iter()
        .enumerate()
        .filter_map(move |(i, arg)| match arg.strip_prefix(flag)? {
            "" => argv.get(i + 1).map(String::as_str),
            rest => rest.strip_prefix('='),
        })
}

/// Whether Claude Code reads the user's `cowork_settings.json` in place of
/// its `settings.json`: the agent's `CLAUDE_CODE_USE_COWORK_PLUGINS` true as
/// Claude Code reads a flag variable (`1`, `true`, `yes` or `on`, in any
/// case, white space trimmed). Its `--cowork` sets the same only for a
/// `claude plugin` command, never for a session.
fn cowork(worker: &WorkerEnv) -> bool {
    worker
        .var("CLAUDE_CODE_USE_COWORK_PLUGINS")
        .is_some_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
}

/// Each document of [`sources`], in order, parsed as Claude Code parses a
/// settings document ([`parse`]): `Ok(None)` where there is none, `Err`
/// saying why where one is there but cannot be read whole or does not parse
/// (`do not parse`, read after the document's name).
pub(crate) fn parsed(
    worker: &WorkerEnv,
    launch: &Path,
) -> Vec<(String, Result<Option<Value>, String>)> {
    sources(worker, launch)
        .into_iter()
        .map(|s| {
            let value = s
                .text
                .and_then(|text| text.as_deref().map(parse).transpose());
            (s.name, value)
        })
        .collect()
}

/// A settings document's text as Claude Code's settings loader reads it
/// (2.1.295 and 2.1.299): text that trims to nothing is `{}` (JavaScript's
/// `trim`, which takes a byte-order mark as white space), and otherwise one
/// leading byte-order mark is dropped before the JSON is parsed. `Err` (`do
/// not parse`) for anything else that is not JSON. Every READER of a
/// settings document takes its text through it: the chain's (module header,
/// "Its readers"), the route's (`crate::harness::netwatch::settings_of`) and
/// the upgrade's model half
/// (`crate::harness::upgrade_models::parse_user_settings`); aterm-primer's
/// editor keeps its own.
pub fn parse(text: &str) -> Result<Value, String> {
    if text
        .trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .is_empty()
    {
        return Ok(Value::Object(aterm_json::Map::new()));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    aterm_json::from_str::<Value>(text).map_err(|_| "do not parse".to_string())
}

/// A bare-tool ask rule of the agent's own settings: every call of `tool` is
/// put to a person, whatever its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskRule {
    /// The tool, as Claude Code names it (`Write`, `Edit`, `Read`).
    pub tool: String,
    /// The rule as written (`Write`, `Write(*)`).
    pub rule: String,
    /// The settings document it is in.
    pub source: String,
}

/// Every bare-tool rule in `permissions.ask` along the agent's settings chain
/// ([`sources`]), launched in `launch` ([`bare_tool`]). `Err` names the first
/// document that is there but cannot be read whole or does not parse, or
/// whose `permissions.ask` is not a list of strings: what it asks is unknown.
/// A rule with a specifier (`Write(src/**)`) is not returned: its matcher is
/// Claude Code's, and no reader here evaluates it.
pub fn bare_asks(worker: &WorkerEnv, launch: &Path) -> Result<Vec<AskRule>, String> {
    let mut out = Vec::new();
    for (source, value) in parsed(worker, launch) {
        let unread = |why: &str| format!("its settings ({source}) {why}");
        let Some(value) = value.map_err(|why| unread(&why))? else {
            continue;
        };
        let Some(asks) = value.get("permissions").and_then(|p| p.get("ask")) else {
            continue;
        };
        let rules = asks
            .as_array()
            .ok_or_else(|| unread("hold a `permissions.ask` that is no list"))?;
        for rule in rules {
            let rule = rule
                .as_str()
                .ok_or_else(|| unread("hold a `permissions.ask` entry that is no string"))?;
            if let Some(tool) = bare_tool(rule) {
                out.push(AskRule {
                    tool: tool.to_string(),
                    rule: rule.to_string(),
                    source: source.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// The tool a permission rule names when it names one with NO specifier, as
/// Claude Code 2.1.295 parses a rule (its bundle's rule parser, read
/// 2026-10-09): `Write`, and `Write()` and `Write(*)`, which it reads as the
/// bare tool. `None` for a rule with a specifier (`Edit(src/**)`) and for a
/// malformed one (`Write(x`, which names no tool).
#[must_use]
pub fn bare_tool(rule: &str) -> Option<&str> {
    if !rule.contains(['(', ')']) {
        return (!rule.is_empty()).then_some(rule);
    }
    let open = rule.find('(')?;
    let tool = &rule[..open];
    let content = rule[open + 1..].strip_suffix(')')?;
    let well_formed = !tool.is_empty() && !tool.contains(')');
    (well_formed && matches!(content, "" | "*")).then_some(tool)
}

/// The main worktree of the repository `dir` is in: the directory holding its
/// `.git` directory, or for a linked worktree the one its `.git` file's
/// `gitdir:` and that git directory's `commondir` lead to. `Ok(None)` outside
/// a repository, and where the shape is none of those (a submodule's, a bare
/// repository's). Each git file it opens is opened as a settings document is
/// ([`opens`]): where `fence` names a home and one is there inside a
/// file-provider domain of it (a git directory can lie in one from outside
/// one), it is never opened, and where the main worktree is is `Err`.
fn main_worktree(dir: &Path, fence: Option<&Path>) -> Result<Option<PathBuf>, String> {
    let small = |path: &Path| -> Result<Option<String>, String> {
        Ok(match opens(fence, path) {
            Ok(true) => crate::harness::footer::read_small(path, 4096),
            Ok(false) => None,
            Err(why) => return Err(format!("cannot be found: {} lies in {why}", path.display())),
        })
    };
    let Some(top) = dir.ancestors().find(|a| a.join(".git").exists()) else {
        return Ok(None);
    };
    let dot_git = top.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(top.to_path_buf()));
    }
    let Some(text) = small(&dot_git)? else {
        return Ok(None);
    };
    let Some(gitdir) = text.lines().find_map(|l| l.strip_prefix("gitdir:")) else {
        return Ok(None);
    };
    let gitdir = top.join(gitdir.trim());
    let common =
        small(&gitdir.join("commondir"))?.map_or_else(|| gitdir.clone(), |c| gitdir.join(c.trim()));
    let common = super::git_config::normalise(&common);
    if common.file_name() != Some(std::ffi::OsStr::new(".git")) {
        return Ok(None);
    }
    Ok(common.parent().map(Path::to_path_buf))
}

/// The `*.json` files of a managed drop-in directory, sorted: none where the
/// directory is not there, `Err` where it is there but cannot be listed.
fn drop_ins(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot be listed ({e})")),
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    if out.len() > MAX_DROP_INS {
        return Err(format!("hold more than {MAX_DROP_INS} documents"));
    }
    out.sort();
    Ok(out)
}

/// Whether the chain opens `path`, the one gate of every open it makes (a
/// settings document, [`read_settings_in`]; a git file, [`main_worktree`]):
/// `Ok(false)` where nothing is there, asked by a `stat`, which no consent
/// gates (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.5); `Err`, saying
/// so, where it is there inside a file-provider domain of the home `fence`
/// names (`footer::under_file_provider_domain`: atpkg's lexical predicate fed
/// the resolved paths), which is then never opened — the open can raise a
/// system consent dialog that has no timeout, on whichever thread reads it
/// (module header, "The file-provider fence is the route's"); else `Ok(true)`.
fn opens(fence: Option<&Path>, path: &Path) -> Result<bool, &'static str> {
    if let Err(e) = std::fs::symlink_metadata(path)
        && e.kind() == std::io::ErrorKind::NotFound
    {
        return Ok(false);
    }
    if crate::harness::footer::under_file_provider_domain(fence, path) {
        return Err(
            "a file-provider domain (iCloud Drive, a sync provider's folder), never opened",
        );
    }
    Ok(true)
}

/// A settings file's text: `Ok(None)` where there is none, `Err` naming why
/// where one is there but cannot be read whole — not a regular file (a FIFO
/// planted there is never opened to block on), larger than
/// [`MAX_SETTINGS_BYTES`], or unreadable — and, where `fence` names a home,
/// where it is there inside a file-provider domain of that home, which is
/// then never opened ([`opens`]).
fn read_settings_in(fence: Option<&Path>, path: &Path) -> Result<Option<String>, String> {
    if !opens(fence, path).map_err(|why| format!("lie in {why}"))? {
        return Ok(None);
    }
    let text = crate::harness::footer::read_small(path, MAX_SETTINGS_BYTES + 1)
        .ok_or("cannot be read as a regular file")?;
    if text.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(format!("are larger than {MAX_SETTINGS_BYTES} bytes"));
    }
    Ok(Some(text))
}

#[cfg(test)]
#[path = "claude_settings_tests.rs"]
mod tests;
