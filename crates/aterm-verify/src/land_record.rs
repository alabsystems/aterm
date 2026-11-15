// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify land`'s verdict records (`docs/DESIGN-landing-economy.md` slice 4): one record per
//! check of a judged tree T, filed in the journal ([`crate::judged`]) at
//! `land/<utc-day>/<T>/<check>.<key>.<word>.toml` ([`Filed`]) and kept in the clone's spool until
//! an append holds it ([`crate::land_spool`]).
//!
//! The file name is the contract every writer keeps, whatever its body's schema: it names the
//! check, its key and its word, so a FAIL can be told by its name even where its body does not
//! read, and a FAIL and a pass at one key are two paths, so a pass never overwrites a FAIL. A
//! name that does not parse is corruption, not a verdict.
//!
//! The body carries its own signature: this Mac's fleet node key ([`Signer`]) over every byte
//! above its `[signature]` line, as a record of [`DOMAIN`], so a land record can never be a fleet
//! book entry or a channel proof, nor either of those a land record. A record written with no
//! node identity has no `[signature]` table. A reader holds every record to [`Entry::read`]'s
//! checks. Records are written and listed (`verify land records`); nothing reads them to decide a
//! landing yet.

use std::collections::BTreeMap;

use aterm_net::fleet::{NodeKey, SIG_LEN};
use serde::{Deserialize, Serialize};

use crate::event_log::{CheckRow, Planned};

/// The body's schema, which a reader must know to read it.
pub const SCHEMA: u32 = 1;
/// The signature domain of a land record ([`aterm_net::fleet::record_message`]).
pub const DOMAIN: &str = "aterm-verify/land-check/v1";
/// The one signature algorithm: a fleet node key's.
pub const ALG: &str = "ed25519";
/// The key schema of a record whose run kept no plan: `H(UNKEYED_SCHEMA, check, T, base)`. No plan
/// keys it, so it names no inputs; only a FAIL is written unkeyed, so the failure is kept.
pub const UNKEYED_SCHEMA: &str = "aterm-land-check-v1-unkeyed";
/// The journal directory every record is filed under.
pub const DIR: &str = "land";
/// The longest record a reader reads; a longer one is unreadable (a record is about 2 KiB).
pub const MAX_BYTES: u64 = 64 * 1024;
/// What separates a record's body from its signature table: the reader splits at the last one.
const SIGNATURE: &str = "\n[signature]\n";

/// A record's outcome word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Word {
    Ok,
    Skip,
    Fail,
    NotRun,
}

impl Word {
    pub const ALL: [Self; 4] = [Self::Ok, Self::Skip, Self::Fail, Self::NotRun];

    /// As a check row and a record's body spell it: `ok`, `skip`, `FAIL`, `NOT RUN`.
    #[must_use]
    pub const fn spelled(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skip => "skip",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT RUN",
        }
    }

    /// As a record's file name spells it: `ok`, `skip`, `FAIL`, `NOTRUN`.
    #[must_use]
    pub const fn named(self) -> &'static str {
        match self {
            Self::NotRun => "NOTRUN",
            word => word.spelled(),
        }
    }

    /// The word a row or a body spells.
    #[must_use]
    pub fn of_spelled(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.spelled() == word)
    }

    /// The word a file name spells.
    #[must_use]
    pub fn of_named(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.named() == word)
    }
}

/// What a record's file name says: `<check>.<key>.<word>.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// The check: lowercase letters, digits and `-`.
    pub check: String,
    /// Its key: 64 lowercase hex digits.
    pub key: String,
    /// Its word; `Err` holds a word this reader does not know.
    pub word: Result<Word, String>,
}

impl Name {
    /// The name `file` spells.
    ///
    /// # Errors
    /// It is not `<check>.<64 lowercase hex>.<word>.toml`, its check being lowercase letters,
    /// digits and `-`, and its word letters, digits, `_` and `-`.
    pub fn parse(file: &str) -> Result<Self, String> {
        let parts: Vec<&str> = file.split('.').collect();
        let [check, key, word, "toml"] = parts[..] else {
            return Err(format!("{file:?} is not <check>.<key>.<word>.toml"));
        };
        if check.is_empty()
            || !check
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(format!("{file:?} names no check"));
        }
        if !aterm_codec::hex::is_lower(key, 64) {
            return Err(format!("{file:?}'s key is not 64 lowercase hex digits"));
        }
        if word.is_empty()
            || !word
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(format!("{file:?} names no word"));
        }
        Ok(Self {
            check: check.to_owned(),
            key: key.to_owned(),
            word: Word::of_named(word).ok_or_else(|| word.to_owned()),
        })
    }

    /// The file name: `<check>.<key>.<word>.toml`.
    #[must_use]
    pub fn file(&self) -> String {
        let word = match &self.word {
            Ok(word) => word.named(),
            Err(word) => word,
        };
        format!("{}.{}.{word}.toml", self.check, self.key)
    }

    /// The word as a line names it: `FAIL`, or the word this reader does not know.
    #[must_use]
    pub fn word_label(&self) -> &str {
        match &self.word {
            Ok(word) => word.spelled(),
            Err(word) => word,
        }
    }
}

