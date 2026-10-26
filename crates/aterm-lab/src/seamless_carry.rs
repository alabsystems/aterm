// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `seamless`'s privacy leg, the carry side: every file a seamless update wrote for its
//! successor, read from the copies the outgoing process kept through its development seam
//! (`ATERM_DEBUG_SEAMLESS_KEEP`, `aterm_gui::seamless::keep_attempt_in`), since the successor
//! consumes the originals; and each planted sentinel (`QZP6SECRET<n>`) held to the places the
//! design keeps it.
//!
//! Two kinds of text cross an update, and the sweep tells them apart by where in the carry it
//! reads:
//!
//! * **The session log's own carry**, the control sidecar's `log` member, is encoded at the Host
//!   tier (DESIGN-session-log §3.6, §5.2): a post's `key=` and `via=` hop (`idem=`, `via=`) may
//!   be there, and nothing of the Content tier (a title, an attention text, a topic name, a hold
//!   reason) or of what is never stored (a command's later word, a mail body).
//! * **The session's and the terminal's own state**, which the update hands its successor so the
//!   successor shows and does what the predecessor did: the title the tab and the terminal hold,
//!   the attention text the person set, the topics the session listens on, its standing holds,
//!   a post still waiting for a bridge, the terminal's command marks (OSC 633;E), and the screen
//!   itself (its grids, scrollback and archive rows, which hold what the shell echoed). Each is
//!   named below with the field that holds it; text anywhere else is a leak. The same state is
//!   what the restore store keeps on this machine at rest (`session.toml` and the crash journal's
//!   leaves hold the titles, the attention text and the holds; the crash restore's journal holds
//!   the screens and the bytes the terminal read), and the bridge's state keeps the topic names:
//!   §3.6's tiers say where the session log stores its keys, and this sweep holds the log's carry
//!   to them.
//!
//! Every file must be 0600 in a 0700 directory, every file the seam listed must have been read,
//! and none may outlive the Commit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The sentinels (`QZP6SECRET<n>`, any case) `bytes` holds, by their digit.
pub(crate) fn sentinels(bytes: &[u8]) -> Vec<u8> {
    let low = bytes.to_ascii_lowercase();
    let needle = b"qzp6secret";
    let mut out: Vec<u8> = low
        .windows(needle.len() + 1)
        .filter(|w| w.starts_with(needle) && w[needle.len()].is_ascii_digit())
        .map(|w| w[needle.len()] - b'0')
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// One file the outgoing process kept: its name, its mode as the original had it, its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KeptFile {
    pub(crate) name: String,
    pub(crate) mode: u32,
    pub(crate) bytes: Vec<u8>,
}

/// One kept attempt: the control directory the files were written in and its mode, the files,
/// and the names the seam listed but did not read (not a regular file, or past its cap).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Kept {
    pub(crate) dir: Option<(u32, String)>,
    pub(crate) files: Vec<KeptFile>,
    pub(crate) skipped: Vec<String>,
}

/// The kept attempt in `folder`: its `index` read (`dir <mode> <path>`, `file <mode> <len>
/// <name>`, `skip <name>`, modes in octal), and each file it lists read beside it.
pub(crate) fn read_kept(folder: &Path) -> Result<Kept, String> {
    let index = std::fs::read_to_string(folder.join("index"))
        .map_err(|e| format!("{}: {e}", folder.join("index").display()))?;
    let mut kept = Kept::default();
    for row in index.lines() {
        let mut w = row.splitn(2, ' ');
        let (Some(word), Some(rest)) = (w.next(), w.next()) else {
            return Err(format!("the index row `{row}` names nothing"));
        };
        let octal = |m: &str| {
            u32::from_str_radix(m, 8).map_err(|_| format!("the index row `{row}` has no mode"))
        };
        match word {
            "dir" => {
                let (mode, path) = rest
                    .split_once(' ')
                    .ok_or_else(|| format!("the index row `{row}` has no path"))?;
                kept.dir = Some((octal(mode)?, path.to_string()));
            }
            "file" => {
                let mut f = rest.splitn(3, ' ');
                let (Some(mode), Some(len), Some(name)) = (f.next(), f.next(), f.next()) else {
                    return Err(format!(
                        "the index row `{row}` is not mode, length and name"
                    ));
                };
                let bytes = std::fs::read(folder.join(name))
                    .map_err(|e| format!("the kept {name}: {e}"))?;
                if len.parse::<usize>().ok() != Some(bytes.len()) {
                    return Err(format!(
                        "the kept {name} holds {} bytes, the index says {len}",
                        bytes.len()
                    ));
                }
                kept.files.push(KeptFile {
                    name: name.to_string(),
                    mode: octal(mode)?,
                    bytes,
                });
            }
            "skip" => kept.skipped.push(rest.to_string()),
            _ => return Err(format!("the index row `{row}` is no dir, file or skip")),
        }
    }
    Ok(kept)
}

