// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TAPE'S HEADER ([`Header`]): what a supervisor loop was given before it
//! ran, written once ahead of everything it then observed, and read back
//! into the same [`SessionSetup`], [`SuperviseOpts`] and policy a replay
//! builds its loop from ([`super::super::run::Session::from_setup`]).
//!
//! The text is one `atape <format>` line, then `key=value` lines in sections
//! (`[roots]`, `[setup]`, `[env]`, `[opts]`, `[policy]`), the run's own
//! facts (`tape`, `build`, `os`, `entry`) ahead of the first. A key alone on
//! its line, with no `=`, is a value the run did not have (`ledger` with no
//! ledger); `key=` is an empty one. Each value is one line: a backslash, a
//! newline, a carriage return, a tab and every other control character are
//! escaped (`\\`, `\n`, `\r`, `\t`, `\xHH`), and so is each byte of a path
//! that is not UTF-8, so a value comes back byte for byte. A duration is
//! exact to the nanosecond in the largest unit that holds it (`20s`,
//! `250ms`, `max` for a budget that never runs out).
//!
//! THE POLICY is written as the owner's own keys
//! ([`SupervisorConfig::policy_texts`]: every `[harness]` key's text, each
//! agent's switch, each rule a retired key withholds, and a session's own
//! word) and rebuilt through [`SupervisorConfig::set`] — the reader of the
//! owner's file — never through a second encoding of the fields that could
//! drift from it. `exact` says whether that rebuild gave back the very policy
//! the run had, checked as the header was captured (a policy no file can
//! spell, as a test builds one, says `false`).
//!
//! THE ROOTS are the directories the run's own files live in: the ledger's,
//! the journal's, the standing rules', the state root and the home directory
//! ([`Roots`]; an older build's header may name a `notes` root too). Every path the
//! loop opens is written under the most specific one, `@ledger/s-1.jsonl`,
//! so a replay can move every root to a directory that does not exist
//! ([`Header::decode_remapped`]) and the same paths still come out under
//! them: whatever the replay would open that no record answers then fails.
//! A file the run was given by a RELATIVE path (`--journal run.jsonl`) is
//! one the loop opens from its own directory, so the header names it from
//! there ([`Header::capture`]): the same file, by a path a root can hold —
//! never written relative, where a replay started in the recording's
//! directory would open the live one. A remapped decode refuses any opened
//! file path that names no root, or leaves its root (`..`, an absolute
//! rest): a hand-made or damaged header cannot point a replay back at the
//! live ledger or journal. This is lexical relocation; a replay must still own its base and handle filesystem effects. The
//! approval policy's own home and `$TMPDIR` ([`super::super::run::ApprovalEnv`])
//! are VALUES it compares the screen's text with, not places it opens, and
//! are written as they are.
//!
//! OLDER AND NEWER TAPES. Keys are added every week (a `[harness]` key, a
//! supervisable agent, a setup knob), and a later build must still replay a
//! run an earlier one recorded, while an earlier build must refuse a tape
//! it cannot represent (a replay under a setup this build cannot spell would
//! explain a run that never happened). So decoding is strict about what it
//! cannot read and open about what it was not told:
//! * a key this build does not have, one said twice, a value that does not
//!   read, or a newer format is refused by name — an older build refusing a
//!   newer tape;
//! * a key this build has that the tape does not name is one the
//!   recording's build did not have yet. A policy key or an agent's switch
//!   keeps its full-power default, as an owner's file written before it is
//!   read ([`SupervisorConfig::rebuild`]); a policy key retired since is read
//!   as the owner's file reads it; a setup knob that arrived after the
//!   format did takes this build's shipped value (`Reader::or_shipped`),
//!   as every constant no key holds is this build's on a replay. Each is
//!   named in [`Header::supplied`], so a replay says what this build chose
//!   and the run never had;
//! * every other key of the format is required: its absence is a damaged
//!   header, and the `exact` line, always last, is what a torn one lacks.
//!
//! The format number therefore changes only with the text's grammar — a
//! line an older reader would misread — never for a key added or retired;
//! a setup field removed keeps its key taken (and dropped) by the reader.
//!
//! What a header never holds: the worker's environment (a stand-in for it is
//! written as its kind, `env.worker=fixed`, and comes back as one that says
//! it was never taped), the host behind [`SuperviseOpts::idle_host`] or the
//! hand-over flag's value (each written as its presence), and the clock — a
//! tape's records answer every read of it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::super::config::{PolicyTexts, SupervisorConfig};
use super::super::run::{SessionSetup, SuperviseOpts};

/// The header's format, the `atape <n>` of its first line. A build refuses a
/// tape whose format it does not read. It names the text's grammar, not its
/// keys: a key added or retired is no new format ([module docs](self)).
pub const FORMAT: u32 = 1;

/// The first line's word.
const MAGIC: &str = "atape";

/// Which loop a tape recorded: the window's own hosted loop
/// ([`super::super::run::Session::run_hosted`]), or the engine tests' `watch`
/// or `supervise` seams — an older build's `aterm drive watch|supervise`,
/// whose tapes still read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Watch,
    Supervise,
    Hosted,
}

