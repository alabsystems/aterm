// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS CONTRACTS 3 AND 4 (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2,
//! `docs/HARNESS-CONTRACT.md` §2): the duties a claim may take (scope words), the claim row of a
//! harness aterm proves a scoped claim from, and the grammar of a scoped claim's words after its
//! holder. The rows are data: the harness registry ([`super::registry`]) composes them from
//! verified records, seed rows and development links.
//!
//! The contract number chooses the rule, not the holder:
//!
//! - no `contract=` is codex-alab's contract 2 self-claim, read by the caller as before;
//! - `contract=3 scope=<word>[,<word>…] [version=] [vendor=] [login=]` keeps its grammar and is
//!   served for every row whose id is a seed id ([`super::registry::is_seed`]), whatever its
//!   source (a seed, a development link over one, a verified record over one), until HX-14b
//!   deletes it: a word outside the row's `allows`, an unknown word or a word named twice refuses
//!   the whole claim, a row whose id is no seed id is refused (it claims contract 4), and a
//!   holder with no row is an ordinary foreign claim ([`Words::Foreign`]) unless its id is a seed
//!   id;
//! - `contract=4 scope=<word>[,<word>…] version=<v> [vendor=] [child=<pid>:<start_us>]
//!   [login=]` keeps the words this aterm knows and drops the rest, refuses none for the row's
//!   `allows`, is granted by where its row came from ([`grant`], which bounds a seed row by its
//!   `allows`), may be granted nothing, names the vendor
//!   process it runs (`child=`), and a holder with no row is refused ([`Words::Unregistered`],
//!   `ERR unproven`), never stored.
//!
//! A seed id whose row is gone (its live build's record failed verification, or the row was
//! refused) is refused the same way under contract 3 and without `contract=`: no row proves it,
//! and a stored foreign claim would take every duty.
//!
//! A scoped claim is proven like codex-alab's v2 self-claim (the kernel's peer pid is `<pid>`,
//! which leads the session's foreground group), with the image rule of its contract: under
//! contract 4 the registry row's (the claimant runs from the row's verified build, a seed row's
//! store build, or a development link under its rule:
//! [`super::registry::Registry::admits_image`]); under contract 3 the rule contract 3 always had
//! (a ready, complete store build of the seed id's package, or its development link's checkout:
//! [`atpkg::harness_record::admits_seed_image`]). codex-alab's v2 claim is not a row here: it
//! keeps its own holder rule and implicit full scope (`crates/aterm-gui/src/self_claim.rs`).

use aterm_phase::Program;

/// The contract versions this aterm serves, as the `harness policy` reply lists them in
/// `contract=` (§8.1 item 1; a set, D14): 2, codex-alab's self-claim, 3, a seed id's scoped
/// claim, and 4, one claim for every registry row (`docs/DESIGN-harness-interface-2026-11-12.md`
/// §2.2). A harness claims at the highest number both sides list.
pub const SERVED: [u32; 3] = [2, 3, 4];

/// The `contract=` value of the `harness policy` reply: [`SERVED`], comma-joined (`2,3,4`).
#[must_use]
pub fn served_word() -> String {
    SERVED
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// The contract a seed id's scoped claim names (`contract=3`).
pub const SCOPED: u32 = 3;

/// The contract every registry row may claim under (`contract=4`).
pub const UNIVERSAL: u32 = 4;

/// One duty a scoped claim may take from aterm's built-in engine for its session (§8.3; §2.2 of
/// the interface design adds `state` and `accounts` under contract 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Relaunching the child on the same conversation after a crash.
    Relaunch,
    /// Moving the child onto a newer vendor build at a safe point.
    Upgrade,
    /// Continuations, Cruise, Land, NEEDS YOU, limit and usage resumes.
    TurnEnd,
    /// Permission boxes, folder trust, plan boxes.
    Approvals,
    /// Question dialogs and prose questions.
    Questions,
    /// Usage, context, login and API-retry walls.
    Walls,
    /// Saying what the agent is doing (`harness_activity`, `harness fact`). It types nothing.
    State,
    /// Sign-in, and which account a session runs on. It types nothing.
    Accounts,
}

