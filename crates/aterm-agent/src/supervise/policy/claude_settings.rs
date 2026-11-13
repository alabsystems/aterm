// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code settings chain reader ([`sources`], [`sources_under`], [`MANAGED_DIR`],
//! [`WorkerEnv::claude_dir`], [`managed_preferences`], [`parse`]) for Bash `env`
//! ([`WorkerEnv::git_settings_env`]), owner ask rules/hooks ([`owner_asks`]) and the default
//! permission mode a line with no permission flag lands in ([`permission_default`]).

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
/// `*.json` in it is a managed settings document too.
const DROP_INS: &str = "managed-settings.d";

/// Where macOS keeps managed preference domains.
pub const MANAGED_PREFERENCES_DIR: &str = "/Library/Managed Preferences";

/// The managed preference domain's file name.
const MANAGED_PREFERENCES_FILE: &str = "com.anthropic.claudecode.plist";

/// The most of a settings file this module reads; a larger one is not read.
const MAX_SETTINGS_BYTES: u64 = 1 << 20;

/// The most drop-ins one managed directory is read for; past it the directory
/// is reported unread.
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
        out.extend(managed_sources(managed, fence));
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

/// The managed documents of the folder `managed`: its [`MANAGED_FILE`], then each drop-in of its
/// [`DROP_INS`] (a drop-in folder that cannot be listed is a source that cannot be read).
fn managed_sources(managed: &Path, fence: Option<&Path>) -> Vec<Source> {
    let file = |path: PathBuf| Source {
        text: read_settings_in(fence, &path),
        name: path.display().to_string(),
    };
    let mut out = vec![file(managed.join(MANAGED_FILE))];
    match drop_ins(&managed.join(DROP_INS)) {
        Ok(paths) => out.extend(paths.into_iter().map(file)),
        Err(why) => out.push(Source {
            name: managed.join(DROP_INS).display().to_string(),
            text: Err(why),
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

/// Whether `argv` sets `flag`, alone or as `flag=value`, before a `--`.
fn flag_set(argv: &[String], flag: &str) -> bool {
    argv.iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == flag || arg.strip_prefix(flag).is_some_and(|r| r.starts_with('=')))
}

/// Each value `argv` gives `flag`, as `flag value` or `flag=value`, in order.
fn flag_values<'a>(argv: &'a [String], flag: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let mut args = argv.iter().map(String::as_str);
    std::iter::from_fn(move || {
        while let Some(arg) = args.next() {
            if arg == "--" {
                return None;
            }
            if arg == flag {
                return args.next();
            }
            if let Some(val) = arg.strip_prefix(flag).and_then(|r| r.strip_prefix('=')) {
                return Some(val);
            }
        }
        None
    })
}

/// Whether Claude Code reads the user's `cowork_settings.json` in place of
/// its `settings.json`: the agent's `CLAUDE_CODE_USE_COWORK_PLUGINS` true as
/// Claude Code reads a flag variable (`1`, `true`, `yes` or `on`, in any
/// case, white space trimmed).
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

/// The server-managed settings Claude Code keeps in its configuration directory (2.1.316:
/// `remote-settings.json` beside the user's `settings.json`), which may pin the sign-in method.
const REMOTE_SETTINGS: &str = "remote-settings.json";

/// The variable that names another managed settings folder, and the one that names another
/// server-managed copy, in Claude Code's own environment list. Claude Code 2.1.316's public build
/// compiles both overrides out (its folder's getter and its copy's answer nothing), so it reads
/// [`MANAGED_DIR`] and [`REMOTE_SETTINGS`]; the pin reader reads what they name as well, on the
/// safe side (the post-landing review, 2026-11-02): a build that honours them pins there too.
const PIN_PATH_VARS: [(&str, bool); 2] = [
    ("CLAUDE_CODE_MANAGED_SETTINGS_PATH", true),
    ("CLAUDE_CODE_REMOTE_SETTINGS_PATH", false),
];

/// The flags that start, or attach to, a cloud session (2.1.316: `--cloud`, its deprecated
/// alias `--remote`, `--teleport`, `--attach-serve`): its `/login` is claude.ai's alone and opens
/// the browser at once, whatever the settings say.
const CLOUD_FLAGS: [&str; 4] = ["--cloud", "--remote", "--teleport", "--attach-serve"];

/// The keys that name a policy helper: an executable that computes managed settings at startup,
/// whose output may pin the sign-in method (2.1.316: `forceLoginMethod` from its output, beside
/// the static documents'). The pin reader runs no executable, so a document that names one is
/// read as [`super::login::LoginPin::Unread`] unless a static document pins a method.
const POLICY_HELPER_KEYS: [&str; 2] = ["policyHelper", "policyHelpers"];

/// WHAT THE AGENT'S SETTINGS MAKE OF `/login` ([`super::login::LoginPin`]): every document of the
/// chain [`sources_under`] reads, its managed preference domains ([`managed_preferences`]) and its
/// server-managed copy ([`REMOTE_SETTINGS`], in the configuration directory
/// [`WorkerEnv::claude_dir`] names, `CLAUDE_CONFIG_DIR` included), and what the agent's
/// environment names in their place ([`PIN_PATH_VARS`]), read by [`login_pin`]; an agent started
/// on a cloud session ([`CLOUD_FLAGS`]) is [`super::login::LoginPin::Method`] whatever they say.
#[must_use]
pub fn login_pin_under(
    worker: &WorkerEnv,
    launch: &Path,
    managed: Option<&Path>,
    prefs: &Path,
    fence: Option<&Path>,
) -> super::login::LoginPin {
    if CLOUD_FLAGS.iter().any(|flag| flag_set(worker.argv(), flag)) {
        return super::login::LoginPin::Method;
    }
    let mut all = sources_under(worker, launch, managed, fence);
    all.extend(managed_preferences(worker, prefs));
    let remote = match worker.claude_dir(launch) {
        Ok(dir) if dir.is_absolute() => {
            let path = dir.join(REMOTE_SETTINGS);
            Source {
                name: path.display().to_string(),
                text: read_settings_in(fence, &path),
            }
        }
        Ok(_) | Err(_) => Source {
            name: format!("the user's {REMOTE_SETTINGS}"),
            text: Err("cannot be found".to_string()),
        },
    };
    all.push(remote);
    for (var, folder) in PIN_PATH_VARS {
        let Some(named) = worker.var(var).filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        let path = Path::new(named);
        if !path.is_absolute() {
            all.push(Source {
                name: format!("{var}={named}"),
                text: Err("is not an absolute path".to_string()),
            });
        } else if folder {
            all.extend(managed_sources(path, fence));
        } else {
            all.push(Source {
                name: path.display().to_string(),
                text: read_settings_in(fence, path),
            });
        }
    }
    login_pin(&all)
}

/// THE PIN `sources` PUT ON `/login` ([`super::login::LoginPin`]), on the safe side: a
/// `forceLoginMethod` that names a method anywhere in any document (`claudeai`, `console`, or a
/// word this build does not know) is [`super::login::LoginPin::Method`], whatever source wins in
/// Claude Code's own merge; else a gateway anywhere (`forceLoginGatewayUrl` set, or
/// `forceLoginMethod` `gateway`) is [`super::login::LoginPin::Gateway`]; else a source that is
/// there and cannot be read or parsed (a managed preference domain, which this reader does not
/// parse, included), or that names a policy helper whose output it cannot read
/// ([`POLICY_HELPER_KEYS`]), is [`super::login::LoginPin::Unread`]. Each document is read whole, nested
/// objects too, since the server-managed copy's shape is Claude Code's own. A missing document
/// pins nothing. The cost of the safe side is one `/login` the person types where aterm could
/// have; never a browser brought forward by aterm's key.
#[must_use]
pub fn login_pin(sources: &[Source]) -> super::login::LoginPin {
    use super::login::LoginPin;
    let (mut gateway, mut unread) = (false, false);
    for source in sources {
        let value = match &source.text {
            Ok(None) => continue,
            Ok(Some(text)) => match parse(text) {
                Ok(value) => value,
                Err(_) => {
                    unread = true;
                    continue;
                }
            },
            Err(_) => {
                unread = true;
                continue;
            }
        };
        match pin_in(&value) {
            LoginPin::Method => return LoginPin::Method,
            LoginPin::Gateway => gateway = true,
            LoginPin::Picker | LoginPin::Unread | LoginPin::Token => {}
        }
        if let Value::Object(map) = &value
            && POLICY_HELPER_KEYS
                .iter()
                .any(|key| map.get(*key).is_some_and(|v| !v.is_null()))
        {
            unread = true;
        }
    }
    if gateway {
        LoginPin::Gateway
    } else if unread {
        LoginPin::Unread
    } else {
        LoginPin::Picker
    }
}

/// The pin one parsed document holds ([`login_pin`]), its nested objects read too.
fn pin_in(value: &Value) -> super::login::LoginPin {
    use super::login::LoginPin;
    match value {
        Value::Object(map) => {
            let mut found = LoginPin::Picker;
            match map.get("forceLoginMethod") {
                Some(Value::String(word)) if word.trim() == "gateway" => {
                    found = LoginPin::Gateway;
                }
                Some(Value::String(word)) if !word.trim().is_empty() => return LoginPin::Method,
                _ => {}
            }
            if map
                .get("forceLoginGatewayUrl")
                .is_some_and(|url| !url.is_null())
            {
                found = LoginPin::Gateway;
            }
            for nested in map.values() {
                match pin_in(nested) {
                    LoginPin::Method => return LoginPin::Method,
                    LoginPin::Gateway => found = LoginPin::Gateway,
                    LoginPin::Picker | LoginPin::Unread | LoginPin::Token => {}
                }
            }
            found
        }
        Value::Array(items) => {
            let mut found = LoginPin::Picker;
            for item in items {
                match pin_in(item) {
                    LoginPin::Method => return LoginPin::Method,
                    LoginPin::Gateway => found = LoginPin::Gateway,
                    LoginPin::Picker | LoginPin::Unread | LoginPin::Token => {}
                }
            }
            found
        }
        _ => LoginPin::Picker,
    }
}

/// What the agent's settings make the permission mode of a line that names none
/// ([`permission_default`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// `permissions.defaultMode` names `mode` in the document `source`, the one that wins in
    /// Claude Code's precedence.
    Mode {
        /// The mode it names.
        mode: crate::harness::lights::Mode,
        /// The settings document that names it, for a person to open.
        source: String,
    },
    /// Every document was read and none names a mode: Claude Code picks its own.
    Unset,
    /// A document that could win cannot be read, or names a mode this build does not know: why.
    Unread(String),
}

/// The default permission mode of an agent's settings, and whether they turn bypass off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDefault {
    /// The mode a line with no permission flag lands in.
    pub mode: Resolved,
    /// The document whose `permissions.disableBypassPermissionsMode` is `disable`, where one
    /// is: Claude Code then starts in no bypass whatever the line asks.
    pub bypass_disabled: Option<String>,
}

