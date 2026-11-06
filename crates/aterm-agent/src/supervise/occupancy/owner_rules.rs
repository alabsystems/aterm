// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OWNER'S RULES AND THEIR SOURCES (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.3; the
//! slice-3 plan's D3 to D5): `[harness.occupancy_source.<name>]` and `[[harness.occupancy_rule]]`,
//! read from the root `[harness]` table alone ([`OwnerTables::from_doc`]). A copy below another
//! table's header is the config reader's misplaced note and grants nothing.
//!
//! Pure and clockless: nothing here reads a repository, the network or a journal. A rule is read
//! whole or not at all: any key it does not take, any value outside its charset or cap, a
//! `when` the grammar refuses, and a source it names that is not declared refuse the rule with
//! one closed note naming the key, never echoing what was written. A refused source refuses
//! every rule that names it.
//!
//! Each rule's [`rev`] is SHA-256 over a canonical form of the rule, the source its `when` names
//! and that source's resolved URL ([`canonical`]): any edit of a field, a source, `mode` or the
//! remote is a new rev. A rule is read and judged only while the owner confirmed its whole
//! current rev ([`RuleState::of`]); its yes is given to a [`Snapshot`], which confirms nothing
//! once the table in force no longer hashes to it ([`Snapshot::still_current`]).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::git_url::{RemoteUrl, hidden};
use super::wake_atoms::PATH_MAX;
use super::wakes::{SysWait, WakeUsage};
use crate::harness::harness_mark;
use crate::supervise::config::Autopilot;

/// The root `[harness]` key of the source tables.
pub const SOURCE_TABLE: &str = "occupancy_source";

/// The root `[harness]` key of the rule list.
pub const RULE_TABLE: &str = "occupancy_rule";

/// Both owner tables' keys.
pub const TABLES: [&str; 2] = [SOURCE_TABLE, RULE_TABLE];

/// The most rules read; the rest are refused with a note.
pub const OWNER_RULES_MAX: usize = 8;

/// The most sources read; the rest are refused with a note.
pub const OWNER_SOURCES_MAX: usize = 8;

/// The longest rule or source name: `[a-z0-9-]{1,32}`.
pub const NAME_MAX: usize = 32;

/// The longest `remote`: `[A-Za-z0-9._-]{1,32}`.
pub const REMOTE_MAX: usize = 32;

/// The longest short name after `refs/heads/` or `refs/tags/`.
pub const REF_SHORT_MAX: usize = 48;

/// The longest `path`: `[A-Za-z0-9._/-]{1,64}`, relative.
pub const SOURCE_PATH_MAX: usize = 64;

/// The longest atpkg `program`: `[a-z0-9-]{1,64}`.
pub const PROGRAM_MAX: usize = 64;

/// The most characters of a rule's `text`.
pub const TEXT_MAX: usize = 200;

/// The longest `scope.jobs_cwd`, in bytes.
pub const GLOB_MAX: usize = 256;

/// The most terms one `when` joins.
pub const WHEN_MAX: usize = 8;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// The shortest `every`.
pub const EVERY_MIN_MS: i64 = MINUTE_MS;

/// The longest `every`.
pub const EVERY_MAX_MS: i64 = HOUR_MS;

/// A git-ref source's `every` unwritten.
pub const GIT_EVERY_MS: i64 = 5 * MINUTE_MS;

/// An atpkg source's `every` unwritten (it reads offline).
pub const ATPKG_EVERY_MS: i64 = MINUTE_MS;

/// The longest `hold>=` span.
const HOLD_MAX_MS: i64 = 48 * HOUR_MS;

/// `remote` unwritten.
pub const DEFAULT_REMOTE: &str = "origin";

/// `ref` unwritten.
pub const DEFAULT_REF: &str = "refs/heads/main";

/// A RULE'S OR SOURCE'S NAME, `[a-z0-9-]{1,32}`: the only owner-written word a line, a row or a
/// journal line may name a rule by.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnerName(String);

impl OwnerName {
    /// The name `text` spells, if it is one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        word_of(text, NAME_MAX, |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
        })
        .map(Self)
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OwnerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for OwnerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A VALUE THE READER CHECKED against its key's charset and cap (a remote, a ref, a path, a
/// program, a repository path): never a character a line or an alert could hide.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Checked(String);

impl Checked {
    /// The value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Checked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

/// A RULE'S `text`: the owner's words, 1 to [`TEXT_MAX`] characters with no control, bidi or
/// invisible character (`git_url::hidden`), no backtick and no harness mark. Its Debug prints
/// its length and a digest prefix, never the words.
#[derive(Clone, PartialEq, Eq)]
pub struct RuleText(String);

impl RuleText {
    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RuleText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sha = aterm_digest::Sha256::digest(self.0.as_bytes());
        write!(
            f,
            "RuleText(len={} sha={})",
            self.0.chars().count(),
            aterm_codec::hex::encode_lower(&sha[..4])
        )
    }
}

/// ONE DECLARED SOURCE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Its name (`[harness.occupancy_source.<name>]`).
    pub name: OwnerName,
    /// What it reads.
    pub kind: SourceKind,
}

impl Source {
    /// Its kind's word (`git-ref`, `atpkg`).
    #[must_use]
    pub const fn kind_word(&self) -> &'static str {
        match self.kind {
            SourceKind::Git(_) => "git-ref",
            SourceKind::Atpkg(_) => "atpkg",
        }
    }

    /// How often it is read.
    #[must_use]
    pub const fn every_ms(&self) -> i64 {
        match &self.kind {
            SourceKind::Git(git) => git.every_ms,
            SourceKind::Atpkg(atpkg) => atpkg.every_ms,
        }
    }

    /// A git source's remote: its `url`, or its `repo`'s remote as `resolved` holds it (none
    /// until the reader has read that repository's `.git/config`, and none for an atpkg source).
    #[must_use]
    pub fn remote<'a>(&'a self, resolved: &'a Resolved) -> Option<&'a RemoteUrl> {
        match &self.kind {
            SourceKind::Git(GitRef {
                origin: Origin::Url(url),
                ..
            }) => Some(url),
            SourceKind::Git(GitRef {
                origin: Origin::Repo { .. },
                ..
            }) => resolved
                .iter()
                .find(|(name, _)| name == &self.name)
                .map(|(_, url)| url),
            SourceKind::Atpkg(_) => None,
        }
    }
}