/// What a carry file is, by its name's ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// `seamless-<pid>-<nonce>.toml`: the nonce, then the sessions' records (TOML).
    Manifest,
    /// `.layout.toml`: the windows and tabs the successor restores.
    Layout,
    /// `.s<local>.ctl`, `.h<local>.ctl`: a session's turn ledger, archive tail and session log.
    Control,
    /// The screens and scrollback: `.grid`, `.altgrid`, `.heldgrid`, `.heldalt`, `.hist`.
    Screen,
    /// `.person`: each session's age of its last person's key.
    Person,
    /// Anything else the attempt wrote.
    Other,
}

impl Kind {
    pub(crate) fn of(name: &str) -> Kind {
        if name.ends_with(".layout.toml") {
            Kind::Layout
        } else if name.ends_with(".toml") {
            Kind::Manifest
        } else if name.ends_with(".ctl") {
            Kind::Control
        } else if [".grid", ".altgrid", ".heldgrid", ".heldalt", ".hist"]
            .iter()
            .any(|s| name.ends_with(s))
        {
            Kind::Screen
        } else if name.ends_with(".person") {
            Kind::Person
        } else {
            Kind::Other
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Kind::Manifest => "manifest",
            Kind::Layout => "layout",
            Kind::Control => "control",
            Kind::Screen => "screen",
            Kind::Person => "person",
            Kind::Other => "other",
        }
    }
}

/// Where in a carry file the design keeps a sentinel, by its digit: `path` is the place with
/// every array index written `[]`, a JSON value held in a string read past a `>`.
///
/// The session log's carry (`log[]` of a control sidecar) is encoded at the Host tier, so only a
/// post's key (8) and hop (9) are there. The rest is the session's and the terminal's own state,
/// which the update carries so the successor holds what the predecessor held: the store's and the
/// terminal's title (2), the attention text the person set (3, and each owner's in
/// `attention_owners`), the topics the session listens on (4), its standing holds with their
/// reasons (5), a post still waiting for a bridge (its body 6, key 8 and hop 9), the terminal's
/// command marks, which keep the command line a shell reported (OSC 633;E, 1) for `blocktext`,
/// and the screen (a grid, the scrollback, a control sidecar's `archive` rows), which holds the
/// command line the shell echoed (1).
pub(crate) fn kept_at(kind: Kind, path: &str) -> &'static [u8] {
    let meta = |rest: &str| -> &'static [u8] {
        if matches!(rest, "title.window" | "title.icon" | "title.stack[][]") {
            &[2]
        } else if rest.starts_with("shell.") && rest.ends_with(".commandline") {
            &[1]
        } else {
            &[]
        }
    };
    match kind {
        Kind::Control if path == "log[]" => &[8, 9],
        Kind::Control if path.starts_with("archive.") => &[1],
        Kind::Screen => &[1],
        Kind::Manifest => match path {
            "sessions[].title" => &[2],
            "sessions[].attention" | "sessions[].attention_owners[]" => &[3],
            "sessions[].topics[]" => &[4],
            "sessions[].hold" | "sessions[].more_holds[]" => &[5],
            WAITING_BODY => &[6],
            "sessions[].fabric.posts[].key" => &[8],
            "sessions[].fabric.posts[].via" => &[9],
            _ => match path
                .strip_prefix("sessions[].screen.meta>")
                .or_else(|| path.strip_prefix("held[].screen.meta>"))
            {
                Some(rest) => meta(rest),
                None => &[],
            },
        },
        Kind::Layout => {
            let last = path.rsplit('.').next().unwrap_or(path);
            match last {
                "title" | "user_title" => &[2],
                "attention" => &[3],
                "holds[]" => &[5],
                _ => &[],
            }
        }
        _ => &[],
    }
}

