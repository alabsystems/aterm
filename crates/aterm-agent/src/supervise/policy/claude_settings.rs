// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a Claude Code agent's own settings live, and the two things the
//! supervisor reads from them: the `env` its Bash tool gets
//! ([`WorkerEnv::git_settings_env`]) and what may send a tool call to a person
//! ([`owner_asks`]): its ask rules and its PreToolUse hooks.
//!
//! **The chain** ([`sources`]), in Claude Code's order: the managed file and
//! its `managed-settings.d` drop-ins in [`MANAGED_DIR`]; each `--settings`
//! (inline JSON, or a file relative to the launch directory); the project's
//! `.claude/settings.local.json` in the launch directory and in the
//! repository's main worktree (Claude Code keeps it there for every
//! subdirectory and linked worktree; both are read, a superset); the
//! project's `.claude/settings.json` in the launch directory (both project
//! files under `--project-config-root` instead, where given, a relative root
//! read against the launch directory); and the user's `settings.json`
//! (`cowork_settings.json` under a true `CLAUDE_CODE_USE_COWORK_PLUGINS`) in
//! the agent's Claude Code directory ([`WorkerEnv::claude_dir`]).
//!
//! A missing file sets nothing; an empty or blank one reads as `{}` and a
//! leading byte-order mark is dropped, as in Claude Code ([`parse`]). A file
//! that is there but cannot be read whole or does not parse, a user directory
//! that cannot be named, and every document named from an unknown (relative)
//! launch directory are reported, never skipped, so readers fail closed. A
//! `--setting-sources` that leaves files out is ignored (a superset). Not
//! read: `--managed-settings`, an SDK parent's policy JSON (an interactive
//! session does not take it), an organisation's settings helper or
//! server-delivered settings, a WSL session's Windows chain, and the MDM
//! profile, which [`managed_preferences`] reports unparsed.
//!
//! **Its readers** all take the chain through [`parse`] and keep no list of
//! their own: the approval lane ([`owner_asks`], [`WorkerEnv::git_settings_env`]),
//! the API route and stored login (`crate::harness::netwatch::RouteFacts`,
//! gathered by aterm-gui's `harness_netprobe` from [`sources_under`] and
//! [`managed_preferences`]), and the upgrade's model half
//! (`crate::harness::upgrade_drive`'s `agent_settings`, fenced at the user's
//! home). aterm-primer edits the user's `settings.json` with its own
//! order-keeping parser: a writer, not a reader.
//!
//! **The file-provider fence is the route's.** Whether a document is there is
//! a `stat`, which no consent gates (docs/DESIGN-macos-tcc-prompts-2026-08-30.md
//! §1.5). The route's gather ([`sources_under`] given this process's home)
//! reports, never opens, one that is there inside a file-provider domain of
//! that home (iCloud Drive, a sync provider's folder): it runs on one thread
//! every session shares, which an unanswered consent dialog (it has no
//! timeout) would park for all of them. The approval lane ([`sources`]) opens
//! it: it reads for one session, on that session's thread, only once the agent
//! shows a box, by when the agent (the same consent subject) has read the same
//! documents, so the domain's persisted answer is given; a denial reads as a
//! document that cannot be read, which escalates. Fencing it there would
//! escalate every read-only Bash box in such a folder, and a git read's probe
//! runs `git` in that folder anyway.

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::git_config::WorkerEnv;

/// Claude Code's managed (system-wide) settings directory on this platform:
/// its `managed-settings.json`, and the `managed-settings.d` drop-ins beside
/// it. A WSL session's Windows directory is not read.
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

/// Where macOS keeps managed preference domains: Claude Code reads its
/// `com.anthropic.claudecode` domain there, the account's and the device's
/// ([`managed_preferences`]).
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
/// Claude Code's order (module doc, "The chain"), with this platform's
/// [`MANAGED_DIR`] and every document that is there opened (the approval
/// lane's read).
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
/// `os.userInfo().username`, read here from the worker's `USER`, else
/// `LOGNAME` (the worker is this process's uid). Each is a plist this module
/// neither opens nor parses: one that is there is reported as a document of
/// unknown contents, one that is not is left out. Not part of [`sources`].
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