impl Scope {
    /// Every scope word, in canonical order: §8.3's six, then contract 4's two.
    pub const ALL: [Scope; 8] = [
        Scope::Relaunch,
        Scope::Upgrade,
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
        Scope::State,
        Scope::Accounts,
    ];

    /// The first contract whose grammar knows this word: 3 for §8.3's six, 4 for `state` and
    /// `accounts`. A contract 3 claim naming a later word is refused as an unknown word.
    #[must_use]
    pub const fn since(self) -> u32 {
        match self {
            Scope::State | Scope::Accounts => UNIVERSAL,
            _ => SCOPED,
        }
    }

    /// The wire word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Scope::Relaunch => "relaunch",
            Scope::Upgrade => "upgrade",
            Scope::TurnEnd => "turn-end",
            Scope::Approvals => "approvals",
            Scope::Questions => "questions",
            Scope::Walls => "walls",
            Scope::State => "state",
            Scope::Accounts => "accounts",
        }
    }

    /// The scope a wire word names, exactly.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|s| s.word() == word)
    }
}

/// Where a registry row came from, which decides what a contract 4 claim is granted ([`grant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A compiled seed row: no verified record stands behind it.
    Seed,
    /// A verified record of an installed build.
    Signed,
    /// A development link's checkout.
    Linked,
}

/// One harness program aterm proves a scoped claim from (§8.1 item 2): a registry row
/// ([`super::registry`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The holder id, which is also the program, repo and tool: one word (§1, D1).
    pub id: &'static str,
    /// The vendor agent it runs as its child, whose reader reads its screen
    /// (`aterm_phase::program_of`).
    pub program: Program,
    /// The atpkg program whose store build, or dev link, must hold the claimant's image.
    pub package: &'static str,
    /// The vendor program the harness runs (its atpkg vendor row and command).
    pub vendor: &'static str,
    /// The scope words a contract 3 claim may name; one naming any other is refused whole
    /// (§8.2). Contract 4 refuses nothing for it and reads it only in a seed row's grant
    /// ([`grant`]).
    pub allows: &'static [Scope],
    /// Where the row came from.
    pub source: Source,
}

/// The row and pid a holder `<id>@<pid>` names: a registered id and a canonical pid above 1.
#[must_use]
pub fn holder(holder: &str) -> Option<(&'static Row, u32)> {
    holder_in(super::registry::current().rows(), holder)
}

/// [`holder`] in an explicitly supplied catalog, with identical pid grammar.
#[must_use]
pub fn holder_in(rows: &'static [Row], holder: &str) -> Option<(&'static Row, u32)> {
    let (id, pid) = holder_words(holder)?;
    Some((rows.iter().find(|r| r.id == id)?, pid))
}

/// A holder's id and pid, by [`holder_in`]'s grammar: `<id>@<pid>`, the pid a canonical decimal
/// above 1.
fn holder_words(holder: &str) -> Option<(&str, u32)> {
    let (id, digits) = holder.split_once('@')?;
    let pid = digits.parse::<u32>().ok().filter(|p| *p > 1)?;
    (pid.to_string() == digits).then_some((id, pid))
}

/// Whether `holder` names a seed id (`<id>@<pid>` by [`holder_in`]'s grammar) that has no row in
/// `rows`: the holder a seed row would prove, while its id's row is gone.
fn unrowed_seed(rows: &[Row], holder: &str) -> bool {
    holder_words(holder)
        .is_some_and(|(id, _)| super::registry::is_seed(id) && rows.iter().all(|r| r.id != id))
}

/// The registered row whose harness runs `program`, if any ([`row_for_in`]).
#[must_use]
pub fn row_for(program: Program) -> Option<&'static Row> {
    row_for_in(super::registry::current().rows(), program)
}

/// [`row_for`] in an explicitly supplied catalog. A generic program has no row: a row that runs no
/// agent aterm knows (`Program::Generic`, `registry::agent_of`) is no generic tab's harness.
#[must_use]
pub fn row_for_in(rows: &'static [Row], program: Program) -> Option<&'static Row> {
    if program == Program::Generic {
        return None;
    }
    rows.iter().find(|r| r.program == program)
}

/// The registered row whose holder id is `word` (a program name, any case), if any.
#[must_use]
pub fn row_named(word: &str) -> Option<&'static Row> {
    super::registry::current()
        .rows()
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(word))
}

