// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `--yolo` alias, and yolo by default.

/// The word the alias claims, exactly as typed.
pub const WORD: &str = "--yolo";

/// What a by-the-flag echo starts with.
pub const ECHO_PREFIX: &str = "aterm: --yolo: running ";

/// What a by-default echo starts with.
pub const DEFAULT_ECHO_PREFIX: &str = "aterm: yolo (default): running ";

/// One program's yolo policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The vendor flags the posture expands to.
    pub flags: &'static [&'static str],
    /// Tokens that already decide the posture: a launch holding one and no `--yolo` runs as typed.
    pub markers: &'static [&'static str],
}

impl Policy {
    /// No policy: no block, no expansion, no default.
    pub const NONE: Self = Self {
        flags: &[],
        markers: &[],
    };
}

/// `program`'s yolo policy.
#[must_use]
pub fn policy_of(program: &str) -> Policy {
    crate::vendor_direct::spec(program).map_or(Policy::NONE, |row| Policy {
        flags: row.yolo_flags,
        markers: row.yolo_markers,
    })
}

/// The flags `--yolo` expands to for `program` ([`Policy::flags`]).
#[must_use]
pub fn flags_of(program: &str) -> &'static [&'static str] {
    policy_of(program).flags
}

/// Whether `f` may be embedded in the twin's `sh` block as an expansion flag.
#[must_use]
pub fn is_shell_safe_flag(f: &str) -> bool {
    f.strip_prefix("--")
        .is_some_and(crate::selfupdate::is_shell_safe_verb)
}

/// Whether `m` may be embedded in the block as a posture marker.
#[must_use]
pub fn is_shell_safe_marker(m: &str) -> bool {
    let word = m.strip_suffix('=').unwrap_or(m);
    let bytes = word.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'-' {
        return bytes[1].is_ascii_alphabetic();
    }
    word.strip_prefix("--").is_some_and(|name| {
        name.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Whether `token` is decided by `marker`, in every spelling the vendors'
/// parsers give the same flag: a long flag also matches its `--flag=value`
/// form, and a short flag also matches its attached forms (`-sread-only`,
/// `-a=never`: clap takes the rest of the token as the value).
fn marker_matches(marker: &str, token: &str) -> bool {
    if let Some(word) = marker.strip_suffix('=') {
        return token.len() > word.len()
            && token.starts_with(word)
            && token[word.len()..].starts_with('=');
    }
    let bytes = marker.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'-' && bytes[1] != b'-' {
        return token.starts_with(marker);
    }
    token == marker
        || token
            .strip_prefix(marker)
            .is_some_and(|rest| rest.starts_with('='))
}

/// A program's leading options as its parser reads them ([`word_options`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WordOptions {
    /// Options that take the next token as their value.
    pub(crate) values: &'static [&'static str],
    /// Options that take no value.
    pub(crate) switches: &'static [&'static str],
}

/// The CLI a managed name's vendor alias runs.
pub(crate) fn cli_name(program: &str) -> &str {
    crate::vendor_direct::spec(program)
        .and_then(|row| row.aliases.iter().find(|(name, _)| *name == program))
        .map_or(program, |(_, target)| *target)
}

/// Leading options whose arity this word scan knows.
pub(crate) fn word_options(program: &str) -> WordOptions {
    match cli_name(program) {
        "codex" => WordOptions {
            values: &[
                "--model",
                "-m",
                "--cd",
                "-C",
                "--add-dir",
                "--local-provider",
                "--config",
                "-c",
                "--profile",
                "-p",
            ],
            switches: &[
                "--oss",
                "--worktree",
                "--search",
                "--no-alt-screen",
                "--strict-config",
                "--no-daemon",
            ],
        },
        "claude" => WordOptions {
            switches: &["-p", "--print"],
            ..WordOptions::default()
        },
        _ => WordOptions::default(),
    }
}

/// The word scan stopped short of the first word ([`first_word`]): an option it does not know,
/// or a value missing at the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unread;

/// The actual argument position of the first word, past known leading values.
fn first_word(program: &str, policy: &Policy, head: &[String]) -> Result<Option<usize>, Unread> {
    let opts = word_options(program);
    let mut value = false;
    for (i, arg) in head.iter().enumerate() {
        let a = arg.as_str();
        if value {
            value = false;
            continue;
        }
        if a == WORD || policy.flags.contains(&a) || opts.switches.contains(&a) {
            continue;
        } else if opts.values.contains(&a) {
            value = true;
        } else if opts.values.iter().any(|flag| marker_matches(flag, a)) {
            // A long `=value` or attached short value is already consumed.
            continue;
        } else if a.starts_with('-') {
            return Err(Unread);
        } else {
            return Ok(Some(i));
        }
    }
    if value { Err(Unread) } else { Ok(None) }
}