/// What a source reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// `kind = "git-ref"`: a ref's tip on a remote, or a tree entry under it.
    Git(GitRef),
    /// `kind = "atpkg"`: the build the last verified index pins for a program, read offline.
    Atpkg(AtpkgSource),
}

/// A `git-ref` source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRef {
    /// Where its remote's URL comes from.
    pub origin: Origin,
    /// `ref`: `refs/heads/<short>` or `refs/tags/<short>`.
    pub git_ref: Checked,
    /// `path`: the tree entry compared tip to tip, if any.
    pub path: Option<Checked>,
    /// `every`, ms.
    pub every_ms: i64,
}

/// Where a git source's URL comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `repo` (and `remote`): the URL of a remote of the owner's repository, as written (`~` kept).
    Repo {
        /// The repository's path, absolute or `~/…`.
        path: Checked,
        /// The remote's name.
        remote: Checked,
    },
    /// `url`.
    Url(RemoteUrl),
}

/// An `atpkg` source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtpkgSource {
    /// `program`.
    pub program: Checked,
    /// `every`, ms.
    pub every_ms: i64,
}

/// THE SOURCE ATOM OF A RULE'S `when`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRef {
    /// `git(<name>).moved`.
    Git(OwnerName),
    /// `atpkg(<program>).moved`, naming the atpkg source that reads that program.
    Atpkg {
        /// The program.
        program: Checked,
        /// The source declared for it.
        source: OwnerName,
    },
    /// `workflow.returned`: the session's own Claude Code workflows.
    Workflow,
}

impl SourceRef {
    /// The source table it names, if any.
    #[must_use]
    pub const fn source(&self) -> Option<&OwnerName> {
        match self {
            SourceRef::Git(name) | SourceRef::Atpkg { source: name, .. } => Some(name),
            SourceRef::Workflow => None,
        }
    }

    fn canonical(&self) -> String {
        match self {
            SourceRef::Git(name) => format!("git({name}).moved"),
            SourceRef::Atpkg { program, .. } => format!("atpkg({}).moved", program.as_str()),
            SourceRef::Workflow => "workflow.returned".to_string(),
        }
    }
}

/// A SESSION TERM (§3.1: `hold=background | hold>=DUR | hold=undeclared | autopilot=WORD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTerm {
    /// `hold=background`: the session waits on its own background work.
    Background,
    /// `hold>=DUR`: and has for at least so long, ms.
    HoldAtLeast(i64),
    /// `hold=undeclared`: and has declared no wait.
    Undeclared,
    /// `autopilot=cruise|land`.
    Autopilot(Autopilot),
}

impl SessionTerm {
    fn canonical(self) -> String {
        match self {
            SessionTerm::Background => "hold=background".to_string(),
            SessionTerm::HoldAtLeast(ms) => format!("hold>={}", span_text(ms)),
            SessionTerm::Undeclared => "hold=undeclared".to_string(),
            SessionTerm::Autopilot(mode) => format!("autopilot={}", mode.word()),
        }
    }

    const fn class(self) -> u8 {
        match self {
            SessionTerm::Background => 0,
            SessionTerm::HoldAtLeast(_) => 1,
            SessionTerm::Undeclared => 2,
            SessionTerm::Autopilot(_) => 3,
        }
    }
}

/// A RULE'S `when`, parsed ([`Self::parse`]): exactly one source atom, the machine terms of
/// `await sys`'s grammar as one wait, and the session terms.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerWhen {
    /// The source atom.
    pub source: SourceRef,
    /// The machine terms, if any.
    pub sys: Option<SysWait>,
    /// The session terms, in canonical order, each class at most once.
    pub session: Vec<SessionTerm>,
}

/// Why a `when` is not read: each a closed sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenRefusal {
    /// Not terms joined by `and`.
    Grammar,
    /// A control, bidi or invisible character (`git_url::hidden`).
    Control,
    /// `at:` or `in:`.
    Time,
    /// `free`, `disk=` or `vol=`.
    Volume,
    /// `file:`, `gone:`, `pid:` or `task:`.
    WakeAtom,
    /// A claim atom or `irreversible`.
    Claim,
    /// `for=` with no machine term before it, or given twice.
    ForWithoutSys,
    /// `scope=`.
    Scope,
    /// A second source atom.
    SecondSource,
    /// No source atom.
    NoSource,
    /// The source atom names no source of its kind declared under `[harness.occupancy_source]`.
    Undeclared,
    /// A `hold` or `autopilot` term outside the session grammar (`hold=declared` among them).
    Session,
    /// A session term's class given twice.
    Twice,
    /// A machine term `await sys` refuses.
    Machine,
    /// More than [`WHEN_MAX`] terms.
    TooMany,
}

impl WhenRefusal {
    /// Its sentence.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            WhenRefusal::Grammar => "takes terms joined by `and`",
            WhenRefusal::Control => "holds a control, bidi or invisible character",
            WhenRefusal::Time => "takes no time term: an owner rule is no timer",
            WhenRefusal::Volume => {
                "takes no free, disk= or vol= term: `await sys` waits on a volume alone"
            }
            WhenRefusal::WakeAtom => {
                "takes no file:, gone:, pid: or task: atom: those are a wake's"
            }
            WhenRefusal::Claim => "takes no claim atom yet: claims arrive in slice 4",
            WhenRefusal::ForWithoutSys => "takes for= once, after a machine term",
            WhenRefusal::Scope => "takes no scope=: a rule's reach is its scope table",
            WhenRefusal::SecondSource => "names exactly one source atom",
            WhenRefusal::NoSource => {
                "names one source atom: git(<name>).moved, atpkg(<program>).moved or \
                 workflow.returned"
            }
            WhenRefusal::Undeclared => {
                "names no source of its kind declared under [harness.occupancy_source]"
            }
            WhenRefusal::Session => {
                "takes the session terms hold=background, hold>=<dur>, hold=undeclared and \
                 autopilot=cruise|land alone"
            }
            WhenRefusal::Twice => "gives one session term twice",
            WhenRefusal::Machine => "holds a machine term `await sys` does not read",
            WhenRefusal::TooMany => "joins at most 8 terms",
        }
    }
}