/// A waiting post's body in the manifest ([`kept_at`]).
const WAITING_BODY: &str = "sessions[].fabric.posts[].body";

/// Every string a parsed carry file holds, by its place (as [`kept_at`] reads it).
fn toml_leaves(value: &aterm_toml::Value, at: &str, out: &mut Vec<(String, String)>) {
    match value {
        aterm_toml::Value::String(s) => string_leaf(s, at, out),
        aterm_toml::Value::Array(items) => {
            for item in items {
                toml_leaves(item, &format!("{at}[]"), out);
            }
        }
        aterm_toml::Value::Table(table) => {
            for (k, v) in table {
                toml_leaves(v, &join(at, k), out);
            }
        }
        _ => {}
    }
}

fn json_leaves(value: &aterm_json::Value, at: &str, out: &mut Vec<(String, String)>) {
    match value {
        aterm_json::Value::String(s) => string_leaf(s, at, out),
        aterm_json::Value::Array(items) => {
            for item in items {
                json_leaves(item, &format!("{at}[]"), out);
            }
        }
        aterm_json::Value::Object(map) => {
            for (k, v) in map {
                json_leaves(v, &join(at, k), out);
            }
        }
        _ => {}
    }
}

/// A string, or the JSON value it holds (a checkpoint's meta, a wakes row) read past a `>`.
fn string_leaf(s: &str, at: &str, out: &mut Vec<(String, String)>) {
    let t = s.trim_start();
    if (t.starts_with('{') || t.starts_with('['))
        && let Ok(v) = aterm_json::from_str::<aterm_json::Value>(s)
    {
        json_leaves(&v, &format!("{at}>"), out);
        return;
    }
    out.push((at.to_string(), s.to_string()));
}

fn join(at: &str, key: &str) -> String {
    if at.is_empty() || at.ends_with('>') {
        format!("{at}{key}")
    } else {
        format!("{at}.{key}")
    }
}

/// The places in `file` and the text at each: a manifest's and a layout's TOML, a control
/// sidecar's JSON, or the whole file at `-` (a screen, the person stamps, or one that does not
/// parse as its kind, which [`kept_at`] reads as its kind's whole file).
fn places(kind: Kind, bytes: &[u8]) -> Vec<(String, String)> {
    let whole = || vec![("-".to_string(), String::from_utf8_lossy(bytes).into_owned())];
    let Ok(text) = std::str::from_utf8(bytes) else {
        return whole();
    };
    let mut out = Vec::new();
    match kind {
        Kind::Manifest => {
            let toml = text.split_once('\n').map_or("", |(_, rest)| rest);
            match aterm_toml::from_str::<aterm_toml::Value>(toml) {
                Ok(v) => toml_leaves(&v, "", &mut out),
                Err(_) => return whole(),
            }
        }
        Kind::Layout => match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(v) => toml_leaves(&v, "", &mut out),
            Err(_) => return whole(),
        },
        Kind::Control => match aterm_json::from_str::<aterm_json::Value>(text) {
            Ok(v) => json_leaves(&v, "", &mut out),
            Err(_) => return whole(),
        },
        Kind::Screen | Kind::Person | Kind::Other => return whole(),
    }
    out
}

/// A carry file's name past its attempt's stem (`seamless-<pid>-<nonce>`): `.toml`, `.s2.ctl`.
fn short(name: &str) -> &str {
    name.find('.').map_or(name, |i| &name[i..])
}

/// One place the sweep found sentinels: the file's kind, its name past the attempt's stem, the
/// place in it (as [`kept_at`] reads it) and the digits there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) kind: Kind,
    pub(crate) file: String,
    pub(crate) at: String,
    pub(crate) digits: Vec<u8>,
}

impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} {}: {:?}",
            self.kind.word(),
            self.file,
            self.at,
            self.digits
        )
    }
}

/// What is not the owner's alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Loose {
    /// The index names no control directory.
    NoDir,
    /// The control directory, by its mode.
    Dir { path: String, mode: u32 },
    /// A file, by its name past the stem and the mode the original had.
    File { file: String, mode: u32 },
}

