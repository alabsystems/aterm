// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code settings chain reader ([`sources`], [`sources_under`], [`MANAGED_DIR`],
//! [`WorkerEnv::claude_dir`], [`managed_preferences`], [`parse`]) for Bash `env`
//! ([`WorkerEnv::git_settings_env`]) and owner ask rules/hooks ([`owner_asks`]).

use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::git_config::WorkerEnv;

/// Claude Code's managed (system-wide) settings directory on this platform.
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
const DROP_INS: &str = "managed-settings.d";

/// Where macOS keeps managed preference domains.
pub const MANAGED_PREFERENCES_DIR: &str = "/Library/Managed Preferences";

/// The managed preference domain's file name.
const MANAGED_PREFERENCES_FILE: &str = "com.anthropic.claudecode.plist";

/// The most of a settings file this module reads; a larger one is not read.
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// The most drop-ins one managed directory is read for; past it the directory
const MAX_DROP_INS: usize = 64;

/// One document of the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Where it is, for a reason or a log line.
    pub name: String,
    /// Its text, none, or why it cannot be read.
    pub text: Result<Option<String>, String>,
}

/// The agent's settings documents, for an agent launched in `launch`.
#[must_use]
pub fn sources(worker: &WorkerEnv, launch: &Path) -> Vec<Source> {
    sources_under(worker, launch, MANAGED_DIR.map(Path::new), None)
}

/// [`sources`] with the managed documents read from `managed` (`None`: no managed directory).
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

/// Claude Code's managed preference domains for `worker`, in `dir`.
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
fn cowork(worker: &WorkerEnv) -> bool {
    worker
        .var("CLAUDE_CODE_USE_COWORK_PLUGINS")
        .is_some_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "yes" | "on")
        })
}

/// Each document of [`sources`], in order, parsed as Claude Code parses a settings document.
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

/// A settings document's text as Claude Code's settings loader reads it.
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
    /// A `permissions.ask` rule naming `tool` ([`rule_tool`]).
    Rule {
        /// The tool, as Claude Code names it (`Write`, `Bash`).
        tool: String,
        /// No specifier: every call of the tool.
        bare: bool,
    },
    /// A `hooks.PreToolUse` entry.
    Hook {
        /// Its `matcher` as written.
        matcher: Option<String>,
    },
}

/// Something in the agent's own settings that may put a tool call to a person ([`owner_asks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerAsk {
    /// What it is.
    pub kind: AskKind,
    /// As written: the rule (`Bash(sed:*)`).
    pub text: String,
    /// The settings document it came from.
    pub source: String,
}

impl OwnerAsk {
    /// Whether it puts every call of `tool` to a person.
    #[must_use]
    pub fn asks_every(&self, tool: &str) -> bool {
        matches!(&self.kind, AskKind::Rule { tool: t, bare: true } if t == tool)
    }

    /// Whether it may put a call of `tool` to a person.
    #[must_use]
    pub fn may_ask(&self, tool: &str) -> bool {
        match &self.kind {
            AskKind::Rule { tool: t, .. } => t == tool,
            AskKind::Hook { matcher } => hook_may_match(matcher.as_deref(), tool),
        }
    }
}

/// Everything along the agent's settings chain ([`sources`]), launched in `launch`.
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

/// The tool a permission rule names, and whether it names it bare, as Claude Code parses a rule.
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

/// Whether a PreToolUse hook whose matcher is `matcher` (`None`: none) can run for a call.
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
