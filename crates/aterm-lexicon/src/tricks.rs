// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KITTY-COMMAND VOCABULARY ("tricks"): the words a person TYPES at the
//! terminal to the cursor pet — `sit`, `nap`, `kitty jump`, `good kitty` —
//! compiled from `data/tricks.toml` into one folded-key table.
//!
//! # Why a separate table and not a [`Class`](crate::Class)
//!
//! The sparkle lexicon is the SCREEN scanner's vocabulary: every surface in
//! it is matched against every row of program output, becomes an occurrence,
//! and costs per-frame work. Pet commands are ordinary English verbs (`sit`,
//! `play`, `roll`) that fill every build log and README; as a `Class` they
//! would light up the whole screen. So they live here, in a table the screen
//! scanner never sees, and the only caller is the typed-line listener, which
//! asks about ONE token at a word boundary the person just typed.
//!
//! # What the table answers
//!
//! [`TrickLexicon::classify`] maps one isolated token to a [`TrickRole`]:
//!
//! * a **trick** word — `plain` (`needs_vocative = false`: it may fire on its
//!   own, on a line that holds nothing but pet vocabulary) or an **address**
//!   word (`needs_vocative = true`: it fires only when the line also names
//!   the pet, because the bare word opens too many ordinary lines — `sleep 5`,
//!   `roll back the commit`, `good catch`);
//! * a **vocative** — a name for the pet (`kitty`, `cat`, `pup`, `boy`);
//! * a **filler** — a word that may sit in a pet-directed line without
//!   breaking it (`please`, `now`, `who's a`).
//!
//! WHAT IT DOES NOT ANSWER, on purpose: whether the token sat in a code
//! context, whether the line was pure, whether the key was typed or pasted.
//! That is the listener's policy; this module is vocabulary and nothing else.
//! It is whole-token by construction — `site`, `sitting` and `sleepy` are
//! different keys from `sit` and `sleep` and are simply absent.
//!
//! # Load laws (each one recorded on [`TrickLexicon::conflicts`], never silent)
//!
//! * A surface is ONE token: letters / digits / `_` with `'` `’` `-` as
//!   interior joiners (the scanner's own token law). Anything else can never
//!   be typed as one word and is skipped.
//! * An unknown trick `id` skips its row.
//! * A surface claimed twice with DIFFERENT roles keeps the first claimant
//!   (file order, English first). The SAME role claimed again is not a
//!   conflict: that is `siéntate` beside `sientate`, or two languages sharing
//!   an imperative, and the claims merge.
//! * English is never gated: it is the priority language and loads for
//!   everyone.
//! * MARKS-REQUIRED, exactly as the sparkle lexicon: a surface whose fold
//!   loses diacritics into a bare-ASCII key matches only a token that itself
//!   carries such marks, unless the bare spelling is listed too.
//! * A single-character no-space surface (`坐`, `跳`) ALWAYS needs the
//!   vocative, whichever list it was authored in: a lone committed ideograph
//!   is a syllable of someone's prose far more often than a command.
//! * A surface longer than [`TrickLexicon::MAX_SURFACE_CHARS`] is skipped: the
//!   listener's token window could never hold it, so it would never fire.
//!
//! Nothing on this module's compile path touches the sparkle lexicon's cached
//! accessor; the cross-table collision laws are test-side
//! (`tests/tricks.rs`), where both tables are built independently.

use crate::{DebugText, Langs, LexError, fold, is_matchable_token, is_no_space_surface};
use aterm_hash::FxHashMap;
use std::sync::OnceLock;

const BUILTIN_TRICKS: &str = include_str!("../data/tricks.toml");