impl std::fmt::Display for Loose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Loose::NoDir => write!(f, "the index names no directory"),
            Loose::Dir { path, mode } => write!(f, "the directory {path} is {mode:o}"),
            Loose::File { file, mode } => write!(f, "{file} is {mode:o}"),
        }
    }
}

/// What the sweep read of one kept attempt.
#[derive(Debug, Default)]
pub(crate) struct Sweep {
    /// Every place a sentinel was.
    pub(crate) found: Vec<Found>,
    /// The places that hold a sentinel the design keeps none of there, with only those digits.
    pub(crate) leaks: Vec<Found>,
    /// The control directory or a file that is not the owner's alone (0700, 0600).
    pub(crate) loose: Vec<Loose>,
    /// The digits found in the planted session's own log carry (`log[]` of its control sidecar).
    pub(crate) planted_log: BTreeSet<u8>,
    /// How many lines that log carry held.
    pub(crate) planted_log_lines: usize,
    /// How many files of each kind were kept.
    pub(crate) kinds: BTreeMap<Kind, usize>,
}

/// Sweep `kept`: each file's places held to [`kept_at`], each mode to 0600 in a 0700 directory;
/// `planted` is the planted session's local id, whose control sidecar `.s<planted>.ctl` holds the
/// log carry the positive control reads.
pub(crate) fn sweep(kept: &Kept, planted: &str) -> Sweep {
    let mut s = Sweep::default();
    match &kept.dir {
        Some((mode, _)) if mode & 0o7777 == 0o700 => {}
        Some((mode, path)) => s.loose.push(Loose::Dir {
            path: path.clone(),
            mode: mode & 0o7777,
        }),
        None => s.loose.push(Loose::NoDir),
    }
    let planted_ctl = format!(".s{planted}.ctl");
    for file in &kept.files {
        let kind = Kind::of(&file.name);
        *s.kinds.entry(kind).or_default() += 1;
        if file.mode & 0o7777 != 0o600 {
            s.loose.push(Loose::File {
                file: short(&file.name).to_string(),
                mode: file.mode & 0o7777,
            });
        }
        let is_planted = kind == Kind::Control && file.name.ends_with(&planted_ctl);
        for (place, text) in places(kind, &file.bytes) {
            if is_planted && place == "log[]" {
                s.planted_log_lines += 1;
            }
            let held = sentinels(text.as_bytes());
            if held.is_empty() {
                continue;
            }
            if is_planted && place == "log[]" {
                s.planted_log.extend(held.iter().copied());
            }
            let allowed = kept_at(kind, &place);
            let out: Vec<u8> = held
                .iter()
                .copied()
                .filter(|n| !allowed.contains(n))
                .collect();
            let at = |digits: Vec<u8>| Found {
                kind,
                file: short(&file.name).to_string(),
                at: place.clone(),
                digits,
            };
            if !out.is_empty() {
                s.leaks.push(at(out));
            }
            s.found.push(at(held));
        }
    }
    s
}

/// A clause of [`verdict`] that failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Wrong {
    /// No manifest was kept.
    NoManifest,
    /// The planted session's log carry had no line, or not the post's key and hop (8, 9): the
    /// sweep read no log the plants reached.
    NoPlantedLog { lines: usize, held: BTreeSet<u8> },
    /// No manifest place [`WAITING_BODY`] held the waiting post's body (6): the sweep read no
    /// post the plants left waiting, so the mail body's path went unread.
    NoWaitingPost,
    /// No screen of the planted session held the typed command's later word (1): the sweep read
    /// no screen the typed line reached.
    NoTypedLine,
    /// Sentinels where the design keeps none.
    Leaks(Vec<Found>),
    /// Not the owner's alone.
    Loose(Vec<Loose>),
    /// Names the seam listed and did not read (not a regular file, or past its cap), so the
    /// sweep never saw them.
    Unread(Vec<String>),
    /// The attempt's files still in the control directory after the Commit.
    Left(Vec<String>),
}