impl OwnerWhen {
    /// THE `when` `text` SPELLS, its source atom one of `sources`.
    /// # Errors
    /// [`WhenRefusal`]: the first reason it is not read.
    pub fn parse(text: &str, sources: &[Source]) -> Result<Self, WhenRefusal> {
        if text.chars().any(hidden) {
            return Err(WhenRefusal::Control);
        }
        let mut source: Option<SourceRef> = None;
        let mut machine: Vec<&str> = Vec::new();
        let mut session: Vec<SessionTerm> = Vec::new();
        let mut for_word: Option<&str> = None;
        let mut terms = 0usize;
        let mut want_term = true;
        for tok in text.split_whitespace() {
            if tok == "and" {
                if want_term {
                    return Err(WhenRefusal::Grammar);
                }
                want_term = true;
                continue;
            }
            if let Some(span) = tok.strip_prefix("for=") {
                if want_term || machine.is_empty() || for_word.is_some() || span_ms(span).is_none()
                {
                    return Err(WhenRefusal::ForWithoutSys);
                }
                for_word = Some(tok);
                continue;
            }
            if !want_term || for_word.is_some() {
                return Err(WhenRefusal::Grammar);
            }
            want_term = false;
            terms += 1;
            if terms > WHEN_MAX {
                return Err(WhenRefusal::TooMany);
            }
            match term(tok, sources)? {
                Term::Source(atom) => {
                    if source.is_some() {
                        return Err(WhenRefusal::SecondSource);
                    }
                    source = Some(atom);
                }
                Term::Session(t) => {
                    if session.iter().any(|s| s.class() == t.class()) {
                        return Err(WhenRefusal::Twice);
                    }
                    session.push(t);
                }
                Term::Machine => machine.push(tok),
            }
        }
        if want_term && terms > 0 {
            return Err(WhenRefusal::Grammar);
        }
        let source = source.ok_or(WhenRefusal::NoSource)?;
        let sys = if machine.is_empty() {
            None
        } else {
            let mut words: Vec<&str> = Vec::new();
            for (at, term) in machine.iter().enumerate() {
                if at > 0 {
                    words.push("and");
                }
                words.push(term);
            }
            words.extend(for_word);
            Some(SysWait::parse(&words).map_err(|why| match why {
                WakeUsage::Volume => WhenRefusal::Volume,
                _ => WhenRefusal::Machine,
            })?)
        };
        session.sort_by_key(|t| t.class());
        Ok(Self {
            source,
            sys,
            session,
        })
    }

    /// Its canonical spelling: the source atom, the session terms, then the machine terms as
    /// `await sys` spells them, joined by `and`.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut parts = vec![self.source.canonical()];
        parts.extend(self.session.iter().map(|t| t.canonical()));
        if let Some(sys) = &self.sys {
            let spelled = sys.canonical();
            parts.push(spelled.strip_prefix("sys ").unwrap_or(spelled).to_string());
        }
        parts.join(" and ")
    }
}

enum Term {
    Source(SourceRef),
    Session(SessionTerm),
    Machine,
}

fn term(tok: &str, sources: &[Source]) -> Result<Term, WhenRefusal> {
    if let Some(inner) = tok.strip_prefix("git(") {
        let name = inner
            .strip_suffix(").moved")
            .and_then(OwnerName::parse)
            .ok_or(WhenRefusal::Grammar)?;
        let declared = sources
            .iter()
            .any(|s| s.name == name && matches!(s.kind, SourceKind::Git(_)));
        return if declared {
            Ok(Term::Source(SourceRef::Git(name)))
        } else {
            Err(WhenRefusal::Undeclared)
        };
    }
    if let Some(inner) = tok.strip_prefix("atpkg(") {
        let program = inner
            .strip_suffix(").moved")
            .and_then(program_of)
            .ok_or(WhenRefusal::Grammar)?;
        let source = sources
            .iter()
            .find(|s| matches!(&s.kind, SourceKind::Atpkg(a) if a.program == program))
            .ok_or(WhenRefusal::Undeclared)?;
        return Ok(Term::Source(SourceRef::Atpkg {
            program,
            source: source.name.clone(),
        }));
    }
    if tok == "workflow.returned" {
        return Ok(Term::Source(SourceRef::Workflow));
    }
    if tok.starts_with("workflow.") {
        return Err(WhenRefusal::Grammar);
    }
    if tok.starts_with("at:") || tok.starts_with("in:") {
        return Err(WhenRefusal::Time);
    }
    if ["file:", "gone:", "pid:", "task:"]
        .iter()
        .any(|p| tok.starts_with(p))
    {
        return Err(WhenRefusal::WakeAtom);
    }
    if tok.starts_with("claim=") || tok.starts_with("claim.") || tok == "irreversible" {
        return Err(WhenRefusal::Claim);
    }
    if tok.starts_with("free") || tok.starts_with("disk=") || tok.starts_with("vol=") {
        return Err(WhenRefusal::Volume);
    }
    if tok.starts_with("scope=") {
        return Err(WhenRefusal::Scope);
    }
    if tok.starts_with("hold") || tok.starts_with("autopilot") {
        return session_term(tok).map(Term::Session);
    }
    Ok(Term::Machine)
}

fn session_term(tok: &str) -> Result<SessionTerm, WhenRefusal> {
    match tok {
        "hold=background" => return Ok(SessionTerm::Background),
        "hold=undeclared" => return Ok(SessionTerm::Undeclared),
        "autopilot=cruise" => return Ok(SessionTerm::Autopilot(Autopilot::Cruise)),
        "autopilot=land" => return Ok(SessionTerm::Autopilot(Autopilot::Land)),
        _ => {}
    }
    tok.strip_prefix("hold>=")
        .and_then(span_ms)
        .filter(|ms| *ms <= HOLD_MAX_MS)
        .map(SessionTerm::HoldAtLeast)
        .ok_or(WhenRefusal::Session)
}