impl Entry {
    /// The header's word for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Supervise => "supervise",
            Self::Hosted => "hosted",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::Watch, Self::Supervise, Self::Hosted]
            .into_iter()
            .find(|e| e.word() == word)
    }
}

/// The roots a header can name, in the order a tie between two that are the
/// same directory goes to: the first named keeps it.
const ROOT_IDS: [&str; 6] = ["ledger", "journal", "notes", "rules", "state", "home"];

/// No roots: a path written as it is — a root's own directory, and the
/// approval policy's home and `$TMPDIR`, values it compares the screen's
/// text with and never a place the loop opens.
pub(crate) const VERBATIM: Roots = Roots(Vec::new());

/// THE RUN'S ROOTS: each directory its own files live in, by a fixed name
/// ([`ROOT_IDS`]). A path under one is written `@<name>/<rest>` — under the
/// most specific, so the ledger's `<state>/drive` wins over the state root
/// that holds it — and any other as it is (one that itself starts with `@`
/// with a second `@` before it). Only absolute directories are roots; two
/// that are the same directory are one, under the first name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roots(Vec<(&'static str, PathBuf)>);

impl Roots {
    /// The roots of a run under this setup and these options: the
    /// directories of its ledger, journal and standing rules,
    /// then `state` (the state root the caller names: the process's own,
    /// `aterm_types::dirs::state_dir`, for a live run) and the approval
    /// policy's home.
    #[must_use]
    pub fn capture(state: Option<&Path>, setup: &SessionSetup, opts: &SuperviseOpts) -> Self {
        let dir_of = |file: Option<&Path>| file.and_then(Path::parent).map(Path::to_path_buf);
        let found = [
            dir_of(setup.ledger_path().or(opts.ledger.as_deref())),
            dir_of(opts.journal.as_deref()),
            // The notes file went with `aterm drive watch --notes`; its root's
            // name stays taken, so an older header reads.
            None,
            dir_of(opts.policy.rules_file.as_deref()),
            state.map(Path::to_path_buf),
            setup.approval_env().home.clone(),
        ];
        let mut roots: Vec<(&'static str, PathBuf)> = Vec::new();
        for (id, dir) in ROOT_IDS.into_iter().zip(found) {
            if let Some(dir) = dir.filter(|d| d.is_absolute())
                && !roots.iter().any(|(_, d)| *d == dir)
            {
                roots.push((id, dir));
            }
        }
        Self(roots)
    }

    /// Roots named outright, for a test: each directory under its name.
    #[cfg(test)]
    pub(crate) fn of(dirs: Vec<(&'static str, PathBuf)>) -> Self {
        Self(dirs)
    }

    /// The same roots moved under `base`, each to its RECORDED PATH beneath
    /// it (`/st/drive` to `<base>/st/drive`): what a replay resolves the
    /// header's paths against, so nothing it opens outside a record is the
    /// recorded run's own file.
    ///
    /// WHY THE WHOLE PATH, NOT THE ROOT'S NAME: the loop reasons about where
    /// its files sit — the ledger's directory is named `drive`, its parent is
    /// the state root the earlier tabs' ledgers are found under
    /// (`harness::upgrade_drive::lefts_beside_in`) — so a remap that renamed
    /// the ledger's directory to `<base>/ledger` sent a replay down another
    /// path than the recorded run took (the live test's first replay diverged
    /// there). Under the recorded path every name and every nesting of one
    /// root in another is kept, and the paths still encode as recorded.
    /// Windows prefix kinds and values get separate namespaces below the
    /// base, so distinct volumes or shares cannot alias after the move.
    #[must_use]
    pub fn remapped(&self, base: &Path) -> Self {
        Self(
            self.0
                .iter()
                .map(|(id, dir)| {
                    let under: PathBuf = dir
                        .components()
                        .filter(|c| matches!(c, std::path::Component::Normal(_)))
                        .collect();
                    #[cfg(windows)]
                    let under = windows_root_namespace(dir).join(under);
                    (*id, base.join(under))
                })
                .collect(),
        )
    }

    /// Each root, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Path)> {
        self.0.iter().map(|(id, dir)| (*id, dir.as_path()))
    }

    /// Whether `path` is under one of these roots.
    #[must_use]
    pub fn holds(&self, path: &Path) -> bool {
        self.0.iter().any(|(_, dir)| path.starts_with(dir))
    }

    /// `path` as a header writes it ([`Roots`]).
    #[must_use]
    pub fn encode(&self, path: &Path) -> Vec<u8> {
        let under = self
            .0
            .iter()
            .filter_map(|(id, dir)| path.strip_prefix(dir).ok().map(|rest| (*id, dir, rest)))
            .max_by_key(|(_, dir, _)| dir.components().count());
        match under {
            Some((id, _, rest)) => {
                let mut out = format!("@{id}").into_bytes();
                if !rest.as_os_str().is_empty() {
                    out.push(b'/');
                    out.extend(os_bytes(rest.as_os_str()));
                }
                out
            }
            None => {
                let bytes = os_bytes(path.as_os_str());
                if bytes.first() == Some(&b'@') {
                    [b"@".as_slice(), &bytes].concat()
                } else {
                    bytes
                }
            }
        }
    }

    /// What [`Self::encode`] wrote, as a path under these roots.
    ///
    /// # Errors
    ///
    /// A root this header does not name, or (off unix) a path that is not
    /// UTF-8.
    pub fn decode(&self, raw: &[u8]) -> Result<PathBuf, String> {
        if let Some(literal) = raw.strip_prefix(b"@@") {
            return path_of([b"@".as_slice(), literal].concat());
        }
        let Some(rooted) = raw.strip_prefix(b"@") else {
            return path_of(raw.to_vec());
        };
        let (id, rest) = match rooted.iter().position(|&b| b == b'/') {
            Some(at) => (&rooted[..at], Some(&rooted[at + 1..])),
            None => (rooted, None),
        };
        let Some((_, dir)) = self.0.iter().find(|(r, _)| r.as_bytes() == id) else {
            return Err(format!(
                "a path under @{}, a root the header does not name",
                String::from_utf8_lossy(id)
            ));
        };
        Ok(match rest {
            Some(rest) => dir.join(path_of(rest.to_vec())?),
            None => dir.clone(),
        })
    }

    /// A replay's file path must name one of its remapped roots. The ordinary
    /// byte codec above deliberately accepts verbatim values; this boundary does not.
    fn decode_file(&self, raw: &[u8]) -> Result<PathBuf, String> {
        if !raw.starts_with(b"@") || raw.starts_with(b"@@") {
            return Err("a remapped file path must name a root".to_string());
        }
        if let Some(at) = raw.iter().position(|&b| b == b'/') {
            let rest = path_of(raw[at + 1..].to_vec())?;
            if rest.components().any(|c| {
                !matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            }) {
                return Err("a remapped file path must stay below its root".to_string());
            }
        }
        self.decode(raw)
    }
}

/// Prefix-free Windows namespaces, followed by the recorded normal path.
/// Encoding each prefix field in bounded ASCII components avoids adopting a
/// server/share/device name as a path component or losing its exact bytes.
#[cfg(windows)]
fn windows_root_namespace(dir: &Path) -> PathBuf {
    use std::path::{Component, Prefix};

    fn named(kind: &str, parts: &[&std::ffi::OsStr]) -> PathBuf {
        let mut path = PathBuf::from(kind);
        for part in parts {
            path.push("part");
            for chunk in part.as_encoded_bytes().chunks(32) {
                let mut word = String::from("x");
                for byte in chunk {
                    let _ = write!(word, "{byte:02x}");
                }
                path.push(word);
            }
            path.push("end");
        }
        path
    }

    let mut path = match dir.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) => PathBuf::from(format!("disk-{drive:02x}")),
            Prefix::VerbatimDisk(drive) => PathBuf::from(format!("verbatim-disk-{drive:02x}")),
            Prefix::UNC(server, share) => named("unc", &[server, share]),
            Prefix::VerbatimUNC(server, share) => named("verbatim-unc", &[server, share]),
            Prefix::DeviceNS(device) => named("device", &[device]),
            Prefix::Verbatim(value) => named("verbatim", &[value]),
        },
        _ => PathBuf::from("no-prefix"),
    };
    path.push("path");
    path
}