/// One thing the cursor pet can be told to do. The variant order is the
/// listing order ([`Trick::ALL`]) and the `data/tricks.toml` row order.
///
/// Every consumer matches this EXHAUSTIVELY (no wildcard arm): a seventeenth
/// trick must break the build at each place that has to learn it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Trick {
    /// Sit up and pay attention.
    Sit,
    /// Lie down (the loaf).
    Down,
    /// Go to sleep.
    Sleep,
    /// Stretch — which is also how a sleeper is woken.
    Stretch,
    /// Jump on the spot.
    Jump,
    /// Play: a frolic, the zoomies.
    Play,
    /// Roll over.
    Roll,
    /// Be petted: purr.
    Purr,
    /// Wash.
    Groom,
    /// Make a noise.
    Speak,
    /// Look at the person.
    Look,
    /// Give a paw / bat at something.
    Paw,
    /// Hide.
    Hide,
    /// Be startled.
    Boo,
    /// Be told off.
    Scold,
    /// Be praised or fed.
    Treat,
}

impl Trick {
    /// Every trick, in declaration order.
    pub const ALL: [Trick; 16] = [
        Trick::Sit,
        Trick::Down,
        Trick::Sleep,
        Trick::Stretch,
        Trick::Jump,
        Trick::Play,
        Trick::Roll,
        Trick::Purr,
        Trick::Groom,
        Trick::Speak,
        Trick::Look,
        Trick::Paw,
        Trick::Hide,
        Trick::Boo,
        Trick::Scold,
        Trick::Treat,
    ];

    /// The stable lowercase id: the `id` key in `data/tricks.toml`, the
    /// `trick=` field of [`row_line`], and the spelling config and docs use.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Trick::Sit => "sit",
            Trick::Down => "down",
            Trick::Sleep => "sleep",
            Trick::Stretch => "stretch",
            Trick::Jump => "jump",
            Trick::Play => "play",
            Trick::Roll => "roll",
            Trick::Purr => "purr",
            Trick::Groom => "groom",
            Trick::Speak => "speak",
            Trick::Look => "look",
            Trick::Paw => "paw",
            Trick::Hide => "hide",
            Trick::Boo => "boo",
            Trick::Scold => "scold",
            Trick::Treat => "treat",
        }
    }

    /// The inverse of [`code`](Self::code). It walks [`ALL`](Self::ALL)
    /// instead of restating the sixteen strings, so `code` stays the ONE
    /// spelling table and the two can never disagree. Exact match only: the
    /// ids are data keys, not typed words, so nothing is folded.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Trick> {
        Trick::ALL.into_iter().find(|trick| trick.code() == code)
    }

    /// What the pet does, as one short clause for the command listing. Plain
    /// words and spaces only: [`row_line`] spells the spaces as `_` so the
    /// clause stays one field, which an authored `_` or `=` would corrupt
    /// (pinned by `blurbs_are_one_clean_listing_field`).
    #[must_use]
    pub fn blurb(self) -> &'static str {
        match self {
            Trick::Sit => "sits up and looks at you for a few seconds",
            Trick::Down => "lies down in a loaf",
            Trick::Sleep => "curls up and goes to sleep",
            Trick::Stretch => "wakes up with a long stretch",
            Trick::Jump => "crouches and jumps on the spot",
            Trick::Play => "frolics about",
            Trick::Roll => "rolls over and wriggles",
            Trick::Purr => "purrs with a little heart",
            Trick::Groom => "stops for a wash",
            Trick::Speak => "perks up and meows a note",
            Trick::Look => "perks up and looks at you",
            Trick::Paw => "bats a paw at the air",
            Trick::Hide => "ducks behind the nearest text",
            Trick::Boo => "jumps out of its skin",
            Trick::Scold => "flattens its ears for a moment",
            Trick::Treat => "perks up and dances with sparkles",
        }
    }
}

/// What one typed token means to the pet. See the module docs for the three
/// families and for what this deliberately does NOT decide.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum TrickRole {
    /// A command word.
    Trick {
        /// Which trick it asks for.
        trick: Trick,
        /// `false`: a PLAIN word — it may fire unaddressed, on a line that is
        /// nothing but pet vocabulary. `true`: an ADDRESS word — it fires only
        /// when the same line also carries a [`TrickRole::Vocative`]. Always
        /// `true` for a single-character no-space surface.
        needs_vocative: bool,
    },
    /// A name for the pet. Fires nothing by itself; it is what turns an
    /// address word into a command and a tentative fire into a confirmed one.
    Vocative,
    /// A word that keeps a pet-directed line pure and means nothing else.
    Filler,
}