/// `<n>(s|m|h)`, 1 to 6 digits, positive.
fn span_ms(text: &str) -> Option<i64> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (n, unit) = text.split_at(digits);
    if !(1..=6).contains(&n.len()) || n.starts_with('0') {
        return None;
    }
    let scale = match unit {
        "s" => 1_000,
        "m" => MINUTE_MS,
        "h" => HOUR_MS,
        _ => return None,
    };
    n.parse::<i64>().ok()?.checked_mul(scale)
}

/// A span in its largest whole unit (`10m`, `2h`, `90s`).
fn span_text(ms: i64) -> String {
    if ms % HOUR_MS == 0 {
        format!("{}h", ms / HOUR_MS)
    } else if ms % MINUTE_MS == 0 {
        format!("{}m", ms / MINUTE_MS)
    } else {
        format!("{}s", ms / 1_000)
    }
}

/// A RULE'S REACH BY THE KERNEL CWD OF A SESSION'S JOB LEADERS (`scope = { jobs_cwd = "…" }`):
/// an absolute or `~/…` path whose components may hold `*` (any run of characters within one
/// component, never `**`). It matches a folder and every folder below it.
///
/// Its lexical rules are a wake path's (`wake_atoms::admit`): no control, bidi or invisible
/// character, no `.`, `..` or dot-named component, nothing outside ASCII, at most [`GLOB_MAX`]
/// bytes. It is matched against a path the kernel names and never opened.
#[derive(Clone, PartialEq, Eq)]
pub struct CwdGlob {
    written: String,
    home: bool,
    segs: Vec<String>,
}

/// Why a `jobs_cwd` is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobRefusal {
    /// A control, bidi or invisible character (`git_url::hidden`).
    Control,
    /// Neither absolute nor `~/…`.
    Relative,
    /// An empty, `.`, `..` or dot-named component.
    Dot,
    /// A character outside ASCII, or a space.
    Unicode,
    /// `**`.
    DoubleStar,
    /// Over [`GLOB_MAX`] bytes.
    Long,
}

impl GlobRefusal {
    /// Its sentence.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            GlobRefusal::Control => "holds a control, bidi or invisible character",
            GlobRefusal::Relative => "is absolute or starts with ~/",
            GlobRefusal::Dot => "has no empty, dot or dot-named component",
            GlobRefusal::Unicode => "is ASCII with no space",
            GlobRefusal::DoubleStar => "takes * within one component, never **",
            GlobRefusal::Long => "is at most 256 bytes",
        }
    }
}

impl CwdGlob {
    /// The glob `text` spells.
    /// # Errors
    /// [`GlobRefusal`]: the first reason it is not read.
    pub fn parse(text: &str) -> Result<Self, GlobRefusal> {
        if text.chars().any(hidden) {
            return Err(GlobRefusal::Control);
        }
        if text.len() > GLOB_MAX {
            return Err(GlobRefusal::Long);
        }
        if !text.is_ascii() || text.contains(char::is_whitespace) {
            return Err(GlobRefusal::Unicode);
        }
        let (home, body) = match (text.strip_prefix("~/"), text.strip_prefix('/')) {
            (Some(body), _) => (true, body),
            (None, Some(body)) => (false, body),
            (None, None) => return Err(GlobRefusal::Relative),
        };
        let body = body.strip_suffix('/').unwrap_or(body);
        if body.contains("**") {
            return Err(GlobRefusal::DoubleStar);
        }
        let segs: Vec<String> = body.split('/').map(str::to_string).collect();
        if segs.iter().any(|s| s.is_empty() || s.starts_with('.')) {
            return Err(GlobRefusal::Dot);
        }
        let written = format!("{}{}", if home { "~/" } else { "/" }, segs.join("/"));
        Ok(Self {
            written,
            home,
            segs,
        })
    }

    /// Whether `cwd` (absolute, as the kernel names it) is the folder it names or below one,
    /// `~` being `home`.
    #[must_use]
    pub fn matches(&self, cwd: &Path, home: &Path) -> bool {
        let Some(cwd) = cwd.to_str() else {
            return false;
        };
        let Some(cwd) = cwd.strip_prefix('/') else {
            return false;
        };
        let mut want: Vec<&str> = Vec::new();
        if self.home {
            let Some(home) = home.to_str().and_then(|h| h.strip_prefix('/')) else {
                return false;
            };
            want.extend(home.split('/').filter(|s| !s.is_empty()));
        }
        let have: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
        let prefix = want.len();
        if have.len() < prefix + self.segs.len() || have[..prefix] != want[..] {
            return false;
        }
        self.segs
            .iter()
            .zip(&have[prefix..])
            .all(|(glob, seg)| component_matches(glob, seg))
    }

    /// Its canonical spelling (one trailing `/` dropped).
    #[must_use]
    pub fn canonical(&self) -> &str {
        &self.written
    }
}

impl fmt::Debug for CwdGlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CwdGlob({})", self.written)
    }
}

/// `glob` (`*` any run, possibly empty) against one whole component.
fn component_matches(glob: &str, seg: &str) -> bool {
    let mut parts = glob.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = seg.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// `mode = "live" | "dry-run"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RuleMode {
    /// Typed under `prompt` once confirmed.
    #[default]
    Live,
    /// Read, judged and booked `why=dry-run`, never typed.
    DryRun,
}

impl RuleMode {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            RuleMode::Live => "live",
            RuleMode::DryRun => "dry-run",
        }
    }
}

/// ONE OWNER RULE (`[[harness.occupancy_rule]]`).
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerRule {
    /// `name`, unique among the rules.
    pub name: OwnerName,
    /// `when`, parsed.
    pub when: OwnerWhen,
    /// `scope.jobs_cwd`, if any (none reaches every session of its modes).
    pub scope: Option<CwdGlob>,
    /// `modes`, canonical order (Cruise, then Land), each once.
    pub modes: Vec<Autopilot>,
    /// `urgent`.
    pub urgent: bool,
    /// `mode`.
    pub mode: RuleMode,
    /// `text`.
    pub text: RuleText,
}

/// A rule's rev: SHA-256 of its canonical form ([`canonical`]). Compared and stored whole;
/// [`Self::short`] is for reading only.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rev([u8; 32]);