/// Where a path files a record: `land/<utc-day>/<T>/<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    /// The UTC day, as days since the epoch, of the record's end.
    pub day: u64,
    /// The tree it judged.
    pub tree: String,
    pub name: Name,
}

/// The day a directory name spells: decimal digits, canonical.
#[must_use]
pub fn day_of(name: &str) -> Option<u64> {
    let day = name.parse::<u64>().ok()?;
    (day.to_string() == name).then_some(day)
}

/// Whether `tree` can name a tree directory: a SHA-1 or SHA-256 object id, lowercase hex.
#[must_use]
pub fn tree_id(tree: &str) -> bool {
    aterm_codec::hex::is_lower(tree, 40) || aterm_codec::hex::is_lower(tree, 64)
}

/// Whether `text` is spelled in a trailer token's alphabet alone: letters, digits, `.`, `_` and
/// `-` (a record's host and run id, which a line and a trailer print as they are).
#[must_use]
pub fn is_token(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `text` in a trailer token's alphabet ([`is_token`]): each other character as `-`.
#[must_use]
pub fn tokened(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

impl Filed {
    /// Where `path` files a record.
    ///
    /// # Errors
    /// It is not `land/<day>/<tree>/<name>` with a canonical decimal day, an object id and a
    /// [`Name`].
    pub fn parse(path: &str) -> Result<Self, String> {
        let mut parts = path.split('/');
        let (Some(DIR), Some(day), Some(tree), Some(file), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(format!("{path:?} is not {DIR}/<day>/<tree>/<name>"));
        };
        let day = day_of(day).ok_or_else(|| format!("{path:?} names no day"))?;
        if !tree_id(tree) {
            return Err(format!("{path:?} names no tree"));
        }
        Ok(Self {
            day,
            tree: tree.to_owned(),
            name: Name::parse(file)?,
        })
    }

    /// The path: `land/<day>/<tree>/<name>`.
    #[must_use]
    pub fn path(&self) -> String {
        format!("{DIR}/{}/{}/{}", self.day, self.tree, self.name.file())
    }
}

/// One red item as a key, not prose: the check whose row named it, the finding id the row
/// carried, and, where the id is a test's (`-p <spec> <target> -- <test path>`,
/// [`crate::findings::test_findings`]), its package, its target (`lib`, `doc`, or
/// `test:<name>`, `bin:<name>`, `example:<name>`, `bench:<name>`) and its test path. A reader
/// matches items on these fields; the id is kept for what no field names (a lint's, a row's).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub check: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

impl Item {
    /// The item `check`'s row names as finding `id`: a test's package, target and test path
    /// where `id` is `-p <spec> <target> -- <test path>` with one package and a target this
    /// reader knows; else the check and the id alone.
    #[must_use]
    pub fn of_finding(check: &str, id: &str) -> Self {
        let mut item = Self {
            check: check.to_owned(),
            id: id.to_owned(),
            ..Self::default()
        };
        if let Some((spec, test)) = id.split_once(" -- ")
            && !test.is_empty()
            && let words = spec.split_whitespace().collect::<Vec<_>>()
            && let ["-p", package, ref target @ ..] = words[..]
            && let Some(target) = target_of(target)
            && let Some(package) = crate::scope::spec_name(package)
        {
            item.package = Some(package.to_owned());
            item.target = Some(target);
            item.test = Some(test.to_owned());
        }
        item
    }
}

/// A test target as cargo's rerun spec names it: `--lib`, `--doc`, or `--test`, `--bin`,
/// `--example` or `--bench` and its name.
fn target_of(words: &[&str]) -> Option<String> {
    match words {
        ["--lib"] => Some("lib".to_owned()),
        ["--doc"] => Some("doc".to_owned()),
        [flag @ ("--test" | "--bin" | "--example" | "--bench"), name] if !name.starts_with('-') => {
            Some(format!("{}:{name}", &flag[2..]))
        }
        _ => None,
    }
}

/// The crates a crate-scoped check judged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// By `-p` spec; none where `widened` holds.
    pub crates: Vec<String>,
    /// The selection was the whole workspace: every crate.
    pub widened: bool,
}

/// This Mac's attributes, outside the key ([`crate::land_seal::LOCAL`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Local {
    /// The run's local digest ([`crate::land_seal::local_digest`]); empty for a run that kept no
    /// plan.
    pub digest: String,
    /// The host name of the Mac that wrote the record.
    pub host: String,
    /// Each local attribute's digest, by name.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// The run a record came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// The run's invocation id (`event_log`'s 32-hex id), for local diagnostics.
    pub id: String,
    /// When the run started, in Unix seconds.
    pub started: u64,
    /// When the run ended; the record's day.
    pub ended: u64,
    /// The run's wall seconds; absent where the writer did not know them.
    #[serde(default)]
    pub secs: Option<u64>,
    /// The run's `run-finished` agrees with its rows ([`crate::event_log::Judged::whole`]);
    /// absent where the writer did not know it.
    #[serde(default)]
    pub whole: Option<bool>,
    /// What the run's keys sealed stood through it
    /// ([`crate::event_log::Judged::sealed_through`]); absent where the writer did not know it.
    #[serde(default)]
    pub sealed_through: Option<bool>,
}

