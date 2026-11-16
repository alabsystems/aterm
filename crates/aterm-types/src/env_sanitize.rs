// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Environment variable deny lists for PTY child processes.
//!
//! Two lists decide what a child loses: [`ENV_DENY_VARS`], exact names aterm itself sets, and the
//! strip list of prefixes ([`EnvStrip`]). The strip list is aterm.toml's `shell_env_strip`
//! (default [`DEFAULT_ENV_STRIP`]) plus each verified harness record's `env_strip`
//! (`docs/HARNESS-CONTRACT.md` §2), which can add entries and remove none. The process that
//! spawns shells installs the list it composed ([`install_env_strip`]); a process that installs
//! none strips the default.

use std::sync::{Arc, RwLock};

/// The default of aterm.toml's `shell_env_strip`: prefixes of environment variables stripped
/// before spawning a PTY child.
pub const DEFAULT_ENV_STRIP: &[&str] = &[
    "CLAUDE",
    "ANTHROPIC_",
    "COPILOT_",
    "CODEX_",
    "ANTIGRAVITY_",
    "GEMINI_",
    "CURSOR_",
    "GROK_",
    "XAI_",
    "KIMI_",
    "MOONSHOT_",
    "AI_",
    "_DEVTOOL_",
];

/// Environment variable passing a child's root session ID (`s-<20hex>`).
pub const ENV_SESSION_ID: &str = "ATERM_SESSION_ID";
/// Environment variable passing a child's root launch nonce (`<32hex>`).
pub const ENV_LAUNCH_NONCE: &str = "ATERM_LAUNCH_NONCE";
/// Environment variable passing the parent session ID (`s-<20hex>`).
pub const ENV_PARENT_SESSION_ID: &str = "ATERM_PARENT_SESSION_ID";
/// Environment variable recording the multiplexer signature at spawn (`"<$TMUX>|<$STY>"`).
pub const ENV_MUX_BASE: &str = "ATERM_MUX_BASE";
/// Environment variable naming the 0600 file holding parent-to-child edge tokens.
pub const ENV_EDGE_TOKENS: &str = "ATERM_EDGE_TOKENS";
/// Retired environment variable for a `ReadScreen` `EdgeToken`.
pub const ENV_EDGE_READ: &str = "ATERM_EDGE_READ";
/// Retired environment variable for a `WriteInput` `EdgeToken`.
pub const ENV_EDGE_WRITE: &str = "ATERM_EDGE_WRITE";
/// Retired environment variable for a `Signal` `EdgeToken`.
pub const ENV_EDGE_SIGNAL: &str = "ATERM_EDGE_SIGNAL";

/// Environment variable naming the session ID a controller tab was spawned to observe.
pub const ENV_OBSERVE_SESSION_ID: &str = "ATERM_OBSERVE_SESSION_ID";

/// Environment variable naming a spawned worker's one-shot task file.
pub const ENV_TASK_FILE: &str = "ATERM_TASK_FILE";

/// Environment variable recording the resolved shell executable for a tab.
pub const ENV_TAB_SHELL: &str = "ATERM_TAB_SHELL";

/// Development-seam environment variable enabling fast harness timers in debug builds.
pub const ENV_DEV_FAST_TIMERS: &str = "ATERM_DEV_FAST_TIMERS";

/// Development-seam environment variable naming the sleep a debug build's hosted harness loops
/// read: `none`, a host that never sleeps, which every live test's instance sets.
pub const ENV_DEBUG_HOST_SLEEP: &str = "ATERM_DEBUG_HOST_SLEEP";

/// Environment variable advertising `amacs` as the default editor to shell integration.
pub const ENV_DEFAULT_EDITOR: &str = "ATERM_DEFAULT_EDITOR";

/// The word an aterm tab exports as `EDITOR` when the person set none: amacs, bare, because a
/// bundle path holds spaces that `sh -c "$EDITOR file"` would split.
pub const DEFAULT_EDITOR_WORD: &str = "amacs";

/// Environment variable every aterm tab's session carries (`ATERM_CHILD=1`).
pub const ENV_ATERM_CHILD: &str = "ATERM_CHILD";

/// Whether an inherited `EDITOR` is the default an outer aterm's tab exported rather than the
/// person's own: exactly [`DEFAULT_EDITOR_WORD`], in an environment an aterm tab handed down
/// (`aterm_child`, the inherited [`ENV_ATERM_CHILD`], is non-empty). Each aterm drops such an
/// `EDITOR` and derives its own default under its own settings, as it does
/// [`ENV_DEFAULT_EDITOR`]; a person who exports `EDITOR=amacs` in their startup files gets it back
/// when those files run again.
#[must_use]
pub fn is_handed_down_editor(
    editor: &std::ffi::OsStr,
    aterm_child: Option<&std::ffi::OsStr>,
) -> bool {
    editor == DEFAULT_EDITOR_WORD && aterm_child.is_some_and(|value| !value.is_empty())
}