/// A claim's scope words, in [`Scope::ALL`]'s order: non-empty under contract 3, possibly empty
/// under contract 4 (a grant of no duty, whose claim still names its child).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scopes(Vec<Scope>);

impl Scopes {
    /// The set of `duties`, each once, in canonical order.
    fn of(duties: &[Scope]) -> Self {
        let mut duties = duties.to_vec();
        duties.sort_unstable();
        duties.dedup();
        Self(duties)
    }

    /// A canonical scope fixture for tests of paired host duties, independent of a roster row.
    /// Wire claims still pass the grammar and the grant in [`read`].
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_test(duties: &[Scope]) -> Self {
        Self::of(duties)
    }

    /// Whether the set holds `scope`.
    #[must_use]
    pub fn contains(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }

    /// Whether the set holds no duty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The duties, in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = Scope> + '_ {
        self.0.iter().copied()
    }

    /// The words, comma-joined, as a claim spells them (`relaunch,upgrade`), or `-` for none.
    #[must_use]
    pub fn words(&self) -> String {
        if self.0.is_empty() {
            return "-".to_string();
        }
        self.0
            .iter()
            .map(|s| s.word())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// WHAT A CONTRACT 4 CLAIM IS GRANTED: the known words it `claimed`, by where `row` came from
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2, HX-06's decisions):
///
/// - a verified installed record ([`Source::Signed`]) is granted every word it claims;
/// - a seed row ([`Source::Seed`]), which no verified record stands behind, is granted the words
///   its row allows (every duty for Claude Code, `relaunch,upgrade` for every other agent:
///   [`super::registry`]'s `allows`) and the two that type nothing (`state`, `accounts`);
/// - a development link ([`Source::Linked`]) is granted every word but `accounts`, because aterm
///   hands an account's home only to a verified installed or seed build.
///
/// This is the one place a grant is narrowed; a person's cap (`[harness.<word>] scopes`, HX-22)
/// narrows here too.
#[must_use]
pub fn grant(row: &Row, claimed: &[Scope]) -> Scopes {
    let granted: Vec<Scope> = claimed
        .iter()
        .copied()
        .filter(|scope| match row.source {
            Source::Signed => true,
            Source::Seed => {
                row.allows.contains(scope) || matches!(scope, Scope::State | Scope::Accounts)
            }
            Source::Linked => *scope != Scope::Accounts,
        })
        .collect();
    Scopes::of(&granted)
}

/// The vendor process a contract 4 claim names: `child=<pid>:<start_us>`, its pid above 1 and its
/// kernel start in microseconds since the Unix epoch, each a canonical decimal. aterm keeps it
/// only when its own kernel reads agree (`crates/aterm-gui/src/harness_claim.rs`, `keep_child`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildWord {
    /// The child's pid.
    pub pid: u32,
    /// Its start as the harness read it, microseconds since the Unix epoch.
    pub start_us: u64,
}

impl ChildWord {
    /// Read `<pid>:<start_us>`: canonical decimals, a pid above 1.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let (pid, start) = value.split_once(':')?;
        let canonical = |digits: &str| {
            !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'))
        };
        if !canonical(pid) || !canonical(start) {
            return None;
        }
        let pid = pid.parse::<u32>().ok().filter(|pid| *pid > 1)?;
        let start_us = start.parse::<u64>().ok()?;
        Some(Self { pid, start_us })
    }

    /// The word's value as a claim spells it.
    #[must_use]
    pub fn word(self) -> String {
        format!("{}:{}", self.pid, self.start_us)
    }
}

/// A harness's own version in a claim (§5): `MAJOR.MINOR.0`, with any `+…` build metadata kept
/// for display only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimVersion {
    /// The release line, `MAJOR.MINOR`.
    pub major: u32,
    /// See [`Self::major`].
    pub minor: u32,
    /// The text as the claim spelled it.
    pub text: String,
}