/// One check's verdict on a tree: everything above a record's `[signature]` line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// [`SCHEMA`].
    pub schema: u32,
    pub check: String,
    /// `ok`, `skip`, `FAIL` or `NOT RUN`: the name's word ([`Word::spelled`]).
    pub word: String,
    /// `H(KEY_SCHEMA, check, tree, part, seal)`, or the unkeyed key ([`Record::expected_key`]).
    pub key: String,
    /// T.
    pub tree: String,
    /// The commit judged.
    pub commit: String,
    /// The main commit T merged, which the key's part names.
    pub base: String,
    /// A FAIL's first finding; why a check did not run or was skipped.
    pub cause: String,
    /// The finding ids the row carried (at most `report::FINDINGS_SHOWN`).
    pub findings: Vec<String>,
    /// How many findings the check had.
    pub findings_total: usize,
    /// Each finding as a key ([`Item::of_finding`]): the check, the id, and a test's package,
    /// target and test path.
    pub items: Vec<Item>,
    /// The check's planned part ([`crate::checks::part`]); empty when unkeyed.
    pub part: String,
    /// Its portable seal ([`crate::land_seal::seal`]); empty when unkeyed.
    pub seal: String,
    /// The crates a crate-scoped check judged ([`crate::checks::Check::crate_scoped`]: lint,
    /// test, doc).
    #[serde(default)]
    pub scope: Option<Scope>,
    /// Each portable field the seal names, with its digest, so a mismatch names the field.
    #[serde(default)]
    pub seal_fields: BTreeMap<String, String>,
    pub local: Local,
    pub run: Run,
}

/// What a land judged when a row came: the commit, its tree, the main commit it merged, this
/// Mac's host name, and the run's `run-planned` where it was read.
#[derive(Clone, Copy, Debug)]
pub struct Judging<'a> {
    pub commit: &'a str,
    pub tree: &'a str,
    pub base: &'a str,
    pub host: &'a str,
    pub planned: Option<&'a Planned>,
}