impl Rev {
    /// Its 64 hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        aterm_codec::hex::encode_lower(&self.0)
    }

    /// Its first 12 hex digits, for reading only.
    #[must_use]
    pub fn short(&self) -> String {
        aterm_codec::hex::encode_lower(&self.0[..6])
    }

    /// The rev 64 lower-case hex digits spell.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut out = [0u8; 32];
        aterm_codec::hex::decode_into(text.as_bytes(), &mut out).then_some(Self(out))
    }
}

impl fmt::Debug for Rev {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Rev({})", self.hex())
    }
}

impl Serialize for Rev {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Rev {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Self::parse(&text)
            .ok_or_else(|| serde::de::Error::custom("a rev is 64 lower-case hex digits"))
    }
}

/// A git source's resolved URL, by source name: a `repo =` source's as the reader found it in
/// the repository's `.git/config` (a `url =` source resolves itself).
///
/// A `repo =` source not in it has no `resolved_url` line, so its rule's rev before the reader
/// has read that repository is another rev than after: a yes given before the URL was read is no
/// yes to the rule over that URL ([`Snapshot::still_current`] is false once it resolves). Such a
/// yes can cover nothing but the in-process read of the repository's `.git/config`; every git
/// read waits for a yes to the resolved rev.
pub type Resolved = [(OwnerName, RemoteUrl)];

/// THE CANONICAL FORM A RULE'S REV IS TAKEN OVER: one `key=value` line per field of the rule,
/// of the source its `when` names and of that source's resolved URL, keys sorted, every value
/// as Rust's debug escape writes it and every default written out. An optional field not given
/// (`scope`, `path`, a URL not yet resolved) writes no line, so no value a table could write
/// reads as its absence. Key order, spacing, a default written or left out, and `when`'s
/// spelling (it is printed from its parsed form) leave it as it is; any other edit changes it.
#[must_use]
pub fn canonical(rule: &OwnerRule, sources: &[Source], resolved: &Resolved) -> String {
    use super::fault::{Fault, armed};
    let mut lines: BTreeMap<String, String> = BTreeMap::new();
    let mut put = |key: String, value: &str| {
        lines.insert(key, format!("{value:?}"));
    };
    put("rule.name".into(), rule.name.as_str());
    put("rule.when".into(), &rule.when.canonical());
    if let Some(glob) = &rule.scope {
        put("rule.scope.jobs_cwd".into(), glob.canonical());
    }
    let modes: Vec<&str> = rule.modes.iter().map(|m| m.word()).collect();
    put("rule.modes".into(), &modes.join(","));
    put(
        "rule.urgent".into(),
        if rule.urgent { "true" } else { "false" },
    );
    put("rule.mode".into(), rule.mode.word());
    if !armed(Fault::RevSkipsField) {
        put("rule.text".into(), rule.text.as_str());
    }
    let named = rule
        .when
        .source
        .source()
        .and_then(|name| sources.iter().find(|s| &s.name == name));
    if let Some(source) = named {
        let at = |key: &str| format!("source.{}.{key}", source.name);
        put(at("kind"), source.kind_word());
        put(at("every_ms"), &source.every_ms().to_string());
        match &source.kind {
            SourceKind::Git(git) => {
                put(at("ref"), git.git_ref.as_str());
                if let Some(path) = &git.path {
                    put(at("path"), path.as_str());
                }
                match &git.origin {
                    Origin::Repo { path, remote } => {
                        put(at("repo"), path.as_str());
                        put(at("remote"), remote.as_str());
                    }
                    Origin::Url(url) => put(at("url"), &url.stored()),
                }
                if let Some(url) = source.remote(resolved)
                    && !armed(Fault::RevSkipsUrl)
                {
                    put(at("resolved_url"), &url.stored());
                }
            }
            SourceKind::Atpkg(atpkg) => put(at("program"), atpkg.program.as_str()),
        }
    }
    let mut out = String::from("owner-rule@1\n");
    for (key, value) in &lines {
        out.push_str(key);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }
    out
}

/// THE RULE'S REV: SHA-256 of [`canonical`].
#[must_use]
pub fn rev(rule: &OwnerRule, sources: &[Source], resolved: &Resolved) -> Rev {
    Rev(aterm_digest::Sha256::digest(
        canonical(rule, sources, resolved).as_bytes(),
    ))
}

/// WHAT A PERSON SEES BEFORE THEIR YES (S12): every line printed from the parsed form, never a
/// raw `when`: the rule's name and rev, its mode, `when` canonical, its source's kind, host and
/// path (or its program), auth mode and `every`, its reach in words, urgency and its text.
#[must_use]
pub fn rendering(rule: &OwnerRule, sources: &[Source], resolved: &Resolved) -> String {
    let rev = rev(rule, sources, resolved);
    let mut lines = vec![
        format!(
            "Rule {} (rev {}), {}",
            rule.name,
            rev.short(),
            rule.mode.word()
        ),
        format!("When: {}", rule.when.canonical()),
    ];
    lines.push(match rule.when.source.source() {
        None => "Source: this session's own Claude Code workflows".to_string(),
        Some(name) => sources.iter().find(|s| &s.name == name).map_or_else(
            || format!("Source: {name}, which these tables do not declare"),
            |source| source_words(source, resolved),
        ),
    });
    let modes: Vec<&str> = rule
        .modes
        .iter()
        .map(|m| match m {
            Autopilot::Cruise => "Cruise",
            Autopilot::Land => "Land",
            Autopilot::Off => "Off",
        })
        .collect();
    let modes = modes.join(" and ");
    lines.push(match &rule.scope {
        None => format!("Reach: every {modes} session on this Mac"),
        Some(glob) => format!(
            "Reach: {modes} sessions whose jobs run under {}",
            glob.canonical()
        ),
    });
    if rule.urgent {
        lines.push(
            "Urgent: passes the session's 20 min pace, at most 3 tells an hour; never a guard"
                .to_string(),
        );
    }
    lines.push(format!("Text: {}", rule.text.as_str()));
    lines.join("\n")
}

