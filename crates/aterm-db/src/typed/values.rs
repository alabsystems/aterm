// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab tracker's value types (docs/DESIGN-lab-tracker-2026-11-14.md §3, package TK-04):
//! a commit, a digest, a repository path, a decimal, a principal (who made a change) and one
//! piece of evidence. Each is checked here by its grammar, and every string in it by the
//! tracker's secret rule ([`guard::secret_shaped`]), never by the registry's text guard, which
//! refuses a 40-character run, any `@` and a phone-shaped run of digits, and so a cell of 821 of
//! the 1,121 packages of publication 384b3a9f (§7, finding 13). A rule names no part of the
//! value.

use super::{Link, Named, guard, keys, pk_grammar};
use aterm_json::{Map, Value};

/// Longest repository path.
pub const MAX_PATH: usize = 1024;
/// Longest decimal, sign and point included.
pub const MAX_DECIMAL: usize = 64;
/// Longest word of a principal (a session, a program, a model, a harness).
pub const MAX_WORD: usize = 64;
/// Longest text a principal carries as what a writer called itself (`said`).
pub const MAX_SAID: usize = 256;
/// Longest command a run of evidence names.
pub const MAX_COMMAND: usize = 4096;
/// Longest output line a run of evidence keeps.
pub const MAX_OUTPUT: usize = 2048;
/// Longest note of evidence.
pub const MAX_NOTE: usize = 8192;
/// The most pieces of evidence one change cites (§2).
pub const MAX_EVIDENCE: usize = 16;
/// The algorithm word a digest starts with.
pub const DIGEST_PREFIX: &str = "sha256:";

/// Whether `text` is lower-case hex.
fn lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Check a git commit: lower-case hex, abbreviated to at least 7 digits or whole (40, or 64 for
/// a SHA-256 repository).
///
/// # Errors
/// The rule it breaks.
pub fn commit(text: &str) -> Result<(), &'static str> {
    ((7..=40).contains(&text.len()) || text.len() == 64)
        .then_some(())
        .filter(|()| lower_hex(text))
        .ok_or("a commit is 7 to 40 (or 64) lower-case hex digits")
}

/// Check a digest: `sha256:` and 64 lower-case hex digits.
///
/// # Errors
/// The rule it breaks.
pub fn digest(text: &str) -> Result<(), &'static str> {
    text.strip_prefix(DIGEST_PREFIX)
        .filter(|hex| hex.len() == 64 && lower_hex(hex))
        .map(drop)
        .ok_or("a digest is sha256: and 64 lower-case hex digits")
}

/// Check a path inside a repository, as a package's `owns` and `design` name one: relative (no
/// leading `/` or `~`), `/`-separated with no empty, `.` or `..` part (a trailing `/` names a
/// directory), no white space, `\` or control character, at most [`MAX_PATH`] bytes, and no
/// secret. A `*` stands for any run of a name's characters (`crates/trust-js-*`).
///
/// # Errors
/// The rule it breaks.
pub fn path(text: &str) -> Result<(), &'static str> {
    if text.is_empty() || text.len() > MAX_PATH {
        return Err("a path is 1 to 1024 bytes");
    }
    if text.starts_with(['/', '~']) {
        return Err("a path is relative to its repository");
    }
    if text
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err("a path holds no white space, backslash or control character");
    }
    let parts = text.strip_suffix('/').unwrap_or(text);
    if parts
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("a path has no empty, . or .. part");
    }
    if guard::secret_shaped(text) {
        return Err(guard::Refusal::CredentialShape.rule());
    }
    Ok(())
}

/// Check a decimal number written exactly: an optional `-`, whole digits with no leading zero,
/// and an optional fraction (`0.68`, `1322.1367705`, `-3`), at most [`MAX_DECIMAL`] bytes.
/// Trailing zeros of a fraction stay (they state a measurement's precision); a zero has no sign,
/// so equal values are equal text.
///
/// # Errors
/// The rule it breaks.
pub fn decimal(text: &str) -> Result<(), &'static str> {
    const RULE: &str = "a decimal is an optional -, digits with no leading zero, and an optional \
                        fraction";
    if text.is_empty() || text.len() > MAX_DECIMAL {
        return Err(RULE);
    }
    let (negative, magnitude) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = match magnitude.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (magnitude, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole)
        || (whole.len() > 1 && whole.starts_with('0'))
        || fraction.is_some_and(|fraction| !digits(fraction))
    {
        return Err(RULE);
    }
    if negative && magnitude.bytes().all(|b| matches!(b, b'0' | b'.')) {
        return Err("a zero has no sign");
    }
    Ok(())
}