impl Record {
    /// The record of `row`, judged `at`, from `run`. It is keyed when the run's plan judged this
    /// tree from a clean checkout and keyed this check with the row's own key: then the base,
    /// part, seal and local attributes are the plan's. Otherwise (a dirty checkout judged what no
    /// tree holds) it is unkeyed under `at`'s base, naming no inputs, so a FAIL is still
    /// recorded.
    /// The host and the run id are written in a token's alphabet ([`tokened`]), as its reader
    /// requires. `None` for a row whose word is no record's.
    #[must_use]
    pub fn of_row(row: &CheckRow, at: &Judging<'_>, mut run: Run) -> Option<Self> {
        let word = Word::of_spelled(&row.word)?;
        run.id = tokened(&run.id);
        let cause = match (word, row.findings.first()) {
            (Word::Ok, _) => String::new(),
            (Word::Fail, Some(first)) => first.clone(),
            _ => row.detail.clone(),
        };
        let mut record = Self {
            schema: SCHEMA,
            check: row.check.clone(),
            word: row.word.clone(),
            key: String::new(),
            tree: at.tree.to_owned(),
            commit: at.commit.to_owned(),
            base: at.base.to_owned(),
            cause,
            findings: row.findings.clone(),
            findings_total: row.findings_total,
            items: row
                .findings
                .iter()
                .map(|id| Item::of_finding(&row.check, id))
                .collect(),
            part: String::new(),
            seal: String::new(),
            scope: row
                .scope
                .clone()
                .map(|(crates, widened)| Scope { crates, widened }),
            seal_fields: BTreeMap::new(),
            local: Local {
                digest: String::new(),
                host: match tokened(at.host) {
                    host if host.is_empty() => "unknown".to_owned(),
                    host => host,
                },
                fields: BTreeMap::new(),
            },
            run,
        };
        let planned = at
            .planned
            .filter(|p| p.clean && p.tree == at.tree)
            .and_then(|p| {
                let Some(crate::checks::Word::Check(check)) = crate::checks::lookup(&row.check)
                else {
                    return None;
                };
                let planned = p.row(check)?;
                (row.key.as_deref() == Some(planned.key.as_str())).then_some((p, planned))
            });
        if let Some((plan, planned)) = planned {
            record.base.clone_from(&plan.base);
            record.part.clone_from(&planned.part);
            record.seal.clone_from(&planned.seal);
            record.seal_fields.clone_from(&planned.fields);
            record.local.digest.clone_from(&plan.local);
            record.local.fields.clone_from(&plan.local_fields);
        }
        record.key = record.expected_key();
        Some(record)
    }

    /// Whether a plan keyed it: it names a part and a seal.
    #[must_use]
    pub fn keyed(&self) -> bool {
        !self.part.is_empty() && !self.seal.is_empty()
    }

    /// The key its body gives: `H(`[`crate::land_seal::KEY_SCHEMA`]`, check, tree, part, seal)`
    /// ([`crate::land_seal::key`]) when keyed, else `H(`[`UNKEYED_SCHEMA`]`, check, tree, base)`.
    #[must_use]
    pub fn expected_key(&self) -> String {
        if self.keyed() {
            crate::land_seal::hash([
                crate::land_seal::KEY_SCHEMA,
                &self.check,
                &self.tree,
                &self.part,
                &self.seal,
            ])
        } else {
            crate::land_seal::hash([UNKEYED_SCHEMA, &self.check, &self.tree, &self.base])
        }
    }

    /// Where it is filed: under the UTC day of its end, its tree, and its name.
    ///
    /// # Errors
    /// Its word is no record's, or its check, key or tree cannot spell a path.
    pub fn filed(&self) -> Result<Filed, String> {
        let word = Word::of_spelled(&self.word)
            .ok_or_else(|| format!("{:?} is no record's word", self.word))?;
        let filed = Filed {
            day: self.run.ended / 86_400,
            tree: self.tree.clone(),
            name: Name {
                check: self.check.clone(),
                key: self.key.clone(),
                word: Ok(word),
            },
        };
        let path = filed.path();
        if Filed::parse(&path).as_ref() != Ok(&filed) {
            return Err(format!("{path:?} is not a record's path"));
        }
        Ok(filed)
    }

    /// The record as its file holds it: its body, then, where `signer` is given, the
    /// `[signature]` table signing every byte of it.
    ///
    /// # Errors
    /// It cannot be filed ([`Record::filed`]) or encoded, its body would hold a second signature
    /// line, or the whole is longer than [`MAX_BYTES`], which no reader reads.
    pub fn sign(&self, signer: Option<&dyn Signer>) -> Result<Signed, String> {
        let path = self.filed()?.path();
        let text = aterm_toml::to_string(self).map_err(|e| e.to_string())?;
        Ok(Signed {
            path,
            bytes: seal(text, signer, DOMAIN)?,
        })
    }
}