fn source_words(source: &Source, resolved: &Resolved) -> String {
    let every = span_text(source.every_ms());
    match &source.kind {
        SourceKind::Atpkg(atpkg) => format!(
            "Source {}: atpkg, the build the last verified index pins for {}, read on this Mac \
             every {every}",
            source.name,
            atpkg.program.as_str()
        ),
        SourceKind::Git(git) => {
            let url = source.remote(resolved);
            let at = match (url, &git.origin) {
                (Some(url), _) => format!("{}/{}", url.host(), url.path()),
                (None, Origin::Repo { path, remote }) => format!(
                    "the remote {} of {} (its URL not read yet)",
                    remote.as_str(),
                    path.as_str()
                ),
                (None, Origin::Url(url)) => url.stored(),
            };
            let by = url.map_or_else(String::new, |u| format!(" by {}", u.auth().words()));
            let path = git
                .path
                .as_ref()
                .map_or_else(String::new, |p| format!(", its tree entry {}", p.as_str()));
            format!(
                "Source {}: git-ref, {} of {at}{path}, read every {every}{by}",
                source.name,
                git.git_ref.as_str()
            )
        }
    }
}

/// WHETHER A RULE IS READ, AND WHETHER IT MAY TYPE (D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    /// The owner confirmed this whole rev and its `mode` is `live`.
    Live,
    /// The owner confirmed this whole rev and its `mode` is `dry-run`: read, judged, never typed.
    DryRun,
    /// No confirmation of this rev: not read, not judged, no row.
    Unconfirmed,
}

impl RuleState {
    /// Its rev's state, the owner's confirmed revs being `confirmed`.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OwnerTell",
            action = "Tell",
            project = "conformance_sources::project_tell"
        )
    )]
    pub fn of(rev: &Rev, confirmed: &[Rev], mode: RuleMode) -> Self {
        use super::fault::{Fault, armed};
        let yes = if armed(Fault::ConfirmByName) {
            !confirmed.is_empty()
        } else {
            confirmed.contains(rev)
        };
        match (yes, mode) {
            (false, _) => RuleState::Unconfirmed,
            (true, RuleMode::Live) => RuleState::Live,
            (true, RuleMode::DryRun) => RuleState::DryRun,
        }
    }

    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            RuleState::Live => "live",
            RuleState::DryRun => "dry-run",
            RuleState::Unconfirmed => "unconfirmed",
        }
    }
}

/// WHAT A PERSON'S PRESS OF Confirm SAW: the rule's name, its whole rev and its rendering. A yes
/// to it confirms that rev only while the table in force still hashes to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The rule's name.
    pub name: OwnerName,
    /// Its whole rev at the press.
    pub rev: Rev,
    /// What the alert shows ([`rendering`]).
    pub rendering: String,
}

impl Snapshot {
    /// The snapshot of the rule `name` of `tables`, if it reads.
    #[must_use]
    pub fn of(tables: &OwnerTables, name: &OwnerName, resolved: &Resolved) -> Option<Self> {
        let rule = tables.rule(name)?;
        Some(Self {
            name: name.clone(),
            rev: rev(rule, &tables.sources, resolved),
            rendering: rendering(rule, &tables.sources, resolved),
        })
    }

    /// Whether the rule it names still reads, in `tables` (the table in force), at its rev.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OwnerTell",
            action = "ConfirmWrite",
            project = "conformance_sources::project_tell"
        )
    )]
    pub fn still_current(&self, tables: &OwnerTables, resolved: &Resolved) -> bool {
        use super::fault::{Fault, armed};
        let Some(rule) = tables.rule(&self.name) else {
            return false;
        };
        armed(Fault::ConfirmStaleSnapshot) || rev(rule, &tables.sources, resolved) == self.rev
    }
}

/// THE OWNER'S TABLES, AS READ ([`Self::from_doc`]): the sources declared and the rules that read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwnerTables {
    sources: Vec<Source>,
    rules: Vec<OwnerRule>,
}

impl OwnerTables {
    /// THE ROOT `[harness]` TABLE'S OWNER TABLES of `doc` (aterm.toml, parsed), with one closed
    /// note per refusal. Any other table's copy is not read here.
    #[must_use]
    pub fn from_doc(doc: &aterm_toml::Value) -> (Self, Vec<String>) {
        let mut notes = Vec::new();
        let Some(root) = doc.get("harness").and_then(aterm_toml::Value::as_table) else {
            return (Self::default(), notes);
        };
        let sources = root
            .get(SOURCE_TABLE)
            .map(|v| read_sources(v, &mut notes))
            .unwrap_or_default();
        let rules = root
            .get(RULE_TABLE)
            .map(|v| read_rules(v, &sources, &mut notes))
            .unwrap_or_default();
        (Self { sources, rules }, notes)
    }

    /// [`Self::from_doc`] over aterm.toml's text; text TOML refuses reads no owner table.
    #[must_use]
    pub fn from_aterm_toml(text: &str) -> (Self, Vec<String>) {
        match aterm_toml::from_str::<aterm_toml::Value>(text) {
            Ok(doc) => Self::from_doc(&doc),
            Err(_) => (Self::default(), Vec::new()),
        }
    }

    /// The sources declared, by name.
    #[must_use]
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// The rules that read, in table order.
    #[must_use]
    pub fn rules(&self) -> &[OwnerRule] {
        &self.rules
    }

    /// The rule named `name`.
    #[must_use]
    pub fn rule(&self, name: &OwnerName) -> Option<&OwnerRule> {
        self.rules.iter().find(|r| &r.name == name)
    }

    /// The source named `name`.
    #[must_use]
    pub fn source(&self, name: &OwnerName) -> Option<&Source> {
        self.sources.iter().find(|s| &s.name == name)
    }

    /// Whether no rule reads.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The rule's rev in these tables.
    #[must_use]
    pub fn rev(&self, rule: &OwnerRule, resolved: &Resolved) -> Rev {
        rev(rule, &self.sources, resolved)
    }
}