/// Which table a listing row came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RowKind {
    /// A `[[trick]]` row for this trick.
    Trick(Trick),
    /// A `[[vocative]]` row.
    Vocative,
    /// A `[[filler]]` row.
    Filler,
}

/// One authored row of `data/tricks.toml`, as the listings show it: the
/// ORIGINAL spellings (diacritics and all — never the folded keys), whether or
/// not the configured languages would load them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrickRow {
    /// Trick / vocative / filler.
    pub kind: RowKind,
    /// The row's language code (`"en"`, `"ja"`, …). Never empty.
    pub lang: String,
    /// Plain surfaces for a trick row; every surface for the other two kinds.
    pub words: Vec<String>,
    /// Address surfaces (trick rows only; empty otherwise).
    pub address: Vec<String>,
    /// `true`: loads only when the configured languages list [`lang`](Self::lang)
    /// (or `"all"`).
    pub gated: bool,
    /// `true`: a no-space script row — surfaces are RAW keys and the whole
    /// typed run must equal one.
    pub cjk: bool,
}

/// THE one listing formatter — the CLI prints exactly these lines and the
/// manual documents exactly this shape, so there is one spelling to pin:
///
/// ```text
/// command trick=down lang=en gated=0 words=loaf,sploot address=down,rest does=lies_down_in_a_loaf
/// vocative lang=en gated=0 words=kitty,cat
/// filler lang=en gated=0 words=please,now
/// ```
///
/// One line, space-separated `key=value` fields, no spaces inside a value
/// (surfaces are single tokens by the load law; the blurb's spaces become
/// `_`), an empty list spelled `-`. Composed with `push_str` — this crate
/// keeps std format macros out of its non-test bodies (see `DebugText`).
#[must_use]
pub fn row_line(row: &TrickRow) -> String {
    let mut line = String::new();
    match row.kind {
        RowKind::Trick(trick) => {
            line.push_str("command trick=");
            line.push_str(trick.code());
            line.push(' ');
        }
        RowKind::Vocative => line.push_str("vocative "),
        RowKind::Filler => line.push_str("filler "),
    }
    line.push_str("lang=");
    line.push_str(&row.lang);
    line.push_str(" gated=");
    line.push(if row.gated { '1' } else { '0' });
    line.push_str(" words=");
    push_surface_list(&mut line, &row.words);
    match row.kind {
        RowKind::Trick(trick) => {
            line.push_str(" address=");
            push_surface_list(&mut line, &row.address);
            line.push_str(" does=");
            for c in trick.blurb().chars() {
                line.push(if c == ' ' { '_' } else { c });
            }
        }
        // Only a trick row HAS an address list or a performance; printing
        // `address=- does=-` on the other two would be noise that reads as
        // missing data.
        RowKind::Vocative | RowKind::Filler => {}
    }
    line
}

/// `a,b,c`, or `-` for an empty list (so every field always has a value and a
/// consumer can split on single spaces).
fn push_surface_list(line: &mut String, surfaces: &[String]) {
    if surfaces.is_empty() {
        line.push('-');
        return;
    }
    for (i, surface) in surfaces.iter().enumerate() {
        if i > 0 {
            line.push(',');
        }
        line.push_str(surface);
    }
}

