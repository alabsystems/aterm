// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s run records (`docs/DESIGN-judged-main-release-pump-2026-11-11.md` §3.1,
//! JM-01): one signed record for each judgment, round, compose and abort of a landing run, and
//! one at its end whatever its exit (but a run killed outright, or held past the interrupt's
//! report grace by a step it does not cut short), filed in the journal ([`crate::judged`]) beside the verdict
//! records, at `land/runs/<utc-day>/<run>.<seq>.<kind>.toml` ([`Filed`]), and kept in the
//! clone's spool ([`crate::land_spool`]) until an append holds them. The verdict records and these
//! are one spool and one store, with a retention per kind: a run record keeps
//! [`crate::judged::RUN_RETENTION`], a verdict record [`crate::judged::LAND_RETENTION`].
//!
//! - A **judgment** record is a judgment that ran to its end: its cause, result, red items and
//!   the checks that did not run. A **round** record is a round's (`docs/DESIGN-landing-economy.md`
//!   §2.1c), with the crates it judged. A **compose** record is a passed judgment composed onto a
//!   main that moved before its push. An **abort** is a judgment the landing gave up because of
//!   main's tip: `cut` while it ran, or `doomed` at the push after it passed; it names the tip,
//!   the paths it shares with the branch and what each side's change reaches. The **end** record
//!   is the run's exit, the signal that interrupted it, and, for a run that landed, the landing's
//!   commit and kind.
//! - A body carries its own signature, as a verdict record's does ([`crate::land_record::seal`]),
//!   under [`DOMAIN`], so a run record never reads as a verdict record nor one as the other.
//! - `verify land --census` reads them ([`crate::land_census`]). Nothing reads them to decide a
//!   landing.

use serde::{Deserialize, Serialize};

use crate::land_record::{Item, Origin, Signature, Signed, Signer, is_token, tree_id};

/// The body's schema, which a reader must know to read it.
pub const SCHEMA: u32 = 1;
/// The signature domain of a run record ([`aterm_net::fleet::record_message`]).
pub const DOMAIN: &str = "aterm-verify/land-run/v1";
/// The journal directory every run record is filed under, inside the verdict records' `land/`.
pub const DIR: &str = "land/runs";
/// The most items, shared paths or crates one list of a record holds; the rest are counted.
pub const LISTED: usize = 64;
/// The longest reason a record keeps, in bytes; a longer one is cut at a character boundary.
pub const WHY_BYTES: usize = 1024;

/// What a run record records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Judgment,
    Round,
    Compose,
    Abort,
    End,
}

impl Kind {
    pub const ALL: [Self; 5] = [
        Self::Judgment,
        Self::Round,
        Self::Compose,
        Self::Abort,
        Self::End,
    ];

    /// As a body and a file name spell it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Judgment => "judgment",
            Self::Round => "round",
            Self::Compose => "compose",
            Self::Abort => "abort",
            Self::End => "end",
        }
    }

    /// The kind `word` spells.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }
}

/// Where a path files a run record: `land/runs/<utc-day>/<run>.<seq>.<kind>.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    /// The UTC day, as days since the epoch, of the record's moment.
    pub day: u64,
    /// The run's id: 32 lowercase hex digits.
    pub run: String,
    /// The record's ordinal in its run, from 0.
    pub seq: u32,
    pub kind: Kind,
}