/// Whether `text` is a principal's word: 1 to [`MAX_WORD`] letters, digits, `_`, `-` and `.`
/// (`s-47051b9a207955a92b14`, `15`, `claude`, `claude-opus-5-5`, `gpt-5.5`), and no secret.
fn word(text: &str) -> bool {
    (1..=MAX_WORD).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !guard::secret_shaped(text)
}

/// Check the text of a link to a row of another table family ([`Link`], design §4.6): the
/// row's key, in the key grammar and no secret, or for a machine its 32-digit Commons machine
/// uuid, lower-case hex. Whether the family holds that row is not read here: no table holds
/// another family's rows, and the link check follows it (TK-07).
///
/// # Errors
/// The rule it breaks.
pub fn link(link: &Link, text: &str) -> Result<(), &'static str> {
    match link.named {
        Named::Key => (pk_grammar(text) && guard::word(text).is_ok())
            .then_some(())
            .ok_or("a link names its row by the row's key"),
        Named::Machine => (text.len() == 32 && keys::parse_uuid32(text).is_some())
            .then_some(())
            .ok_or("a link names a machine by its 32-digit machine uuid"),
    }
}

/// Whether `text` names a principal's machine: its 32-digit Commons machine uuid, the text of a
/// link to a registry machine ([`link`], [`Named::Machine`]), or its `m<N>` label, which the
/// tracker's free-text records name a machine by and the link check refuses (TK-07).
fn machine(text: &str) -> bool {
    keys::is_label(text) || (text.len() == 32 && keys::parse_uuid32(text).is_some())
}

/// Who made a change (design §4.5): the session, the agent program and the Mac aterm reads
/// from the process tree, and what the program says of itself (its model, account and
/// harness), which is the program's own claim and only that. A record imported from a system
/// that kept a name as free text keeps it as `said` (`s595dac on m18`, `planning session`).
/// The Mac that signs the record is the record's own; `machine` is the one the principal acted
/// on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Principal {
    /// The aterm session: its id (`s-47051b9a207955a92b14`, `15`).
    pub session: Option<String>,
    /// The program that made the change (`claude`, `codex-alab`, `person`, `import`).
    pub program: Option<String>,
    /// The machine: its machine uuid, the text of a link to a registry machine
    /// ([`super::Named::Machine`]), or its label.
    pub machine: Option<String>,
    /// The program's claim: its account, a registry account key.
    pub account: Option<String>,
    /// The program's claim: its model.
    pub model: Option<String>,
    /// The program's claim: its harness.
    pub harness: Option<String>,
    /// What an imported record called its writer, as written.
    pub said: Option<String>,
}

/// A principal's members, in the order a value spells them.
const PRINCIPAL_MEMBERS: [&str; 7] = [
    "session", "program", "machine", "account", "model", "harness", "said",
];

impl Principal {
    /// A principal imported as free text: what the record called its writer.
    #[must_use]
    pub fn said(text: &str) -> Self {
        Self {
            said: Some(text.to_owned()),
            ..Self::default()
        }
    }

    fn members(&self) -> [(&'static str, Option<&String>); 7] {
        [
            ("session", self.session.as_ref()),
            ("program", self.program.as_ref()),
            ("machine", self.machine.as_ref()),
            ("account", self.account.as_ref()),
            ("model", self.model.as_ref()),
            ("harness", self.harness.as_ref()),
            ("said", self.said.as_ref()),
        ]
    }

    /// The value a [`super::Ty::Principal`] cell holds: an object of the members it has.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        for (name, member) in self.members() {
            if let Some(member) = member {
                object.insert(name.into(), Value::String(member.clone()));
            }
        }
        Value::Object(object)
    }

    /// The principal a checked value holds.
    ///
    /// # Errors
    /// The value is no principal ([`principal`]).
    pub fn from_value(x: &Value) -> Result<Self, &'static str> {
        principal(x)?;
        let get = |name: &str| x.get(name).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            session: get("session"),
            program: get("program"),
            machine: get("machine"),
            account: get("account"),
            model: get("model"),
            harness: get("harness"),
            said: get("said"),
        })
    }
}

/// Check a principal's value: an object of [`Principal`]'s members and no other, naming a
/// session, a program or what an imported record said; each word in its grammar, the machine
/// a label or machine uuid, the account a registry key, and `said` the tracker's prose of at most
/// [`MAX_SAID`] bytes.
///
/// # Errors
/// The rule it breaks.
pub fn principal(x: &Value) -> Result<(), &'static str> {
    let object = x.as_object().ok_or("a principal is a record")?;
    if object
        .keys()
        .any(|key| !PRINCIPAL_MEMBERS.contains(&key.as_str()))
    {
        return Err("a principal has a member its type does not name");
    }
    for (name, member) in object {
        let text = member.as_str().ok_or("a principal's members are text")?;
        let ok = match name.as_str() {
            "machine" => machine(text),
            "account" => pk_grammar(text) && !guard::secret_shaped(text),
            "said" => !text.trim().is_empty() && guard::prose(text, MAX_SAID).is_ok(),
            _ => word(text),
        };
        if !ok {
            return Err(match name.as_str() {
                "machine" => "a principal's machine is its label or machine uuid",
                "account" => "a principal's account is a registry key",
                "said" => "what a principal said is prose of at most 256 bytes",
                _ => "a principal's session, program, model and harness are words",
            });
        }
    }
    if !["session", "program", "said"]
        .iter()
        .any(|name| object.contains_key(*name))
    {
        return Err("a principal names its session, its program or what it said");
    }
    Ok(())
}