impl std::fmt::Display for Wrong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list = |items: &mut dyn Iterator<Item = String>| items.collect::<Vec<_>>().join("; ");
        match self {
            Wrong::NoManifest => write!(f, "no manifest was kept"),
            Wrong::NoPlantedLog { lines, held } => write!(
                f,
                "the planted tab's log carry ({lines} lines) holds {held:?}, not the post's key and hop (8, 9): the sweep read no log the plants reached"
            ),
            Wrong::NoWaitingPost => write!(
                f,
                "no manifest {WAITING_BODY} holds the waiting post's body (6): the sweep read no post the plants left waiting"
            ),
            Wrong::NoTypedLine => write!(
                f,
                "no screen of the planted tab holds the typed command's later word (1): the sweep read no screen the typed line reached"
            ),
            Wrong::Leaks(found) => write!(
                f,
                "sentinels where the design keeps none: {}",
                list(&mut found.iter().map(ToString::to_string))
            ),
            Wrong::Loose(loose) => write!(
                f,
                "not the owner's alone: {}",
                list(&mut loose.iter().map(ToString::to_string))
            ),
            Wrong::Unread(names) => write!(
                f,
                "listed by the seam and never read, so never swept: {}",
                names.join(" ")
            ),
            Wrong::Left(names) => write!(
                f,
                "still in the control directory after the Commit: {}",
                names.join(" ")
            ),
        }
    }
}

/// The leg's verdict on one kept attempt: each clause that failed, and what the sweep read.
#[derive(Debug)]
pub(crate) struct Judged {
    pub(crate) wrong: Vec<Wrong>,
    said: String,
}

impl Judged {
    /// `Ok` with what was read when no clause failed, else `Err` naming each failed clause first.
    pub(crate) fn rendered(&self) -> Result<String, String> {
        if self.wrong.is_empty() {
            Ok(format!("{}; none outlived the Commit", self.said))
        } else {
            let wrong: Vec<String> = self.wrong.iter().map(ToString::to_string).collect();
            Err(format!("{}; {}", wrong.join("; "), self.said))
        }
    }
}