/// A settings document's text as Claude Code's settings loader reads it: text
/// that trims to nothing is `{}` (JavaScript's `trim`, which takes a
/// byte-order mark as white space), and otherwise one leading byte-order mark
/// is dropped before the JSON is parsed. `Err` (`do not parse`) for anything
/// else that is not JSON. Every reader of a settings document takes its text
/// through it (module doc, "Its readers").
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

/// What one entry of the agent's own settings is ([`OwnerAsk`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// A `permissions.ask` rule naming `tool` ([`rule_tool`]). A `bare` one
    /// (`Write`, `Write()`, `Write(*)`) puts every call of the tool to a
    /// person; one with a specifier (`Bash(sed:*)`, `Edit(src/**)`) the calls
    /// its matcher takes, which is Claude Code's to evaluate, never this
    /// reader's.
    Rule {
        /// The tool, as Claude Code names it (`Write`, `Bash`).
        tool: String,
        /// No specifier: every call of the tool.
        bare: bool,
    },
    /// A `hooks.PreToolUse` entry: a program Claude Code runs before a call
    /// of every tool its matcher can match ([`hook_may_match`]), whose answer
    /// (`ask` among them) is unknown until it runs.
    Hook {
        /// Its `matcher` as written; `None` where it has none (every tool).
        matcher: Option<String>,
    },
}

/// Something in the agent's own settings that may put a tool call to a
/// person ([`owner_asks`]): an ask rule or a PreToolUse hook, where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerAsk {
    /// What it is.
    pub kind: AskKind,
    /// As written: the rule (`Bash(sed:*)`), or the hook's matcher (`*`;
    /// `(none)` for a hook with none), for a reason or a log line.
    pub text: String,
    /// The settings document it is in.
    pub source: String,
}

impl OwnerAsk {
    /// Whether it puts every call of `tool` to a person: a bare ask rule for
    /// it.
    #[must_use]
    pub fn asks_every(&self, tool: &str) -> bool {
        matches!(&self.kind, AskKind::Rule { tool: t, bare: true } if t == tool)
    }

    /// Whether it may put a call of `tool` to a person: an ask rule for it,
    /// with or without a specifier, or a PreToolUse hook whose matcher can
    /// match it ([`hook_may_match`]).
    #[must_use]
    pub fn may_ask(&self, tool: &str) -> bool {
        match &self.kind {
            AskKind::Rule { tool: t, .. } => t == tool,
            AskKind::Hook { matcher } => hook_may_match(matcher.as_deref(), tool),
        }
    }
}