impl Filed {
    /// Where `path` files a run record.
    ///
    /// # Errors
    /// It is not `land/runs/<day>/<run>.<seq>.<kind>.toml` with a canonical decimal day and
    /// ordinal, a 32-hex run id and a kind this reader knows.
    pub fn parse(path: &str) -> Result<Self, String> {
        let not = || format!("{path:?} is not {DIR}/<day>/<run>.<seq>.<kind>.toml");
        let rest = path
            .strip_prefix(DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(not)?;
        let (day, file) = rest.split_once('/').ok_or_else(not)?;
        let day = crate::land_record::day_of(day).ok_or_else(not)?;
        let parts: Vec<&str> = file.split('.').collect();
        let [run, seq, kind, "toml"] = parts[..] else {
            return Err(not());
        };
        if !aterm_codec::hex::is_lower(run, 32) {
            return Err(format!("{path:?} names no run"));
        }
        let seq = seq
            .parse::<u32>()
            .ok()
            .filter(|n| n.to_string() == seq)
            .ok_or_else(|| format!("{path:?} names no ordinal"))?;
        let kind = Kind::of_word(kind).ok_or_else(|| format!("{path:?} names no kind"))?;
        Ok(Self {
            day,
            run: run.to_owned(),
            seq,
            kind,
        })
    }

    /// The file name: `<run>.<seq>.<kind>.toml`.
    #[must_use]
    pub fn file(&self) -> String {
        format!("{}.{}.{}.toml", self.run, self.seq, self.kind.word())
    }

    /// The path: `land/runs/<day>/<run>.<seq>.<kind>.toml`.
    #[must_use]
    pub fn path(&self) -> String {
        format!("{DIR}/{}/{}", self.day, self.file())
    }
}

/// The run a record came from: its id, when it started, and the Mac and session that ran it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// 32 lowercase hex digits, the same in every image the run handed itself to.
    pub id: String,
    /// When the run started, in Unix seconds.
    pub started: u64,
    /// The Mac's kernel host name, as a token ([`is_token`]).
    pub host: String,
    /// Its fleet node id, where it has a node identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The aterm session the run was started in (`ATERM_SESSION_ID`), as a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The Mac's physical memory, in MiB, where it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// Its logical cores, where they were read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
}

/// What a set of changed paths reached when the record was made ([`crate::land::Reach`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reached {
    /// `crates`, `everything` or `unread`.
    pub word: String,
    /// The crates, for `crates` (at most [`LISTED`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crates: Vec<String>,
    /// How many crates it named.
    #[serde(default)]
    pub crates_total: usize,
    /// Why it reached every crate, or was not read.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

impl Reached {
    /// `reach` as a record keeps it.
    #[must_use]
    pub fn of(reach: &crate::land::Reach) -> Self {
        use crate::land::Reach;
        match reach {
            Reach::Crates(crates) => Self {
                word: "crates".to_owned(),
                crates: crates.iter().take(LISTED).cloned().collect(),
                crates_total: crates.len(),
                why: String::new(),
            },
            Reach::Everything(why) => Self {
                word: "everything".to_owned(),
                why: clip(why),
                ..Self::default()
            },
            Reach::Unread(why) => Self {
                word: "unread".to_owned(),
                why: clip(why),
                ..Self::default()
            },
        }
    }
}

/// Why a judgment or a round ran, or what made a judgment abort.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cause {
    /// A judgment: `first` (the run's first), `cut` or `doomed` (after an abort of that word),
    /// `no-round` (no round answers for a merge), `merge-differs` (the merge's tree was not the
    /// one the composition tested) or `handed` (resumed from another image's handoff with no
    /// cause). A round: `shared` (main moved into a path the branch shares, before the push),
    /// `kept-merge` (HEAD merges main into the judgment an earlier run kept, a person's merge or
    /// the tool's own) or `handed` (resumed from another image's handoff with no cause). An
    /// abort: `cut` or `doomed`. A compose: `disjoint`.
    pub word: String,
    /// The main tip that caused it; empty where none did.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tip: String,
    /// The tool's reason, as it said it (at most [`WHY_BYTES`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The paths the tip shares with the branch, where they were read (at most [`LISTED`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared: Vec<String>,
    /// How many shared paths there were.
    #[serde(default)]
    pub shared_total: usize,
    /// What the tip's change, with the shared paths, reaches; where it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<Reached>,
    /// What the branch's own change reaches; where it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<Reached>,
}

impl Cause {
    /// The cause `word`, from main tip `tip` (empty: none), for `why`.
    #[must_use]
    pub fn new(word: &str, tip: &str, why: &str) -> Self {
        Self {
            word: word.to_owned(),
            tip: tip.to_owned(),
            why: clip(why),
            ..Self::default()
        }
    }