// ---- TOML shapes ----
//
// `deny_unknown_fields` on all three: a typo'd key (`adress = [...]`) must be
// a parse error the test suite trips over, not a list that silently never
// loads.

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTricks {
    #[serde(default)]
    trick: Vec<RawTrickRow>,
    #[serde(default)]
    vocative: Vec<RawWordRow>,
    #[serde(default)]
    filler: Vec<RawWordRow>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrickRow {
    id: String,
    lang: String,
    #[serde(default)]
    words: Vec<String>,
    #[serde(default)]
    address: Vec<String>,
    #[serde(default)]
    gated: bool,
    #[serde(default)]
    cjk: bool,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation field for human reviewers")]
    notes: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWordRow {
    lang: String,
    #[serde(default)]
    words: Vec<String>,
    #[serde(default)]
    gated: bool,
    #[serde(default)]
    cjk: bool,
    #[serde(default)]
    #[allow(dead_code, reason = "documentation field for human reviewers")]
    notes: String,
}

/// Compiled value behind one key.
#[derive(Clone, Copy, Debug)]
struct TrickEntry {
    /// The role AS AUTHORED (which list the surface sat in).
    role: TrickRole,
    /// The marks-required gate, the sparkle lexicon's law verbatim: every
    /// claiming surface lost diacritics in folding AND landed on a bare-ASCII
    /// key, so unmarked typing spells a different word that merely shares the
    /// skeleton. Such a key admits only a token that itself carries
    /// folded-away marks.
    marks_required: bool,
    /// A one-character no-space surface. [`effective_role`] forces
    /// `needs_vocative` for it, whichever list it was authored in.
    single_cjk: bool,
}

/// The role a lookup reports: the authored role, with the lone-ideograph law
/// applied. ONE function, used by both the lookup and the load-time
/// double-claim comparison, so "same role" means the same thing in both.
fn effective_role(entry: &TrickEntry) -> TrickRole {
    match entry.role {
        TrickRole::Trick {
            trick,
            needs_vocative,
        } => TrickRole::Trick {
            trick,
            needs_vocative: needs_vocative || entry.single_cjk,
        },
        TrickRole::Vocative => TrickRole::Vocative,
        TrickRole::Filler => TrickRole::Filler,
    }
}

/// The compiled kitty-command vocabulary: folded whole-token key (or RAW
/// no-space-script key) → role. The two key spaces cannot alias: a folded
/// spaced key begins and ends with a token character and a raw key is made
/// only of no-space-script characters, which are never token characters.
///
/// `Default` is the EMPTY vocabulary (classifies nothing, no conflicts) — what
/// a host holds before its off-thread compile lands.
#[derive(Debug, Clone, Default)]
pub struct TrickLexicon {
    table: FxHashMap<String, TrickEntry>,
    /// Data problems found at load. Empty for well-formed data; the test
    /// suite asserts it is empty for the embedded file under every language
    /// configuration.
    conflicts: Vec<String>,
}

impl TrickLexicon {
    /// The longest surface, in characters, that can ever fire. The typed-line
    /// listener holds the current token in a fixed window of exactly this many
    /// characters and the flash holds the word in an inline buffer of the same
    /// size; a longer token poisons itself. Owning the number HERE lets the
    /// loader refuse a surface that could never be typed into that window,
    /// instead of shipping a word that silently never works.
    pub const MAX_SURFACE_CHARS: usize = 32;

    /// The embedded vocabulary with `gated` rows loaded for `langs` (`["all"]`
    /// un-gates everything). Ungated rows — all of English — load for every
    /// configuration. Panics only if the EMBEDDED data fails to parse, which
    /// the test suite forbids.
    #[must_use]
    pub fn with_languages(langs: &[&str]) -> TrickLexicon {
        // Explicit match instead of `.expect(..)`, mirroring
        // `Lexicon::with_languages`: `expect`'s panic-freedom is not modeled by
        // the verifier, and the message is the one `expect` would have built.
        match Self::from_source(BUILTIN_TRICKS, langs) {
            Ok(tricks) => tricks,
            Err(e) => std::panic::panic_any(embedded_parse_panic(&e)),
        }
    }

    /// The embedded vocabulary with nothing un-gated beyond English (the
    /// default `languages = ["en"]`). Built once and cached.
    ///
    /// For tests and tools. A HOST compiles [`with_languages`](Self::with_languages)
    /// from its configured languages off the keystroke path and keeps the
    /// result; it must not first-touch this cell on the input thread.
    #[must_use]
    pub fn shared_en() -> &'static TrickLexicon {
        static TRICKS_EN: OnceLock<TrickLexicon> = OnceLock::new();
        TRICKS_EN.get_or_init(|| Self::with_languages(&["en"]))
    }

    /// Compile a vocabulary from a `tricks.toml`-shaped document. `langs`
    /// selects which `gated` rows load. A malformed document is an `Err`; a
    /// well-formed document with bad rows compiles, with each dropped row or
    /// surface named on [`conflicts`](Self::conflicts).
    ///
    /// Public for the same reason `Lexicon::from_sources` is: the load laws
    /// are testable only against documents that BREAK them, and a listener
    /// test needs scripts the embedded seed may not carry yet.
    pub fn from_source(tricks_toml: &str, langs: &[&str]) -> Result<TrickLexicon, LexError> {
        let raw: RawTricks = aterm_toml::from_str(tricks_toml).map_err(LexError::Toml)?;
        let mut conflicts = Vec::new();
        let rows = ordered_rows(raw, &mut conflicts);
        Ok(compile(&rows, &Langs::new(langs), conflicts))
    }

    /// The listing rows of a `tricks.toml`-shaped document, in the order
    /// [`all_rows`](Self::all_rows) documents — the SAME rows, in the same
    /// order, that [`from_source`](Self::from_source) compiles, so a validator
    /// can hold the authored spellings beside the compiled table.
    pub fn rows_from_source(tricks_toml: &str) -> Result<Vec<TrickRow>, LexError> {
        let raw: RawTricks = aterm_toml::from_str(tricks_toml).map_err(LexError::Toml)?;
        // The discarded conflicts are the row-shape ones (unknown id, no
        // lang); `conflicts()` on a compiled instance reports them.
        Ok(ordered_rows(raw, &mut Vec::new()))
    }

    /// Classify one already-isolated token. `scratch` is the caller's resident
    /// fold buffer: after it has grown to the longest token seen, a call
    /// allocates nothing (pinned by `tests/tricks_alloc.rs`).
    ///
    /// The cost is one fold plus one hash probe — or, for a token made only
    /// of no-space-script characters, the probe alone: those keys are RAW, so
    /// the whole typed run must equal a surface (there is no maximal-munch
    /// search here; `坐下来` is not `坐下`). Then the marks-required gate, then
    /// the lone-ideograph law. No possessive or clitic fallbacks: `kitty's`
    /// is not something one says TO a cat.
    ///
    /// THE ELONGATION FOLD. People draw a word out when they talk to a pet —
    /// `goooood kitty`, `purrrrr`, `nooo`, `staaay` — and no list can hold
    /// every length. So on a MISS only, and only for a spaced token that
    /// carries a run of three or more identical characters, the lookup is
    /// retried with every such run collapsed to two (`goooood` → `good`,
    /// `purrrrr` → `purr`, `pssssst` → `psst`), then to one (`nooo` → `no`,
    /// `staaay` → `stay`, `meooow` → `meow`); the first hit wins. A run of
    /// exactly two is never touched, so `kitty` stays `kitty` and `sitt` stays
    /// a miss: the fold answers a held key, it does not forgive a typo.
    #[must_use]
    pub fn classify(&self, token: &str, scratch: &mut String) -> Option<TrickRole> {
        let entry = if is_no_space_surface(token) {
            self.table.get(token)?
        } else {
            fold::fold_into(token, scratch);
            match self.table.get(scratch.as_str()) {
                Some(entry) => entry,
                None => self.classify_elongated(scratch)?,
            }
        };
        // Evaluated lazily, like the scanner's `admits`: only a token that
        // actually hit a gated key pays the walk.
        if entry.marks_required && !fold::has_foldable_marks(token) {
            return None;
        }
        Some(effective_role(entry))
    }

    /// The elongation fold's retry (see [`classify`](Self::classify)).
    /// `scratch` holds the folded token on entry and is rewritten with the
    /// collapsed spellings, which are never longer than it — so the retry
    /// allocates nothing either.
    fn classify_elongated(&self, scratch: &mut String) -> Option<&TrickEntry> {
        // The folded token, copied out so `scratch` can be rewritten. A token
        // the listener can hold is at most MAX_SURFACE_CHARS characters and a
        // fold at most doubles one (`ß` → `ss`); anything longer is not a word
        // someone drew out, it is a line typed without spaces.
        let mut window = ['\0'; ELONGATION_WINDOW];
        let mut len = 0usize;
        for c in scratch.chars() {
            *window.get_mut(len)? = c;
            len = len.saturating_add(1);
        }
        let folded = window.get(..len)?;
        if !has_elongated_run(folded) {
            return None;
        }
        // Two first: a doubled letter is the commoner true spelling (`good`,
        // `purr`, `psst`), and `goood` must not become `god`.
        for keep in [2usize, 1usize] {
            scratch.clear();
            push_collapsed(folded, keep, scratch);
            if let Some(entry) = self.table.get(scratch.as_str()) {
                return Some(entry);
            }
        }
        None
    }

    /// Data problems found at load (unknown trick id, a surface that is not
    /// one token or is too long, a surface claimed under two roles, a gated
    /// English row). Empty for well-formed data.
    #[must_use]
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    /// Number of distinct compiled keys (validator surface: lets a test prove
    /// a configuration is not vacuous).
    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.table.len()
    }

    /// Every well-formed row of the EMBEDDED file, every language, gated or
    /// not, for listings. Cold path: one TOML parse per call.
    ///
    /// ORDER: grouped by language in first-appearance order (English first,
    /// by the data file's own law), and within a language the trick rows in
    /// file order, then its vocative rows, then its filler rows. For a file
    /// authored language-group by language-group that IS file order; a TOML
    /// document does not record how three different arrays-of-tables
    /// interleave, so this is the one order both the loader and the listing
    /// can share. It is also the first-claimant order of the load law.
    #[must_use]
    pub fn all_rows() -> Vec<TrickRow> {
        match Self::rows_from_source(BUILTIN_TRICKS) {
            Ok(rows) => rows,
            Err(e) => std::panic::panic_any(embedded_parse_panic(&e)),
        }
    }
}