/// What a launch runs as ([`decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The argv as typed, nothing printed.
    AsTyped,
    /// `--yolo` was typed.
    Flag(Vec<String>),
    /// Nothing decided the posture.
    Default(Vec<String>),
}

fn config_decides_posture(program: &str, head: &[String]) -> bool {
    if cli_name(program) != "codex" {
        return false;
    }
    let mut next = false;
    for arg in head {
        let value = if next {
            next = false;
            arg.as_str()
        } else if matches!(arg.as_str(), "-c" | "--config") {
            next = true;
            continue;
        } else if let Some(value) = arg.strip_prefix("--config=") {
            value
        } else if let Some(value) = arg.strip_prefix("-c").filter(|value| !value.is_empty()) {
            value.strip_prefix('=').unwrap_or(value)
        } else {
            continue;
        };
        if value
            .split_once('=')
            .is_some_and(|(key, _)| matches!(key.trim(), "sandbox_mode" | "approval_policy"))
        {
            return true;
        }
    }
    false
}

/// Decide `program args…`, reading the words ahead of a literal `--`. A program with no policy
/// runs as typed. A typed `--yolo` becomes the policy's flags in place, each once
/// ([`Launch::Flag`]). Else an option the word scan does not know, a marker, a Codex permission
/// config assignment or a self-update verb as the first word runs as typed; else the flags go
/// in front ([`Launch::Default`]).
#[must_use]
pub fn decide(program: &str, args: &[String]) -> Launch {
    let policy = policy_of(program);
    if policy.flags.is_empty() || policy.markers.is_empty() {
        return Launch::AsTyped;
    }
    let head_len = args.iter().take_while(|a| a.as_str() != "--").count();
    let head = &args[..head_len];
    if head.iter().any(|a| a == WORD) {
        let mut present: Vec<&str> = head.iter().map(String::as_str).collect();
        let mut out: Vec<String> = Vec::with_capacity(args.len() + policy.flags.len());
        for arg in head {
            if arg != WORD {
                out.push(arg.clone());
                continue;
            }
            for flag in policy.flags {
                if !present.contains(flag) {
                    present.push(flag);
                    out.push((*flag).to_string());
                }
            }
        }
        out.extend(args[head_len..].iter().cloned());
        return Launch::Flag(out);
    }
    let Ok(word) = first_word(program, &policy, head) else {
        return Launch::AsTyped;
    };
    if head
        .iter()
        .any(|a| policy.markers.iter().any(|m| marker_matches(m, a)))
        || config_decides_posture(program, head)
    {
        return Launch::AsTyped;
    }
    // The self-update verbs skip, on every lane: an update shape is decided by that intercept
    // first (`claude update --help` forwards to the vendor), and the default must not land on
    // the forward.
    if word.is_some_and(|i| crate::selfupdate::verbs_of(program).contains(&head[i].as_str())) {
        return Launch::AsTyped;
    }
    let mut out: Vec<String> = policy.flags.iter().map(|f| (*f).to_string()).collect();
    out.extend(args.iter().cloned());
    Launch::Default(out)
}

fn line(prefix: &str, program: &str, args: &[String]) -> String {
    let mut s = String::from(prefix);
    s.push_str(&crate::platform::sh_quote_str(program));
    for arg in args {
        s.push(' ');
        s.push_str(&crate::platform::sh_quote_str(arg));
    }
    s
}

/// The one stderr line a by-the-flag launch prints (no trailing newline).
#[must_use]
pub fn echo_line(program: &str, args: &[String]) -> String {
    line(ECHO_PREFIX, program, args)
}

/// [`echo_line`] for a by-default launch ([`DEFAULT_ECHO_PREFIX`]).
#[must_use]
pub fn default_echo_line(program: &str, args: &[String]) -> String {
    line(DEFAULT_ECHO_PREFIX, program, args)
}

/// Set to `1` when an agent launch passes `--no-harness` or `--no-alab-harness`.
pub const NO_HARNESS_ENV: &str = "ATERM_NO_HARNESS";

/// Strip `--no-harness` and `--no-alab-harness` ahead of a literal `--` for an
/// agent program, returning `Some(stripped)` when at least one was removed.
#[must_use]
pub fn strip_no_harness(program: &str, args: &[String]) -> Option<Vec<String>> {
    if !crate::stub::is_agent_program(program) {
        return None;
    }
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if !args[..end]
        .iter()
        .any(|a| matches!(a.as_str(), "--no-harness" | "--no-alab-harness"))
    {
        return None;
    }
    let mut out = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        if i < end && matches!(arg.as_str(), "--no-harness" | "--no-alab-harness") {
            continue;
        }
        out.push(arg.clone());
    }
    Some(out)
}

#[cfg(test)]
#[path = "yolo_tests.rs"]
mod tests;