/// The leg's verdict on `kept`: it must hold a manifest; the planted session's log carry must
/// hold the Host-tier key and hop (8, 9), a manifest's waiting post its body (6) and a screen of
/// the planted session the typed line (1), each the proof that the sweep read the path that
/// sentinel takes; nothing may leak or be loose; every name the seam listed must have been read;
/// and `left` (the attempt's files still in the control directory after the Commit) must be
/// empty.
pub(crate) fn verdict(kept: &Kept, planted: &str, left: &[String]) -> Judged {
    let s = sweep(kept, planted);
    let mut wrong = Vec::new();
    if !s.kinds.contains_key(&Kind::Manifest) {
        wrong.push(Wrong::NoManifest);
    }
    if s.planted_log_lines == 0 || !s.planted_log.is_superset(&BTreeSet::from([8, 9])) {
        wrong.push(Wrong::NoPlantedLog {
            lines: s.planted_log_lines,
            held: s.planted_log.clone(),
        });
    }
    if !s
        .found
        .iter()
        .any(|f| f.kind == Kind::Manifest && f.at == WAITING_BODY && f.digits.contains(&6))
    {
        wrong.push(Wrong::NoWaitingPost);
    }
    let planted_screen = format!(".s{planted}.");
    if !s.found.iter().any(|f| {
        f.kind == Kind::Screen && f.file.starts_with(&planted_screen) && f.digits.contains(&1)
    }) {
        wrong.push(Wrong::NoTypedLine);
    }
    if !s.leaks.is_empty() {
        wrong.push(Wrong::Leaks(s.leaks.clone()));
    }
    if !s.loose.is_empty() {
        wrong.push(Wrong::Loose(s.loose.clone()));
    }
    if !kept.skipped.is_empty() {
        wrong.push(Wrong::Unread(kept.skipped.clone()));
    }
    if !left.is_empty() {
        wrong.push(Wrong::Left(left.to_vec()));
    }
    let kinds = s
        .kinds
        .iter()
        .map(|(k, n)| format!("{} {n}", k.word()))
        .collect::<Vec<_>>()
        .join(", ");
    let stem = kept
        .files
        .first()
        .map_or("", |f| f.name.split('.').next().unwrap_or(""));
    let said = format!(
        "kept {} files of {stem} ({kinds}), each read; the planted tab's log carry ({} lines) holds {:?}; found {}",
        kept.files.len(),
        s.planted_log_lines,
        s.planted_log,
        if s.found.is_empty() {
            "nothing".to_string()
        } else {
            s.found
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        }
    );
    let said = if wrong.is_empty() {
        format!("{said}; each 0600 in a 0700 directory")
    } else {
        said
    };
    Judged { wrong, said }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "0123abcd\nschema = 1\n\n[[sessions]]\nlocal_id = 3\nsid = \"s-p\"\nstate = \"running\"\ntitle = \"QZP6SECRET2\"\nattention = \"QZP6SECRET3\"\nattention_owners = [\"- QZP6SECRET3\"]\ntopics = [\"qzp6secret4 since=9\"]\nhold = \"local QZP6SECRET5\"\nlog_ids = \"2 1 0 0\"\n\n[sessions.screen]\nschema = 1\nmeta = '{\"rows\":24,\"title\":{\"window\":\"QZP6SECRET2\",\"icon\":\"QZP6SECRET2\"},\"shell\":{\"output_blocks\":[{\"id\":1,\"span\":{\"commandline\":\"echo QZP6SECRET1 42\"}}]}}'\ngrid_file = \"g\"\nrepaint = false\n\n[[sessions.fabric.posts]]\nid = 1\nto = \"@s-q\"\nkind = \"note\"\nkey = \"QZP6SECRET8w\"\nvia = \"h-qzp6secret9w\"\nbody = \"QZP6SECRET6 waits\"\n";

    fn ctl(log: &[&str], archive: &str) -> Vec<u8> {
        let lines = log
            .iter()
            .map(|l| format!("\"{l}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"version\":1,\"turns\":[{{\"id\":1,\"text\":\"sh /private/tmp/x/marked.sh\"}}],\"unheld_below\":0,\"archive\":{{\"rows\":[\"{archive}\"]}},\"log\":[{lines}]}}"
        )
        .into_bytes()
    }

    const CLEAN_LOG: [&str; 3] = [
        "1 open - session prog=sh",
        "9 mark - topic op=add fp=00ff00ff00ff00ff",
        "12 mark - mail dir=out kind=note to=@s-q via=h-qzp6secret9 idem=QZP6SECRET8",
    ];

    const TYPED: &str = "$ echo QZP6SECRET1 42";

    fn file(name: &str, mode: u32, bytes: Vec<u8>) -> KeptFile {
        KeptFile {
            name: name.to_string(),
            mode,
            bytes,
        }
    }

    fn attempt(log: &[&str]) -> Kept {
        Kept {
            dir: Some((0o700, "/private/tmp/x/run".to_string())),
            files: vec![
                file("seamless-1-ab.toml", 0o600, MANIFEST.as_bytes().to_vec()),
                file(
                    "seamless-1-ab.layout.toml",
                    0o600,
                    b"schema = 1\n[[windows]]\n[[windows.tabs]]\ntitle = \"QZP6SECRET2\"\nholds = [\"local QZP6SECRET5\"]\n".to_vec(),
                ),
                file("seamless-1-ab.s3.ctl", 0o600, ctl(log, TYPED)),
                file(
                    "seamless-1-ab.s4.ctl",
                    0o600,
                    ctl(&["1 open - session prog=sh"], "$ ls"),
                ),
                file(
                    "seamless-1-ab.s3.grid",
                    0o600,
                    format!("\x03\x00\x00\x00$ ran-42\r\n{TYPED}\r\nQZP6SECRET1 42").into_bytes(),
                ),
                file("seamless-1-ab.person", 0o600, b"3 1200\n4 -900\n".to_vec()),
            ],
            skipped: Vec::new(),
        }
    }

    fn at(kind: Kind, file: &str, place: &str, digits: &[u8]) -> Found {
        Found {
            kind,
            file: file.to_string(),
            at: place.to_string(),
            digits: digits.to_vec(),
        }
    }

    /// The places `wrong` names as leaks.
    fn leaks(wrong: &[Wrong]) -> Vec<Found> {
        wrong
            .iter()
            .filter_map(|w| match w {
                Wrong::Leaks(found) => Some(found.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn the_sentinels_are_read_in_any_case_by_their_digit() {
        assert_eq!(
            sentinels(b"a QZP6SECRET3 b qzp6secret9 c QzP6sEcReT3"),
            [3, 9]
        );
        assert!(sentinels(b"QZP6SECRET x QZP6SECRETS").is_empty());
    }

    #[test]
    fn a_carry_with_the_session_state_and_a_host_tier_log_passes() {
        let kept = attempt(&CLEAN_LOG);
        let judged = verdict(&kept, "3", &[]);
        assert_eq!(judged.wrong, [], "{:?}", judged.rendered());
        assert!(judged.rendered().is_ok());
        let swept = sweep(&kept, "3");
        assert_eq!(swept.planted_log, BTreeSet::from([8, 9]));
        assert_eq!(swept.planted_log_lines, 3);
        for place in [
            at(Kind::Manifest, ".toml", "sessions[].title", &[2]),
            at(
                Kind::Manifest,
                ".toml",
                "sessions[].attention_owners[]",
                &[3],
            ),
            at(
                Kind::Manifest,
                ".toml",
                "sessions[].screen.meta>title.window",
                &[2],
            ),
            at(
                Kind::Manifest,
                ".toml",
                "sessions[].screen.meta>shell.output_blocks[].span.commandline",
                &[1],
            ),
            at(Kind::Manifest, ".toml", WAITING_BODY, &[6]),
            at(
                Kind::Manifest,
                ".toml",
                "sessions[].fabric.posts[].key",
                &[8],
            ),
            at(
                Kind::Layout,
                ".layout.toml",
                "windows[].tabs[].holds[]",
                &[5],
            ),
            at(Kind::Control, ".s3.ctl", "log[]", &[8, 9]),
            at(Kind::Control, ".s3.ctl", "archive.rows[]", &[1]),
            at(Kind::Screen, ".s3.grid", "-", &[1]),
        ] {
            assert!(swept.found.contains(&place), "{place} in {:?}", swept.found);
        }
    }

    /// THE NEGATIVE CONTROL the leg names: a Content-tier key planted in the session log's carry
    /// (a topic's name, a title, a hold reason) is found, and so is what is never stored (a
    /// command's later word, a mail body).
    #[test]
    fn a_content_tier_key_in_the_log_carry_is_found() {
        for (planted, digit) in [
            (
                "9 mark - topic op=add fp=00ff00ff00ff00ff name=qzp6secret4",
                4,
            ),
            ("10 mark - title title=QZP6SECRET2", 2),
            ("11 mark - hold on reason=QZP6SECRET5", 5),
            ("13 open r1 cmd detail=echo%20QZP6SECRET1", 1),
            ("14 mark - mail dir=out body=QZP6SECRET6", 6),
            ("15 mark - meta field=attention attn_text=QZP6SECRET3", 3),
        ] {
            let mut log = CLEAN_LOG.to_vec();
            log.push(planted);
            let judged = verdict(&attempt(&log), "3", &[]);
            assert_eq!(
                leaks(&judged.wrong),
                [at(Kind::Control, ".s3.ctl", "log[]", &[digit])],
                "{planted}"
            );
            assert!(judged.rendered().is_err(), "{planted}");
        }
    }

    #[test]
    fn a_sentinel_outside_its_field_or_a_loose_mode_or_a_leftover_fails() {
        let mut kept = attempt(&CLEAN_LOG);
        kept.files.push(file(
            "seamless-1-ab.s3.hist",
            0o600,
            b"\x01\x00\x00\x00$ QZP6SECRET1 QZP6SECRET2".to_vec(),
        ));
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Leaks(vec![at(Kind::Screen, ".s3.hist", "-", &[2])])],
            "a title on a screen is not where the plants put it; the echoed line is"
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.files[2].bytes = ctl(&CLEAN_LOG, "QZP6SECRET5");
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Leaks(vec![at(
                Kind::Control,
                ".s3.ctl",
                "archive.rows[]",
                &[5]
            )])],
            "a hold reason in the archive rows"
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.files[0].bytes = MANIFEST
            .replace(
                "state = \"running\"",
                "state = \"running\"\ndescription = \"QZP6SECRET3\"",
            )
            .into_bytes();
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Leaks(vec![at(
                Kind::Manifest,
                ".toml",
                "sessions[].description",
                &[3]
            )])],
            "the attention text where only the description is"
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.files[2].mode = 0o644;
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Loose(vec![Loose::File {
                file: ".s3.ctl".to_string(),
                mode: 0o644
            }])]
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.dir = Some((0o755, "/private/tmp/x/run".to_string()));
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Loose(vec![Loose::Dir {
                path: "/private/tmp/x/run".to_string(),
                mode: 0o755
            }])]
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.dir = None;
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::Loose(vec![Loose::NoDir])]
        );
        let left = ["seamless-1-ab.s3.ctl".to_string()];
        assert_eq!(
            verdict(&attempt(&CLEAN_LOG), "3", &left).wrong,
            [Wrong::Left(left.to_vec())]
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.files[0].bytes = b"0123abcd\nnot = [toml QZP6SECRET2".to_vec();
        let judged = verdict(&kept, "3", &[]);
        assert_eq!(
            leaks(&judged.wrong),
            [at(Kind::Manifest, ".toml", "-", &[2])],
            "a manifest that does not parse keeps nothing"
        );
    }

    /// A name the keep seam listed and did not read (`skip`: no regular file, or past its cap)
    /// was never swept, so the leg cannot say every file was read.
    #[test]
    fn a_name_the_seam_did_not_read_fails() {
        let mut kept = attempt(&CLEAN_LOG);
        kept.skipped = vec!["seamless-1-ab.s4.hist".to_string()];
        let judged = verdict(&kept, "3", &[]);
        assert_eq!(
            judged.wrong,
            [Wrong::Unread(vec!["seamless-1-ab.s4.hist".to_string()])]
        );
        assert!(judged.rendered().is_err());
    }

    #[test]
    fn a_sweep_that_read_no_planted_path_is_vacuous() {
        let vacuous = |w: &Wrong| matches!(w, Wrong::NoPlantedLog { .. });
        let judged = verdict(&attempt(&CLEAN_LOG), "4", &[]);
        assert!(judged.wrong.iter().any(vacuous), "{:?}", judged.wrong);
        let judged = verdict(&attempt(&CLEAN_LOG[..2]), "3", &[]);
        assert_eq!(
            judged.wrong,
            [Wrong::NoPlantedLog {
                lines: 2,
                held: BTreeSet::new()
            }]
        );
        let mut kept = attempt(&CLEAN_LOG);
        kept.files.retain(|f| Kind::of(&f.name) != Kind::Manifest);
        assert_eq!(
            verdict(&kept, "3", &[]).wrong,
            [Wrong::NoManifest, Wrong::NoWaitingPost]
        );
        let mut kept = attempt(&CLEAN_LOG);
        let unwaited = MANIFEST.split("\n[[sessions.fabric.posts]]").next();
        kept.files[0].bytes = unwaited.unwrap_or_default().as_bytes().to_vec();
        assert_eq!(verdict(&kept, "3", &[]).wrong, [Wrong::NoWaitingPost]);
        let mut kept = attempt(&CLEAN_LOG);
        kept.files[4].bytes = b"\x03\x00\x00\x00$ ran-42".to_vec();
        assert_eq!(verdict(&kept, "3", &[]).wrong, [Wrong::NoTypedLine]);
    }

    #[test]
    fn the_index_is_read_with_each_file_beside_it() {
        let tmp = aterm_tempfile::tempdir().expect("a scratch dir");
        let dir = tmp.path();
        std::fs::write(dir.join("seamless-1-ab.toml"), "0123\nschema = 1\n").expect("a file");
        std::fs::write(
            dir.join("index"),
            "dir 700 /private/tmp/x/run\nfile 600 16 seamless-1-ab.toml\nskip seamless-1-ab.sock\n",
        )
        .expect("the index");
        let kept = read_kept(dir).expect("reads");
        assert_eq!(kept.dir, Some((0o700, "/private/tmp/x/run".to_string())));
        assert_eq!(kept.files.len(), 1);
        assert_eq!(kept.files[0].mode, 0o600);
        assert_eq!(kept.skipped, ["seamless-1-ab.sock"]);
        std::fs::write(dir.join("index"), "file 600 99 seamless-1-ab.toml\n").expect("the index");
        assert!(read_kept(dir).is_err(), "a length the copy does not have");
    }
}