fn read_sources(value: &aterm_toml::Value, notes: &mut Vec<String>) -> Vec<Source> {
    let Some(table) = value.as_table() else {
        notes.push(format!(
            "harness.{SOURCE_TABLE}: not a table of named sources — no source is read"
        ));
        return Vec::new();
    };
    let mut sources = Vec::new();
    for (written, value) in table {
        let Some(name) = OwnerName::parse(written) else {
            notes.push(format!(
                "harness.{SOURCE_TABLE}: a source's name takes 1 to 32 of a-z 0-9 - — that \
                 source is ignored"
            ));
            continue;
        };
        if sources.len() == OWNER_SOURCES_MAX {
            notes.push(format!(
                "harness.{SOURCE_TABLE}.{name}: at most {OWNER_SOURCES_MAX} sources are read — \
                 the source is ignored"
            ));
            continue;
        }
        match read_source(&name, value) {
            Ok(source) => sources.push(source),
            Err((key, why)) => notes.push(format!(
                "harness.{SOURCE_TABLE}.{name}{key}: {why} — the source is ignored"
            )),
        }
    }
    sources
}

/// A refusal: the key below the table (`.every`, empty for the table itself) and its sentence.
type Refused = (String, &'static str);

fn refused(key: &str, why: &'static str) -> Refused {
    let key = if key.is_empty() {
        String::new()
    } else {
        format!(".{key}")
    };
    (key, why)
}

fn read_source(name: &OwnerName, value: &aterm_toml::Value) -> Result<Source, Refused> {
    let table = value
        .as_table()
        .ok_or_else(|| refused("", "is not a table"))?;
    let text = |key: &str| -> Result<Option<&str>, Refused> {
        match table.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_str()
                .map(Some)
                .ok_or_else(|| refused(key, "takes a text")),
        }
    };
    let kind = text("kind")?.ok_or_else(|| refused("kind", "is required: git-ref or atpkg"))?;
    let allowed: &[&str] = match kind {
        "git-ref" => &["kind", "repo", "url", "remote", "ref", "path", "every"],
        "atpkg" => &["kind", "program", "every"],
        _ => return Err(refused("kind", "is git-ref or atpkg")),
    };
    if let Some((key, _)) = table.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
        let key = if word_of(key, NAME_MAX, |b| b.is_ascii_alphanumeric() || b == b'_').is_some() {
            key.as_str()
        } else {
            ""
        };
        return Err(refused(key, "is no key of a source of its kind"));
    }
    let every_ms = match text("every")? {
        None if kind == "atpkg" => ATPKG_EVERY_MS,
        None => GIT_EVERY_MS,
        Some(written) => span_ms(written)
            .filter(|ms| (EVERY_MIN_MS..=EVERY_MAX_MS).contains(ms))
            .ok_or_else(|| refused("every", "takes 1m to 1h, as <n>s, <n>m or <n>h"))?,
    };
    if kind == "atpkg" {
        let program = text("program")?
            .ok_or_else(|| refused("program", "is required"))
            .and_then(|p| {
                program_of(p).ok_or_else(|| refused("program", "takes 1 to 64 of a-z 0-9 -"))
            })?;
        return Ok(Source {
            name: name.clone(),
            kind: SourceKind::Atpkg(AtpkgSource { program, every_ms }),
        });
    }
    let origin = match (text("repo")?, text("url")?) {
        (Some(repo), None) => Origin::Repo {
            path: repo_path(repo).ok_or_else(|| {
                refused(
                    "repo",
                    "is an absolute or ~/ path of ASCII with no dot-named component",
                )
            })?,
            remote: match text("remote")? {
                None => Checked(DEFAULT_REMOTE.to_string()),
                Some(remote) => word_of(remote, REMOTE_MAX, |b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
                })
                .filter(|r| !r.starts_with('-'))
                .map(Checked)
                .ok_or_else(|| refused("remote", "takes 1 to 32 of A-Z a-z 0-9 . _ -"))?,
            },
        },
        (None, Some(url)) => {
            if table.get("remote").is_some() {
                return Err(refused("remote", "names a remote of `repo` alone"));
            }
            Origin::Url(RemoteUrl::parse(url).map_err(|why| {
                refused(
                    "url",
                    match why {
                        super::git_url::UrlRefusal::Userinfo => {
                            "carries a password or token, which aterm never reads"
                        }
                        super::git_url::UrlRefusal::Local => {
                            "names a repository on this Mac, which git would read with that \
                             repository's own config: aterm reads a remote"
                        }
                        _ => "is no https://, ssh:// or scp-style URL aterm reads",
                    },
                )
            })?)
        }
        (Some(_), Some(_)) => return Err(refused("url", "and repo are one or the other")),
        (None, None) => return Err(refused("", "names its repo or its url")),
    };
    let git_ref = match text("ref")? {
        None => Checked(DEFAULT_REF.to_string()),
        Some(written) => ref_of(written).ok_or_else(|| {
            refused(
                "ref",
                "is refs/heads/<name> or refs/tags/<name>, the name 1 to 48 of A-Z a-z 0-9 . _ / - \
                 with no ..",
            )
        })?,
    };
    let path = text("path")?
        .map(|p| {
            tree_path(p).ok_or_else(|| {
                refused(
                    "path",
                    "is relative, 1 to 64 of A-Z a-z 0-9 . _ / - with no .. component",
                )
            })
        })
        .transpose()?;
    Ok(Source {
        name: name.clone(),
        kind: SourceKind::Git(GitRef {
            origin,
            git_ref,
            path,
            every_ms,
        }),
    })
}

fn read_rules(
    value: &aterm_toml::Value,
    sources: &[Source],
    notes: &mut Vec<String>,
) -> Vec<OwnerRule> {
    let Some(items) = value.as_array() else {
        notes.push(format!(
            "harness.{RULE_TABLE}: not a list of rule tables (write [[harness.{RULE_TABLE}]]) — \
             no rule is read"
        ));
        return Vec::new();
    };
    let mut rules: Vec<OwnerRule> = Vec::new();
    for (at, item) in items.iter().enumerate() {
        let key = |k: &str| {
            if k.is_empty() {
                format!("harness.{RULE_TABLE}[{at}]")
            } else {
                format!("harness.{RULE_TABLE}[{at}].{k}")
            }
        };
        if rules.len() == OWNER_RULES_MAX {
            notes.push(format!(
                "{}: at most {OWNER_RULES_MAX} rules are read — the rule is ignored",
                key("")
            ));
            continue;
        }
        match read_rule(item, sources) {
            Ok(rule) if rules.iter().any(|r| r.name == rule.name) => notes.push(format!(
                "{}: names a rule already read — the rule is ignored",
                key("name")
            )),
            Ok(rule) => rules.push(rule),
            Err((k, why)) => notes.push(format!(
                "{}: {why} — the rule is ignored",
                key(k.trim_start_matches('.'))
            )),
        }
    }
    rules
}