/// One piece of evidence a change cites (design §2, up to [`MAX_EVIDENCE`] of them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A commit of a repository: `commit:<repo>@<commit>`.
    Commit { repo: String, commit: String },
    /// A run of a command, its exit status and the line it printed: what `done` records when it
    /// runs a package's exit test.
    Run {
        command: String,
        exit: i64,
        output: Option<String>,
        repo: Option<String>,
        commit: Option<String>,
    },
    /// A measurement of a metric: `measure:<metric>=<decimal>`.
    Measure { metric: String, value: String },
    /// A file of a repository: `path:<repo>:<path>`.
    Path { repo: String, path: String },
    /// A page: `https://…`.
    Url(String),
    /// A sentence: what someone wrote, graded `claimed` (§4.4), with no white space around it
    /// ([`Evidence::note`]). An imported record's evidence is one.
    Note(String),
}

impl Evidence {
    /// A note of `text`, trimmed: the one form a note is held in, so it spells and reads back
    /// as itself.
    #[must_use]
    pub fn note(text: &str) -> Self {
        Self::Note(text.trim().to_owned())
    }

    /// The word a value's `kind` spells it with.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit",
            Self::Run { .. } => "run",
            Self::Measure { .. } => "measure",
            Self::Path { .. } => "path",
            Self::Url(_) => "url",
            Self::Note(_) => "note",
        }
    }

    /// Read a piece of evidence as a command line gives one: `commit:<repo>@<commit>`,
    /// `measure:<metric>=<decimal>`, `path:<repo>:<path>`, `note:<text>` or an `https://` URL.
    /// A run is never typed: `done` records the run it made.
    ///
    /// # Errors
    /// The text is none of these, or its parts break their types ([`evidence`]).
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let piece = if let Some(rest) = text.strip_prefix("commit:") {
            let (repo, commit) = rest
                .split_once('@')
                .ok_or("commit evidence is commit:<repo>@<commit>")?;
            Self::Commit {
                repo: repo.to_owned(),
                commit: commit.to_owned(),
            }
        } else if let Some(rest) = text.strip_prefix("measure:") {
            let (metric, value) = rest
                .split_once('=')
                .ok_or("measure evidence is measure:<metric>=<decimal>")?;
            Self::Measure {
                metric: metric.to_owned(),
                value: value.to_owned(),
            }
        } else if let Some(rest) = text.strip_prefix("path:") {
            let (repo, path) = rest
                .split_once(':')
                .ok_or("path evidence is path:<repo>:<path>")?;
            Self::Path {
                repo: repo.to_owned(),
                path: path.to_owned(),
            }
        } else if let Some(rest) = text.strip_prefix("note:") {
            Self::note(rest)
        } else if text.starts_with("https://") {
            Self::Url(text.to_owned())
        } else {
            return Err(
                "evidence is commit:<repo>@<commit>, measure:<metric>=<decimal>, \
                 path:<repo>:<path>, note:<text> or an https URL",
            );
        };
        evidence(&piece.to_value())?;
        Ok(piece)
    }

    /// The text [`Evidence::parse`] reads back, for every piece but a run, which is spelled
    /// `run:<exit>:<command>`.
    #[must_use]
    pub fn spell(&self) -> String {
        match self {
            Self::Commit { repo, commit } => format!("commit:{repo}@{commit}"),
            Self::Run { command, exit, .. } => format!("run:{exit}:{command}"),
            Self::Measure { metric, value } => format!("measure:{metric}={value}"),
            Self::Path { repo, path } => format!("path:{repo}:{path}"),
            Self::Url(url) => url.clone(),
            Self::Note(text) => format!("note:{text}"),
        }
    }

    /// The value a [`super::Ty::Evidence`] cell holds: an object with its `kind`.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("kind".into(), Value::String(self.kind().into()));
        let mut put = |name: &str, text: &str| {
            object.insert(name.into(), Value::String(text.to_owned()));
        };
        match self {
            Self::Commit { repo, commit } => {
                put("repo", repo);
                put("commit", commit);
            }
            Self::Run {
                command,
                exit: _,
                output,
                repo,
                commit,
            } => {
                put("command", command);
                for (name, member) in [("output", output), ("repo", repo), ("commit", commit)] {
                    if let Some(member) = member {
                        put(name, member);
                    }
                }
            }
            Self::Measure { metric, value } => {
                put("metric", metric);
                put("value", value);
            }
            Self::Path { repo, path } => {
                put("repo", repo);
                put("path", path);
            }
            Self::Url(url) => put("url", url),
            Self::Note(text) => put("text", text),
        }
        if let Self::Run { exit, .. } = self {
            object.insert("exit".into(), Value::from(*exit));
        }
        Value::Object(object)
    }

    /// The piece a checked value holds.
    ///
    /// # Errors
    /// The value is no piece of evidence ([`evidence`]).
    pub fn from_value(x: &Value) -> Result<Self, &'static str> {
        evidence(x)?;
        let get = |name: &str| x.get(name).and_then(Value::as_str).map(str::to_owned);
        let need = |name: &str| get(name).unwrap_or_default();
        Ok(match x.get("kind").and_then(Value::as_str) {
            Some("commit") => Self::Commit {
                repo: need("repo"),
                commit: need("commit"),
            },
            Some("run") => Self::Run {
                command: need("command"),
                exit: x.get("exit").and_then(Value::as_i64).unwrap_or_default(),
                output: get("output"),
                repo: get("repo"),
                commit: get("commit"),
            },
            Some("measure") => Self::Measure {
                metric: need("metric"),
                value: need("value"),
            },
            Some("path") => Self::Path {
                repo: need("repo"),
                path: need("path"),
            },
            Some("url") => Self::Url(need("url")),
            _ => Self::Note(need("text")),
        })
    }
}