/// THE DEFAULT PERMISSION MODE OF THE AGENT'S SETTINGS ([`permission_default`]), its documents
/// read in Claude Code's precedence, highest first: the server-managed copy
/// ([`REMOTE_SETTINGS`]), the managed preference domains ([`managed_preferences`], which this
/// reader cannot parse), the managed folder's drop-ins from the last name to the first and then
/// its file (a drop-in is merged over the file, a later one over an earlier), and the rest of
/// [`sources_under`]'s chain as it orders it: the command line's `--settings`, the project's
/// local settings, the project's, the user's. A `--permission-mode` or bypass flag on the line is
/// not read: this is the mode of a line that has none (`harness::upgrade::permission_click_argv`).
#[must_use]
pub fn permission_default_under(
    worker: &WorkerEnv,
    launch: &Path,
    managed: Option<&Path>,
    prefs: &Path,
    fence: Option<&Path>,
) -> PermissionDefault {
    let mut all = Vec::new();
    all.push(match worker.claude_dir(launch) {
        Ok(dir) if dir.is_absolute() => {
            let path = dir.join(REMOTE_SETTINGS);
            Source {
                name: path.display().to_string(),
                text: read_settings_in(fence, &path),
            }
        }
        Ok(_) | Err(_) => Source {
            name: format!("the user's {REMOTE_SETTINGS}"),
            text: Err("cannot be found".to_string()),
        },
    });
    all.extend(managed_preferences(worker, prefs));
    if let Some(managed) = managed {
        all.extend(managed_sources(managed, fence).into_iter().rev());
    }
    all.extend(sources_under(worker, launch, None, fence));
    permission_default(&all)
}