/// The longest `version=` value.
const VERSION_MAX: usize = 128;

impl ClaimVersion {
    /// The three-part parse: `MAJOR.MINOR.0`, each part canonical decimal, then optionally `+`
    /// and dot-separated alphanumeric identifiers. A non-zero patch, a pre-release (`-…`) and
    /// anything else is refused.
    #[must_use]
    pub fn parse(text: &str) -> Option<ClaimVersion> {
        if text.len() > VERSION_MAX {
            return None;
        }
        let (core, meta) = match text.split_once('+') {
            Some((core, meta)) => (core, Some(meta)),
            None => (text, None),
        };
        if let Some(meta) = meta
            && (meta.is_empty()
                || meta.split('.').any(|id| {
                    id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }))
        {
            return None;
        }
        let mut parts = core.split('.');
        let (major, minor, patch) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || patch != "0" {
            return None;
        }
        let number = |p: &str| {
            let canonical = !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p == "0" || !p.starts_with('0'));
            canonical.then(|| p.parse::<u32>().ok()).flatten()
        };
        Some(ClaimVersion {
            major: number(major)?,
            minor: number(minor)?,
            text: text.to_string(),
        })
    }
}

/// The longest `vendor=` value.
const VENDOR_MAX: usize = 64;

/// Whether `v` is a vendor version a claim may announce: 1 to [`VENDOR_MAX`] bytes of ASCII
/// alphanumerics and `.`, `-`, `+`, `_`.
#[must_use]
pub fn vendor_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= VENDOR_MAX
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

/// A registered harness's scoped claim (contract 3 or 4), read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The harness's row.
    pub row: &'static Row,
    /// The pid the holder names.
    pub pid: u32,
    /// The contract it names: [`SCOPED`] or [`UNIVERSAL`].
    pub contract: u32,
    /// The duties it takes: under contract 4, the grant ([`grant`]).
    pub scopes: Scopes,
    /// The validated requested scope words in lexical order, including unknown and ungranted
    /// words under contract 4. The grant may hold less of this set, down to nothing. A claim
    /// placed again for its holder asks this set again, independent of the original word order.
    pub claimed: String,
    /// Its own version (`version=`), when it named one.
    pub version: Option<ClaimVersion>,
    /// The vendor build its child runs (`vendor=`), when it named one.
    pub vendor: Option<String>,
    /// The managed account row its child last started on (`login=`), when it named one.
    pub login: Option<String>,
    /// The vendor process it runs (`child=`, contract 4 only), as it named it.
    pub child: Option<ChildWord>,
}

impl Claim {
    /// Whether these claims name the same present authority. Requested words are retained for
    /// later re-granting, but only the current grant authorizes a duty. Every other claim field
    /// remains part of its authority, including the named child and versions.
    #[must_use]
    pub fn same_authority(&self, other: &Self) -> bool {
        let Self {
            row,
            pid,
            contract,
            scopes,
            claimed: _,
            version,
            vendor,
            login,
            child,
        } = self;
        row == &other.row
            && pid == &other.pid
            && contract == &other.contract
            && scopes == &other.scopes
            && version == &other.version
            && vendor == &other.vendor
            && login == &other.login
            && child == &other.child
    }

    /// `grok-alab 0.1.0 (grok 1.0.70)`, as the harness's own `--version` spells it, with
    /// `unknown` for what the claim did not name.
    #[must_use]
    pub fn versions(&self) -> String {
        format!(
            "{} {} ({} {})",
            self.row.id,
            self.version.as_ref().map_or("unknown", |v| v.text.as_str()),
            self.row.vendor,
            self.vendor.as_deref().unwrap_or("unknown")
        )
    }
}

