// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS CONTRACT v3 (`docs/DESIGN-harness-repos-2026-10-24.md` §8): the harness programs aterm
//! proves a scoped claim from (the roster), the duties a claim may take (scope words), and the
//! grammar of a v3 claim's words after its holder.
//!
//! A v3 claim is `meta set supervisor <id>@<pid> contract=3 scope=<word>[,<word>…]
//! [version=<MAJOR.MINOR.0[+meta]>] [vendor=<version>] [login=<account>]`. It is proven like
//! codex-alab's v2
//! self-claim (the kernel's peer pid is `<pid>`, which leads the session's foreground group),
//! with the image rule taken from the row: the claimant runs from this user's atpkg store build of
//! the row's program, or from its dev link (`crates/aterm-gui/src/harness_claim.rs`). codex-alab's
//! v2 claim is not a row here: it keeps its own holder rule and implicit full scope
//! (`crates/aterm-gui/src/self_claim.rs`).

use aterm_phase::Program;

/// The contract versions this aterm serves, as the `harness policy` reply lists them in
/// `contract=` (§8.1 item 1; a set, D14): 2, codex-alab's self-claim, and 3, a rostered harness's
/// scoped claim.
pub const SERVED: [u32; 2] = [2, 3];

/// The `contract=` value of the `harness policy` reply: [`SERVED`], comma-joined (`2,3`).
#[must_use]
pub fn served_word() -> String {
    SERVED
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// The contract a scoped claim names (`contract=3`).
pub const SCOPED: u32 = 3;

/// One duty a v3 claim may take from aterm's built-in engine for its session (§8.3).
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
}

impl Scope {
    /// Every scope word, in the order §8.3 lists them.
    pub const ALL: [Scope; 6] = [
        Scope::Relaunch,
        Scope::Upgrade,
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
    ];

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
        }
    }

    /// The scope a wire word names, exactly.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Scope> {
        Scope::ALL.into_iter().find(|s| s.word() == word)
    }
}

/// One harness program aterm proves a scoped claim from (§8.1 item 2).
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
    /// The scope words it may claim; a claim naming any other is refused whole (§8.2).
    pub allows: &'static [Scope],
}

/// The roster. A row allows only the duties aterm's built-in engine can stand down from for its
/// agent. Grok Build has no relaunch or upgrade lane in aterm (`lanes()` is `Lanes::NONE`), so
/// grok-alab's `relaunch` and `upgrade` take nothing anyone else does. Muse Code's built-in
/// relaunch and upgrade lanes stand down from a session whose proven claim takes those duties
/// (`SupervisorClaim::takes` and `SupervisorAuthority::builtin_keeps` in aterm-gui's
/// `session_timeline.rs`, held by its unit tests). `lab harness-proof muse-alab`
/// (`docs/DESIGN-harness-repos-2026-10-24.md` §9) does not measure that stand-down: it
/// sees only that aterm logged no built-in act, which holds with no claim too, since in a tab
/// the harness leads aterm sees no exit and its relaunch lane knows no conversation for the
/// harness's pid. Claude Code's built-in lanes stand down the same way under claude-code-alab's
/// proven claim (§11 D53): the upgrade lane on `status supervisor_proven=1`, whatever the scope
/// (`upgrade_drive::self_claimed_by_status`, D29); the relaunch lane and the restart act on a
/// scope naming `relaunch` (`SupervisorAuthority::builtin_keeps`, and aterm-gui's `restart_act`,
/// which then sends the harness `restart cause=` in place of a signal). Its tool is `claude-alab`
/// (D19); the image rule reads the package, never the tool's name. A harness whose agent aterm
/// supervises joins in the change that teaches the built-in engine to stand down from the scopes
/// its row allows (§8.3).
pub const ROSTER: &[Row] = &[
    Row {
        id: "grok-alab",
        program: Program::Grok,
        package: "grok-alab",
        vendor: "grok",
        allows: &[Scope::Relaunch, Scope::Upgrade],
    },
    Row {
        id: "muse-alab",
        program: Program::Muse,
        package: "muse-alab",
        vendor: "muse",
        allows: &[Scope::Relaunch, Scope::Upgrade],
    },
    Row {
        id: "kimi-alab",
        program: Program::Kimi,
        package: "kimi-alab",
        vendor: "kimi",
        allows: &[Scope::Relaunch, Scope::Upgrade],
    },
    Row {
        id: "claude-code-alab",
        program: Program::Claude,
        package: "claude-code-alab",
        vendor: "claude",
        allows: &[Scope::Relaunch, Scope::Upgrade],
    },
];

/// The row and pid a holder `<id>@<pid>` names: a rostered id and a canonical pid above 1.
#[must_use]
pub fn holder(holder: &str) -> Option<(&'static Row, u32)> {
    holder_in(ROSTER, holder)
}

/// [`holder`] in an explicitly supplied static catalog, with identical pid grammar.
#[must_use]
pub fn holder_in(rows: &'static [Row], holder: &str) -> Option<(&'static Row, u32)> {
    let (id, digits) = holder.split_once('@')?;
    let row = rows.iter().find(|r| r.id == id)?;
    let pid = digits.parse::<u32>().ok().filter(|p| *p > 1)?;
    (pid.to_string() == digits).then_some((row, pid))
}

/// The row whose harness runs `program`, if any.
#[must_use]
pub fn row_for(program: Program) -> Option<&'static Row> {
    ROSTER.iter().find(|r| r.program == program)
}