/// The shortest run the elongation fold treats as a held key. Two identical
/// letters are spelling (`kitty`, `purr`); three are someone drawing it out.
const ELONGATED_RUN: usize = 3;

/// Characters of folded token the elongation fold will look at: the
/// listener's token window, doubled for the folds that expand (`ß` → `ss`).
const ELONGATION_WINDOW: usize = TrickLexicon::MAX_SURFACE_CHARS * 2;

/// Length of the run of identical characters that starts at `at`.
fn run_len(chars: &[char], at: usize) -> usize {
    let Some(first) = chars.get(at) else {
        return 0;
    };
    let mut run = 1usize;
    while chars.get(at.saturating_add(run)) == Some(first) {
        run = run.saturating_add(1);
    }
    run
}

/// Whether any run of identical characters reaches [`ELONGATED_RUN`] — the
/// gate that keeps the retry off every ordinary miss.
fn has_elongated_run(chars: &[char]) -> bool {
    let mut at = 0usize;
    while at < chars.len() {
        let run = run_len(chars, at);
        if run >= ELONGATED_RUN {
            return true;
        }
        at = at.saturating_add(run.max(1));
    }
    false
}

/// `chars` with every run of [`ELONGATED_RUN`] or more cut down to `keep`;
/// shorter runs are copied untouched.
fn push_collapsed(chars: &[char], keep: usize, out: &mut String) {
    let mut at = 0usize;
    while let Some(&c) = chars.get(at) {
        let run = run_len(chars, at);
        let emit = if run >= ELONGATED_RUN { keep } else { run };
        for _ in 0..emit {
            out.push(c);
        }
        at = at.saturating_add(run.max(1));
    }
}