/// What a `meta set supervisor <holder> <words…>` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Words<'a> {
    /// No `contract=`: the v2 grammar (`[ttl=<ms>]`), read by the caller as before.
    V2(&'a [&'a str]),
    /// A contract 3 claim from a holder that neither a registry row nor a seed id names: an
    /// ordinary foreign claim, bound to its connection and never proven (§8.2).
    Foreign,
    /// A claim from a holder no registry row proves, where only a row may make it: any contract
    /// 4 claim, and a seed id's contract 3 or contract-less claim while that id has no row (its
    /// live build's record failed verification, or the row was refused). Answered `ERR
    /// unproven` and never stored, since a stored foreign claim would take every duty with
    /// nothing proven.
    Unregistered,
    /// A registered harness's scoped claim, to be proven.
    Scoped(Claim),
    /// Refused whole (`ERR usage`), and why.
    Refused(&'static str),
}

/// Read the words after a `meta set supervisor <holder>`.
///
/// Without `contract=` they are the v2 grammar, except that a rostered holder never claims
/// under it (its proof is the image rule, its duties are its scope) and a seed id with no row is
/// [`Words::Unregistered`]. With `contract=` the words are each named once: `contract=`,
/// `scope=`, `version=`, `vendor=` and `login=`, and under contract 4 `child=` too; `ttl=` and
/// any other word refuse. The contract number chooses the rule ([`read_v3`], [`read_v4`]).
#[must_use]
pub fn read<'a>(holder: &str, words: &'a [&'a str]) -> Words<'a> {
    read_in(super::registry::current().rows(), holder, words)
}

/// The words of a scoped claim, each at most once.
#[derive(Default)]
struct Named<'a> {
    contract: Option<&'a str>,
    scope: Option<&'a str>,
    version: Option<&'a str>,
    vendor: Option<&'a str>,
    login: Option<&'a str>,
    child: Option<&'a str>,
}

/// [`read`] against one explicitly supplied catalog; all claim grammar and scope validation
/// are shared with the shipping reader.
#[must_use]
pub fn read_in<'a>(rows: &'static [Row], holder: &str, words: &'a [&'a str]) -> Words<'a> {
    let rostered = holder_in(rows, holder);
    let unrowed = unrowed_seed(rows, holder);
    if !words.iter().any(|w| w.starts_with("contract=")) {
        return if rostered.is_some() {
            Words::Refused(
                "a rostered harness claims under contract=3 or contract=4 with its scope=",
            )
        } else if unrowed {
            Words::Unregistered
        } else {
            Words::V2(words)
        };
    }
    let mut named = Named::default();
    for word in words {
        let Some((key, value)) = word.split_once('=') else {
            return Words::Refused("a claim word is key=value");
        };
        let slot = match key {
            "contract" => &mut named.contract,
            "scope" => &mut named.scope,
            "version" => &mut named.version,
            "vendor" => &mut named.vendor,
            "login" => &mut named.login,
            "child" => &mut named.child,
            _ => {
                return Words::Refused(
                    "a scoped claim takes contract= scope= version= vendor= login= (and child= \
                     under contract 4)",
                );
            }
        };
        if slot.replace(value).is_some() {
            return Words::Refused("a claim word is named twice");
        }
    }
    match named.contract {
        Some("3") => read_v3(rostered, unrowed, &named),
        Some("4") => read_v4(rostered, &named),
        _ => Words::Refused("a scoped claim names contract=3 or contract=4"),
    }
}