/// THE MODE `sources` GIVE A LINE WITH NO PERMISSION FLAG, `sources` in precedence order, the
/// highest first: the first document whose `permissions.defaultMode` names a mode wins
/// ([`Resolved::Mode`]); none naming one is [`Resolved::Unset`]. On the safe side it is
/// [`Resolved::Unread`], saying why, where a document at or above the winner is there and cannot
/// be read or parsed, names a policy helper whose output this reader does not run
/// ([`POLICY_HELPER_KEYS`]), names a mode this build does not know, or holds a `defaultMode`
/// anywhere but under its own `permissions` (the server-managed copy's shape is Claude Code's
/// own). A missing document says nothing. `bypass_disabled` is the first readable document, at
/// any precedence, whose `permissions.disableBypassPermissionsMode` is `disable`.
#[must_use]
pub fn permission_default(sources: &[Source]) -> PermissionDefault {
    let unread = |source: &Source, why: &str| format!("its settings ({}) {why}", source.name);
    let (mut mode, mut bypass_disabled) = (None, None);
    for source in sources {
        let value = match &source.text {
            Ok(None) => continue,
            Ok(Some(text)) => parse(text),
            Err(why) => Err(why.clone()),
        };
        let value = match value {
            Ok(value) => value,
            Err(why) => {
                mode.get_or_insert_with(|| Resolved::Unread(unread(source, &why)));
                continue;
            }
        };
        let permissions = value.get("permissions");
        if bypass_disabled.is_none()
            && permissions
                .and_then(|p| p.get("disableBypassPermissionsMode"))
                .and_then(Value::as_str)
                == Some("disable")
        {
            bypass_disabled = Some(source.name.clone());
        }
        if mode.is_some() {
            continue;
        }
        if let Value::Object(map) = &value
            && POLICY_HELPER_KEYS
                .iter()
                .any(|key| map.get(*key).is_some_and(|v| !v.is_null()))
        {
            mode = Some(Resolved::Unread(unread(
                source,
                "name a policy helper, whose output is not read",
            )));
            continue;
        }
        match permissions.and_then(|p| p.get("defaultMode")) {
            None | Some(Value::Null) if names_default_mode(&value, true) => {
                mode = Some(Resolved::Unread(unread(
                    source,
                    "hold a `defaultMode` outside their `permissions`",
                )));
            }
            None | Some(Value::Null) => {}
            Some(Value::String(word)) => {
                mode = Some(
                    match crate::harness::lights::Mode::of_setting(word.trim()) {
                        Some(mode) => Resolved::Mode {
                            mode,
                            source: source.name.clone(),
                        },
                        None => Resolved::Unread(unread(
                            source,
                            &format!("name a `defaultMode` this build does not know ({word})"),
                        )),
                    },
                );
            }
            Some(_) => {
                mode = Some(Resolved::Unread(unread(
                    source,
                    "hold a `permissions.defaultMode` that is no string",
                )));
            }
        }
    }
    PermissionDefault {
        mode: mode.unwrap_or(Resolved::Unset),
        bypass_disabled,
    }
}