/// The panic payload for an embedded file that fails to parse — unreachable
/// for shipped data (the test suite parses it), composed without `format!`.
fn embedded_parse_panic(e: &LexError) -> String {
    let mut msg = String::from("embedded tricks vocabulary must parse: ");
    msg.push_str(&DebugText(e).to_string());
    msg
}

/// Resolve the three raw arrays into listing rows: unknown trick ids and
/// rows without a language are recorded and skipped, surfaces are trimmed
/// and empties dropped, and the result is grouped by language (see
/// [`TrickLexicon::all_rows`] for the order and why).
fn ordered_rows(raw: RawTricks, conflicts: &mut Vec<String>) -> Vec<TrickRow> {
    let mut rows: Vec<TrickRow> = Vec::new();
    for r in raw.trick {
        let id = r.id.trim();
        let Some(trick) = Trick::from_code(id) else {
            let mut msg = String::from("unknown trick id ");
            msg.push_str(&DebugText(id).to_string());
            msg.push_str(" (lang ");
            msg.push_str(r.lang.trim());
            msg.push_str("): row skipped");
            conflicts.push(msg);
            continue;
        };
        stage_row(
            &mut rows,
            conflicts,
            TrickRow {
                kind: RowKind::Trick(trick),
                lang: r.lang,
                words: r.words,
                address: r.address,
                gated: r.gated,
                cjk: r.cjk,
            },
        );
    }
    for (kind, raw_rows) in [
        (RowKind::Vocative, raw.vocative),
        (RowKind::Filler, raw.filler),
    ] {
        for r in raw_rows {
            stage_row(
                &mut rows,
                conflicts,
                TrickRow {
                    kind,
                    lang: r.lang,
                    words: r.words,
                    address: Vec::new(),
                    gated: r.gated,
                    cjk: r.cjk,
                },
            );
        }
    }

    // Group by language, first appearance first. `sort_by_key` is STABLE, so
    // inside one language the staging order above (tricks in file order, then
    // vocatives, then fillers) survives untouched.
    let mut langs: Vec<String> = Vec::new();
    for row in &rows {
        if !langs.contains(&row.lang) {
            langs.push(row.lang.clone());
        }
    }
    rows.sort_by_key(|row| {
        langs
            .iter()
            .position(|lang| *lang == row.lang)
            .unwrap_or(usize::MAX)
    });
    rows
}