fn read_rule(item: &aterm_toml::Value, sources: &[Source]) -> Result<OwnerRule, Refused> {
    const KEYS: [&str; 7] = ["name", "when", "scope", "modes", "urgent", "mode", "text"];
    let table = item
        .as_table()
        .ok_or_else(|| refused("", "is not a table"))?;
    if let Some((key, _)) = table.iter().find(|(k, _)| !KEYS.contains(&k.as_str())) {
        let key = if word_of(key, NAME_MAX, |b| b.is_ascii_alphanumeric() || b == b'_').is_some() {
            key.as_str()
        } else {
            ""
        };
        return Err(refused(key, "is no key of a rule"));
    }
    let text = |key: &str| -> Result<&str, Refused> {
        table
            .get(key)
            .ok_or_else(|| refused(key, "is required"))?
            .as_str()
            .ok_or_else(|| refused(key, "takes a text"))
    };
    let name = OwnerName::parse(text("name")?)
        .ok_or_else(|| refused("name", "takes 1 to 32 of a-z 0-9 -"))?;
    let when =
        OwnerWhen::parse(text("when")?, sources).map_err(|why| refused("when", why.words()))?;
    let scope = match table.get("scope") {
        None => None,
        Some(scope) => {
            let scope = scope
                .as_table()
                .ok_or_else(|| refused("scope", "is a table: { jobs_cwd = \"<glob>\" }"))?;
            if scope.get("claim").is_some() {
                return Err(refused("scope.claim", "arrives in slice 4"));
            }
            if scope.keys().any(|k| k != "jobs_cwd") {
                return Err(refused("scope", "takes jobs_cwd alone"));
            }
            let glob = scope
                .get("jobs_cwd")
                .ok_or_else(|| refused("scope.jobs_cwd", "is required in a scope"))?
                .as_str()
                .ok_or_else(|| refused("scope.jobs_cwd", "takes a text"))?;
            Some(CwdGlob::parse(glob).map_err(|why| refused("scope.jobs_cwd", why.words()))?)
        }
    };
    let modes = match table.get("modes") {
        None => vec![Autopilot::Cruise],
        Some(modes) => {
            let words = modes
                .as_array()
                .ok_or_else(|| refused("modes", "is a list of cruise and land"))?;
            let mut out = Vec::new();
            for word in words {
                let mode = match word.as_str() {
                    Some("cruise") => Autopilot::Cruise,
                    Some("land") => Autopilot::Land,
                    _ => return Err(refused("modes", "is a list of cruise and land")),
                };
                if !out.contains(&mode) {
                    out.push(mode);
                }
            }
            if out.is_empty() {
                return Err(refused("modes", "names cruise, land or both"));
            }
            out.sort_by_key(|m| u8::from(*m == Autopilot::Land));
            out
        }
    };
    let urgent = match table.get("urgent") {
        None => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| refused("urgent", "takes true or false"))?,
    };
    let mode = match table.get("mode") {
        None => RuleMode::Live,
        Some(v) => match v.as_str() {
            Some("live") => RuleMode::Live,
            Some("dry-run") => RuleMode::DryRun,
            _ => return Err(refused("mode", "is live or dry-run")),
        },
    };
    let text = rule_text(text("text")?).ok_or_else(|| {
        refused(
            "text",
            "takes 1 to 200 characters with no control, bidi or invisible character, no \
             backtick and no harness mark",
        )
    })?;
    Ok(OwnerRule {
        name,
        when,
        scope,
        modes,
        urgent,
        mode,
        text,
    })
}

fn word_of(text: &str, max: usize, ok: impl Fn(u8) -> bool) -> Option<String> {
    ((1..=max).contains(&text.len()) && text.bytes().all(ok)).then(|| text.to_string())
}

fn program_of(text: &str) -> Option<Checked> {
    word_of(text, PROGRAM_MAX, |b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
    })
    .map(Checked)
}

fn ref_of(text: &str) -> Option<Checked> {
    let short = text
        .strip_prefix("refs/heads/")
        .or_else(|| text.strip_prefix("refs/tags/"))?;
    let shaped = word_of(short, REF_SHORT_MAX, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-')
    })
    .is_some()
        && !short.contains("..")
        && short
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.') && !seg.starts_with('-'))
        && !short.ends_with(".lock");
    shaped.then(|| Checked(text.to_string()))
}

fn tree_path(text: &str) -> Option<Checked> {
    let shaped = word_of(text, SOURCE_PATH_MAX, |b| {
        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-')
    })
    .is_some()
        && text
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.starts_with('-'));
    shaped.then(|| Checked(text.to_string()))
}

/// A `repo`: absolute or `~/…`, ASCII with no space, no control, bidi or invisible character,
/// no empty or dot-named component, at most a wake path's length. Its admission (under the
/// home, outside every privacy-guarded root, no link) is the reader's, before it opens anything.
fn repo_path(text: &str) -> Option<Checked> {
    if text.len() > PATH_MAX
        || text.chars().any(hidden)
        || !text.is_ascii()
        || text.contains(char::is_whitespace)
    {
        return None;
    }
    let body = text.strip_prefix("~/").or_else(|| text.strip_prefix('/'))?;
    let body = body.strip_suffix('/').unwrap_or(body);
    let shaped = !body.is_empty()
        && body
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.') && !seg.starts_with('-'));
    shaped.then(|| Checked(text.strip_suffix('/').unwrap_or(text).to_string()))
}

fn rule_text(text: &str) -> Option<RuleText> {
    let count = text.chars().count();
    let shaped = (1..=TEXT_MAX).contains(&count)
        && !text.trim().is_empty()
        && !text.chars().any(|c| hidden(c) || c == '`')
        && !text
            .to_ascii_lowercase()
            .contains(&harness_mark!().to_ascii_lowercase());
    shaped.then(|| RuleText(text.to_string()))
}

#[cfg(test)]
#[path = "owner_rules_tests.rs"]
mod tests;