/// The checks both scoped contracts share: `vendor=` and `login=`.
fn versions_ok(named: &Named<'_>) -> Result<Option<ClaimVersion>, &'static str> {
    if named.vendor.is_some_and(|v| !vendor_ok(v)) {
        return Err("vendor= is a version word");
    }
    if named
        .login
        .is_some_and(|label| !crate::harness::accounts::admissible_label(label))
    {
        return Err("login= is an account label");
    }
    match named.version {
        None => Ok(None),
        Some(v) => ClaimVersion::parse(v)
            .map(Some)
            .ok_or("version= is MAJOR.MINOR.0[+meta]"),
    }
}

/// Canonicalize an already validated request. Its parser has refused malformed or repeated
/// words; this neither grants nor drops a word, including ones a later build may understand.
fn requested_words(scope: &str) -> String {
    let mut words: Vec<_> = scope.split(',').collect();
    words.sort_unstable();
    words.join(",")
}

/// Contract 3, served for a row whose id is a seed id, whatever the row's source (a seed, a
/// development link over one, a verified record over one): every scope word known to contract 3
/// and allowed by the row, none twice, the set non-empty; `child=` is not a contract 3 word. A row
/// whose id is no seed id is refused (it claims contract 4); a seed id with no row (`unrowed`) is
/// [`Words::Unregistered`]; any other unrostered holder's claim is an ordinary foreign claim.
fn read_v3<'a>(
    rostered: Option<(&'static Row, u32)>,
    unrowed: bool,
    named: &Named<'_>,
) -> Words<'a> {
    if named.child.is_some() {
        return Words::Refused("a v3 claim takes contract= scope= version= vendor= login=");
    }
    let Some(scope) = named.scope else {
        return Words::Refused("a v3 claim names its scope=");
    };
    let mut scopes = Vec::new();
    for word in scope.split(',') {
        let Some(s) = Scope::from_word(word).filter(|s| s.since() <= SCOPED) else {
            return Words::Refused("an unknown scope word");
        };
        if scopes.contains(&s) {
            return Words::Refused("a scope word is named twice");
        }
        scopes.push(s);
    }
    let version = match versions_ok(named) {
        Ok(version) => version,
        Err(why) => return Words::Refused(why),
    };
    let Some((row, pid)) = rostered else {
        return if unrowed {
            Words::Unregistered
        } else {
            Words::Foreign
        };
    };
    // Contract 3 is the seed ids' until HX-14b deletes it (`docs/HARNESS-CONTRACT.md` §2): a
    // harness that speaks only contract 3 keeps its claim when it is dev-linked or its build
    // carries a record. Any other row claims under contract 4, which grants by its source.
    if !super::registry::is_seed(row.id) {
        return Words::Refused("contract 3 is a seed id's; this harness claims contract=4");
    }
    if scopes.iter().any(|s| !row.allows.contains(s)) {
        return Words::Refused("a scope word outside the harness's row");
    }
    Words::Scoped(Claim {
        row,
        pid,
        contract: SCOPED,
        scopes: Scopes::of(&scopes),
        claimed: requested_words(scope),
        version,
        vendor: named.vendor.map(str::to_string),
        login: named.login.map(str::to_string),
        child: None,
    })
}

/// Whether `word` is a well-formed scope word, known or not: `[a-z][a-z-]*`.
fn scope_word_shaped(word: &str) -> bool {
    let mut bytes = word.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b == b'-')
}

/// Contract 4: `version=` required; each requested scope word well-formed and named once, all
/// preserved in the request, none refused for the row's `allows`. The known words are granted
/// by the row's source ([`grant`]), possibly as an empty set; a malformed `child=` is refused;
/// a holder no row proves [`Words::Unregistered`].
fn read_v4<'a>(rostered: Option<(&'static Row, u32)>, named: &Named<'_>) -> Words<'a> {
    let Some(scope) = named.scope else {
        return Words::Refused("a contract 4 claim names its scope=");
    };
    if named.version.is_none() {
        return Words::Refused("a contract 4 claim names its version=");
    }
    let mut raw: Vec<&str> = Vec::new();
    for word in scope.split(',') {
        if !scope_word_shaped(word) {
            return Words::Refused("a scope word is [a-z][a-z-]*");
        }
        if raw.contains(&word) {
            return Words::Refused("a scope word is named twice");
        }
        raw.push(word);
    }
    let child = match named.child {
        None => None,
        Some(value) => match ChildWord::parse(value) {
            Some(child) => Some(child),
            None => return Words::Refused("child= is <pid>:<start_us>, canonical, pid above 1"),
        },
    };
    let version = match versions_ok(named) {
        Ok(version) => version,
        Err(why) => return Words::Refused(why),
    };
    let Some((row, pid)) = rostered else {
        return Words::Unregistered;
    };
    let claimed: Vec<Scope> = raw.into_iter().filter_map(Scope::from_word).collect();
    Words::Scoped(Claim {
        row,
        pid,
        contract: UNIVERSAL,
        scopes: grant(row, &claimed),
        claimed: requested_words(scope),
        version,
        vendor: named.vendor.map(str::to_string),
        login: named.login.map(str::to_string),
        child,
    })
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