/// Normalise one row (trim the language and every surface, drop empty
/// surfaces) and stage it; a row with no language is recorded and skipped —
/// the gate, the listing and the coverage pin all key on it.
fn stage_row(rows: &mut Vec<TrickRow>, conflicts: &mut Vec<String>, mut row: TrickRow) {
    row.lang = row.lang.trim().to_string();
    if row.lang.is_empty() {
        let mut msg = String::from("row without lang (");
        push_kind_label(&mut msg, row.kind);
        msg.push_str("): row skipped");
        conflicts.push(msg);
        return;
    }
    for list in [&mut row.words, &mut row.address] {
        for surface in list.iter_mut() {
            *surface = surface.trim().to_string();
        }
        list.retain(|surface| !surface.is_empty());
    }
    rows.push(row);
}

/// A row's kind for a conflict message: `trick sit` / `vocative` / `filler`.
fn push_kind_label(msg: &mut String, kind: RowKind) {
    match kind {
        RowKind::Trick(trick) => {
            msg.push_str("trick ");
            msg.push_str(trick.code());
        }
        RowKind::Vocative => msg.push_str("vocative"),
        RowKind::Filler => msg.push_str("filler"),
    }
}

/// Compile listing rows into the lookup table under the load laws (module
/// docs). Rows arrive in first-claimant order — English first.
fn compile(rows: &[TrickRow], langs: &Langs, mut conflicts: Vec<String>) -> TrickLexicon {
    let mut table: FxHashMap<String, TrickEntry> = FxHashMap::default();
    for row in rows {
        if row.gated {
            if row.lang == "en" {
                // English is the priority language: a gated English row would
                // vanish for a user who configures `languages = ["fr"]`, and
                // the feature would stop answering `sit`. Load it anyway and
                // say so.
                let mut msg = String::from("English row marked gated (");
                push_kind_label(&mut msg, row.kind);
                msg.push_str("): English is never gated, loaded anyway");
                conflicts.push(msg);
            } else if !langs.enabled(&row.lang) {
                continue;
            }
        }
        let lists: [(&[String], bool); 2] = [(&row.words, false), (&row.address, true)];
        for (surfaces, is_address) in lists {
            let role = match row.kind {
                RowKind::Trick(trick) => TrickRole::Trick {
                    trick,
                    needs_vocative: is_address,
                },
                RowKind::Vocative => TrickRole::Vocative,
                RowKind::Filler => TrickRole::Filler,
            };
            for surface in surfaces {
                claim_surface(&mut table, &mut conflicts, row, surface, role);
            }
        }
    }
    TrickLexicon { table, conflicts }
}