/// `text`, a record's TOML body, as its file holds it: the body, then, where `signer` is given,
/// the `[signature]` table signing every byte of it as a record of `domain`.
///
/// # Errors
/// The body would hold a second signature line, or the whole is longer than [`MAX_BYTES`],
/// which no reader reads.
pub(crate) fn seal(
    mut text: String,
    signer: Option<&dyn Signer>,
    domain: &str,
) -> Result<Vec<u8>, String> {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if text.contains(SIGNATURE) || text.starts_with(&SIGNATURE[1..]) {
        return Err("the record's body holds a [signature] line".to_owned());
    }
    if let Some(signer) = signer {
        let sig = signer.sign(domain, text.as_bytes());
        let table = SignatureTable {
            alg: ALG.to_owned(),
            domain: domain.to_owned(),
            node: signer.node().to_owned(),
            key: signer.key().hex(),
            sig: aterm_codec::hex::encode_lower(&sig),
        };
        text.push_str(&SIGNATURE[1..]);
        text.push_str(&aterm_toml::to_string(&table).map_err(|e| e.to_string())?);
    }
    if text.len() as u64 > MAX_BYTES {
        return Err(format!(
            "the record is {} bytes, longer than the {MAX_BYTES} a reader reads",
            text.len()
        ));
    }
    Ok(text.into_bytes())
}

/// A record as its file holds it, at its journal path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    /// `land/<day>/<T>/<check>.<key>.<word>.toml`.
    pub path: String,
    /// The body and its signature table, byte for byte.
    pub bytes: Vec<u8>,
}

impl Signed {
    /// Where its path files it.
    ///
    /// # Errors
    /// The path does not parse.
    pub fn filed(&self) -> Result<Filed, String> {
        Filed::parse(&self.path)
    }
}

/// What signs a land record: this Mac's fleet node identity. Its seed stays in the controller: no
/// child, log, record or spool sees it.
pub trait Signer {
    /// The node id the signature names.
    fn node(&self) -> &str;
    /// The node's public key.
    fn key(&self) -> NodeKey;
    /// The signature of `body` as a record of `domain` ([`DOMAIN`], or a run record's
    /// [`crate::land_run::DOMAIN`]).
    fn sign(&self, domain: &str, body: &[u8]) -> [u8; SIG_LEN];
}

impl Signer for aterm_net::fleet::NodeIdentity {
    fn node(&self) -> &str {
        Self::node(self)
    }

    fn key(&self) -> NodeKey {
        Self::key(self)
    }

    fn sign(&self, domain: &str, body: &[u8]) -> [u8; SIG_LEN] {
        self.sign_record(domain, body)
    }
}

/// A record's `[signature]` table.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureTable {
    alg: String,
    domain: String,
    node: String,
    key: String,
    sig: String,
}

/// What follows a body: its `[signature]` table and nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tail {
    signature: SignatureTable,
}

/// A body's schema, read before the rest so a newer one is named as such.
#[derive(Deserialize)]
struct Head {
    schema: Option<i64>,
}

/// Whose key signed a record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// The node id its table names: a claim nothing binds to the key, so no reader trusts it.
    pub node: String,
    /// The key that signed it.
    pub key: NodeKey,
}

/// A record that passed every check on read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub record: Record,
    /// Whose key signed it; `None` where it was written unsigned.
    pub signature: Option<Signature>,
}

/// Where a reader found a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// In the journal.
    Journal,
    /// In this clone's spool, not yet appended ([`crate::land_spool`]).
    Spool,
}

/// One record a reader found, with what its path and its body come to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its journal path.
    pub path: String,
    pub origin: Origin,
    /// Where its path files it; `Err` for a path that does not parse: corruption, no verdict.
    pub filed: Result<Filed, String>,
    /// The body, when it passed every check; else why not.
    pub body: Result<Box<Read>, String>,
}

impl Entry {
    /// The entry at `path` holding `bytes`, held to every check: the name parses; the body is
    /// UTF-8 within [`MAX_BYTES`], parses, and its schema is [`SCHEMA`]; it names the path's tree,
    /// check, key and word ([`Word::named`]), its word is a record's, and its key recomputes from
    /// it ([`Record::expected_key`]); its run ended on the path's day; its base and commit are
    /// object ids, and its host and run id tokens ([`is_token`]), as a line and a git argument
    /// take them; and a `[signature]` table, where there is one, is this domain's ed25519
    /// signature of every byte above it.
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

    /// The tree its path files it under.
    #[must_use]
    pub fn tree(&self) -> Option<&str> {
        self.filed.as_ref().ok().map(|filed| filed.tree.as_str())
    }
}