/// Exact environment variable names stripped before spawning a PTY child.
pub const ENV_DENY_VARS: &[&str] = &[
    "ATERM_DEBUG_SEAMLESS_REEXEC",
    "ATERM_DEBUG_SEAMLESS_KEEP",
    "ATERM_DEBUG_RELAUNCH_NUDGE",
    "ATERM_DEBUG_STATUS_BARS",
    "ATERM_DEBUG_PASTE_FILE",
    "ATERM_DEBUG_CLIPBOARD_FILE",
    "ATERM_DEBUG_LOST_SHELL_NONCE",
    "ATERM_DEBUG_TURN_KEY_CLOCK_STEP_MS",
    "ATERM_DEBUG_TURN_KEY_BARRIER",
    "ATERM_DEBUG_PROMPT_CLOCK",
    "ATERM_DEBUG_FLEET_CLOCK",
    "ATERM_DEBUG_ECHO_CLOCK",
    "ATERM_DEBUG_TYPING_CLOCK",
    "ATERM_UPDATE_ROOT",
    "ATERM_DEV_CURL",
    "ATERM_HANDOFF_READY_TIMEOUT_MS",
    "ATERM_HANDOFF_PROOF_TIMEOUT_MS",
    "ATERM_NUDGE_CLASS",
    "ATERM_RESIZE_RUN_GAP_MS",
    "ATERM_SLOG_RING",
    "ATERM_DEBUG_DIE_BEFORE_DIAL",
    "ATERM_DEBUG_HEADLESS_RESTORE",
    "ATERM_DEBUG_QUIT_BYE",
    "ATERM_DEBUG_HEADLESS_PKG_LANE",
    ENV_DEV_FAST_TIMERS,
    ENV_DEBUG_HOST_SLEEP,
    "ATERM_UPDATE_TOKEN",
    "ATPKG_TOKEN",
    ENV_SESSION_ID,
    ENV_LAUNCH_NONCE,
    ENV_PARENT_SESSION_ID,
    ENV_MUX_BASE,
    ENV_EDGE_TOKENS,
    ENV_EDGE_READ,
    ENV_EDGE_WRITE,
    ENV_EDGE_SIGNAL,
    ENV_OBSERVE_SESSION_ID,
    ENV_TAB_SHELL,
    ENV_TASK_FILE,
    ENV_DEFAULT_EDITOR,
    "ATERM_LINK_BROKER",
    "ATERM_LINK_CAP_FILE",
    "ATERM_LINK_FLEET",
    "ATERM_FABRIC_COMMAND",
];

/// The variables that mean an agent is running this command: its tools carry them.
pub const AGENT_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "ATERM_AI_REVIEW",
];

/// The first [`AGENT_MARKERS`] variable set, if any.
#[must_use]
pub fn agent_marker(var: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    AGENT_MARKERS
        .iter()
        .copied()
        .find(|k| var(k).is_some_and(|v| !v.is_empty()))
}

/// Names an `env_strip` entry may not strip ([`admit_env_strip_entry`]): an entry is refused when
/// it or one of these is a prefix of the other, so `LC_ALL` is as protected as `LC_`, `TERM_` and
/// `TERM_PROGRAM` are refused because `TERM` is their prefix (`TERM_PROGRAM` names aterm to its
/// children), `ATERM_SESSION` because it would strip `ATERM_SESSION_ID`, and `__A` or
/// `__ATERM_REROUTE` because they would strip aterm's own `__ATERM_` variables
/// (`__ATERM_REROUTE_PASSTHROUGH`, the reroute stubs' passthrough marker).
pub const PROTECTED_ENV_NAMES: &[&str] = &[
    "ATERM_", "__ATERM_", "PATH", "HOME", "SHELL", "TERM", "LANG", "LC_", "SSH_", "TMPDIR", "USER",
    "LOGNAME", "XDG_", "DYLD_", "GIT_", "TMP",
];

/// The refusal [`admit_env_strip_entry`] gives an entry that would strip a protected name.
const PROTECTED_REFUSAL: &str = "an env_strip entry may not strip ATERM_, __ATERM_, PATH, HOME, \
     SHELL, TERM, LANG, LC_, SSH_, TMPDIR, USER, LOGNAME, XDG_, DYLD_, GIT_ or TMP (neither may \
     be a prefix of the other)";

/// The longest strip-list entry.
const ENV_STRIP_ENTRY_MAX: usize = 64;