/// The members each kind of evidence holds, required then optional.
fn evidence_members(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match kind {
        "commit" => (&["repo", "commit"], &[]),
        "run" => (&["command", "exit"], &["output", "repo", "commit"]),
        "measure" => (&["metric", "value"], &[]),
        "path" => (&["repo", "path"], &[]),
        "url" => (&["url"], &[]),
        "note" => (&["text"], &[]),
        _ => return None,
    })
}

/// Check one member of a piece of evidence.
fn evidence_member(name: &str, x: &Value) -> Result<(), &'static str> {
    if name == "exit" {
        return x
            .as_i64()
            .filter(|status| (0..=255).contains(status))
            .map(drop)
            .ok_or("a run's exit status is 0 to 255");
    }
    let text = x.as_str().ok_or("evidence holds text")?;
    match name {
        "repo" | "metric" => (pk_grammar(text) && !guard::secret_shaped(text))
            .then_some(())
            .ok_or("evidence names a repository or a metric by its key"),
        "commit" => commit(text),
        "value" => decimal(text),
        "path" => path(text),
        "url" => url(text),
        "command" => guard::command(text, MAX_COMMAND).map_err(guard::Refusal::rule),
        "output" => guard::prose(text, MAX_OUTPUT).map_err(guard::Refusal::rule),
        _ => {
            if text.trim().is_empty() || text.trim() != text {
                return Err("a note says something, with no white space around it");
            }
            guard::prose(text, MAX_NOTE).map_err(guard::Refusal::rule)
        }
    }
}

/// Check an https URL as evidence cites a page: no white space, no user part (where a clone URL
/// carries a token as its user), no secret, at most 2,048 bytes. A query and a fragment may stay
/// (a CI run, a line of a file), unlike [`super::Ty::Url`]'s.
fn url(text: &str) -> Result<(), &'static str> {
    let rest = text.strip_prefix("https://").ok_or("a URL is https")?;
    if rest.is_empty()
        || text.len() > 2048
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("a URL is one https address of at most 2048 bytes");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("a URL names its host and no user");
    }
    if guard::secret_shaped(text) {
        return Err(guard::Refusal::CredentialShape.rule());
    }
    Ok(())
}

/// Check a piece of evidence's value: an object with a `kind` this build knows and exactly the
/// members that kind holds, each in its type.
///
/// # Errors
/// The rule it breaks.
pub fn evidence(x: &Value) -> Result<(), &'static str> {
    let object = x.as_object().ok_or("evidence is a record")?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or("evidence names its kind")?;
    let (required, optional) =
        evidence_members(kind).ok_or("evidence is a commit, run, measure, path, url or note")?;
    if object.keys().any(|key| {
        key != "kind" && !required.contains(&key.as_str()) && !optional.contains(&key.as_str())
    }) {
        return Err("this evidence has a member its kind does not name");
    }
    if required.iter().any(|name| !object.contains_key(*name)) {
        return Err("this evidence lacks a member its kind needs");
    }
    object
        .iter()
        .filter(|(key, _)| key.as_str() != "kind")
        .try_for_each(|(name, member)| evidence_member(name, member))
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod tests;