/// The row whose holder id is `word` (a program name, any case), if any.
#[must_use]
pub fn row_named(word: &str) -> Option<&'static Row> {
    ROSTER.iter().find(|r| r.id.eq_ignore_ascii_case(word))
}

/// A claim's scope words: a non-empty set, in [`Scope::ALL`]'s order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scopes(Vec<Scope>);

impl Scopes {
    /// A canonical scope fixture for tests of paired host duties, independent of a roster row.
    /// Wire claims still pass the row's allowance check in [`read`].
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn for_test(duties: &[Scope]) -> Self {
        let mut duties = duties.to_vec();
        duties.sort_unstable();
        duties.dedup();
        assert!(!duties.is_empty(), "a scope fixture is nonempty");
        Self(duties)
    }

    /// Whether the set holds `scope`.
    #[must_use]
    pub fn contains(&self, scope: Scope) -> bool {
        self.0.contains(&scope)
    }

    /// The words, comma-joined, as a claim spells them (`relaunch,upgrade`).
    #[must_use]
    pub fn words(&self) -> String {
        self.0
            .iter()
            .map(|s| s.word())
            .collect::<Vec<_>>()
            .join(",")
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
pub(super) fn vendor_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= VENDOR_MAX
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
}

/// A rostered harness's v3 claim, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The harness's row.
    pub row: &'static Row,
    /// The pid the holder names.
    pub pid: u32,
    /// The duties it takes.
    pub scopes: Scopes,
    /// Its own version (`version=`), when it named one.
    pub version: Option<ClaimVersion>,
    /// The vendor build its child runs (`vendor=`), when it named one.
    pub vendor: Option<String>,
    /// The managed account row its child last started on (`login=`), when it named one.
    pub login: Option<String>,
}

impl Claim {
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
    /// A v3 claim from a holder that is not on the roster: an ordinary foreign claim, bound to
    /// its connection and never proven (§8.2).
    Foreign,
    /// A rostered harness's v3 claim, to be proven.
    Scoped(Claim),
    /// Refused whole (`ERR usage`), and why.
    Refused(&'static str),
}

/// Read the words after a `meta set supervisor <holder>`.
///
/// Without `contract=` they are the v2 grammar, except that a rostered holder never claims
/// under it (its proof is the v3 image rule, its duties are its scope). With `contract=` the
/// words are `contract=3`, `scope=` and optionally `version=`, `vendor=` and `login=`, each once; `ttl=`
/// and any other word refuse. A rostered holder's scope must be non-empty, every word known
/// and allowed by its row, none twice.
#[must_use]
pub fn read<'a>(holder: &str, words: &'a [&'a str]) -> Words<'a> {
    read_in(ROSTER, holder, words)
}

/// [`read`] against one explicitly supplied catalog; all claim grammar and scope validation
/// are shared with the shipping reader.
#[must_use]
pub fn read_in<'a>(rows: &'static [Row], holder: &str, words: &'a [&'a str]) -> Words<'a> {
    let rostered = holder_in(rows, holder);
    if !words.iter().any(|w| w.starts_with("contract=")) {
        return if rostered.is_some() {
            Words::Refused("a rostered harness claims under contract=3 with its scope=")
        } else {
            Words::V2(words)
        };
    }
    let (mut contract, mut scope, mut version, mut vendor, mut login) =
        (None, None, None, None, None);
    for word in words {
        let Some((key, value)) = word.split_once('=') else {
            return Words::Refused("a claim word is key=value");
        };
        let slot = match key {
            "contract" => &mut contract,
            "scope" => &mut scope,
            "version" => &mut version,
            "vendor" => &mut vendor,
            "login" => &mut login,
            _ => {
                return Words::Refused("a v3 claim takes contract= scope= version= vendor= login=");
            }
        };
        if slot.replace(value).is_some() {
            return Words::Refused("a claim word is named twice");
        }
    }
    if contract != Some("3") {
        return Words::Refused("a scoped claim names contract=3");
    }
    let Some(scope) = scope else {
        return Words::Refused("a v3 claim names its scope=");
    };
    let mut scopes = Vec::new();
    for word in scope.split(',') {
        let Some(s) = Scope::from_word(word) else {
            return Words::Refused("an unknown scope word");
        };
        if scopes.contains(&s) {
            return Words::Refused("a scope word is named twice");
        }
        scopes.push(s);
    }
    let version = match version {
        None => None,
        Some(v) => match ClaimVersion::parse(v) {
            Some(v) => Some(v),
            None => return Words::Refused("version= is MAJOR.MINOR.0[+meta]"),
        },
    };
    if vendor.is_some_and(|v| !vendor_ok(v)) {
        return Words::Refused("vendor= is a version word");
    }
    if login.is_some_and(|label| !crate::harness::accounts::admissible_label(label)) {
        return Words::Refused("login= is an account label");
    }
    let Some((row, pid)) = rostered else {
        return Words::Foreign;
    };
    if scopes.iter().any(|s| !row.allows.contains(s)) {
        return Words::Refused("a scope word outside the harness's row");
    }
    scopes.sort_unstable();
    Words::Scoped(Claim {
        row,
        pid,
        scopes: Scopes(scopes),
        version,
        vendor: vendor.map(str::to_string),
        login: login.map(str::to_string),
    })
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod tests;