/// The path the loop opens for a file it was given as `path`: an absolute
/// one as it is; a relative one (`--journal run.jsonl`, a `rules_file` of
/// `standing.md`) from this process's directory, which every open of it
/// resolves against and which the loop never changes — the same file, named
/// so a root can hold it ([`Header::capture`]). Lexical, as the open is:
/// nothing is looked up, so a file not yet made still has its name. One this
/// process cannot name (its directory is gone) stays as given, a path under
/// no root, which a remapped decode refuses.
fn opened(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(unix)]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(s).to_vec()
}

/// Off unix a path is written as its text, a character it cannot spell
/// replaced: the supervisor runs on unix hosts.
#[cfg(not(unix))]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    s.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn path_of(bytes: Vec<u8>) -> Result<PathBuf, String> {
    Ok(PathBuf::from(
        <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes),
    ))
}

#[cfg(not(unix))]
fn path_of(bytes: Vec<u8>) -> Result<PathBuf, String> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| "a path that is not UTF-8".to_string())
}

/// One value's bytes as one line's text ([module docs](self)).
pub(crate) fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if aterm_codec::tty::is_active(c) => {
                    for b in c.encode_utf8(&mut [0; 4]).bytes() {
                        let _ = write!(out, "\\x{b:02x}");
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            let _ = write!(out, "\\x{b:02x}");
        }
    }
    out
}