/// Everything along the agent's settings chain ([`sources`]), launched in
/// `launch`, that may put a tool call to a person: every well-formed rule in
/// `permissions.ask` ([`rule_tool`]), and every `hooks.PreToolUse` entry that
/// names a hook (an empty `hooks` list runs nothing). Which of them holds which
/// box is the approval policy's (`settings_ask` in `approval.rs`). `Err` names
/// the first document that cannot be read whole or parsed, or whose
/// `permissions.ask`, `hooks`, `hooks.PreToolUse` or a matcher has the wrong
/// type: what it asks is unknown.
///
/// Read as a superset, fail-closed: a hook's `if` condition, a
/// `disableAllHooks` or `allowManagedHooksOnly`, and a regex matcher leave the
/// hook in. Not read: hooks a plugin, a skill, a subagent's definition or an
/// SDK host brings.
pub fn owner_asks(worker: &WorkerEnv, launch: &Path) -> Result<Vec<OwnerAsk>, String> {
    let mut out = Vec::new();
    for (source, value) in parsed(worker, launch) {
        let unread = |why: &str| format!("its settings ({source}) {why}");
        let Some(value) = value.map_err(|why| unread(&why))? else {
            continue;
        };
        if let Some(asks) = value.get("permissions").and_then(|p| p.get("ask")) {
            let rules = asks
                .as_array()
                .ok_or_else(|| unread("hold a `permissions.ask` that is no list"))?;
            for rule in rules {
                let rule = rule
                    .as_str()
                    .ok_or_else(|| unread("hold a `permissions.ask` entry that is no string"))?;
                if let Some((tool, bare)) = rule_tool(rule) {
                    out.push(OwnerAsk {
                        kind: AskKind::Rule {
                            tool: tool.to_string(),
                            bare,
                        },
                        text: rule.to_string(),
                        source: source.clone(),
                    });
                }
            }
        }
        if let Some(hooks) = value.get("hooks") {
            let events = hooks
                .as_object()
                .ok_or_else(|| unread("hold a `hooks` that is no object"))?;
            let Some(pre) = events.get("PreToolUse") else {
                continue;
            };
            let entries = pre
                .as_array()
                .ok_or_else(|| unread("hold a `hooks.PreToolUse` that is no list"))?;
            for entry in entries {
                let matcher = match entry.get("matcher") {
                    None | Some(Value::Null) => None,
                    Some(m) => Some(
                        m.as_str()
                            .ok_or_else(|| unread("hold a PreToolUse matcher that is no string"))?
                            .to_string(),
                    ),
                };
                if entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
                {
                    continue;
                }
                out.push(OwnerAsk {
                    text: matcher.clone().unwrap_or_else(|| "(none)".to_string()),
                    kind: AskKind::Hook { matcher },
                    source: source.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// The tool a permission rule names, and whether it names it bare, as Claude
/// Code parses a rule: no parentheses is the bare tool (`Write`);
/// `Tool(content)`, the `)` last and none in the name, is the tool, bare where
/// the content is empty or `*` (`Write()`, `Write(*)`) and with a specifier
/// otherwise (`Bash(sed:*)`). `None` for a malformed rule (`Write(x`, `(*)`,
/// ``), which names no tool.
#[must_use]
pub fn rule_tool(rule: &str) -> Option<(&str, bool)> {
    if !rule.contains(['(', ')']) {
        return (!rule.is_empty()).then_some((rule, true));
    }
    let open = rule.find('(')?;
    let tool = &rule[..open];
    let content = rule[open + 1..].strip_suffix(')')?;
    let well_formed = !tool.is_empty() && !tool.contains(')');
    well_formed.then_some((tool, matches!(content, "" | "*")))
}

/// Whether a PreToolUse hook whose matcher is `matcher` (`None`: none) can
/// run for a call of `tool`, as Claude Code matches a hook: none, an empty one
/// and `*` match every tool; one of names alone (letters, digits, `_`, `|`,
/// `,`, space and `-`) matches a tool it names, split at `|` and `,` and each
/// trimmed; any other is a JavaScript regular expression, not evaluated here,
/// so it can match (fail-closed: `mcp__.*` is taken to match `Bash`).
#[must_use]
pub fn hook_may_match(matcher: Option<&str>, tool: &str) -> bool {
    let Some(matcher) = matcher.filter(|m| !m.is_empty() && *m != "*") else {
        return true;
    };
    let names_only = matcher
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '|' | ',' | ' ' | '-'));
    if names_only {
        return matcher
            .split(['|', ','])
            .map(str::trim)
            .any(|name| name == tool);
    }
    true
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

/// Whether the chain opens `path`, the one gate of every open it makes
/// ([`read_settings_in`], [`main_worktree`]): `Ok(false)` where nothing is
/// there (a `stat`); `Err`, saying so, where it is there inside a
/// file-provider domain of the home `fence` names
/// (`footer::under_file_provider_domain`), which is then never opened (module
/// doc, "The file-provider fence"); else `Ok(true)`.
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
/// where one is there but cannot be read whole: not a regular file (a planted
/// FIFO is never opened to block on), larger than [`MAX_SETTINGS_BYTES`],
/// unreadable, or fenced ([`opens`]).
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