/// A record's bytes as a reader takes them: within [`MAX_BYTES`], UTF-8, its body (every byte
/// above the last `[signature]` line, or all of it) of schema `schema`, and its signature table,
/// where it has one, verified as a record of `domain`: the body, and whose key signed it.
///
/// # Errors
/// Why it does not read.
pub(crate) fn open<'a>(
    bytes: &'a [u8],
    schema: u32,
    domain: &str,
) -> Result<(&'a str, Option<Signature>), String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("it is longer than {MAX_BYTES} bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "it is not UTF-8".to_owned())?;
    let (body, tail) = match text.rfind(SIGNATURE) {
        Some(at) => (&text[..=at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let head: Head =
        aterm_toml::from_str(body).map_err(|e| format!("its body does not parse: {e}"))?;
    match head.schema {
        Some(read) if read == i64::from(schema) => {}
        Some(read) => {
            return Err(format!("its schema is {read}; this reader reads {schema}"));
        }
        None => return Err("its body names no schema".to_owned()),
    }
    let signature = tail.map(|tail| verify(body, tail, domain)).transpose()?;
    Ok((body, signature))
}

fn read_body(filed: &Filed, bytes: &[u8]) -> Result<Read, String> {
    let (body, signature) = open(bytes, SCHEMA, DOMAIN)?;
    let record: Record =
        aterm_toml::from_str(body).map_err(|e| format!("its body does not parse: {e}"))?;
    if record.tree != filed.tree {
        return Err(format!(
            "its body judges tree {}, not the {} its path names",
            record.tree, filed.tree
        ));
    }
    if record.check != filed.name.check {
        return Err(format!(
            "its body is {}'s, not the {} its name says",
            record.check, filed.name.check
        ));
    }
    if record.key != filed.name.key {
        return Err("its body's key is not its name's".to_owned());
    }
    if !matches!(
        (Word::of_spelled(&record.word), &filed.name.word),
        (Some(said), Ok(named)) if said == *named
    ) {
        return Err(format!(
            "its body says {:?}, its name {}",
            record.word,
            filed.name.word_label()
        ));
    }
    if record.key != record.expected_key() {
        return Err("its key does not recompute from its check, tree, part and seal".to_owned());
    }
    if record.run.ended / 86_400 != filed.day {
        return Err(format!(
            "its run ended on day {}, not the {} its path names",
            record.run.ended / 86_400,
            filed.day
        ));
    }
    if !aterm_types::land_trailer::object_id(&record.base)
        || !aterm_types::land_trailer::object_id(&record.commit)
    {
        return Err("its base or its commit is not an object id".to_owned());
    }
    if record.local.host.is_empty() || !is_token(&record.local.host) || !is_token(&record.run.id) {
        return Err("its host or its run id is not a token".to_owned());
    }
    Ok(Read { record, signature })
}

/// The signature `tail` gives `body` as a record of `domain`.
fn verify(body: &str, tail: &str, domain: &str) -> Result<Signature, String> {
    let Tail { signature } = aterm_toml::from_str(tail)
        .map_err(|e| format!("its signature table does not parse: {e}"))?;
    if signature.alg != ALG {
        return Err(format!("it is signed by {:?}, not {ALG}", signature.alg));
    }
    if signature.domain != domain {
        return Err(format!(
            "it is signed as a {:?} record, not a {domain} one",
            signature.domain
        ));
    }
    if !aterm_net::fleet::is_node_id(&signature.node) {
        return Err("its signature names no node".to_owned());
    }
    let key =
        NodeKey::from_hex(&signature.key).ok_or_else(|| "its signature names no key".to_owned())?;
    let mut sig = [0u8; SIG_LEN];
    if !aterm_codec::hex::is_lower(&signature.sig, 2 * SIG_LEN)
        || !aterm_codec::hex::decode_into(signature.sig.as_bytes(), &mut sig)
    {
        return Err("its signature is not 128 lowercase hex digits".to_owned());
    }
    if !aterm_net::fleet::verifies_record(&key, domain, body.as_bytes(), &sig) {
        return Err("its signature does not verify".to_owned());
    }
    Ok(Signature {
        node: signature.node,
        key,
    })
}

/// `secs` as a line says an age: seconds, minutes, hours or days.
#[must_use]
pub fn ago(secs: u64) -> String {
    match secs {
        0..120 => format!("{secs} s"),
        120..7_200 => format!("{} min", secs / 60),
        7_200..172_800 => format!("{} h", secs / 3_600),
        _ => format!("{} days", secs / 86_400),
    }
}

#[cfg(test)]
#[path = "land_record_tests.rs"]
pub(crate) mod tests;