/// [`escape`]'s inverse.
pub(crate) fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'\\') => out.push(b'\\'),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            Some(b't') => out.push(b'\t'),
            Some(b'x') => {
                let hex = [bytes.next(), bytes.next()];
                let byte = match hex {
                    [Some(h), Some(l)] => std::str::from_utf8(&[h, l])
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok()),
                    _ => None,
                };
                out.push(byte.ok_or_else(|| format!("a broken \\x escape in {text:?}"))?);
            }
            _ => return Err(format!("a broken escape in {text:?}")),
        }
    }
    Ok(out)
}

/// A duration as a header writes it: exact, in the largest unit that holds
/// it, and `max` for [`Duration::MAX`] (a budget that never runs out).
fn duration_text(d: Duration) -> String {
    if d == Duration::MAX {
        return "max".to_string();
    }
    let ns = d.as_nanos();
    for (unit, per) in [("s", 1_000_000_000), ("ms", 1_000_000), ("us", 1_000)] {
        if ns.is_multiple_of(per) {
            return format!("{}{unit}", ns / per);
        }
    }
    format!("{ns}ns")
}

/// [`duration_text`]'s inverse.
fn parse_duration(text: &str) -> Option<Duration> {
    if text == "max" {
        return Some(Duration::MAX);
    }
    let (digits, per) = [
        ("ns", 1),
        ("us", 1_000),
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
    ]
    .into_iter()
    .find_map(|(unit, per)| text.strip_suffix(unit).map(|d| (d, per)))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ns = digits.parse::<u128>().ok()?.checked_mul(per)?;
    let secs = u64::try_from(ns / 1_000_000_000).ok()?;
    let nanos = u32::try_from(ns % 1_000_000_000).ok()?;
    Some(Duration::new(secs, nanos))
}

/// The header's text as it is written, section by section
/// ([module docs](self)).
#[derive(Debug, Default)]
pub(crate) struct Writer {
    out: String,
}

impl Writer {
    pub(crate) fn section(&mut self, name: &str) {
        let _ = writeln!(self.out, "[{name}]");
    }

    /// `key=value`, or `key` alone where the run had no value.
    fn raw(&mut self, key: &str, value: Option<&[u8]>) {
        match value {
            Some(v) => {
                let _ = writeln!(self.out, "{key}={}", escape(v));
            }
            None => {
                let _ = writeln!(self.out, "{key}");
            }
        }
    }

    pub(crate) fn text(&mut self, key: &str, value: &str) {
        self.raw(key, Some(value.as_bytes()));
    }

    pub(crate) fn opt_text(&mut self, key: &str, value: Option<&str>) {
        self.raw(key, value.map(str::as_bytes));
    }

    /// A number or a switch, by its `Display`.
    pub(crate) fn shown(&mut self, key: &str, value: impl std::fmt::Display) {
        self.text(key, &value.to_string());
    }

    pub(crate) fn duration(&mut self, key: &str, value: Duration) {
        self.text(key, &duration_text(value));
    }

    /// An optional duration (a test's knob, carried by every build).
    pub(crate) fn opt_duration(&mut self, key: &str, value: Option<Duration>) {
        self.opt_text(key, value.map(duration_text).as_deref());
    }

    /// A list of durations, one space between each (none: `key=`).
    pub(crate) fn durations(&mut self, key: &str, values: &[Duration]) {
        let words: Vec<String> = values.iter().map(|d| duration_text(*d)).collect();
        self.text(key, &words.join(" "));
    }

    pub(crate) fn path(&mut self, key: &str, value: &Path, roots: &Roots) {
        self.raw(key, Some(&roots.encode(value)));
    }

    pub(crate) fn opt_path(&mut self, key: &str, value: Option<&Path>, roots: &Roots) {
        self.raw(key, value.map(|p| roots.encode(p)).as_deref());
    }

    /// A list of texts: its length as `key`, then each at `key.<i>`.
    pub(crate) fn texts(&mut self, key: &str, values: &[String]) {
        self.shown(key, values.len());
        for (i, v) in values.iter().enumerate() {
            self.text(&format!("{key}.{i}"), v);
        }
    }
}

/// One line of a header being read.
#[derive(Debug)]
struct Line {
    section: String,
    key: String,
    /// Still escaped; `None` for a key alone.
    value: Option<String>,
    /// 1-based, counting the `atape` line.
    at: usize,
    taken: bool,
}

/// A header's lines, taken one key at a time from the section last named
/// ([`Self::section`]); what is left untaken at the end is a key this build
/// does not have ([`Self::finish`]).
#[derive(Debug)]
pub(crate) struct Reader {
    lines: Vec<Line>,
    section: String,
    remapped_io: bool,
    /// Each `[section] key` the header did not name that this build
    /// supplied ([`Self::or_shipped`]).
    supplied: Vec<String>,
}