/// Whether `entry` may join a strip list: it matches `[A-Z_][A-Z0-9_]{2,}` (at most 64 bytes),
/// and neither it nor any [`PROTECTED_ENV_NAMES`] name is a prefix of the other.
///
/// # Errors
/// The rule the entry breaks, in words a refusal line prints.
pub fn admit_env_strip_entry(entry: &str) -> Result<(), &'static str> {
    let bytes = entry.as_bytes();
    let first_ok = bytes
        .first()
        .is_some_and(|b| b.is_ascii_uppercase() || *b == b'_');
    if bytes.len() < 3
        || bytes.len() > ENV_STRIP_ENTRY_MAX
        || !first_ok
        || !bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
    {
        return Err("an env_strip entry matches [A-Z_][A-Z0-9_]{2,}");
    }
    if PROTECTED_ENV_NAMES
        .iter()
        .any(|p| p.starts_with(entry) || entry.starts_with(p))
    {
        return Err(PROTECTED_REFUSAL);
    }
    Ok(())
}

/// One entry a strip list refused, and why ([`EnvStrip::compose`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvStripRefusal {
    /// Who asked for it: `aterm.toml` or a harness id.
    pub origin: String,
    /// The entry as written.
    pub entry: String,
    /// The rule it broke ([`admit_env_strip_entry`]).
    pub reason: &'static str,
}

/// The prefixes a spawned child loses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvStrip {
    prefixes: Vec<String>,
}

impl Default for EnvStrip {
    /// [`DEFAULT_ENV_STRIP`].
    fn default() -> Self {
        Self {
            prefixes: DEFAULT_ENV_STRIP.iter().map(|p| (*p).to_string()).collect(),
        }
    }
}

impl EnvStrip {
    /// The list a person's `shell_env_strip` (`None`: unset, so [`DEFAULT_ENV_STRIP`]) and the
    /// harness records' `(id, entry)` pairs make. A record's entry only adds: nothing a record
    /// says removes a prefix. Every entry passes [`admit_env_strip_entry`] or is refused, and a
    /// repeated one is kept once.
    #[must_use]
    pub fn compose<'a>(
        person: Option<&'a [String]>,
        records: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> (Self, Vec<EnvStripRefusal>) {
        let mut strip = Self {
            prefixes: Vec::new(),
        };
        let mut refused = Vec::new();
        let base: Vec<(&str, &str)> = match person {
            Some(list) => list.iter().map(|e| ("aterm.toml", e.as_str())).collect(),
            None => DEFAULT_ENV_STRIP.iter().map(|e| ("aterm", *e)).collect(),
        };
        for (origin, entry) in base.into_iter().chain(records) {
            match admit_env_strip_entry(entry) {
                Ok(()) => {
                    if !strip.prefixes.iter().any(|p| p == entry) {
                        strip.prefixes.push(entry.to_string());
                    }
                }
                Err(reason) => refused.push(EnvStripRefusal {
                    origin: origin.to_string(),
                    entry: entry.to_string(),
                    reason,
                }),
            }
        }
        (strip, refused)
    }

    /// The prefixes, in the order they were composed.
    #[must_use]
    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// Whether `key` starts with one of the prefixes.
    #[must_use]
    pub fn strips(&self, key: &str) -> bool {
        self.prefixes.iter().any(|p| key.starts_with(p.as_str()))
    }
}

/// The strip list this process installed, or `None` before any install.
static INSTALLED: RwLock<Option<Arc<EnvStrip>>> = RwLock::new(None);

/// Make `strip` the list every later spawn in this process strips ([`is_ai_env_var`]).
pub fn install_env_strip(strip: EnvStrip) {
    let mut slot = INSTALLED
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(Arc::new(strip));
}

/// The installed strip list, or the default one ([`EnvStrip::default`]) before any install.
#[must_use]
pub fn effective_env_strip() -> Arc<EnvStrip> {
    static DEFAULT: std::sync::OnceLock<Arc<EnvStrip>> = std::sync::OnceLock::new();
    let slot = INSTALLED
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match slot.as_ref() {
        Some(strip) => Arc::clone(strip),
        None => Arc::clone(DEFAULT.get_or_init(|| Arc::new(EnvStrip::default()))),
    }
}

/// Returns `true` if `key` matches a deny-listed AI or provisioning environment variable: an
/// exact [`ENV_DENY_VARS`] name, or a prefix of the installed strip list ([`install_env_strip`]).
#[must_use]
#[inline]
pub fn is_ai_env_var(key: &str) -> bool {
    let mut denied = false;
    for k in ENV_DENY_VARS {
        if *k == key {
            denied = true;
            break;
        }
    }
    denied || effective_env_strip().strips(key)
}

/// Classifies an OS environment variable name against [`is_ai_env_var`].
#[must_use]
pub fn is_ai_env_key(key: &std::ffi::OsStr) -> bool {
    let key = key.to_string_lossy();
    #[cfg(windows)]
    {
        is_ai_env_var(&key.to_ascii_uppercase())
    }
    #[cfg(not(windows))]
    {
        is_ai_env_var(&key)
    }
}

#[cfg(test)]
#[path = "env_sanitize_tests.rs"]
mod tests;
