// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARNESS CONTRACT v3 (`docs/DESIGN-harness-repos-2026-10-24.md` §8): the harness programs aterm
//! proves a scoped claim from (the roster), the duties a claim may take (scope words), and the
//! grammar of a v3 claim's words after its holder.
//!
//! A v3 claim is `meta set supervisor <id>@<pid> contract=3 scope=<word>[,<word>…]
//! [version=<MAJOR.MINOR.0[+meta]>] [vendor=<version>]`. It is proven like codex-alab's v2
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
/// agent: Grok Build has no relaunch or upgrade lane in aterm (`lanes()` is `Lanes::NONE`), so
/// grok-alab's `relaunch` and `upgrade` take nothing anyone else does. A harness whose agent
/// aterm supervises joins in the change that teaches the built-in engine to stand down from the
/// scopes its row allows (§8.3).
pub const ROSTER: &[Row] = &[Row {
    id: "grok-alab",
    program: Program::Grok,
    package: "grok-alab",
    vendor: "grok",
    allows: &[Scope::Relaunch, Scope::Upgrade],
}];

/// The row and pid a holder `<id>@<pid>` names: a rostered id and a canonical pid above 1.
#[must_use]
pub fn holder(holder: &str) -> Option<(&'static Row, u32)> {
    let (id, digits) = holder.split_once('@')?;
    let row = ROSTER.iter().find(|r| r.id == id)?;
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

impl ClaimVersion {
    /// The three-part parse: `MAJOR.MINOR.0`, each part canonical decimal, then optionally `+`
    /// and dot-separated alphanumeric identifiers. A non-zero patch, a pre-release (`-…`) and
    /// anything else is refused.
    #[must_use]
    pub fn parse(text: &str) -> Option<ClaimVersion> {
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
fn vendor_ok(v: &str) -> bool {
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
/// words are `contract=3`, `scope=` and optionally `version=` and `vendor=`, each once; `ttl=`
/// and any other word refuse. A rostered holder's scope must be non-empty, every word known
/// and allowed by its row, none twice.
#[must_use]
pub fn read<'a>(holder: &str, words: &'a [&'a str]) -> Words<'a> {
    let rostered = self::holder(holder);
    if !words.iter().any(|w| w.starts_with("contract=")) {
        return if rostered.is_some() {
            Words::Refused("a rostered harness claims under contract=3 with its scope=")
        } else {
            Words::V2(words)
        };
    }
    let (mut contract, mut scope, mut version, mut vendor) = (None, None, None, None);
    for word in words {
        let Some((key, value)) = word.split_once('=') else {
            return Words::Refused("a claim word is key=value");
        };
        let slot = match key {
            "contract" => &mut contract,
            "scope" => &mut scope,
            "version" => &mut version,
            "vendor" => &mut vendor,
            _ => return Words::Refused("a v3 claim takes contract= scope= version= vendor="),
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped(holder: &str, words: &[&str]) -> Result<Claim, String> {
        match read(holder, words) {
            Words::Scoped(c) => Ok(c),
            other => Err(format!("{other:?}")),
        }
    }

    fn refused(holder: &str, words: &[&str]) -> bool {
        matches!(read(holder, words), Words::Refused(_))
    }

    /// THE HANDSHAKE: aterm serves codex-alab's contract 2 and the roster's contract 3, as a set.
    #[test]
    fn the_served_contracts_are_two_and_three() {
        assert_eq!(served_word(), "2,3");
        assert!(SERVED.contains(&SCOPED));
    }

    /// grok-alab is on the roster, for Grok Build, allowed exactly relaunch and upgrade; nothing
    /// else is a rostered holder, codex-alab included (its v2 rule stays its own).
    #[test]
    fn the_roster_names_grok_alab_for_relaunch_and_upgrade() {
        let (row, pid) = holder("grok-alab@4242").expect("rostered");
        assert_eq!(
            (row.id, row.program, row.package, pid),
            ("grok-alab", Program::Grok, "grok-alab", 4242)
        );
        assert_eq!(row.allows, &[Scope::Relaunch, Scope::Upgrade]);
        assert_eq!(row_for(Program::Grok).map(|r| r.id), Some("grok-alab"));
        assert_eq!(row_named("Grok-Alab").map(|r| r.id), Some("grok-alab"));
        // Every row's id reads as its vendor (§8.1 item 5), so presence keeps the vendor's reader.
        for row in ROSTER {
            assert_eq!(
                aterm_phase::program_of(row.id),
                Some(row.program),
                "{}",
                row.id
            );
        }
        for other in [
            "grok-alab@",
            "grok-alab@1",
            "grok-alab@0",
            "grok-alab@04242",
            "grok-alab@+4242",
            "grok-alab@42x",
            "grok-alab",
            "codex-alab@4242",
            "claude-code-alab@4242",
            "kimi-alab@4242",
            "aterm-harness@4242",
            "GROK-ALAB@4242",
        ] {
            assert_eq!(holder(other), None, "{other}");
        }
        for p in [
            Program::Claude,
            Program::Codex,
            Program::Kimi,
            Program::Generic,
        ] {
            assert_eq!(row_for(p), None, "{p:?}");
        }
    }

    /// A rostered harness's claim: its scope read in canonical order, its versions kept.
    #[test]
    fn a_rostered_claim_is_read_with_its_scope_and_versions() {
        let c = scoped(
            "grok-alab@500",
            &[
                "contract=3",
                "scope=upgrade,relaunch",
                "version=0.1.0",
                "vendor=1.0.70",
            ],
        )
        .expect("a claim");
        assert_eq!(c.pid, 500);
        assert_eq!(c.scopes.words(), "relaunch,upgrade");
        assert!(c.scopes.contains(Scope::Upgrade) && !c.scopes.contains(Scope::Walls));
        assert_eq!(c.versions(), "grok-alab 0.1.0 (grok 1.0.70)");
        let bare = scoped("grok-alab@500", &["scope=upgrade", "contract=3"]).expect("bare");
        assert_eq!(bare.versions(), "grok-alab unknown (grok unknown)");
    }

    /// Every malformed or over-reaching v3 claim is refused whole; a rostered holder never claims
    /// under v2; an unrostered holder's v3 claim is an ordinary foreign claim.
    #[test]
    fn the_scope_parser_refuses_whole_and_admits_only_the_rows_words() {
        let ok = ["contract=3", "scope=relaunch,upgrade"];
        assert!(scoped("grok-alab@500", &ok).is_ok(), "positive control");
        for words in [
            &["scope=relaunch"][..],
            &["contract=3"],
            &["contract=3", "scope="],
            &["contract=2", "scope=relaunch"],
            &["contract=3,2", "scope=relaunch"],
            &["contract=3", "scope=relaunch,walls"],
            &["contract=3", "scope=turn-end"],
            &["contract=3", "scope=relaunch,bogus"],
            &["contract=3", "scope=relaunch,relaunch"],
            &["contract=3", "scope=relaunch,"],
            &["contract=3", "scope=Relaunch"],
            &["contract=3", "contract=3", "scope=relaunch"],
            &["contract=3", "scope=relaunch", "ttl=5000"],
            &["contract=3", "scope=relaunch", "record=/tmp/x"],
            &["contract=3", "scope=relaunch", "loose"],
            &["contract=3", "scope=relaunch", "version=0.1.1"],
            &["contract=3", "scope=relaunch", "version=0.1"],
            &["contract=3", "scope=relaunch", "version=0.1.0-dev.3"],
            &["contract=3", "scope=relaunch", "version=00.1.0"],
            &["contract=3", "scope=relaunch", "version=0.1.0+"],
            &["contract=3", "scope=relaunch", "vendor="],
            &["contract=3", "scope=relaunch", "vendor=1.0.70;rm"],
            &[],
            &["ttl=5000"],
        ] {
            assert!(refused("grok-alab@500", words), "{words:?}");
        }
        assert_eq!(read("watcher@9", &["ttl=5000"]), Words::V2(&["ttl=5000"]));
        assert_eq!(read("codex-alab@9", &[]), Words::V2(&[]));
        assert_eq!(read("kimi-alab@9", &ok), Words::Foreign);
        assert!(refused("kimi-alab@9", &["contract=3", "scope=bogus"]));
    }

    /// The three-part parse (§5): `MAJOR.MINOR.0`, metadata kept for display, a patch refused.
    #[test]
    fn the_claim_version_is_major_minor_zero() {
        let v = ClaimVersion::parse("0.1.0").expect("0.1.0");
        assert_eq!((v.major, v.minor, v.text.as_str()), (0, 1, "0.1.0"));
        let v = ClaimVersion::parse("12.30.0+g1a2b3c4d5").expect("dev build");
        assert_eq!(
            (v.major, v.minor, v.text.as_str()),
            (12, 30, "12.30.0+g1a2b3c4d5")
        );
        for bad in [
            "0.1.1",
            "0.1",
            "0.1.0.0",
            "v0.1.0",
            "0.01.0",
            "0.1.0-rc.1",
            "0.1.0+",
            "0.1.0+a..b",
            "",
            "a.b.0",
        ] {
            assert_eq!(ClaimVersion::parse(bad), None, "{bad}");
        }
    }
}