impl Reader {
    /// The lines after the `atape` line, each `[section]`, `key=value` or
    /// `key`; a key said twice in one section is refused.
    fn parse(body: &str) -> Result<Self, String> {
        let mut lines: Vec<Line> = Vec::new();
        let mut section = String::new();
        for (i, line) in body.split_terminator('\n').enumerate() {
            let at = i + 2;
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_string();
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k, Some(v.to_string())),
                None => (line, None),
            };
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.".contains(&b))
            {
                return Err(format!("atape header line {at}: not a key: {line:?}"));
            }
            if lines.iter().any(|l| l.section == section && l.key == key) {
                return Err(format!(
                    "atape header line {at}: [{section}] {key} said twice"
                ));
            }
            lines.push(Line {
                section: section.clone(),
                key: key.to_string(),
                value,
                at,
                taken: false,
            });
        }
        Ok(Self {
            lines,
            section: String::new(),
            remapped_io: false,
            supplied: Vec::new(),
        })
    }

    /// `key` read by `read` where the section names it. Where it does not,
    /// the tape is from a build before the key — one that ran with the value
    /// this field had no knob for — and `shipped`, this build's own value, is
    /// what a replay runs with, as it does every constant no key holds; the
    /// key is named in [`Header::supplied`]. Only a key that arrived after
    /// the format did is read this way: every other is required
    /// ([module docs](self)).
    pub(crate) fn or_shipped<T>(
        &mut self,
        key: &str,
        shipped: T,
        read: impl FnOnce(&mut Self, &str) -> Result<T, String>,
    ) -> Result<T, String> {
        let section = &self.section;
        if self
            .lines
            .iter()
            .any(|l| l.section == *section && l.key == key)
        {
            read(self, key)
        } else {
            self.supplied.push(format!("[{section}] {key}"));
            Ok(shipped)
        }
    }

    /// A retired duration, if present: validate its old wire type and consume
    /// it, without supplying a value or reviving a removed setup field.
    pub(crate) fn retired_duration(&mut self, key: &str) -> Result<(), String> {
        if self
            .lines
            .iter()
            .any(|line| line.section == self.section && line.key == key)
        {
            let _ = self.duration(key)?;
        }
        Ok(())
    }

    /// A retired text (a value, an empty one or none), if present: validate
    /// its old wire type and consume it, as [`Self::retired_duration`] does.
    pub(crate) fn retired_text(&mut self, key: &str) -> Result<(), String> {
        if self
            .lines
            .iter()
            .any(|line| line.section == self.section && line.key == key)
        {
            let _ = self.opt_text(key)?;
        }
        Ok(())
    }

    /// A retired switch or number, if present, read by its old type `T`, as
    /// [`Self::retired_duration`] does.
    pub(crate) fn retired_parsed<T: std::str::FromStr>(&mut self, key: &str) -> Result<(), String> {
        if self
            .lines
            .iter()
            .any(|line| line.section == self.section && line.key == key)
        {
            let _ = self.parsed::<T>(key)?;
        }
        Ok(())
    }

    /// Take keys from `[name]` from here on.
    pub(crate) fn section(&mut self, name: &str) {
        self.section = name.to_string();
    }

    /// `key`'s value, unescaped (`None`: the key alone), and its line.
    fn take(&mut self, key: &str) -> Result<(Option<Vec<u8>>, usize), String> {
        let section = &self.section;
        let Some(line) = self
            .lines
            .iter_mut()
            .find(|l| l.section == *section && l.key == key)
        else {
            return Err(format!("atape header: [{section}] names no {key}"));
        };
        line.taken = true;
        let value = line
            .value
            .as_deref()
            .map(unescape)
            .transpose()
            .map_err(|e| format!("atape header line {}: {key}: {e}", line.at))?;
        Ok((value, line.at))
    }

    fn bad(&self, at: usize, key: &str, what: &str) -> String {
        format!("atape header line {at}: [{}] {key}: {what}", self.section)
    }

    fn opt_text_at(&mut self, key: &str) -> Result<(Option<String>, usize), String> {
        let (value, at) = self.take(key)?;
        let value = value
            .map(String::from_utf8)
            .transpose()
            .map_err(|_| self.bad(at, key, "not UTF-8"))?;
        Ok((value, at))
    }

    pub(crate) fn opt_text(&mut self, key: &str) -> Result<Option<String>, String> {
        self.opt_text_at(key).map(|(v, _)| v)
    }

    fn text_at(&mut self, key: &str) -> Result<(String, usize), String> {
        match self.opt_text_at(key)? {
            (Some(v), at) => Ok((v, at)),
            (None, at) => Err(self.bad(at, key, "has no value")),
        }
    }

    pub(crate) fn text(&mut self, key: &str) -> Result<String, String> {
        self.text_at(key).map(|(v, _)| v)
    }

    /// A number or a switch, by its `FromStr`.
    pub(crate) fn parsed<T: std::str::FromStr>(&mut self, key: &str) -> Result<T, String> {
        let (text, at) = self.text_at(key)?;
        text.parse()
            .map_err(|_| self.bad(at, key, &format!("cannot be read: {text:?}")))
    }

    pub(crate) fn duration(&mut self, key: &str) -> Result<Duration, String> {
        let (text, at) = self.text_at(key)?;
        parse_duration(&text).ok_or_else(|| self.bad(at, key, &format!("no duration: {text:?}")))
    }

    /// [`Writer::opt_duration`]'s, read back.
    pub(crate) fn opt_duration(&mut self, key: &str) -> Result<Option<Duration>, String> {
        let (text, at) = self.opt_text_at(key)?;
        text.map(|text| {
            parse_duration(&text)
                .ok_or_else(|| self.bad(at, key, &format!("no duration: {text:?}")))
        })
        .transpose()
    }

    pub(crate) fn durations(&mut self, key: &str) -> Result<Vec<Duration>, String> {
        let (text, at) = self.text_at(key)?;
        text.split_whitespace()
            .map(|w| {
                parse_duration(w).ok_or_else(|| self.bad(at, key, &format!("no duration: {w:?}")))
            })
            .collect()
    }

    pub(crate) fn opt_path(&mut self, key: &str, roots: &Roots) -> Result<Option<PathBuf>, String> {
        let (value, at) = self.take(key)?;
        value
            .map(|raw| roots.decode(&raw).map_err(|e| self.bad(at, key, &e)))
            .transpose()
    }

    /// Files the loop opens use the strict boundary only for a replay remap.
    /// Approval environment values continue through `opt_path` above unchanged.
    fn io_path(&self, raw: &[u8], roots: &Roots) -> Result<PathBuf, String> {
        if self.remapped_io {
            roots.decode_file(raw)
        } else {
            roots.decode(raw)
        }
    }

    pub(crate) fn opt_io_path(
        &mut self,
        key: &str,
        roots: &Roots,
    ) -> Result<Option<PathBuf>, String> {
        let (value, at) = self.take(key)?;
        value
            .map(|raw| self.io_path(&raw, roots).map_err(|e| self.bad(at, key, &e)))
            .transpose()
    }

    pub(crate) fn texts(&mut self, key: &str) -> Result<Vec<String>, String> {
        let n: usize = self.parsed(key)?;
        (0..n).map(|i| self.text(&format!("{key}.{i}"))).collect()
    }

    /// Every key of the section that starts `prefix` and is not yet taken,
    /// taken, in order: `(rest of the key, value, line)`.
    fn prefixed(&mut self, prefix: &str) -> Vec<(String, Option<String>, usize)> {
        let section = &self.section;
        self.lines
            .iter_mut()
            .filter(|l| l.section == *section && !l.taken && l.key.starts_with(prefix))
            .map(|l| {
                l.taken = true;
                (l.key[prefix.len()..].to_string(), l.value.clone(), l.at)
            })
            .collect()
    }

    /// A line [`Self::prefixed`] took, unescaped: it must have a value.
    fn prefixed_value(&self, key: &str, value: Option<&str>, at: usize) -> Result<Vec<u8>, String> {
        value
            .map(unescape)
            .transpose()
            .map_err(|e| self.bad(at, key, &e))?
            .ok_or_else(|| self.bad(at, key, "has no value"))
    }

    /// Every key was taken: one left is a key this build does not have.
    fn finish(&self) -> Result<(), String> {
        match self.lines.iter().find(|l| !l.taken) {
            Some(l) => Err(format!(
                "atape header line {}: [{}] {}: a key this build does not have",
                l.at, l.section, l.key
            )),
            None => Ok(()),
        }
    }
}