    /// This cause, naming `shared` as the paths the tip shares with the branch.
    #[must_use]
    pub fn sharing(mut self, shared: &[String]) -> Self {
        self.shared = shared.iter().take(LISTED).cloned().collect();
        self.shared_total = shared.len();
        self
    }
}

/// What a landed run put on main.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Landing {
    /// The landing commit main holds.
    pub commit: String,
    /// The main commit it was pushed onto.
    pub onto: String,
    /// `exact` (onto the main its last judgment merged), `composed` (onto a main that moved
    /// since, its last qualification a judgment), `rechecked` (its last qualification a round) or
    /// `markdown` (a round of shared Markdown no build reads alone).
    pub kind: String,
    /// Whether the run judged nothing, resuming the judgment an earlier run kept
    /// ([`crate::land::KEPT`]).
    #[serde(default)]
    pub resumed: bool,
}

/// One run record's body: everything above its `[signature]` line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// [`SCHEMA`].
    pub schema: u32,
    /// The kind's word ([`Kind::word`]).
    pub kind: String,
    /// Its ordinal in its run: its name's.
    pub seq: u32,
    /// When the step began, in Unix seconds.
    pub started: u64,
    /// When the record was made, in Unix seconds: the path's day.
    pub at: u64,
    /// The step's wall seconds (the run's, for an end).
    pub secs: u64,
    /// The commit the step ran at (HEAD); empty where none was read.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head: String,
    /// The main commit it merged; empty where none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// The tree it judged; empty where none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tree: String,
    /// A judgment's or a round's `pass`, `fail` or `not-run`; an abort's `cut` or `doomed`; a
    /// compose's `composed`; an end's `landed`, `nothing` (nothing to land) or `not-landed`.
    pub result: String,
    /// Each red item (at most [`LISTED`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<Item>,
    /// How many red items there were.
    #[serde(default)]
    pub items_total: usize,
    /// The checks, and the reasons, a judgment or a round did not run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_run: Vec<String>,
    /// The crates a judgment's or a round's crate-scoped rows judged, at most [`LISTED`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crates: Vec<String>,
    /// How many crates it judged.
    #[serde(default)]
    pub crates_total: usize,
    /// Whether a crate-scoped row of the round was widened to the whole workspace.
    #[serde(default)]
    pub widened: bool,
    /// An end's exit code, as the landing returned it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// An end's interrupt: the signal the landing took (`SIGINT`, `SIGTERM`, `SIGHUP`), which it
    /// then dies of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// An end's: whether a judgment that passed is kept for a later run ([`crate::land::KEPT`]).
    #[serde(default)]
    pub kept: bool,
    pub run: Run,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing: Option<Landing>,
}

impl Record {
    /// Where it is filed: under the UTC day of its moment, its run, its ordinal and its kind.
    ///
    /// # Errors
    /// Its kind is no run record's, or its run cannot spell a path.
    pub fn filed(&self) -> Result<Filed, String> {
        let kind = Kind::of_word(&self.kind)
            .ok_or_else(|| format!("{:?} is no record's kind", self.kind))?;
        let filed = Filed {
            day: self.at / 86_400,
            run: self.run.id.clone(),
            seq: self.seq,
            kind,
        };
        let path = filed.path();
        if Filed::parse(&path).as_ref() != Ok(&filed) {
            return Err(format!("{path:?} is not a run record's path"));
        }
        Ok(filed)
    }

    /// The record as its file holds it, signed by `signer` where one is given.
    ///
    /// # Errors
    /// It cannot be filed or encoded, or it is longer than a reader reads
    /// ([`crate::land_record::seal`]).
    pub fn sign(&self, signer: Option<&dyn Signer>) -> Result<Signed, String> {
        let path = self.filed()?.path();
        let text = aterm_toml::to_string(self).map_err(|e| e.to_string())?;
        Ok(Signed {
            path,
            bytes: crate::land_record::seal(text, signer, DOMAIN)?,
        })
    }
}

/// `items`, at most [`LISTED`] of them, and how many there were.
#[must_use]
pub fn listed<T: Clone>(items: &[T]) -> (Vec<T>, usize) {
    (items.iter().take(LISTED).cloned().collect(), items.len())
}