/// Whether `value` holds a non-null `defaultMode` key anywhere but the top document's own
/// `permissions` (`top`), whose key [`permission_default`] reads itself.
fn names_default_mode(value: &Value, top: bool) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, nested)| {
            if key == "defaultMode" {
                return !nested.is_null();
            }
            if top && key == "permissions" {
                return match nested {
                    Value::Object(own) => own
                        .iter()
                        .any(|(k, v)| k != "defaultMode" && names_default_mode(v, false)),
                    other => names_default_mode(other, false),
                };
            }
            names_default_mode(nested, false)
        }),
        Value::Array(items) => items.iter().any(|item| names_default_mode(item, false)),
        _ => false,
    }
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
/// `.git` directory, or for a linked worktree the one its `.git` file's
/// `gitdir:` and that git directory's `commondir` lead to.
fn main_worktree(dir: &Path, fence: Option<&Path>) -> Result<Option<PathBuf>, String> {
    let small = |path: &Path| -> Result<Option<String>, String> {
        Ok(match opens(fence, path) {
            Ok(true) => {
                let text = crate::harness::footer::read_small(path, 4097).ok_or_else(|| {
                    format!(
                        "cannot be found: {} cannot be read as a regular file",
                        path.display()
                    )
                })?;
                if text.len() > 4096 {
                    return Err(format!(
                        "cannot be found: {} is larger than 4096 bytes",
                        path.display()
                    ));
                }
                Some(text)
            }
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
        .filter(|e| {
            Path::new(&e.file_name())
                .extension()
                .is_some_and(|x| x == "json")
        })
        .map(|e| e.path())
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
pub(crate) fn read_settings_in(
    fence: Option<&Path>,
    path: &Path,
) -> Result<Option<String>, String> {
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