/// Validate one authored surface and insert it, resolving a double claim.
fn claim_surface(
    table: &mut FxHashMap<String, TrickEntry>,
    conflicts: &mut Vec<String>,
    row: &TrickRow,
    surface: &str,
    role: TrickRole,
) {
    let chars = surface.chars().count();
    if chars > TrickLexicon::MAX_SURFACE_CHARS {
        push_surface_conflict(
            conflicts,
            surface,
            &row.lang,
            "dropped: longer than the typed-token window, it could never fire",
        );
        return;
    }
    let (key, entry) = if row.cjk {
        // A no-space row's keys are RAW and the typed run must equal one, so
        // every character has to BE no-space script: a mixed surface would
        // take the folded path at lookup and never find this key.
        if !is_no_space_surface(surface) {
            push_surface_conflict(
                conflicts,
                surface,
                &row.lang,
                "dropped: a cjk row holds no-space-script surfaces only",
            );
            return;
        }
        (
            surface.to_string(),
            TrickEntry {
                role,
                // Raw keys are never folded, so no mark is ever lost.
                marks_required: false,
                single_cjk: chars == 1,
            },
        )
    } else {
        let key = fold::fold(surface);
        // The scanner's own single-token law, judged on the folded key exactly
        // as the sparkle lexicon does. It also refuses a no-space-script
        // surface in a row that forgot `cjk = true` (those are not token
        // characters), which would otherwise compile to a key no typed token
        // can reach.
        if key.is_empty() || !is_matchable_token(&key) {
            push_surface_conflict(
                conflicts,
                surface,
                &row.lang,
                "dropped: not a single whole-word token (multi-word, punctuated and \
                 mixed-script surfaces can never be typed as one word)",
            );
            return;
        }
        let marks_required = key.is_ascii() && fold::has_foldable_marks(surface);
        (
            key,
            TrickEntry {
                role,
                marks_required,
                single_cjk: false,
            },
        )
    };
    match table.get_mut(&key) {
        Some(prev) if effective_role(prev) == effective_role(&entry) => {
            // The same claim again — the bare spelling beside the marked one,
            // or a second language sharing the imperative. One claimant
            // listing the BARE spelling makes the key typeable without marks:
            // explicit data is consent (the `scheisse` beside `scheiße` law).
            prev.marks_required &= entry.marks_required;
        }
        Some(_) => push_surface_conflict(
            conflicts,
            surface,
            &row.lang,
            "already claimed under another role: kept the first",
        ),
        None => {
            table.insert(key, entry);
        }
    }
}

/// `surface "<s>" (lang <l>) <what>` — composed without `format!` (see
/// `DebugText`).
fn push_surface_conflict(conflicts: &mut Vec<String>, surface: &str, lang: &str, what: &str) {
    let mut msg = String::from("surface ");
    msg.push_str(&DebugText(surface).to_string());
    msg.push_str(" (lang ");
    msg.push_str(lang);
    msg.push_str(") ");
    msg.push_str(what);
    conflicts.push(msg);
}