/// [module docs](self).
#[derive(Debug, Clone)]
pub struct Header {
    /// The tape's own id.
    pub tape: String,
    /// The build that recorded it (`aterm_types::version::build_id`).
    pub build: String,
    /// The OS and architecture it ran on.
    pub os: String,
    pub entry: Entry,
    /// The roots its paths are written under — on a decode, the ones they
    /// were resolved against (remapped ones, for [`Self::decode_remapped`]).
    pub roots: Roots,
    pub setup: SessionSetup,
    /// The options, [`SuperviseOpts::idle_host`] always `None`: whether the
    /// run had one is [`Self::idle_host`].
    pub opts: SuperviseOpts,
    /// The run had a host behind [`SuperviseOpts::idle_host`].
    pub idle_host: bool,
    /// The policy's rebuild gave back the very policy the run had
    /// ([module docs](self)).
    pub policy_exact: bool,
    /// What this build supplied that the tape did not say — a tape from a
    /// build before a key ([module docs](self)): each `[section] key` read
    /// at this build's value, and each policy key retired since with the
    /// note its reading gave. Empty for a header this build captured; never
    /// written.
    pub supplied: Vec<String>,
}

impl Header {
    /// The header of a run about to start: the tape's id, which loop, the
    /// state root ([`Roots::capture`]) and the run's setup and options, the
    /// policy's rebuild checked once, here.
    ///
    /// Each file the loop opens — the ledger, the journal, the standing
    /// rules — is held by the path it opens (`opened`): one given
    /// relative is named from this process's directory, the one the loop
    /// opens it from, so it has a root a replay moves.
    #[must_use]
    pub fn capture(
        tape: impl Into<String>,
        entry: Entry,
        state: Option<&Path>,
        setup: &SessionSetup,
        opts: &SuperviseOpts,
    ) -> Self {
        let idle_host = opts.idle_host.is_some();
        let setup = setup.with_ledger_path(setup.ledger_path().map(opened));
        let mut opts = SuperviseOpts {
            idle_host: None,
            ..opts.clone()
        };
        for file in [
            &mut opts.tasks,
            &mut opts.journal,
            &mut opts.ledger,
            &mut opts.policy.rules_file,
        ] {
            if let Some(path) = file.as_mut() {
                *path = opened(path);
            }
        }
        let policy_exact = SupervisorConfig::rebuild(&opts.policy.policy_texts())
            .is_ok_and(|back| back.policy == opts.policy);
        Self {
            tape: tape.into(),
            build: aterm_types::version::build_id(),
            os: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            entry,
            roots: Roots::capture(state, &setup, &opts),
            setup,
            opts,
            idle_host,
            policy_exact,
            supplied: Vec::new(),
        }
    }