/// `why` within [`WHY_BYTES`], cut at a character boundary, on one line.
#[must_use]
pub fn clip(why: &str) -> String {
    let line = why.replace(['\n', '\r'], " ");
    if line.len() <= WHY_BYTES {
        return line;
    }
    let mut end = WHY_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

/// A run record that passed every check on read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub record: Record,
    /// Whose key signed it; `None` where it was written unsigned.
    pub signature: Option<Signature>,
}

/// One run record a reader found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its journal path.
    pub path: String,
    pub origin: Origin,
    /// Where its path files it; `Err` for a path that does not parse.
    pub filed: Result<Filed, String>,
    /// The body, when it passed every check; else why not.
    pub body: Result<Box<Read>, String>,
}

impl Entry {
    /// The entry at `path` holding `bytes`, held to every check: the name parses; the body is
    /// UTF-8 within [`crate::land_record::MAX_BYTES`] of schema [`SCHEMA`], its signature, where
    /// it has one, this domain's; it names the path's run, ordinal and kind, and was made on the
    /// path's day; each commit it names (head, base, a cause's tip, a landing's commit and onto)
    /// is an object id and its tree a tree id, where named; and its host and session are tokens.
    #[must_use]
    pub fn read(path: &str, bytes: &[u8], origin: Origin) -> Self {
        let filed = Filed::parse(path);
        let body = match &filed {
            Ok(filed) => read_body(filed, bytes).map(Box::new),
            Err(_) => Err("its path does not parse".to_owned()),
        };
        Self {
            path: path.to_owned(),
            origin,
            filed,
            body,
        }
    }

    /// The entry at `path` that could not be read at all: `why`.
    #[must_use]
    pub fn unread(path: &str, origin: Origin, why: String) -> Self {
        Self {
            path: path.to_owned(),
            origin,
            filed: Filed::parse(path),
            body: Err(why),
        }
    }
}

fn read_body(filed: &Filed, bytes: &[u8]) -> Result<Read, String> {
    let (body, signature) = crate::land_record::open(bytes, SCHEMA, DOMAIN)?;
    let record: Record =
        aterm_toml::from_str(body).map_err(|e| format!("its body does not parse: {e}"))?;
    if record.run.id != filed.run {
        return Err(format!(
            "its body is run {}'s, not the {} its path names",
            record.run.id, filed.run
        ));
    }
    if record.seq != filed.seq || Kind::of_word(&record.kind) != Some(filed.kind) {
        return Err(format!(
            "its body is record {} ({}), not the {} ({}) its name says",
            record.seq,
            record.kind,
            filed.seq,
            filed.kind.word()
        ));
    }
    if record.at / 86_400 != filed.day {
        return Err(format!(
            "it was made on day {}, not the {} its path names",
            record.at / 86_400,
            filed.day
        ));
    }
    let commit = |id: &str| id.is_empty() || aterm_types::land_trailer::object_id(id);
    let mut commits = vec![record.head.as_str(), record.base.as_str()];
    if let Some(cause) = &record.cause {
        commits.push(&cause.tip);
    }
    if let Some(landing) = &record.landing {
        if landing.commit.is_empty() || landing.onto.is_empty() {
            return Err("its landing names no commit".to_owned());
        }
        commits.extend([landing.commit.as_str(), landing.onto.as_str()]);
    }
    if !commits.into_iter().all(commit) || !(record.tree.is_empty() || tree_id(&record.tree)) {
        return Err("a commit or a tree it names is not an object id".to_owned());
    }
    let tokens = [Some(&record.run.host), record.run.session.as_ref()];
    if record.run.host.is_empty() || !tokens.into_iter().flatten().all(|t| is_token(t)) {
        return Err("its host or its session is not a token".to_owned());
    }
    if record
        .run
        .node
        .as_deref()
        .is_some_and(|node| !aterm_net::fleet::is_node_id(node))
    {
        return Err("its node is no node id".to_owned());
    }
    Ok(Read { record, signature })
}

#[cfg(test)]
#[path = "land_run_tests.rs"]
pub(crate) mod tests;