    /// The header's text ([module docs](self)).
    #[must_use]
    pub fn encode(&self) -> String {
        let Self {
            tape,
            build,
            os,
            entry,
            roots,
            setup,
            opts,
            idle_host,
            policy_exact,
            // What a decode found missing: this build writes every key.
            supplied: _,
        } = self;
        let mut w = Writer::default();
        let _ = writeln!(w.out, "{MAGIC} {FORMAT}");
        w.text("tape", tape);
        w.text("build", build);
        w.text("os", os);
        w.text("entry", entry.word());
        w.section("roots");
        for (id, dir) in roots.iter() {
            w.path(id, dir, &VERBATIM);
        }
        setup.write(&mut w, roots, digest_seed(tape));
        let SuperviseOpts {
            max,
            python_allow,
            context_warn,
            journal,
            ledger,
            policy,
            yield_when_held,
            idle_host: _,
            tasks,
        } = opts;
        w.section("opts");
        w.duration("max", *max);
        w.texts("python_allow", python_allow);
        w.shown("context_warn", context_warn);
        w.opt_path("journal", journal.as_deref(), roots);
        w.opt_path("ledger", ledger.as_deref(), roots);
        // The directory a done's result is written under: a value the
        // result's round trip names (`doors::TASK_RESULT`), never opened by a
        // replay.
        w.opt_path("tasks", tasks.as_deref(), roots);
        w.shown("yield_when_held", yield_when_held);
        w.shown("idle_host", idle_host);
        w.section("policy");
        let PolicyTexts {
            keys,
            agents,
            withheld,
        } = policy.policy_texts();
        for (key, value) in &keys {
            // The standing rules are a file the loop reads: under a root,
            // as every path it opens.
            if key == RULES_FILE && !value.is_empty() {
                w.path(key, Path::new(value), roots);
            } else {
                w.text(key, value);
            }
        }
        for (key, value) in &agents {
            w.text(key, value);
        }
        for (key, value) in &withheld {
            w.text(&format!("{WITHHELD}{key}"), value);
        }
        w.shown("exact", policy_exact);
        w.out
    }

    /// A header's text, read back ([module docs](self)).
    ///
    /// # Errors
    ///
    /// Not a header, a format this build does not read, or a key missing,
    /// unknown, said twice or unreadable — each named with its line.
    pub fn decode(text: &str) -> Result<Self, String> {
        Self::decode_under(text, None)
    }

    /// [`Self::decode`] with every root moved under `base`
    /// ([`Roots::remapped`]): the header a replay builds its loop from.
    ///
    /// # Errors
    ///
    /// As [`Self::decode`], plus any opened file path that names no root or
    /// escapes one lexically. Approval-environment values stay verbatim.
    /// This does not check filesystem symlinks: the replay must own its base.
    pub fn decode_remapped(text: &str, base: &Path) -> Result<Self, String> {
        Self::decode_under(text, Some(base))
    }

    fn decode_under(text: &str, base: Option<&Path>) -> Result<Self, String> {
        let (first, body) = text.split_once('\n').unwrap_or((text, ""));
        let format = first
            .strip_prefix(MAGIC)
            .and_then(|f| f.strip_prefix(' '))
            .and_then(|f| f.parse::<u32>().ok())
            .ok_or_else(|| format!("not a tape header: {first:?}"))?;
        if format != FORMAT {
            return Err(format!(
                "a tape header of format {format}: this build reads {MAGIC} {FORMAT}"
            ));
        }
        let mut r = Reader::parse(body)?;
        r.remapped_io = base.is_some();
        let tape = r.text("tape")?;
        let build = r.text("build")?;
        let os = r.text("os")?;
        let (word, at) = r.text_at("entry")?;
        let entry =
            Entry::parse(&word).ok_or_else(|| r.bad(at, "entry", "no loop of this build's"))?;
        r.section("roots");
        let mut recorded = Vec::new();
        for (id, value, _) in r.prefixed("") {
            let Some(id) = ROOT_IDS.into_iter().find(|r| *r == id) else {
                return Err(format!(
                    "atape header: [roots] {id}: no root this build names"
                ));
            };
            let raw = value
                .as_deref()
                .map(unescape)
                .transpose()?
                .ok_or_else(|| format!("atape header: [roots] {id} has no directory"))?;
            recorded.push((id, VERBATIM.decode(&raw)?));
        }
        let recorded = Roots(recorded);
        let roots = match base {
            Some(base) => recorded.remapped(base),
            None => recorded,
        };
        let setup = SessionSetup::read(&mut r, &roots)?;
        r.section("opts");
        let max = r.duration("max")?;
        let python_allow = r.texts("python_allow")?;
        // Retired with `aterm drive watch --notes|--report|--mail`: an older
        // build's header reads, its values dropped (a recording refused
        // `--mail`, so no tape has the lane's keys on).
        r.retired_text("notes")?;
        r.retired_parsed::<bool>("report")?;
        r.retired_text("mail")?;
        r.retired_text("mail.inbox")?;
        r.retired_duration("mail.report_window")?;
        r.retired_duration("mail.idle_grace")?;
        let context_warn = r.parsed("context_warn")?;
        let journal = r.opt_io_path("journal", &roots)?;
        let ledger = r.opt_io_path("ledger", &roots)?;
        // Arrived after the format did: a tape from before wrote no result.
        let tasks = r.or_shipped("tasks", None, |r, key| r.opt_path(key, &roots))?;
        let yield_when_held = r.parsed("yield_when_held")?;
        let idle_host = r.parsed("idle_host")?;
        r.section("policy");
        // The verdict first: always written (`exact` last of all, what a torn
        // header lacks); an older build's `session_questions` beside it, a
        // switch no key sets any more, read and dropped. Then the withheld
        // rules by their prefix, and every line left is one of the policy's
        // texts — a `[harness]` key, or an agent's `<agent>.<key>` — in the
        // order written: which keys there are is the recording build's, and
        // the rebuild reads them as the owner's file is read, a key this build
        // has and they lack at its default and one it lacks refused
        // ([`SupervisorConfig::rebuild`]).
        r.retired_parsed::<bool>("session_questions")?;
        let policy_exact = r.parsed("exact")?;
        let mut withheld = Vec::new();
        for (key, value, at) in r.prefixed(WITHHELD) {
            let raw = r.prefixed_value(&format!("{WITHHELD}{key}"), value.as_deref(), at)?;
            let value = String::from_utf8(raw)
                .map_err(|_| r.bad(at, &format!("{WITHHELD}{key}"), "not UTF-8"))?;
            withheld.push((key, value));
        }
        let mut keys = Vec::new();
        let mut agents = Vec::new();
        for (key, value, at) in r.prefixed("") {
            let raw = r.prefixed_value(&key, value.as_deref(), at)?;
            let value = if key == RULES_FILE && !raw.is_empty() {
                // The standing rules are a file the loop reads: under a root,
                // as every path it opens.
                r.io_path(&raw, &roots)
                    .map_err(|e| r.bad(at, &key, &e))?
                    .to_str()
                    .ok_or_else(|| r.bad(at, &key, "not UTF-8"))?
                    .to_string()
            } else {
                String::from_utf8(raw).map_err(|_| r.bad(at, &key, "not UTF-8"))?
            };
            if key.contains('.') {
                agents.push((key, value));
            } else {
                keys.push((key, value));
            }
        }
        r.finish()?;
        let rebuilt = SupervisorConfig::rebuild(&PolicyTexts {
            keys,
            agents,
            withheld,
        })
        .map_err(|e| format!("atape header: [policy] {e}"))?;
        let mut supplied = r.supplied;
        supplied.extend(rebuilt.supplied.iter().map(|key| format!("[policy] {key}")));
        supplied.extend(
            rebuilt
                .retired
                .iter()
                .map(|note| format!("[policy] {note}")),
        );
        Ok(Self {
            tape,
            build,
            os,
            entry,
            roots,
            setup,
            opts: SuperviseOpts {
                max,
                python_allow,
                context_warn,
                journal,
                ledger,
                policy: rebuilt.policy,
                yield_when_held,
                idle_host: None,
                tasks,
            },
            idle_host,
            policy_exact,
            supplied,
        })
    }
}

/// The seed of the worker's digest on the tape whose id is `tape`
/// (`WorkerEnv::digest`): an xxh64 of the id, so the same worker reads as
/// another digest on another tape. The recorder, the header and a replay
/// each seed with it, so a replay names the worker as the recording did.
#[must_use]
pub(crate) fn digest_seed(tape: &str) -> u64 {
    aterm_zstd::xxh64::xxh64(0, tape.as_bytes())
}

/// The policy key that names a file the loop reads ([`Header::encode`]).
const RULES_FILE: &str = "rules_file";

/// The prefix of a withheld rule's line in `[policy]`.
const WITHHELD: &str = "withheld.";

#[cfg(test)]
#[path = "header_tests.rs"]
mod tests;
