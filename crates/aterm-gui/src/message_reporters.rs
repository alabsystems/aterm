// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONFIG BAND'S REPORTERS, AS MESSAGES — every producer that used to
//! write a line into the retired overwrite banner (`config_notice.rs`) and the
//! retired floating toast (`notice.rs`), each a pure `fn … -> Message`
//! (docs/DESIGN-unified-messages-2026-09-21.md §6, §10.3). Nothing here reads
//! a clock, a window or `App`: a site that runs before `App` exists or off the
//! loop thread queues what these build on the pre-App inbox
//! ([`crate::message_inbox::queue_message`]); a site on the loop posts it
//! (`App::post_message`).
//!
//! # The owner's attention rule (2026-09-23, design §10)
//!
//! The glass carries ONLY three things ([`attention`]): work in flight with an
//! animated indicator that says what is coming and how long (P, progress),
//! a genuine decision (D), or a failure the person must act on (F).
//! Everything else — confirmations, "done", FYI, precautionary notices,
//! disclosures, repeats — is a [`Hold::LogOnly`] RECORD that Settings ▸
//! Messages and `messages.log` keep. A glass title is a few words, verb- or
//! noun-first, with no clauses ([`aterm_messages::GLASS_TITLE_WORDS`], [`GLASS_TITLE_CHARS`]);
//! `detail[0]` is painted only when it changes what the person does
//! (`Message::no_excerpt` otherwise); every other word waits behind Details.
//!
//! # One message per config FAMILY (R1, R4)
//!
//! A launch or reload collects its sentences into [`ConfigWarnings`], one
//! bucket per [`ConfigFamily`], and each bucket becomes ONE message: a terse
//! count title ([`ConfigFamily::title`], singular for one), the head of the
//! first sentence as `detail[0]` ([`excerpt_head`]), and every sentence whole
//! behind it (up to the engine's line cap; past it, the last line is the
//! roll-up that names the validator). Each family has a supersede key under
//! `config.`, so a reload replaces the family's row rather than stacking a
//! second one. A reload that re-derives every family has every live
//! `config.` row in scope; the content-equal reload, which re-derives only
//! the two text-read families, scopes to those keys
//! (`App::replace_config_messages_keyed`). Within the scope a row whose
//! family comes back in the same words stands, a gone or changed family is
//! resolved, and the same words coming back after their row left the glass
//! are a record — a reload runs on every Settings toggle, and a repeat is
//! the log's (`App::replace_config_set`, review 2026-09-24). The
//! restart-only family and the retired spellings are records.
//!
//! # The crash message (R2)
//!
//! The artifact's absolute path rides behind Details — the log keeps it
//! whole — with its first lines (`logging::CrashEvidence`); `Open log` opens
//! it as text (`Intent::OpenPath`, re-validated at press time). The row
//! paints its title alone.
//!
//! # A failed gesture (R9–R14)
//!
//! The floating card (`notice.rs`, retired 2026-09-23) carried a gesture's
//! failure, its decisions (Full Disk Access; the admin step was deleted
//! with the OS-installer protocols, 2026-09-24), their follow-ups and a
//! handful of disclosures. They are messages now, sorted by
//! the owner's attention rule ([`attention`], ruling 76): the glass carries
//! work in flight, very heavy system use while it lasts, and a decision or a
//! failure the person must act on — everything else is a [`Hold::LogOnly`]
//! record. A gesture the person made that did not happen is ONE shape
//! ([`gesture_failure`]): a few words for a title, the whole error behind
//! Details, a short `HOLD_GESTURE`. (R20, the toolchain offer, went with the
//! sealed seed's `seed-pending:` marker that raised it, Phase 5.)

use std::time::Duration;

use aterm_messages::{
    DETAIL_LINES_CAP, Decision, Glyph, HOLD_ASK, HOLD_GESTURE, Hold, Intent, Message, Severity,
    TITLE_CAP, Tag, tags,
};

use crate::crash_journal::PanesWithoutFolder;
use crate::native_settings::SettingsRoute;

/// What earns a row on the glass (the owner's attention rule, 2026-09-23). The
/// rule as code: the tests hold every builder to it (design §10.1, ruling 76).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attention {
    /// Work in flight, with its animated indicator.
    Progress,
    /// A genuine decision: the row asks, or carries a consequential capsule.
    Decision,
    /// A failure the person must act on.
    Failure,
    /// Everything else: a record, never on the glass.
    Record,
}

/// Glass titles: a few words, verb- or noun-first, no clauses — ONE copy, the
/// engine's (design ruling 179), so the wire's `notice` and every builder here
/// meet the same form. Only the character cap is read here now: the one builder
/// that counted words, the admin step's `capped_title`, went with the admin row
/// (design 2026-09-22 §5.3(b)).
use aterm_messages::GLASS_TITLE_CHARS;

/// Classify `msg`, or say why it may not be on the glass: a measured level
/// off the strain row is refused; a record is a record; a confirmation, an FYI and progress with no indicator are not
/// allowed on the glass; a live row with its indicator — a fill or busy
/// (ruling 139: the indicator is the meter's state, never the hold's) — is
/// progress, and may carry a consequential capsule only when it stops that
/// row's own work (`Intent::stops_work`, ruling 232); a live row that waits on the person (Warn or Error, still) is a
/// failure the person acts on; an ask or a consequential capsule is a
/// decision; a warning or an error a failure.
/// Every non-record title is at most [`aterm_messages::GLASS_TITLE_WORDS`] words and
/// [`GLASS_TITLE_CHARS`] characters, carries no clause seam (` — `, `; `,
/// `: `) and does not end in a period.
#[cfg(test)]
pub(crate) fn attention(msg: &Message) -> Result<Attention, &'static str> {
    // A measured LEVEL is Progress on the strain row alone (tag `system`,
    // key `system.strain`); anywhere else it is refused (design ruling 208).
    if let Some(fault) = aterm_messages::strain::level_fault(msg) {
        return Err(fault);
    }
    if msg.hold == Hold::LogOnly {
        return Ok(Attention::Record);
    }
    if msg.severity == Severity::Success {
        return Err("a confirmation on glass");
    }
    let indicator = msg
        .meter
        .as_ref()
        .is_some_and(|m| m.fill_permille.is_some() || m.busy);
    let class = if matches!(msg.hold, Hold::Live { .. }) {
        if indicator {
            // Work in flight is no decision (ruling 101), with ONE exception
            // (ruling 232): a capsule that stops the row's own work — `Stop
            // paste` — which the person may want while they wait. Any other
            // consequential press on a moving row is refused.
            if msg
                .actions
                .iter()
                .any(|i| i.is_consequential() && !i.stops_work())
            {
                return Err("a decision on work in flight");
            }
            Attention::Progress
        } else if msg.severity >= Severity::Warn {
            // A live row blocked on the person — the update flow's editor
            // block — is still, and what it asks is the person's to do.
            Attention::Failure
        } else {
            return Err("progress with no indicator");
        }
    } else if msg.is_ask() || msg.actions.iter().any(Intent::is_consequential) {
        Attention::Decision
    } else if matches!(msg.severity, Severity::Warn | Severity::Error) {
        Attention::Failure
    } else {
        return Err("an FYI on glass");
    };
    if let Some(fault) = aterm_messages::text::glass_title_fault(&msg.title) {
        return Err(fault);
    }
    Ok(class)
}

/// The hold of the two launch-time rows (the crash message, a launch load
/// failure): long enough to read a crash log's head or a load failure's
/// path and go and fix it, short enough that a window left open all day is
/// not wearing last week's crash.
pub(crate) const HOLD_LAUNCH: Duration = Duration::from_secs(120);

/// The supersede key of the crash message: one per launch, and a launch
/// posts at most one.
pub(crate) const KEY_CRASH: &str = "crash.last";

/// The supersede key of the launch load failure (R3).
pub(crate) const KEY_LAUNCH_LOAD: &str = "config.launch-load";

/// The supersede key of the GPU-lost row (R7): Standing until a window
/// created after the loss paints (`App::finalize_successful_present`).
pub(crate) const KEY_GPU_LOST: &str = "render.gpu-lost";

/// The supersede key of the dead accessibility publisher (R8). Its producer
/// is the panic hook of a build WITH an accessibility tree; the builder is
/// platform-neutral (D10) and this build's tests still exercise it.
#[cfg(any(a11y_tree, test))]
pub(crate) const KEY_A11Y_PUBLISHER: &str = "a11y.publisher";

/// The supersede key of the Windows backdrop family (R6): every backdrop
/// decline is one record, whichever site declined it first. The producers are
/// Windows-only; the builder is platform-neutral (D10).
#[cfg(any(windows, test))]
pub(crate) const KEY_BACKDROP: &str = "render.backdrop";

/// The supersede key of the Serious Mode write feedback (R12).
pub(crate) const KEY_SERIOUS_MODE: &str = "config.serious-mode";

/// The supersede key of the native config lane's own errors (R13).
pub(crate) const KEY_CONFIG_LANE: &str = "config.lane";

/// The supersede key of a refused or fleet-owned hold (R10).
pub(crate) const KEY_FABRIC_HOLD: &str = "fabric.hold";

/// The key the Full Disk Access question, its route words and its grant
/// share (R15–R17): one question per process, restated in place after *Open
/// Settings*, answered by *Not now* or resolved by the grant. The card's
/// policy switch resolves the whole `privacy.` prefix.
pub(crate) const KEY_FILE_ACCESS: &str = "privacy.fda";

/// The `Intent::OpenSystemPane` pane name of the Full Disk Access list — the
/// only pane the codec carries (`messages_host::privacy_pane`).
pub(crate) const PANE_FULL_DISK_ACCESS: &str = "full-disk-access";

/// The key of the install-posture row (R21): one per launch.
#[cfg(any(target_os = "macos", test))]
pub(crate) const KEY_INSTALL_POSTURE: &str = "packages.posture";

/// The roll-up's tail when a family has more sentences than a message
/// holds lines: where the whole list lives.
const VALIDATOR_HINT: &str = "run `aterm --window --validate-config` for the full list";

/// The widest `detail[0]` a config family's excerpt is cut to at a clause
/// seam ([`excerpt_head`]); the band shapes it further by its own width law.
const EXCERPT_CAP: usize = 80;

/// The families a launch or reload sorts its config warnings into — one
/// message each. The order is the band's reading order when several are
/// posted together: what a person typed wrong first (a chord, a key, a
/// value), then what this build cannot draw, then what waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ConfigFamily {
    /// `[keybindings]` / `[key_sequences]` rules that were skipped.
    Keybindings,
    /// Keys `aterm.toml` sets that this build does nothing with.
    IgnoredKeys,
    /// Retired and deprecated spellings (`[packages] auto_update` /
    /// `seed_install`, `game_font`): keys aterm or atpkg still reads, or that a
    /// newer key now decides. A RECORD, never a row: nothing is broken and
    /// nothing presses — the editor marks each one, and Settings ▸ Packages
    /// already explains an Off the retired `auto_update` holds. Kept apart from
    /// [`Self::IgnoredKeys`], whose words say "no effect in this build": told
    /// that, a person who deletes `auto_update = false` turns automatic updates
    /// back on (review of the second origin/main merge, 2026-09-23; design
    /// ruling 42).
    RetiredKeys,
    /// Keys spelled right whose VALUE this build does not accept.
    UnacceptedValues,
    /// `cursor_trail_style` and the Trail Pack manifests.
    CursorTrail,
    /// Font families, faces and variation requests that did not resolve.
    Fonts,
    /// Image assets (`wallpaper`, `cursor_nyan_sprite`) that were not admitted.
    Assets,
    /// Restart-only keys a live reload cannot apply (`columns`/`lines`, `gpu`):
    /// a RECORD — the edit waits, and Settings ▸ Advanced, Modified and Manual
    /// already mark the key beside it.
    Restart,
    /// A Secure Keyboard Entry transition the OS refused.
    SecureKeyboard,
}

impl ConfigFamily {
    /// The family's supersede key: `config.<family>`, so a reload replaces
    /// the family's live row and a clean reload resolves the `config.` prefix.
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Keybindings => "config.keybindings",
            Self::IgnoredKeys => "config.ignored-keys",
            Self::RetiredKeys => "config.retired-keys",
            Self::UnacceptedValues => "config.unaccepted-values",
            Self::CursorTrail => "config.cursor-trail",
            Self::Fonts => "config.fonts",
            Self::Assets => "config.assets",
            Self::Restart => "config.restart",
            Self::SecureKeyboard => "config.ske",
        }
    }

    /// A restart-only edit is information — nothing is broken, the edit
    /// simply waits — and so is a retired spelling, which says what it still
    /// does; every other family is an edit that did not take.
    const fn severity(self) -> Severity {
        match self {
            Self::Restart | Self::RetiredKeys => Severity::Info,
            _ => Severity::Warn,
        }
    }

    /// The family's hold: a record for the two families that are information
    /// ([`Self::Restart`], [`Self::RetiredKeys`]), the engine's default for
    /// every edit to go and fix.
    const fn hold(self) -> Hold {
        match self {
            Self::Restart | Self::RetiredKeys => Hold::LogOnly,
            _ => Hold::Default,
        }
    }

    /// The family's title for `n` sentences: a few words, singular for one.
    /// The restart-only family is a record and keeps its sentence (one) or
    /// its count (several).
    pub(crate) fn title(self, n: usize) -> String {
        let one = n == 1;
        match self {
            Self::Keybindings if one => "Keybinding skipped".to_string(),
            Self::Keybindings => format!("{n} keybindings skipped"),
            // `config` is the row's tag and its `Open aterm.toml` capsule
            // already: the title spends its cells on nothing else, so the
            // excerpt's near miss (`windw_padding → window_padding?`) fits
            // at 80 columns (review 2026-09-24). "No effect", never
            // "unknown": this family also carries every key a REMOVED
            // feature left behind (`show_scene_hud`, `scene_rows`, … —
            // `RETIRED_CONFIG_KEYS`), and this build knows those precisely;
            // "no effect in this build" is true of every member (fffef97a1,
            // ruling 146).
            // Ruling 261: the config row names what is wrong in a person's
            // words — `Misspelled setting` where the validator has a near
            // miss ([`config_family_message`] reads the sentences), `Unknown
            // setting` otherwise; a key a REMOVED feature left behind is the
            // retired family's record (ruling 213), never this row.
            Self::IgnoredKeys if one => "Unknown setting".to_string(),
            Self::IgnoredKeys => format!("{n} unknown settings"),
            Self::RetiredKeys if one => "Retired setting".to_string(),
            Self::RetiredKeys => format!("{n} retired settings"),
            Self::UnacceptedValues if one => "Couldn't use a setting's value".to_string(),
            Self::UnacceptedValues => format!("Couldn't use {n} settings' values"),
            Self::CursorTrail if one => "Couldn't apply the cursor trail".to_string(),
            // A count of the family's sentences, which are trail packs and the
            // style alike — not "settings" — and short enough that a 60-column
            // row keeps its `Open aterm.toml` beside `Value not accepted`'s
            // (design ruling 217).
            Self::CursorTrail => format!("{n} errors in cursor trail settings"),
            Self::Fonts if one => "Couldn't apply the font".to_string(),
            Self::Fonts => format!("Couldn't apply {n} fonts"),
            Self::Assets if one => "Couldn't load the image".to_string(),
            Self::Assets => format!("Couldn't load {n} images"),
            Self::Restart if one => "A setting applies after restart".to_string(),
            Self::Restart => format!("{n} settings apply after restart"),
            Self::SecureKeyboard => "Couldn't change Secure Keyboard Entry".to_string(),
        }
    }

    /// The words a family's row is logged under once a clean reload resolves
    /// it (design ruling 265): `Misspelled setting fixed`, `Font applied`.
    /// `None` for the two records, which are never resolved.
    pub(crate) fn fixed_title(self, title: &str, n: usize) -> Option<String> {
        let one = n == 1;
        Some(match self {
            // `Misspelled setting fixed`, `3 unknown settings fixed`.
            Self::IgnoredKeys => format!("{title} fixed"),
            Self::Keybindings if one => "Keybinding fixed".to_string(),
            Self::Keybindings => format!("{n} keybindings fixed"),
            Self::UnacceptedValues if one => "Setting's value fixed".to_string(),
            Self::UnacceptedValues => format!("{n} settings' values fixed"),
            Self::CursorTrail => "Cursor trail applied".to_string(),
            Self::Fonts if one => "Font applied".to_string(),
            Self::Fonts => format!("{n} fonts applied"),
            Self::Assets if one => "Image loaded".to_string(),
            Self::Assets => format!("{n} images loaded"),
            Self::SecureKeyboard => "Secure Keyboard Entry changed".to_string(),
            Self::Restart | Self::RetiredKeys => return None,
        })
    }

    /// The first sentence's excerpt — what changes what the person does,
    /// as a fragment, never a sentence cut mid-thought (review round 2,
    /// 2026-09-23): Secure Keyboard Entry's state (the clause after its last
    /// ` — `); an unknown key as the key and its near miss (`windw_padding →
    /// window_padding?`, or the bare key), the line number behind Details;
    /// a skipped chord as `ctrl+x: no action foo`; the head of every other
    /// family's sentence at a clause seam.
    fn excerpt(self, first: &str) -> String {
        match self {
            Self::SecureKeyboard => first
                .rsplit_once(" \u{2014} ")
                .map_or(first, |(_, state)| state.trim())
                .to_string(),
            Self::IgnoredKeys => unknown_key_excerpt(first)
                .unwrap_or_else(|| excerpt_head(first, EXCERPT_CAP).to_string()),
            Self::Keybindings => {
                let chord = first
                    .replace(": unknown action ", ": no action ")
                    .replace('"', "");
                excerpt_head(&chord, EXCERPT_CAP).to_string()
            }
            Self::Fonts => {
                font_excerpt(first).unwrap_or_else(|| excerpt_head(first, EXCERPT_CAP).to_string())
            }
            _ => excerpt_head(first, EXCERPT_CAP).to_string(),
        }
    }

    /// The prefixes a sentence sheds on its way into a message: the
    /// `config ` every banner line carried (the row's tag says it now), and
    /// the family word of a keybinding warning, whose subject is the chord —
    /// with its `skipping ` (the title already says skipped: `Keybinding
    /// skipped · skipping "ctrl+x"` said it twice, review 2026-09-23).
    fn strip_prefix(self, sentence: &str) -> &str {
        let s = sentence.strip_prefix("config ").unwrap_or(sentence);
        match self {
            Self::Keybindings => {
                let s = s
                    .strip_prefix("keybindings: ")
                    .or_else(|| s.strip_prefix("key_sequences: "))
                    .unwrap_or(s);
                s.strip_prefix("skipping ").unwrap_or(s)
            }
            _ => s,
        }
    }
}

/// The config warnings of one launch or one reload, sorted into families as
/// they are collected. Exact repeats within a family fold (a live reload
/// re-declining the same material must not list it twice). The sentences
/// are kept verbatim — [`ConfigWarnings::sentences`] is the stderr echo, in
/// the order they were collected, so a console launch reads what it always
/// read — and shed their prefixes only on the way into a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConfigWarnings {
    /// Families in first-push order, each with its sentences in push order.
    families: Vec<(ConfigFamily, Vec<String>)>,
}

impl ConfigWarnings {
    /// Add one sentence to `family`; an exact repeat within the family folds.
    pub(crate) fn push(&mut self, family: ConfigFamily, sentence: String) {
        let bucket = match self.families.iter_mut().find(|(f, _)| *f == family) {
            Some((_, lines)) => lines,
            None => {
                self.families.push((family, Vec::new()));
                &mut self.families.last_mut().expect("just pushed").1
            }
        };
        if !bucket.contains(&sentence) {
            bucket.push(sentence);
        }
    }

    /// Add every sentence of `family`.
    pub(crate) fn extend(
        &mut self,
        family: ConfigFamily,
        sentences: impl IntoIterator<Item = String>,
    ) {
        for sentence in sentences {
            self.push(family, sentence);
        }
    }

    /// Every sentence collected so far, verbatim, family by family in
    /// collection order — the stderr echo, and the `already_told` slice the
    /// unaccepted-value filter reads (a resolver that named the value it
    /// refused keeps its fuller sentence; the generic one stands down).
    pub(crate) fn sentences(&self) -> impl Iterator<Item = &str> {
        self.families
            .iter()
            .flat_map(|(_, lines)| lines.iter().map(String::as_str))
    }

    /// [`Self::sentences`] owned, for a filter that takes `&[String]`.
    pub(crate) fn told(&self) -> Vec<String> {
        self.sentences().map(str::to_owned).collect()
    }

    /// One message per family that collected anything, in collection order.
    pub(crate) fn into_messages(self) -> Vec<Message> {
        self.families
            .into_iter()
            .filter(|(_, lines)| !lines.is_empty())
            .map(|(family, lines)| config_family_message(family, &lines))
            .collect()
    }
}

/// An unknown key's excerpt from the validator's sentence (its `config `
/// shed): `windw_padding → window_padding?` from `line 3: windw_padding —
/// did you mean "window_padding"? (…)`, and the bare key from `line 4: foo
/// is unknown to this build and has no effect` — the title already says
/// unknown, and the line number is the sentence's, behind Details. `None` for
/// a sentence in neither shape.
fn unknown_key_excerpt(sentence: &str) -> Option<String> {
    let s = sentence
        .strip_prefix("line ")
        .and_then(|rest| rest.split_once(": "))
        .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map_or(sentence, |(_, rest)| rest);
    let bare = |key: &str| {
        let key = key.trim();
        key.strip_prefix("unknown key ")
            .unwrap_or(key)
            .trim_matches('"')
            .to_string()
    };
    if let Some((key, rest)) = s.split_once(" \u{2014} ")
        && let Some(near) = rest.strip_prefix("did you mean ")
    {
        let near = near.split('?').next()?.trim_matches('"');
        let key = bare(key);
        return (!key.is_empty() && !near.is_empty()).then(|| format!("{key} \u{2192} {near}"));
    }
    let (key, _) = s.split_once(" is unknown")?;
    let key = bare(key);
    (!key.is_empty()).then_some(key)
}

/// A font verdict's excerpt in a person's words (design ruling 306):
/// `no font named Nope` from `font_family_bold: "Nope" is not an admissible
/// font (not found)` — the validator's key, quoting and `admissible` stay
/// behind Details, where the sentence is kept whole. `None` for a sentence in
/// another shape.
fn font_excerpt(sentence: &str) -> Option<String> {
    let (head, _) = sentence.split_once(" is not an admissible font")?;
    let (_, quoted) = head.split_once('"')?;
    let name = quoted
        .rsplit_once('"')
        .map_or(quoted, |(name, _)| name)
        .trim();
    (!name.is_empty()).then(|| format!("no font named {name}"))
}

/// A TOML parser's diagnostic in a person's words for the band's excerpt
/// (design ruling 306): `line 3: missing =` where the parser names the line
/// and only missing tokens (`expected \`.\`, \`=\``), `a mistake on line 3`
/// where it names the line alone; `None` where it names no line — the row
/// then paints its title alone, and the parser's rows, caret diagram and
/// all, stay behind Details.
fn toml_words(rows: &[String]) -> Option<String> {
    let line = rows.iter().find_map(|row| {
        let (_, rest) = row.split_once("at line ")?;
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!n.is_empty()).then_some(n)
    })?;
    let missing = rows.iter().rev().find_map(|row| {
        let rest = row.trim().strip_prefix("expected ")?;
        let tokens: Vec<&str> = rest
            .split(", ")
            .flat_map(|t| t.split(" or "))
            .map(str::trim)
            .collect();
        tokens
            .iter()
            .all(|t| t.len() >= 3 && t.starts_with('`') && t.ends_with('`'))
            .then(|| {
                tokens
                    .iter()
                    .map(|t| t.trim_matches('`'))
                    .collect::<Vec<_>>()
                    .join(" or ")
            })
    });
    Some(match missing {
        Some(missing) => format!("line {line}: missing {missing}"),
        None => format!("a mistake on line {line}"),
    })
}

/// The longest head of `sentence` that ends at a clause seam — before a
/// parenthesised aside (` (`), a semicolon (`; `) or a dash (` — `), OUTSIDE
/// any parenthesis (a seam inside an aside cuts the aside open, not the
/// sentence) — and fits `cap` characters; the whole sentence when none does
/// (the band's width law shapes it from there). The seams are the ones the
/// config sentences use: the near-miss key keeps its `did you mean` and sheds
/// `(no effect in this build)`, a font sentence keeps its verdict and `(not
/// found)` and sheds `; ignored`; the plain unknown key has no seam and is its
/// own head.
pub(crate) fn excerpt_head(sentence: &str, cap: usize) -> &str {
    const SEAMS: [&str; 3] = [" (", "; ", " \u{2014} "];
    let mut best = None;
    let mut depth = 0usize;
    for (chars_before, (at, ch)) in sentence.char_indices().enumerate() {
        if chars_before > cap {
            break;
        }
        if at > 0 && depth == 0 && SEAMS.iter().any(|seam| sentence[at..].starts_with(seam)) {
            best = Some(at);
        }
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    best.map_or(sentence, |at| &sentence[..at])
}

/// R1/R4 — one family's message: the terse count title, the first
/// sentence's excerpt as `detail[0]`, every sentence whole behind it (the
/// excerpt not repeated when it IS the first sentence), and — when there are
/// more than a message holds — a last line saying how many more and where to
/// read them: a list that looks complete while it is not is worse than no
/// list. The restart-only family is a record that keeps its sentence as its
/// title when it has one.
fn config_family_message(family: ConfigFamily, sentences: &[String]) -> Message {
    let stripped: Vec<&str> = sentences.iter().map(|s| family.strip_prefix(s)).collect();
    let title = match (family, stripped.as_slice()) {
        (ConfigFamily::Restart, [only]) => restart_title(only),
        // Every unknown key has a near miss: they are misspellings (ruling 261).
        (ConfigFamily::IgnoredKeys, all)
            if all
                .iter()
                .all(|s| unknown_key_excerpt(s).is_some_and(|e| e.contains('\u{2192}'))) =>
        {
            match all.len() {
                1 => "Misspelled setting".to_string(),
                n => format!("{n} misspelled settings"),
            }
        }
        _ => family.title(stripped.len()),
    };
    // How the row is logged once a clean reload resolves it (ruling 265):
    // the problem, fixed — a ✓ record that leaves Problems, never the
    // warning's own title still standing in amber.
    let fixed = family.fixed_title(&title, stripped.len());
    let mut msg = Message::new(tags::CONFIG, family.severity(), title)
        .action(Intent::OpenConfigEditor { line: None })
        .key(family.key())
        .hold(family.hold());
    if let Some(fixed) = fixed {
        msg = msg.finished_as(fixed);
    }
    if family == ConfigFamily::Restart && stripped.len() == 1 {
        return msg.lines(stripped.iter().map(|s| (*s).to_string()));
    }
    let first = stripped.first().copied().unwrap_or_default();
    let excerpt = family.excerpt(first);
    let mut used = 0;
    // The excerpt is the PAINTED line; a record is never painted, so there it
    // would only say the first sentence twice (audit 2026-09-24).
    if excerpt != first && family.hold() != Hold::LogOnly {
        msg = msg.line(excerpt);
        used += 1;
    }
    // Each sentence in its PHYSICAL rows ([`diagnostic_lines`], fffef97a1),
    // and the budget counted in rows, not sentences: one Trail Pack parse
    // error is a sentence and a four-row caret diagram, and counting it as one
    // line would let `Message::line`'s cap silently drop the roll-up below —
    // the list that looks complete while it is not. A slot is kept for the
    // roll-up whenever a sentence could still follow; a single sentence longer
    // than a whole message keeps its head and leaves that slot.
    let mut shown = 0;
    for (i, sentence) in stripped.iter().enumerate() {
        let mut rows = diagnostic_lines(sentence);
        let roll_up = usize::from(i + 1 < stripped.len());
        if used + rows.len() + roll_up > DETAIL_LINES_CAP {
            if shown > 0 {
                break;
            }
            rows.truncate(DETAIL_LINES_CAP.saturating_sub(used + roll_up));
        }
        used += rows.len();
        shown += 1;
        msg = msg.lines(rows);
    }
    if shown < stripped.len() {
        msg = msg.line(format!(
            "\u{2026} and {} more \u{2014} {VALIDATOR_HINT}",
            stripped.len() - shown
        ));
    }
    msg
}

/// A restart-only setting's record title (ruling 261): the setting in a
/// person's words and when it applies — `GPU setting applies after restart`
/// for `gpu applies on next launch (…)` — the sentence behind it. A key this
/// map does not know reads as its words (`font_px` → `Font px`).
fn restart_title(sentence: &str) -> String {
    let key = sentence.split_whitespace().next().unwrap_or_default();
    let name = match key {
        "gpu" => "GPU".to_string(),
        "columns/lines" | "columns" | "lines" => "Window size".to_string(),
        other => {
            let words = other.replace(['_', '/'], " ");
            let mut chars = words.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().chain(chars).collect::<String>()
            })
        }
    };
    if name.is_empty() {
        ConfigFamily::Restart.title(1)
    } else {
        format!("{name} setting applies after restart")
    }
}

/// The `font_family` the backend build thread could not admit — a config
/// warning like the launch's font row, but minted on another thread after the
/// launch's families were queued, so it carries its OWN key: sharing
/// `config.fonts` would have it supersede the launch's font row and take
/// those sentences off the glass.
pub(crate) fn font_family_rejected(warning: &str) -> Message {
    config_family_message(ConfigFamily::Fonts, &[warning.to_string()]).key("config.font-family")
}

/// The crash row's first line: plain words for what is there to look at —
/// never the report's path (design ruling 74). Not painted: the title says
/// what happened and `Open log` is the press (ruling 145).
pub(crate) const CRASH_EXCERPT: &str = "a crash report was saved";

/// R2 — the previous run died (ruling 145). The title says so in a few words
/// — `aterm crashed last time` — and the row paints it alone
/// ([`Message::no_excerpt`]): [`CRASH_EXCERPT`] is `detail[0]`, behind
/// `Details ›`; the artifact's ABSOLUTE path is the next line (`crash log at
/// <path>`: the band printing it home-abbreviated still showed
/// `crash-signal-<pid>-<nanos>.log.seen`, an internal file name, so it is
/// never on glass), then the file's head; `Open log` opens it as text.
pub(crate) fn crash_message(evidence: &crate::logging::CrashEvidence) -> Message {
    let path = evidence.path.display().to_string();
    // An Error wears the cross (ruling 302): the red triangle read as a
    // warning in an error's ink.
    Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .retrospective()
        .line(CRASH_EXCERPT)
        .line(format!("crash log at {path}"))
        .lines(evidence.head.iter().cloned())
        .no_excerpt()
        .action(Intent::OpenPath { path })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_CRASH)
}

/// The killed row's first line: what is — and is not — there to look at.
pub(crate) const KILLED_EXCERPT: &str =
    "no crash report: it ended without a crash signal or a quit";

/// R2b — the installed app's previous run was KILLED: its crash marker was left
/// empty and unlocked, so the process ended with no fatal signal and no exit
/// path at all (Force Quit, SIGKILL, running out of memory, a system watchdog,
/// power loss). Before the marker lifecycle (`crash_signal::markers`) that
/// death read exactly like a clean quit. A WARNING, not an error: aterm did not
/// fail, something ended it — but it is the same "last time" class of fact as
/// the crash row and shares its key, so a launch shows one of the two.
/// `Open log` opens `aterm.log`, whose last lines are what the run was doing.
pub(crate) fn killed_message(evidence: &crate::logging::KillEvidence) -> Message {
    let log = evidence.log.display().to_string();
    Message::new(tags::CRASH, Severity::Warn, "aterm was stopped last time")
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(KILLED_EXCERPT)
        .line("Force Quit, running out of memory or a system watchdog ends a process this way")
        .line(format!("its last lines are in {log}"))
        .line(format!("marker {}", evidence.marker.display()))
        .no_excerpt()
        .action(Intent::OpenPath { path: log })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_CRASH)
}

/// THE PTY KEEPER'S RECOVERY ROW (P3, design §5.3 step 7): the last window
/// crashed or was killed, and the keeper kept `n` of its sessions running;
/// this launch reattached them. It takes the crash row's slot. `End
/// sessions` hangs every reattached shell up (Force Quit and jetsam look the
/// same, decision 4); `refused` offers the admission turned down are counted.
pub(crate) fn keeper_recovered_message(n: usize, refused: usize) -> Message {
    let mut message = Message::new(tags::CRASH, Severity::Warn, "aterm stopped unexpectedly")
        .glyph(Glyph::or_fallback('\u{21bb}'))
        .line(format!(
            "{} kept running and {} reattached",
            counted(n, "session"),
            if n == 1 { "was" } else { "were" }
        ))
        .line("each screen was redrawn by its program; the scrollback before the stop is not here");
    if refused > 0 {
        message = message.line(format!(
            "{} could not be reattached and {} ended",
            counted(refused, "session"),
            if refused == 1 { "was" } else { "were" }
        ));
    }
    message
        .no_excerpt()
        .action(Intent::EndRecovered)
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_CRASH)
}

/// The reopened layout's loss line: what a crash journal cannot bring back.
pub(crate) const JOURNAL_LOSS: &str =
    "the programs that were running in them, and their scrollback, did not survive";

/// The supersede key of a crash journal this launch took and could not
/// reopen: one per launch.
pub(crate) const KEY_CRASH_JOURNAL: &str = "crash.journal";

/// `n` and `word`, plural when `n` is not one (`word` takes an `s`).
fn counted(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// The tail of the reopened layout's line when every program that ran was an
/// agent that resumes its conversation (ruling 293): nothing else was lost.
pub(crate) const JOURNAL_ONLY_SCROLLBACK: &str = "only the scrollback did not survive";

/// [`JOURNAL_ONLY_SCROLLBACK`] when a pane also could not open its folder:
/// the scrollback was not all that was lost.
pub(crate) const JOURNAL_SCROLLBACK: &str = "the scrollback did not survive";

/// The reopened layout's quiet line (D16, ruling 282): every leaf sat at its
/// shell's prompt, so nothing a person ran was lost.
pub(crate) const JOURNAL_NOTHING_RAN: &str =
    "nothing was running in them: only their scrollback did not survive";

/// [`JOURNAL_NOTHING_RAN`] when a pane also could not open its folder.
pub(crate) const JOURNAL_NOTHING_RAN_FOLDERLESS: &str =
    "nothing was running in them: their scrollback did not survive";

/// `names` as a person reads a list — `vim`, `vim and claude`, `vim, claude
/// and less` — at most three named and the rest counted, then `unnamed`
/// programs whose names were not known.
fn listed(names: &[String], unnamed: usize) -> String {
    const SHOWN: usize = 3;
    let mut shown: Vec<String> = names.iter().take(SHOWN).cloned().collect();
    let others = names.len().saturating_sub(SHOWN) + unnamed;
    match (shown.is_empty(), others) {
        (_, 0) => {}
        (true, 1) => shown.push("a program".to_string()),
        (true, n) => shown.push(format!("{n} programs")),
        (false, 1) => shown.push("another program".to_string()),
        (false, n) => shown.push(format!("{n} other programs")),
    }
    match shown.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Which programs were running, in how many of the layout's tabs: `Python
/// was running in 1 of 2 tabs`.
fn journal_ran_line(lost: &crate::crash_journal::Lost, tabs: usize) -> String {
    let place = match (lost.tabs == tabs, tabs) {
        (true, 1) => "its tab".to_string(),
        (true, 2) => "both tabs".to_string(),
        (true, n) => format!("all {n} tabs"),
        (false, n) => format!("{} of {n} tabs", lost.tabs),
    };
    let what = listed(&lost.named, lost.unnamed);
    let were = if lost.named.len() + lost.unnamed == 1 {
        "was"
    } else {
        "were"
    };
    format!("{what} {were} running in {place}")
}

/// The lost case's loss line: which programs were running, in how many of
/// the reopened tabs, and that they and the scrollback did not survive.
fn journal_loss_line(lost: &crate::crash_journal::Lost, tabs: usize) -> String {
    format!(
        "{} and did not survive, nor did the scrollback",
        journal_ran_line(lost, tabs)
    )
}

/// THE AGENTS THAT COME BACK, in a person's words (P6a, ruling 293): `aterm
/// starts Claude again on its conversation in tab 2`, `… its conversations in
/// tabs 1 and 3`, `… in 4 tabs`; `tab 2 of window 1` when the layout has more
/// than one window. `resumed` is [`crate::crash_journal::Reopened::resumed`]'s,
/// one entry per agent; empty for none. What aterm DOES, said as the launch
/// starts it — never a promise of how it ends (day six, D33: `Claude resumes
/// …` stood in the log beside the row that said it did not); a relaunch that
/// does not take is said by its own row ([`restored_agents_not_resumed`]).
fn journal_resumed_line(resumed: &[(u32, u32)], windows: usize) -> String {
    const SHOWN: usize = 3;
    let mut places: Vec<(u32, u32)> = Vec::new();
    for place in resumed {
        if !places.contains(place) {
            places.push(*place);
        }
    }
    let conversations = if resumed.len() == 1 {
        "its conversation"
    } else {
        "its conversations"
    };
    let one = |(window, tab): (u32, u32)| {
        if windows > 1 {
            format!(
                "tab {} of window {}",
                u64::from(tab) + 1,
                u64::from(window) + 1
            )
        } else {
            format!("tab {}", u64::from(tab) + 1)
        }
    };
    let place = match places.as_slice() {
        [] => return String::new(),
        [only] => one(*only),
        many if many.len() > SHOWN => format!("{} tabs", many.len()),
        [init @ .., last] if windows <= 1 => format!(
            "tabs {} and {}",
            init.iter()
                .map(|(_, tab)| (u64::from(*tab) + 1).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            u64::from(last.1) + 1
        ),
        [init @ .., last] => format!(
            "{} and {}",
            init.iter().map(|p| one(*p)).collect::<Vec<_>>().join(", "),
            one(*last)
        ),
    };
    format!("aterm starts Claude again on {conversations} in {place}")
}

/// What the reopened layout brought back, and from how many journals: `2 tabs
/// in 1 window restored in their folders from its crash journal`. The panes
/// whose folder is not here
/// ([`crate::crash_journal::Reopened::panes_without_folder`], audit #7
/// finding 48): those whose folder no shell could start in are counted,
/// whether the home folder stood in for them or not; with only panes a
/// program held (audit #8), the line claims no folders.
fn journal_restored_line(
    tabs: usize,
    windows: usize,
    sources: usize,
    folders: PanesWithoutFolder,
) -> String {
    let restored = format!("{} in {}", counted(tabs, "tab"), counted(windows, "window"));
    let from = match sources {
        1 => "its crash journal".to_string(),
        n => format!("{n} crash journals"),
    };
    match folders.gone {
        0 if folders == PanesWithoutFolder::NONE => format!(
            "{restored} restored in {} from {from}",
            if tabs == 1 {
                "its folder"
            } else {
                "their folders"
            }
        ),
        0 => format!("{restored} restored from {from}"),
        1 => format!("{restored} restored from {from}; 1 pane could not open its folder"),
        n => format!("{restored} restored from {from}; {n} panes could not open their folders"),
    }
}

/// The reopened layout's title when programs were lost: the one program by
/// its name (day five, D19: `a program lost` said not which), else how many.
fn journal_lost_title(lost: Option<&crate::crash_journal::Lost>) -> String {
    match lost {
        Some(lost) if lost.unnamed == 0 && lost.named.len() == 1 => {
            format!("Tabs restored, {} lost", lost.named[0])
        }
        Some(lost) if lost.unnamed + lost.named.len() == 1 => {
            "Tabs restored, a program lost".to_string()
        }
        _ => "Tabs restored, programs lost".to_string(),
    }
}

/// The tag a journal's entry files under (day five, D19): `crash` for a
/// crash, `session` (restore) for a kill — a kill is not aterm crashing.
fn journal_tag(class: crate::crash_journal::DeathClass) -> Tag {
    if class.crashed() {
        tags::CRASH
    } else {
        tags::SESSION
    }
}

/// P1 of the PTY keeper — the previous run ended uncleanly and its crash
/// journal (`crate::crash_journal`) reopened its layout: the windows, the tabs
/// and their folders came back, the programs and their scrollback did not. It
/// takes the crash/kill row's slot (`KEY_CRASH`, one per launch) and carries
/// what that row would have: the crash log's path and head with `Open log` on
/// it, or the killed run's log (`log`, `aterm.log` in the log dir, when no
/// crash-row evidence names one — a development start's kill is no row of its
/// own). An ERROR for a crash, a WARNING for a kill, like the rows it replaces.
///
/// A QUIET RELAUNCH (D16, ruling 282): the journal also says which leaves had
/// a program running ([`crate::crash_journal::Reopened::lost`]). When none
/// did, a kill lost nothing a person ran — every tab came back at its prompt,
/// in its folder when the restored line says so — so it is a RECORD (`Info`,
/// [`Hold::LogOnly`]) that says so, and the band stays quiet. A crash with
/// nothing lost keeps its row: aterm failed, and the crash log is the
/// person's to open or send; its words say nothing was running. When
/// programs ran, the row names how many tabs lost one and which programs; a
/// journal from before the field (`None`) reads as it always did.
///
/// THE ANSWER IS THE SENTENCE (day five, D19): `detail[0]` — the plain
/// sentence Settings ▸ Messages sets above the technical block — says what
/// was lost or that nothing was; how many tabs came back and how aterm ended
/// follow as details. The title names the one lost program by its name. A
/// kill files under `session`, a crash under `crash`.
///
/// AN AGENT THAT COMES BACK IS NOT LOST (P6a, ruling 293): `relaunching` is
/// whether this launch's supervisor host relaunches the agents the layout
/// carried ([`crate::harness_host::HostHandle::relaunches_restored`]). Each
/// one it brings back on its conversation
/// ([`crate::crash_journal::Reopened::resumed`]) is left out of what was
/// lost and said as coming back (`aterm starts Claude again on its
/// conversation in tab 2`): when nothing else ran, only scrollback was lost, and a kill is a
/// record; when another program was lost, the warning names that one and its
/// sentence says which agent resumes (ruling 314). If the relaunch then fails, the host says
/// so once ([`restored_agents_not_resumed`]).
///
/// A PANE WHOSE FOLDER NO SHELL CAN START IN did not come back in it (audit
/// #7 finding 48): `folders` counts them, and the restored line says how many
/// rather than that every tab came back in its folder
/// ([`journal_restored_line`]). A pane a program held whose folder is not
/// here is not counted, and the line then claims no folders (audit #8).
/// Either way the loss sentence drops its `only`: the scrollback was not all
/// that was lost.
pub(crate) fn journal_reopened_message(
    reopened: &crate::crash_journal::Reopened,
    crash: Option<&crate::logging::CrashEvidence>,
    killed: Option<&crate::logging::KillEvidence>,
    log: Option<&std::path::Path>,
    relaunching: bool,
    folders: PanesWithoutFolder,
) -> Message {
    let (windows, tabs) = reopened.counts();
    let lost = reopened.lost_given(relaunching);
    let resumed = journal_resumed_line(&reopened.resumed(relaunching), windows);
    let nothing_ran = lost.as_ref().is_some_and(|lost| lost.tabs == 0);
    let quiet = nothing_ran && !reopened.class.crashed();
    let severity = if quiet {
        Severity::Info
    } else if reopened.class.crashed() {
        Severity::Error
    } else {
        Severity::Warn
    };
    let mut evidence_lines = Vec::new();
    let open = if let Some(evidence) = crash {
        let path = evidence.path.display().to_string();
        evidence_lines.push(format!("crash log at {path}"));
        evidence_lines.extend(evidence.head.iter().cloned());
        Some(path)
    } else if let Some(evidence) = killed {
        let log = evidence.log.display().to_string();
        evidence_lines.push(format!("its last lines are in {log}"));
        evidence_lines.push(format!("marker {}", evidence.marker.display()));
        Some(log)
    } else {
        log.map(|log| {
            let log = log.display().to_string();
            evidence_lines.push(format!("its last lines are in {log}"));
            log
        })
    };
    let title = if quiet {
        "Tabs restored after aterm stopped".to_string()
    } else if nothing_ran {
        "Tabs restored after aterm crashed".to_string()
    } else {
        journal_lost_title(lost.as_ref())
    };
    // ONLY THE SCROLLBACK holds while every pane came back in its folder.
    let (idle_line, scrollback_line) = if folders == PanesWithoutFolder::NONE {
        (JOURNAL_NOTHING_RAN, JOURNAL_ONLY_SCROLLBACK)
    } else {
        (JOURNAL_NOTHING_RAN_FOLDERLESS, JOURNAL_SCROLLBACK)
    };
    // ONE SENTENCE (ruling 314, day eight E2): the agent coming back rides
    // the loss sentence, never a detail of its own — Settings ▸ Messages sets
    // every detail after the first as technical text, and `aterm starts
    // Claude again …` stood there in monospace.
    let loss = match &lost {
        Some(lost) if lost.tabs == 0 && resumed.is_empty() => idle_line.to_string(),
        Some(lost) if lost.tabs == 0 => format!("{resumed}; {scrollback_line}"),
        None if resumed.is_empty() => JOURNAL_LOSS.to_string(),
        None => format!("{JOURNAL_LOSS}; {resumed}"),
        Some(lost) if resumed.is_empty() => journal_loss_line(lost, tabs),
        Some(lost) => format!("{}; {resumed}", journal_loss_line(lost, tabs)),
    };
    // The mark is the severity's (ruling 302): a crash's row the cross, a
    // kill's the triangle, the quiet relaunch's record `ℹ` — never the
    // working `↻`, which read as a restore still under way.
    let mut msg = Message::new(journal_tag(reopened.class), severity, title).line(loss);
    msg = msg
        .line(journal_restored_line(
            tabs,
            windows,
            reopened.sources.len(),
            folders,
        ))
        .line(reopened.class.sentence())
        .lines(evidence_lines);
    if reopened.class.crashed() {
        msg = msg.retrospective();
    }
    if let Some(path) = open {
        msg = msg.action(Intent::OpenPath { path });
    }
    let hold = if quiet {
        Hold::LogOnly
    } else {
        Hold::For(HOLD_LAUNCH)
    };
    msg.no_excerpt().hold(hold).key(KEY_CRASH)
}

/// P1 of the PTY keeper — a crashed run's journal this launch took and did
/// NOT reopen: it could not be read (a torn file after a power loss, a newer
/// build's schema, a planted link), or the brake skipped it (the run that wrote
/// it had reopened a crash journal and crashed within 90 s, or was stopped
/// within them a second time in a row — ruling 285). Either way that layout is
/// gone, and a person who expected it back is owed the reason, what it held
/// and what ran in it (day five, D18: `Tab restore skipped after a crash` over
/// a kill, naming nothing), and `Open log` on `log` — the crash log when
/// there is one, else `aterm.log`.
pub(crate) fn journal_note_message(
    note: &crate::crash_journal::Note,
    log: Option<&std::path::Path>,
) -> Message {
    use crate::crash_journal::Note;
    let mut msg = match note {
        Note::Unreadable {
            class, path, error, ..
        } => Message::new(
            journal_tag(*class),
            Severity::Warn,
            "Couldn't restore your last tabs",
        )
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(format!("its crash journal could not be read: {error}"))
        .line(class.sentence())
        .line(format!("journal {}", path.display())),
        Note::Relapsed {
            class,
            windows,
            tabs,
            lost,
            ..
        } => {
            let held = format!(
                "{} in {}",
                counted(*tabs, "tab"),
                counted(*windows, "window")
            );
            let (title, why) = if class.crashed() {
                (
                    "Couldn't restore tabs after a crash",
                    format!(
                        "{held} were not restored: aterm crashed within 90 s of restoring them, \
                         so they were skipped in case they caused it"
                    ),
                )
            } else {
                (
                    "Couldn't restore tabs after two stops",
                    format!(
                        "{held} were not restored: aterm was stopped twice in a row within 90 s \
                         of restoring them, so they were skipped in case they caused it"
                    ),
                )
            };
            let ran = match lost {
                None => JOURNAL_LOSS.to_string(),
                Some(lost) if lost.tabs == 0 => "nothing was running in them".to_string(),
                Some(lost) => journal_ran_line(lost, *tabs),
            };
            Message::new(journal_tag(*class), Severity::Warn, title)
                .glyph(Glyph::or_fallback('\u{26a0}'))
                .line(why)
                .line(ran)
                .line(class.sentence())
        }
    };
    if let Some(log) = log {
        let log = log.display().to_string();
        msg = msg
            .line(format!("its last lines are in {log}"))
            .action(Intent::OpenPath { path: log });
    }
    msg.no_excerpt()
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_CRASH_JOURNAL)
}

/// The supersede key of the row that says restored tabs' agents did not come
/// back ([`restored_agents_not_resumed`]): one per launch.
pub(crate) const KEY_RESTORED_AGENTS: &str = "harness.restored";

/// A restored tab's relaunch that was typed and whose agent started but did
/// not pick its conversation up (day six, D31): it ended as it started
/// (Claude's `No conversation found`), never registered it, or was still
/// coming up when the host stopped waiting.
fn started_without_conversation(outcome: &aterm_agent::harness::relaunch::Outcome) -> bool {
    use aterm_agent::harness::relaunch::Outcome;
    matches!(outcome, Outcome::NotYet(step) if step == "failed:no-resume" || step == "wait:resume")
}

/// Why a restored tab's agent did not come back, in a person's words — never
/// the relaunch's step word, which `aterm.log` keeps. An agent that started
/// and did not pick its conversation up is said as that, never as a tab that
/// was not ready (day six, D31: the tab was at its prompt, and Claude started
/// there and refused).
fn not_resumed_why(outcome: &aterm_agent::harness::relaunch::Outcome) -> &'static str {
    use aterm_agent::harness::relaunch::Outcome;
    match outcome {
        o if started_without_conversation(o) => {
            "Claude started but did not pick its conversation up"
        }
        Outcome::NotYet(step) if step.starts_with("failed:relaunch:") => {
            "its prompt refused the line that starts it"
        }
        Outcome::Cannot(why) if why == "shell-gone" => "its tab's shell had ended",
        Outcome::Left(why) if why == "not-exited" => {
            "the Claude that ran there before is still running, outside the tab"
        }
        Outcome::Left(why) if why == "no-conversation" => "its conversation could not be found",
        Outcome::NotYet(_) | Outcome::Busy => "its tab was not ready for it in time",
        _ => "it could not be started there",
    }
}

/// P6a, RULING 293 — THE AGENTS THAT DID NOT COME BACK. The reopened layout's
/// row ([`journal_reopened_message`]) told the person that aterm starts Claude
/// again on its conversation in these tabs; the relaunch then could not
/// bring it back in each `(place, outcome)` of `missed` (`place` as `in tab
/// 2`). Said ONCE, by the supervisor host once it has tried every restored
/// tab (`HostHandle::relaunch_restored`): a warning — the person resumes it by
/// hand — with `Open log` on `log`, where each step is. THE REMEDY IS IN THE
/// SENTENCE (day six, D32): `detail[0]`, the plain sentence Settings sets
/// above the technical block, says what happened and what to type; the
/// per-tab reasons and the log's path follow.
pub(crate) fn restored_agents_not_resumed(
    missed: &[(String, aterm_agent::harness::relaunch::Outcome)],
    log: Option<&std::path::Path>,
) -> Message {
    let (title, first) = match missed {
        [(place, outcome)] => (
            format!("Couldn't resume Claude {place}"),
            if started_without_conversation(outcome) {
                "Claude started in the tab but did not pick its conversation up after aterm \
                 stopped: type claude --resume there to pick it up again"
                    .to_string()
            } else {
                format!(
                    "{}, so its conversation did not come back after aterm stopped: type claude \
                     --resume there to pick it up again",
                    not_resumed_why(outcome)
                )
            },
        ),
        _ => (
            format!("Couldn't resume Claude in {} tabs", missed.len()),
            "their conversations did not come back after aterm stopped: type claude --resume in \
             each tab to pick one up again"
                .to_string(),
        ),
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Warn, title)
        .glyph(Glyph::or_fallback('\u{26a0}'))
        .line(first);
    if missed.len() > 1 {
        for (place, outcome) in missed {
            msg = msg.line(format!("{place}, {}", not_resumed_why(outcome)));
        }
    }
    if let Some(log) = log {
        let log = log.display().to_string();
        msg = msg
            .line(format!("what aterm tried is in {log}"))
            .action(Intent::OpenPath { path: log });
    }
    msg.no_excerpt()
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_RESTORED_AGENTS)
}

/// R3 — `aterm.toml` did not load at launch. `notice` is
/// `app_config::launch_config_notice`'s sentence (problem, path,
/// consequence): `aterm.toml could not be read at launch (…) — …` or
/// `aterm.toml is not a valid configuration (…) — …`. The title is terse
/// (ruling 146) and says the file is broken, the parser's own error's first
/// row (between ` (` and `) — `) is `detail[0]`, and the consequence and the
/// whole sentence follow behind Details — the sentence in its PHYSICAL rows
/// ([`diagnostic_lines`], fffef97a1).
///
/// The rows matter because the parenthesised error of an `Invalid` launch is
/// a `toml` parse error: a sentence, then a gutter, the source line and a
/// caret row. Handed to one `Message::line`, the engine's sanitizer DELETED
/// each `\n`, so the band, the Details page and the screen reader all read
/// `…column 11  |1 | font_px =   |           ^expected a value) — …` and the
/// 240-char cut landed mid-word in the consequence. That is the multi-line
/// fix (e7dc1feee) the retired banner made; [`config_lane_error`] kept it,
/// and this reads the same way — the caret rows open with whitespace, so the
/// Details page sets them in the monospace face with their columns intact.
pub(crate) fn launch_load_failure(notice: &str) -> Message {
    let title = if notice.starts_with("aterm.toml could not be read") {
        "Couldn't read your settings"
    } else {
        "Couldn't load your settings"
    };
    let rows = notice
        .split_once(" (")
        .and_then(|(_, rest)| rest.rsplit_once(") \u{2014} "))
        .map(|(error, _)| diagnostic_lines(error))
        .unwrap_or_default();
    // An invalid file's excerpt is the parser's finding in a person's words
    // (ruling 306: `expected \`=\`` reached the glass); an unreadable one's
    // is the system's own few words (`permission denied`).
    let error = if notice.starts_with("aterm.toml is not a valid configuration") {
        toml_words(&rows)
    } else {
        rows.into_iter().next().filter(|row| !row.is_empty())
    };
    let mut msg = Message::new(tags::CONFIG, Severity::Error, title);
    let painted = error.is_some();
    if let Some(error) = error {
        msg = msg.line(error);
    }
    // The notice's own tail says the consequence (`— every setting is
    // running at its default`): no line of its own before it.
    let msg = msg
        .lines(diagnostic_lines(notice))
        .action(Intent::OpenConfigEditor { line: None })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_LAUNCH_LOAD);
    if painted { msg } else { msg.no_excerpt() }
}

/// R5 — a key the CPU renderer cannot honour (`background_opacity`,
/// `background_material`): a DISCLOSURE, so a RECORD — the Manual already
/// marks the key inert while the CPU renderer is active — pointing at the
/// Appearance page where the renderer is chosen.
pub(crate) fn cpu_renderer_no_effect(key: &str, rest: &str) -> Message {
    // The setting in a person's words (ruling 261); the key and why follow.
    let name = match key {
        "background_opacity" => "Transparency",
        "background_material" => "Window material",
        _ => "This setting",
    };
    Message::new(
        tags::RENDER,
        Severity::Info,
        format!("{name} needs the GPU renderer"),
    )
    .line(format!("{key} has no effect on the CPU renderer"))
    .line(rest)
    .action(Intent::OpenSettings {
        route: SettingsRoute::Appearance.path().to_string(),
    })
    .hold(Hold::LogOnly)
    .key(format!("render.cpu.{key}").as_str())
}

/// R6 — the Windows client-area backdrop was declined (DirectComposition
/// unavailable, `hdr_glow` on, no GPU, not requested at launch): a RECORD for
/// the family — a blank window is the GPU-lost row's, not this one's.
#[cfg(any(windows, test))]
pub(crate) fn backdrop_declined(title: &str, detail: &str) -> Message {
    // One title in the failure grammar (ruling 261); the site's words follow.
    Message::new(tags::RENDER, Severity::Info, "Couldn't show the backdrop")
        .line(title)
        .line(detail)
        .action(Intent::OpenConfigEditor { line: None })
        .hold(Hold::LogOnly)
        .key(KEY_BACKDROP)
}

/// R7 — the GPU was lost and the windows created for the backdrop cannot be
/// redrawn by the CPU fallback. The remedy is a gesture (`New window`), so
/// the row STANDS until a window created after the loss paints, or until it
/// is read; a timer must not fold the one instruction that recovers the
/// screen. The `New window` capsule IS that instruction, so the title and
/// the capsule are the row; why and the alternative ride behind Details
/// (review 2026-09-23).
#[cfg(any(windows, test))]
pub(crate) fn gpu_lost() -> Message {
    Message::new(tags::RENDER, Severity::Error, "GPU lost")
        .line("windows opened for the backdrop cannot redraw")
        .line("open a new window or restart aterm to get a visible one back")
        .no_excerpt()
        .action(Intent::NewWindow)
        .hold(Hold::Standing)
        .key(KEY_GPU_LOST)
}

/// R8 — the accessibility publisher's thread died. A process gets one
/// publisher, so nothing short of a new process brings it back; the retry is
/// `detail[0]`, alone on its line (the sanctioned anchor), the reason and the
/// consequence behind it. On the glass (Standing) only when assistive
/// technology was in use — `at_in_use`: an AT client had attached to this
/// process's tree ([`crate::a11y_backend`]'s latch) — and a RECORD
/// otherwise: for everyone not using a screen reader the row was an FYI
/// about a feature they do not use (review round 2, 2026-09-23).
/// Settings ▸ Messages and `messages.log` keep it either way.
#[cfg(any(a11y_tree, test))]
pub(crate) fn a11y_publisher_dead(reason: &str, at_in_use: bool) -> Message {
    Message::new(tags::A11Y, Severity::Error, "Screen reader access lost")
        .line("restart aterm to retry")
        .line(reason)
        .hold(if at_in_use {
            Hold::Standing
        } else {
            Hold::LogOnly
        })
        .key(KEY_A11Y_PUBLISHER)
}

/// R9 — a presence toggle's durable write did not land: the person's toggle
/// reverted (`label not saved`, an error) or may not have persisted
/// (`label may not be saved`, a warning). `detail` is the cause. Keyed by the
/// config leaf, so a second failure on the same toggle replaces the first.
pub(crate) fn presence_not_saved(
    leaf: &str,
    label: &str,
    detail: &str,
    indeterminate: bool,
) -> Message {
    let (severity, title) = if indeterminate {
        (
            Severity::Warn,
            format!("Couldn't confirm {label} was saved"),
        )
    } else {
        (Severity::Error, format!("Couldn't save {label}"))
    };
    gesture_failure(tags::FABRIC, severity, &title, detail)
        .key(format!("fabric.presence-save.{leaf}").as_str())
}

/// R10 — the person's Hold press met a standing FLEET hold (`HOLD_DENIED`,
/// which the bridge lifts, not this window). Pressing Hold (`on`) asked for
/// what already is — the session is held — so that is a RECORD (`Session
/// already held`); pressing Release failed, a gesture failure the person sees
/// (`Hold not lifted`). Titled with the outcome, not a state (review
/// 2026-09-24).
pub(crate) fn fleet_hold(on: bool) -> Message {
    const WHY: &str = "the fleet holds it";
    if on {
        Message::new(tags::FABRIC, Severity::Info, "Session already held")
            .line(WHY)
            .hold(Hold::LogOnly)
            .key(KEY_FABRIC_HOLD)
    } else {
        gesture_failure(
            tags::FABRIC,
            Severity::Warn,
            "Couldn't release the hold",
            WHY,
        )
        .key(KEY_FABRIC_HOLD)
    }
}

/// R10 — the bridge refused the person's Hold press (`ERR …`): the reason is
/// `detail`.
pub(crate) fn hold_refused(detail: &str) -> Message {
    gesture_failure(
        tags::FABRIC,
        Severity::Warn,
        "Couldn't hold the session",
        detail,
    )
    .key(KEY_FABRIC_HOLD)
}

/// R11 — a fabric menu item that could not do its thing (a document that
/// would not open, a thread that would not spawn): a terse title, the cause
/// and the path behind it, keyed by the verb.
pub(crate) fn fabric_failure(verb: &str, title: &str, lines: &[&str]) -> Message {
    let mut msg = gesture_failure(tags::FABRIC, Severity::Error, title, "");
    for line in lines {
        msg = msg.line(*line);
    }
    msg.key(format!("fabric.{verb}").as_str())
}

/// R11 — `aterm fabric …` ended. A clean exit is a RECORD (the output tab
/// opens anyway, and says the rest); anything else is the person's command
/// failing — `title` (`Fabric On failed`) alone on the glass, the reason
/// (`exited 2`) behind Details: an exit code is nothing the person can act
/// on, and the output tab that opens is the pointer (review 2026-09-24).
pub(crate) fn fabric_status(verb: &str, title: &str, detail: &str, ok: bool) -> Message {
    let key = format!("fabric.{verb}");
    if ok {
        Message::new(tags::FABRIC, Severity::Success, title)
            .line(detail)
            .hold(Hold::LogOnly)
            .key(&key)
    } else {
        gesture_failure(tags::FABRIC, Severity::Error, title, detail)
            .no_excerpt()
            .key(&key)
    }
}

/// R12 — the Serious Mode write's feedback: the completion's sentence (`Serious
/// Mode was not changed: …`, `Serious Mode may have been written but could not
/// be verified; reload before retrying: …`, `Serious Mode was saved, but …`)
/// as a terse title and the cause behind it — the cause, or the instruction
/// to reload first, is `detail[0]`.
pub(crate) fn serious_mode_feedback(words: &str) -> Message {
    let words = words.trim_end_matches('.');
    let mut overridden = false;
    let (title, rest) = if let Some(rest) = words.strip_prefix("Serious Mode was not changed") {
        ("Couldn't change Serious Mode", rest)
    } else if let Some(rest) = words.strip_prefix("Serious Mode was saved but could not be applied")
    {
        ("Couldn't apply Serious Mode", rest)
    } else if let Some(rest) =
        words.strip_prefix("Serious Mode was saved, but a newer aterm.toml edit now controls it")
    {
        overridden = true;
        ("Couldn't apply Serious Mode", rest)
    } else {
        (
            "Couldn't confirm Serious Mode",
            words.strip_prefix("Serious Mode").unwrap_or(words),
        )
    };
    let rest = rest
        .trim_start_matches(':')
        .trim_start_matches(" because")
        .trim();
    let mut lines: Vec<String> = Vec::new();
    if let Some((head, tail)) = rest.split_once("; reload before retrying") {
        lines.push("reload before retrying".to_string());
        let head = head.trim();
        if !head.is_empty() {
            lines.push(head.to_string());
        }
        let tail = tail.trim_start_matches(':').trim();
        if !tail.is_empty() {
            lines.push(tail.to_string());
        }
    } else {
        lines.extend(
            rest.split("; ")
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    if overridden {
        // What overrode it, first — the capsule is `Open aterm.toml` (review
        // 2026-09-24: the bare title did not say).
        lines.insert(0, "aterm.toml sets it".to_string());
    }
    let mut msg = gesture_failure(tags::CONFIG, Severity::Warn, title, "");
    for line in lines {
        msg = msg.line(line);
    }
    msg.action(Intent::OpenConfigEditor { line: None })
        .key(KEY_SERIOUS_MODE)
}

/// The native config lane's heads, as terse titles (design §10.3, C22's
/// map), and whether the head is the person's own gesture failing (Robi's
/// dismissal). An unknown head is a failed settings change, its whole first
/// row the excerpt.
fn lane_title(head: &str) -> (&'static str, bool, bool) {
    let known = |title, gesture| (title, gesture, true);
    if head.starts_with("Config observation was not valid TOML") {
        known("Couldn't read your settings", false)
    } else if head.starts_with("Robi was not dismissed") {
        known("Couldn't dismiss Robi", true)
    } else if head.starts_with("Config reconciliation failed") {
        known("Couldn't save settings changes", false)
    } else if head.starts_with("Manual saved aterm.toml, but")
        || head.starts_with("Config was saved, but its exact disk generation")
    {
        known("Couldn't apply saved settings", false)
    } else if head.starts_with("Config publication could not be verified") {
        known("Couldn't confirm settings saved", false)
    } else if head.starts_with("Config was NOT saved") {
        known("Couldn't save a settings change", false)
    } else {
        ("Couldn't change a setting", false, false)
    }
}

/// R13 — the native config lane's own error (`Config observation was not
/// valid TOML`, a pump that failed, a Robi dismissal the lane refused): a
/// terse title by its head ([`lane_title`]), the cause's first physical row
/// as `detail[0]`, and every later row — a `toml` parse error's caret
/// diagram — behind Details.
///
/// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee, ported onto
/// the message model on the origin/main merge, 2026-09-23): the words are
/// split ONCE into their physical rows ([`diagnostic_lines`]), so the band's
/// excerpt, the Settings ▸ Messages entry and the screen reader's rows are
/// the same rows by construction, and the whole diagnostic stays ONE message.
pub(crate) fn config_lane_error(words: &str) -> Message {
    let mut rows = diagnostic_lines(words).into_iter();
    let first = rows.next().unwrap_or_default();
    let (head, cause) = split_sentence(&first);
    let (title, gesture, known) = lane_title(head);
    // What the band paints (ruling 306): the parser's finding in a person's
    // words for an invalid file, the cause for a known head — and nothing for
    // a head this build does not know, whose row (`Something new went wrong:
    // …`) is someone else's words, whole behind Details.
    let rows: Vec<String> = rows.collect();
    let toml = head
        .starts_with("Config observation was not valid TOML")
        .then(|| {
            let mut all = vec![cause.to_string()];
            all.extend(rows.iter().cloned());
            toml_words(&all)
        });
    let painted = match &toml {
        Some(words) => words.is_some(),
        None => known,
    };
    let excerpt = if known { cause } else { first.as_str() };
    // What a failed reconciliation LOST is what the person does next about
    // (fffef97a1, ruling 146): `2 queued changes were not written` leads, the
    // cause behind it. The head says it only when requests were discarded.
    let lost = head
        .strip_prefix("Config reconciliation failed; ")
        .filter(|lost| !lost.is_empty());
    let msg = Message::new(tags::CONFIG, Severity::Warn, title);
    let msg = match lost {
        Some(lost) => msg.line(lost),
        None => msg,
    };
    let msg = match toml.flatten() {
        Some(words) => msg.line(words),
        None => msg,
    };
    let msg = rows.into_iter().fold(msg.line(excerpt), Message::line);
    let msg = msg
        .action(Intent::OpenConfigEditor { line: None })
        .key(KEY_CONFIG_LANE);
    let msg = if painted { msg } else { msg.no_excerpt() };
    if gesture {
        msg.hold(Hold::For(HOLD_GESTURE))
    } else {
        msg
    }
}

/// R14 — ONE SHAPE FOR "THE PERSON'S OWN GESTURE FAILED" (H11): a failure of
/// their own action earns the glass for a SHORT hold (`HOLD_GESTURE`, 12 s)
/// and nothing more — a title of a few words, the FULL error behind Details
/// (no 160-character cut: the log answers the investigator). Unkeyed unless
/// the caller keys it: a second failed press is a second row's worth of news,
/// folded as a duplicate when the words are the same.
fn gesture_failure(tag: Tag, severity: Severity, title: &str, detail: &str) -> Message {
    // `sentence`, never `line`: the error is someone else's words, kept WHOLE
    // across detail lines (design ruling 64), not clipped at the line cap.
    Message::new(tag, severity, title)
        .sentence(detail)
        .hold(Hold::For(HOLD_GESTURE))
}

/// R14 — `New Tab` did not open one (the spawn failed).
pub(crate) fn new_tab_failed(error: &str) -> Message {
    gesture_failure(
        tags::WINDOW,
        Severity::Error,
        "Couldn't open a new tab",
        error,
    )
    .no_excerpt()
}

/// R14 — `New Window` did not open one (the spawn failed).
pub(crate) fn new_window_failed(error: &str) -> Message {
    gesture_failure(
        tags::WINDOW,
        Severity::Error,
        "Couldn't open a new window",
        error,
    )
    .no_excerpt()
}

/// R14 — a split the pane cannot hold was refused BEFORE anything spawned.
/// `reason` is the refusal's sentence (`Split refused: this pane is 20x5
/// cells; a vertical split needs at least 20x7`): the two sizes are what the
/// person acts on, so `detail[0]` is `pane 20×5, needs 20×7` and the sentence
/// rides whole behind it (review round 2, 2026-09-23: the excerpt read as a
/// sentence cut mid-thought). A refusal in another shape is its own excerpt.
pub(crate) fn split_refused(reason: &str) -> Message {
    let (_, why) = split_sentence(reason);
    match split_sizes(why) {
        Some(sizes) => {
            gesture_failure(tags::WINDOW, Severity::Error, SPLIT_FAILED, &sizes).line(why)
        }
        None => gesture_failure(tags::WINDOW, Severity::Error, SPLIT_FAILED, why),
    }
}

/// `pane 20×5, needs 20×7` from `this pane is 20x5 cells; a vertical split
/// needs at least 20x7`, or `None` when the sentence is not that shape.
fn split_sizes(why: &str) -> Option<String> {
    let (have, rest) = why.strip_prefix("this pane is ")?.split_once(" cells; ")?;
    let (_, need) = rest.rsplit_once("needs at least ")?;
    let size = |s: &str| {
        let (c, r) = s.trim().split_once('x')?;
        let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
        (digits(c) && digits(r)).then(|| format!("{c}\u{00d7}{r}"))
    };
    Some(format!("pane {}, needs {}", size(have)?, size(need)?))
}

/// R14 — a split that fit could not spawn its pane.
pub(crate) fn split_failed(error: &str) -> Message {
    gesture_failure(tags::WINDOW, Severity::Error, SPLIT_FAILED, error).no_excerpt()
}

/// A split refused or failed: one title in the failure grammar (ruling 261).
const SPLIT_FAILED: &str = "Couldn't split the pane";

/// R14 — keystrokes typed while a seamless update finished overflowed the
/// pre-Commit queue and were dropped: the person typed them, so they hear it
/// — and `detail[0]` says what to do, as an instruction, not a clause.
#[cfg(any(unix, test))]
pub(crate) fn keystrokes_dropped(dropped: u32) -> Message {
    let what = if dropped == 1 {
        "retype the last key".to_string()
    } else {
        format!("retype the last {dropped} keys")
    };
    gesture_failure(tags::SESSION, Severity::Error, "Keystrokes dropped", &what)
}

/// R14 — the person's own key or paste did not go: the session's input
/// queue was full (the program is not reading) or its writer was gone
/// (ruling 270; `App::refuse_input`). A gesture that failed, so the one
/// gesture-failure shape — Error, `HOLD_GESTURE` — and a title in the failure
/// grammar; `detail[0]` says why in a few words, what to do rides whole
/// behind it. The host keys it per session, so a burst of refused keys is
/// one row. It names no `Stop paste`: that capsule is the paste row's.
pub(crate) fn input_refused(cause: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't send your input",
        cause,
    )
}

/// R14 — a session restore could not create a window and stopped there. The
/// line restates the title, so it rides behind Details.
pub(crate) fn restore_stopped_early() -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't restore every tab",
        "some saved tabs were not restored",
    )
    .no_excerpt()
}

/// R14 — a shell handed across a seamless update could not be adopted.
pub(crate) fn shell_lost_in_update(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Shell lost in the update",
        error,
    )
    .no_excerpt()
}

/// R14 — a restored tab could not start its shell.
pub(crate) fn restored_tab_failed(error: &str) -> Message {
    gesture_failure(
        tags::SESSION,
        Severity::Error,
        "Couldn't restore a tab",
        &format!("shell did not start: {error}"),
    )
    .no_excerpt()
}

/// The supersede key of the row that says shells started in the home folder
/// because their folder was not there ([`folder_row`]): one row, however many
/// folders.
pub(crate) const KEY_FOLDER_NOT_FOUND: &str = "spawn.folder-not-found";

/// The supersede key of the row that says shells started in the home folder
/// because their folder could not be entered ([`folder_row`]).
pub(crate) const KEY_FOLDER_NOT_OPENED: &str = "spawn.folder-not-opened";

/// The most folders [`folder_row`] names one per line; the rest are counted
/// on the line after them.
const FOLDERS_NAMED: usize = DETAIL_LINES_CAP - 2;

/// The one-folder row's line, before the folder.
const FOLDER_INSTEAD_OF: &str = "opened in your home folder instead of ";

/// The many-folder row's first line, before the folders.
const FOLDERS_INSTEAD: &str = "opened in your home folder instead";

/// The supersede key of `fault`'s row.
pub(crate) const fn folder_row_key(fault: crate::spawn_folder::Fault) -> &'static str {
    match fault {
        crate::spawn_folder::Fault::Missing => KEY_FOLDER_NOT_FOUND,
        crate::spawn_folder::Fault::Shut => KEY_FOLDER_NOT_OPENED,
    }
}

/// Fresh shells started in the home folder because the folder each was asked
/// to start in was not there, or could not be entered — a restored pane's
/// saved folder, the one a New Tab inherited, a profile's (audit #7 finding
/// 48; [`crate::spawn_folder`]). ONE row per fault for every such folder while
/// it is up (`App::fold_folder_faults`, keyed [`folder_row_key`]). A failure
/// in the one grammar (`Couldn't find the folder`, `Couldn't open 3 folders`):
/// the shell is not where the person asked. `detail[0]` is where it is
/// instead, naming the one folder; several are named one per line behind it,
/// and past the line cap the rest, with any a carried row counted without
/// naming ([`crate::spawn_folder::Named::unnamed`]), are counted.
pub(crate) fn folder_row(
    fault: crate::spawn_folder::Fault,
    named: &crate::spawn_folder::Named,
) -> Message {
    debug_assert!(!named.dirs.is_empty(), "a row about no folder");
    let verb = match fault {
        crate::spawn_folder::Fault::Missing => "find",
        crate::spawn_folder::Fault::Shut => "open",
    };
    let total = named.dirs.len() + named.unnamed;
    let msg = match named.dirs.as_slice() {
        [one] if total == 1 => Message::new(
            tags::WINDOW,
            Severity::Warn,
            format!("Couldn't {verb} the folder"),
        )
        .sentence(format!("{FOLDER_INSTEAD_OF}{one}")),
        many => {
            let unnamed = total.saturating_sub(many.len().min(FOLDERS_NAMED));
            let msg = Message::new(
                tags::WINDOW,
                Severity::Warn,
                format!("Couldn't {verb} {total} folders"),
            )
            .line(FOLDERS_INSTEAD)
            .lines(many.iter().take(FOLDERS_NAMED).cloned());
            if unnamed == 0 {
                msg
            } else {
                msg.line(format!("and {unnamed} more"))
            }
        }
    };
    msg.key(folder_row_key(fault))
}

/// The folders a [`folder_row`] names, read back from the row: how a row
/// carried across an update keeps its folders when the next one joins it
/// (the new instance's list starts empty). `None` for a row of another shape.
pub(crate) fn folder_row_named(msg: &Message) -> Option<crate::spawn_folder::Named> {
    if msg.detail.first().map(String::as_str) == Some(FOLDERS_INSTEAD) {
        let rest = &msg.detail[1..];
        let (dirs, unnamed) = match rest.split_last() {
            Some((count, dirs)) if rest.len() == FOLDERS_NAMED + 1 => (
                dirs,
                count
                    .strip_prefix("and ")
                    .and_then(|n| n.strip_suffix(" more"))
                    .and_then(|n| n.parse().ok())?,
            ),
            _ => (rest, 0),
        };
        return Some(crate::spawn_folder::Named {
            dirs: dirs.to_vec(),
            unnamed,
        });
    }
    // The one-folder form is one sentence, split across lines only when it
    // is longer than a line: at the space before the folder and then hard
    // when the folder has no space in it, else at its spaces.
    let (head, rest) = msg.detail.split_first()?;
    let one = if head == FOLDER_INSTEAD_OF.trim_end() && !rest.is_empty() {
        rest.concat()
    } else {
        let first = head.strip_prefix(FOLDER_INSTEAD_OF)?;
        std::iter::once(first)
            .chain(rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some(crate::spawn_folder::Named {
        dirs: vec![one],
        unnamed: 0,
    })
}

/// The Open-log press refused (the path was not a regular file under the log
/// dir, or nothing could be spawned to open it): the person's gesture, so a
/// short row naming the path behind Details.
pub(crate) fn log_did_not_open(path: &str) -> Message {
    gesture_failure(tags::SYSTEM, Severity::Error, "Couldn't open the log", path).no_excerpt()
}

/// R15 — THE FULL DISK ACCESS QUESTION (`consent_card`'s decision; the row
/// the card posts once and watches by id). A DECISION row: it ranks first
/// and holds for `HOLD_ASK` (ten minutes) until a capsule answers it. Info,
/// not Warn — a quiet question, never an alarm (the copy fence forbids alarm
/// words). The title is the ask ([`crate::consent_card::FDA_TITLE`]) and the
/// row paints it alone: [`crate::consent_card::FDA_DETAIL`]'s hedge, the
/// route in words and what aterm does next wait behind Details (review round
/// 2, 2026-09-23: a status title with a hedge for its excerpt never said
/// what it asked).
pub(crate) fn file_access_question() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Info,
        crate::consent_card::FDA_TITLE,
    )
    .line(crate::consent_card::FDA_DETAIL)
    .line(crate::menu::privacy_settings_path_words(
        crate::menu::PrivacyPane::FullDiskAccess,
    ))
    .line("aterm notices when it is on")
    .no_excerpt()
    .action(Intent::OpenSystemPane {
        pane: PANE_FULL_DISK_ACCESS.to_string(),
    })
    .action(Intent::NotNow {
        decision: Decision::FileAccess,
    })
    .hold(Hold::Ask { for_: HOLD_ASK })
    .key(KEY_FILE_ACCESS)
    // What the log says once it is answered or folds (ruling 259): the
    // question was ASKED — the grant or the decline is its own record.
    .finished_as(ASKED_FILE_ACCESS)
}

/// The Full Disk Access question's log title once it leaves the glass.
pub(crate) const ASKED_FILE_ACCESS: &str = "Asked for Full Disk Access";

/// R17 — the probe observed the grant. A confirmation, so a RECORD
/// (Settings ▸ Messages and `messages.log`), never a row: the question's row
/// leaving the glass is the visible half, resolved by key before this is
/// recorded (`App::show_macos_access_granted`). It names the fact and stops
/// ([`crate::consent_card::GRANTED_CAPTION`]).
pub(crate) fn file_access_granted() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Success,
        crate::consent_card::GRANTED_CAPTION,
    )
    .hold(Hold::LogOnly)
    .key(KEY_FILE_ACCESS)
}

/// A HARNESS NOTE (`aterm ctl appnotice harness <text>`: an agent harness's acts,
/// posted from outside the window — nothing in aterm posts one since the second
/// harness stack and its `APPNOTICE_LANE` were deleted, 2026-09-23): a
/// `harness`-tagged RECORD, never a
/// row — design ruling 39, implemented (ruling 61). The words are the wire's,
/// sanitized and capped like any appnotice text.
pub(crate) fn harness_note(text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Info,
        atpkg::progress::sanitize_for_tty(text, 120),
    )
    .hold(Hold::LogOnly)
}

/// A SESSION SUPERVISOR'S INFORMATION (`aterm_agent::supervise::IdleHost::
/// inform`, from the window's own supervise loop for session `sid`): a
/// `harness`-tagged RECORD, never a row — nothing is asked of the person.
/// Codex's save-then-wait hold begins with one (2026-09-28: a hold can last
/// days, the session idle through it). Keyed per session: the next one
/// replaces it.
pub(crate) fn supervisor_note(sid: &str, text: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Info,
        atpkg::progress::sanitize_for_tty(&format!("@{sid}: {text}"), 200),
    )
    .hold(Hold::LogOnly)
    .key(format!("harness.supervisor.{sid}").as_str())
}

/// The live agent upgrade's supersede key: each new waiting record replaces
/// the last one recorded under it.
pub(crate) const KEY_AGENT_UPGRADE: &str = "harness.upgrade";

/// THE LIVE AGENT UPGRADE, WAITING (gap audit 2026-09-24: two sessions sat one
/// and two releases behind for 8h22m and nothing said so). A RECORD, never a
/// row — waiting for its moment is the upgrade working, and the band carries
/// only what the person acts on (ruling e83d7d233) — so it is on Settings ▸
/// Messages, `messages.log` and `appstatus` as `kind=harness`. `product` is
/// the agent the waiting sessions run (`Claude Code`, `Codex`; `Claude Code
/// and Codex` for a mix, named in the detail under the generic title),
/// `waiting` the versions they move to, one per session; `None` when none
/// waits. ONE session's record is [`agent_upgrade_waiting_in`]'s.
///
/// IT SAYS THERE IS NOTHING TO DO (the owner, 2026-09-28: "do I need to do
/// something? It's not clear. I want upgrades to be applied automatically"),
/// and never "at its next turn end" — false of a goal-mode Codex, whose next
/// turn begins within 14 ms of the last: the upgrade starts at a quiet
/// moment, the longer it waits the less it asks of one (the ladder).
pub(crate) fn agent_upgrade_waiting(product: &str, waiting: &[&str]) -> Option<Message> {
    let first = *waiting.first()?;
    let n = waiting.len();
    // A MIX of products (`Claude Code and Codex`) is named in the detail:
    // the title keeps to the six-word rule with the generic word.
    let (titled, mixed) = if product.contains(" and ") {
        ("agent", Some(product))
    } else {
        (product, None)
    };
    let title = if waiting.iter().all(|v| *v == first) {
        let target = format!("{titled} {}", atpkg::progress::sanitize_for_tty(first, 24));
        if n == 1 {
            format!("{target} installs itself")
        } else {
            format!("{target} installs itself in {n} tabs")
        }
    } else {
        format!("Newer {titled} builds install themselves")
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Info, title);
    if let Some(both) = mixed {
        msg = msg.line(format!("{both} sessions"));
    }
    msg = msg.line(if n == 1 {
        "nothing to do: it starts on its own at a quiet moment in its tab"
    } else {
        "nothing to do: each starts on its own at a quiet moment in its tab"
    });
    Some(msg.no_excerpt().hold(Hold::LogOnly).key(KEY_AGENT_UPGRADE))
}

/// ONE SESSION'S UPGRADE, WAITING (the owner's decision of 2026-09-28): a
/// RECORD titled by its tab — `Codex 0.158.0 installs itself in tab 1` —
/// whose sentence is WORDED BY WHAT HOLDS IT
/// ([`aterm_agent::harness::upgrade_drive::Row::waiting_line`]): where only
/// the ladder's comfort does, that there is nothing to do and when the
/// ladder stops waiting for it — `nothing to do: it starts on its own at a
/// quiet moment in the tab, and from 2:15 PM as soon as it is idle and nobody
/// has typed there for 20 seconds` (its Land rung,
/// [`aterm_agent::harness::upgrade_drive::Row::lands_by`], on the person's
/// clock; `from two hours behind` where the local clock's offset is
/// unknown); where a floor stands — a draft, a dialog, a goal or a turn of
/// this tab's in its Codex daemon, a turn in another session there, the
/// shell integration — what it is and when it moves, never the pause in the
/// person's typing (the review of 2026-09-28: it promised that of a goal
/// that held the tab). A clock time, not a countdown: said once, it stays
/// true. Then the tab and the move. No tab mark and no warning: the upgrade
/// is working (rulings 275 and 283, amended by ruling 380). `place` names the
/// tab (`in tab 1`); the title drops it where it would break the title rule,
/// and the move line keeps it.
pub(crate) fn agent_upgrade_waiting_in(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let head = format!("{} {} installs itself", row.agent.product(), clean(&row.to));
    let placed = format!("{head} {place}");
    let title = if aterm_messages::text::glass_title_fault(&placed).is_none() {
        placed
    } else {
        head
    };
    // The Land rung's own time, past or to come: the words stay what they
    // were, so the record is written once (a time that followed the clock
    // past it would be a new record at every look). What holds it may change
    // from look to look; the host says each sentence once, and keeps a
    // floor's over the ladder's comfort (`App::record_upgrade_waiting`).
    let from = row
        .lands_by()
        .and_then(|at| {
            let offset = crate::presence::local_offset_s()?;
            Some(aterm_messages::words::clock_words(
                at.saturating_mul(1000),
                offset,
            ))
        })
        .unwrap_or_else(|| "two hours behind".to_string());
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(row.waiting_line(&from))
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .no_excerpt()
        .hold(Hold::LogOnly)
        .key(KEY_AGENT_UPGRADE)
}

/// THE LIVE AGENT UPGRADE, STALLED: a ROW, because only the person can move it
/// — it was refused, stopped the same way round after round, runs where
/// typing into the tab cannot reach, waits on what no rung of the ladder
/// passes (`blocked:*`), or has stood four hours on the ladder's last rung
/// with no pause (`overdue`, six hours behind:
/// [`aterm_agent::harness::upgrade_drive::Row::stall`]). One that will ask
/// again on its own — still asking, or resting after a round that gave up or
/// stopped once — is a RECORD with no tab mark
/// ([`aterm_agent::harness::upgrade_drive::Row::asks_on_its_own`], rulings
/// 275 and 283), and names the clock time its next round starts, so its
/// words never need restating.
/// `Standing`: it stays until the stall ends (the host resolves it) or the
/// person reads it. `detail[0]` is why; then what moves it; the tab and the
/// move ([`STALL_MOVE_LINE`]); what runs under the agent and holds it, when
/// anything does (`held by pid …`: pid, name and age, never a command); and
/// the same words spelled for a shell, always the last line (ruling 270) —
/// any shell, never the stalled tab's own (its foreground is Claude, and a
/// line typed there is a prompt). Keyed per tab ([`agent_upgrade_key`]).
///
/// THE REMEDY IS THE ONE THAT WORKS FOR THIS KIND OF STALL (review of
/// 2026-09-25: it named `--now` for every kind, and `--now` moves only two).
/// Overdue: it has stood on the ladder's last rung for hours, which `--now`
/// is too (ruling 380), so what it waits on is named — a turn running (a
/// goal that never pauses), the READY answer, a draft, a box, a hold, work
/// under the agent — and `--now` is offered only where it still moves it
/// (its own text left typed, a full queue). Blocked (`blocked:*`): no rung
/// passes it — quit it in its tab and resume it there. Gave up: `--now` asks it
/// again (at once, where its own next round is at a clock time). Held back in
/// a pane: typing into the tab cannot reach it, so no word moves it — quit it
/// in its pane and resume it there, or `--skip`. Refused or failed: the
/// harness asks it again after a rest (at a clock time, once known) — to move
/// it sooner, quit and resume it by hand, or `--skip`; stopped after the
/// agent it ended was seen gone, nothing runs to quit — `codex resume`, or
/// `claude --resume <conversation>`, in the tab takes it back. Under way and
/// not moving (`stuck:*`, 2026-09-27): no word moves it and nothing is
/// forced — the row offers none and names `--status`. (None says "restart":
/// the reporters' restart guard,
/// `no_reporter_wording_prompts_a_restart`, reads every line.)
pub(crate) fn agent_upgrade_stalled(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
    place: &str,
) -> Message {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    // A CODEX GOAL THE MOVE PAUSED AND HAS NOT RESUMED (the owner's decision
    // of 2026-09-28: aterm pauses a goal only for a moment, and never leaves
    // it paused): a row of its own, whatever became of the move, with the one
    // hand step. No word of the upgrade's moves it. Worded as the wait it is
    // — aterm still resumes it where it can — never as a failure, and never
    // raised while aterm itself holds it as it should (`Row::with_goal`: a
    // switch open, the paused goal's last turn running; the goal-pause review
    // of 2026-09-28). One LEFT PAUSED ON PURPOSE — its thread fell into a
    // sandbox — names the relaunch that brings it out.
    let goal_row = row.stall(now);
    if matches!(goal_row.as_deref(), Some("goal-paused" | "goal-sandboxed")) {
        let tab = clean(&row.tab);
        let (title, why, step) = if goal_row.as_deref() == Some("goal-sandboxed") {
            (
                "Codex goal left paused",
                "aterm paused this tab's Codex goal to install the new Codex, and leaves it \
                 paused: its thread fell into a sandbox, where it can neither commit nor push",
                "quit Codex in the tab, resume the thread with its launch flags (codex resume \
                 --dangerously-bypass-approvals-and-sandbox <thread>), then type /goal resume",
            )
        } else {
            (
                "Codex goal still paused",
                "aterm paused this tab's Codex goal for a moment to install the new Codex, and \
                 has not been able to resume it yet",
                "type /goal resume in the tab to go on with the goal now",
            )
        };
        return Message::new(
            tags::HARNESS,
            Severity::Warn,
            placed_title(title, place, ""),
        )
        .line(why)
        .line(step)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .line(String::new())
        .line(format!(
            "the same in any shell: `aterm harness upgrade {tab} --status`"
        ))
        .hold(Hold::Standing)
        .key(&agent_upgrade_key(&row.tab));
    }
    // The band's excerpt: a person's words, no code quoting (ruling 270;
    // `` `/exit` ended it `` read as a command to run).
    let why = row
        .stall_words(now)
        .unwrap_or_else(|| "it is not moving".to_string())
        .replace('`', "");
    let tab = clean(&row.tab);
    let from = clean(&row.from);
    let cmd = format!("aterm harness upgrade {tab}");
    // THE REMEDY NAMES THE ROW'S OWN BUTTONS (ruling 270): since gap #21 the
    // row carries the words that move it, so the shell spelling of the same
    // words is the last, technical line — never the first way offered.
    // ONLY A BUTTON THE ENTRY HAS (day five, D21: `Skip version keeps it` on
    // an entry whose capsules were `Upgrade now` and `Not today`): the row's
    // own capsule by its label, else the tab menu's item by the menu's own
    // label (`Skip This Version`), which offers every word.
    let offered = agent_upgrade_words(row, now);
    let skip = if agent_upgrade_capsules(row, now).iter().any(|intent| {
        matches!(
            intent,
            Intent::AgentUpgrade {
                word: UpgradeWord::Skip,
                ..
            }
        )
    }) {
        format!("{} keeps it on {from}", UpgradeWord::Skip.label())
    } else if offered.contains(&UpgradeWord::Skip) {
        format!(
            "{} in the tab's menu keeps it on {from}",
            crate::session_chrome::upgrade_menu_label(UpgradeWord::Skip)
        )
    } else {
        format!("`{cmd} --skip` keeps it on {from}")
    };
    let now_word = UpgradeWord::Now.label();
    let who = agent_word(row.agent);
    let under_way = matches!(
        row.phase,
        aterm_agent::harness::upgrade::Phase::Exiting { .. }
            | aterm_agent::harness::upgrade::Phase::Relaunched { .. }
    );
    let (remedy, shell_words) = match row.remedy(now) {
        // A move under way that has not moved (S2 of the in-flight review,
        // 2026-09-27): the harness refuses every word while it is under way,
        // and forces nothing — the row offers no word, and names the shell's
        // view of it.
        Some(Remedy::Waits) if under_way => (
            "no word moves it while it is under way: it goes on once what it waits on ends, \
             and nothing is forced"
                .to_string(),
            "--status",
        ),
        // ITS QUESTION WAITS UNREAD BEHIND A FULL QUEUE (review of 2026-09-27:
        // this row said `Upgrade now` could not move it, and that it moves
        // "once that ends", of a limit over by every word): the agent's next
        // run takes the question, and the owner's word types it once more; so
        // does the upgrade itself once the queue has rested, a rest that grows
        // with every copy unread, up to a day (`upgrade::queue_rest`), so no
        // one interval is promised (ruling 307).
        Some(Remedy::Now) if row.wait == "queued" => {
            const _: () = assert!(aterm_agent::harness::upgrade::QUEUE_REST_MAX_S == 86_400);
            (
                format!(
                    "it moves when {who} next runs: type in its tab, or {now_word} asks it once \
                     more (it also asks again by itself, waiting longer each time, up to a day); \
                     {skip}"
                ),
                "--now` or `--skip",
            )
        }
        // A ROUND RE-ARMED AFTER A GIVE-UP, ITS OWN WORK HOLDING IT (ruling
        // 364): a record, and `Upgrade now` moved nothing sooner there — the
        // agent had answered every notice "not yet, my work still runs". Its
        // words promised it anyway, and offered the button (round 35, D5).
        Some(Remedy::Now) if own_work_round(row, now) => (
            format!(
                "{now_word} cannot move it past its own work: it moves once that ends or the \
                 agent stops it; {skip}"
            ),
            "--skip",
        ),
        // Its own text left typed, backing off (the only other wait `--now`
        // still moves once the ladder stands at its last rung): tried again
        // at once. Never "at its next turn end" (the owner, 2026-09-28: a
        // goal-mode Codex's turns never end).
        Some(Remedy::Now) | None => (
            format!("{now_word} tries it again at its next pause; {skip}"),
            "--now` or `--skip",
        ),
        // A TURN OF THIS TAB'S OWN running in its Codex daemon, hours past
        // the ladder's last rung: a goal that never pauses (2026-09-28).
        // Nothing ends that turn for the upgrade. Only where the kernel named
        // the conversation as this tab's (`goal`, `daemon-turn`: its root's
        // turn or a subagent's it spawned) is the owner told to pause
        // anything here — never for a turn aterm could not place
        // (`daemon-busy`: another tab's goal, or a subagent of this tab's own
        // it could not name; the reviews of 2026-09-28).
        // A goal still running hours past the Land rung, where aterm pauses it
        // for a moment: something kept the pause off (an open save-then-wait
        // switch, a rest after a move that did not come, a person's hand).
        Some(Remedy::Waits) if row.wait == "goal" || row.wait.starts_with("goal:") => (
            format!(
                "aterm pauses the goal for a moment to move it once nothing else holds the tab, \
                 and resumes it after; pausing it in this tab moves it now; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == "daemon-turn" => (
            format!("it moves as soon as this tab's turn is over; {skip}"),
            "--skip",
        ),
        Some(Remedy::Waits) if row.wait == "daemon-busy" => (
            format!(
                "it moves once that turn is over, or once aterm can tell it runs in another tab \
                 (`codex agents` lists the sessions); {skip}"
            ),
            "--skip",
        ),
        // The agent's own work is never ended for it. The move comes once that
        // work ends, or once the agent stops it. After a notice, the agent is
        // asked again once half an hour has passed — at a pause of its own, a
        // break or an idle point, never between them: "every 30 minutes" was
        // said of a session a stop hook kept from pausing for seven hours
        // (2026-09-27, s-d3346: notices at 13:13 and 20:14), and of tab #1's
        // 28 silent hours (2026-09-28). Four notices end in a give-up, a stall
        // of its own. When it was last asked is the record's
        // (`Row::noticed_at`), said as a clock time, and the last notice says
        // the give-up comes next ([`asked_again_words`]). The line stays under
        // `DETAIL_LINE_CAP`, which clips.
        Some(Remedy::Waits) if matches!(row.wait.as_str(), "background" | "not-idle:shell") => {
            let asked = match row.phase {
                aterm_agent::harness::upgrade::Phase::Announced { at_s, asks } => {
                    asked_again_words(
                        row.noticed_at,
                        at_s,
                        asks,
                        now,
                        crate::presence::local_offset_s(),
                    )
                }
                _ => String::new(),
            };
            // No raw wait word (round 18, day four, D3: `past its own work
            // (background)`): the shell line below names the command.
            (
                format!(
                    "{now_word} cannot move it past its own work: it moves once that ends or \
                     the agent stops it{asked}; {skip}"
                ),
                "--skip",
            )
        }
        // ATERM'S OWN FENCE FAILS UNDER THE OWNER'S `--now` (the review of
        // 2026-09-28): the word is in force and asked for the very notice
        // aterm cannot type, so the row says whose failure it is, offers no
        // `Upgrade now` (pressed, it put the same row back at once), and names
        // what the owner can still do.
        Some(Remedy::Waits) if row.fence_fails_under_now() => (
            format!(
                "{now_word} is already in force; what fails is aterm typing its notice, and it \
                 tries again at the agent's turn ends; {skip}"
            ),
            "--skip",
        ),
        Some(Remedy::Waits) => (
            format!("{now_word} does not move it past that wait: it moves once that ends; {skip}"),
            "--skip",
        ),
        Some(Remedy::AskAgain) => (
            match next_round_clock(row, now) {
                Some(at) => format!(
                    "it asks again on its own at {at}; {now_word} asks it at its next pause; \
                     {skip}"
                ),
                None => format!("{now_word} asks it again at its next pause; {skip}"),
            },
            "--now` or `--skip",
        ),
        // Where it runs, and what moves it, once (day five, D23: the why and
        // the remedy said the same thing twice, and the remedy did not
        // parse). The pane is named by what holds it; which pane of it is
        // not known to aterm — the agent's terminal is that program's.
        Some(Remedy::InItsPane) => (
            format!(
                "to move it, quit it in its {} and resume it there; {skip}",
                pane_words(row.wait.strip_prefix("terminal:").unwrap_or_default())
            ),
            "--skip",
        ),
        // Never `the harness` (day five, D24): the upgrade asks again, at
        // the clock time its next round starts — the repeating stop's too
        // (D25), or now while its re-armed round is asking.
        // A WAIT NO RUNG PASSES AND NO PAUSE ENDS (`blocked:*`, the owner's
        // decision of 2026-09-28: a warning only where something is really
        // broken): the one way it moves.
        Some(Remedy::ByHand)
            if row.stall(now).as_deref() == Some("blocked:no-shell-integration") =>
        {
            (
                format!(
                    "to move it now, quit Codex in its tab and resume it with the `codex resume` \
                     line it prints; {skip}"
                ),
                "--skip",
            )
        }
        Some(Remedy::ByHand)
            if row
                .stall(now)
                .is_some_and(|stall| stall.starts_with("blocked:")) =>
        {
            (
                format!("to move it now, quit it in its tab and resume it there; {skip}"),
                "--skip",
            )
        }
        Some(Remedy::ByHand) => {
            let when = next_round_clock(row, now).map_or_else(
                || {
                    if matches!(row.phase, aterm_agent::harness::upgrade::Phase::Failed(_)) {
                        "after a rest".to_string()
                    } else {
                        "now".to_string()
                    }
                },
                |at| format!("at {at}"),
            );
            (
                if row.repeating_stop() {
                    format!(
                        "it stopped this way {} times in a row; the upgrade asks it again {when}, \
                         resting longer each time: to move it sooner, quit it and resume it by \
                         hand; {skip}",
                        row.stop_streak
                    )
                } else {
                    format!(
                        "the upgrade asks it again {when}; to move it sooner, quit it and resume \
                         it by hand; {skip}"
                    )
                },
                "--skip",
            )
        }
        Some(Remedy::ResumeInTab) => (
            format!(
                "it no longer runs in the tab and aterm will not bring it back: `{}` there \
                 takes its conversation back, or {} keeps this row down",
                match row.agent {
                    aterm_agent::harness::upgrade::Agent::Codex => "codex resume".to_string(),
                    // A Claude Code move stopped after its SIGTERM (S1 of the
                    // in-flight review, 2026-09-27): its conversation, by id.
                    aterm_agent::harness::upgrade::Agent::Claude if !row.session.is_empty() => {
                        format!("claude --resume {}", clean(&row.session))
                    }
                    aterm_agent::harness::upgrade::Agent::Claude => "claude --resume".to_string(),
                },
                UpgradeWord::Skip.label()
            ),
            "--skip",
        ),
    };
    // THE TITLE SAYS WHICH TAB AND WHETHER IT FAILED (round 18, day four,
    // D2/D4): two stalls read `Couldn't upgrade Claude` twice — the same
    // words for a move waiting on a turn end and one waiting on its own
    // work, neither of which had failed. An upgrade still waiting says so;
    // one that stopped, or that typing cannot reach, could not be done.
    // Waiting on what `Upgrade now` cannot waive, it could not be done YET.
    let resting = row.asks_on_its_own(now)
        && matches!(row.phase, aterm_agent::harness::upgrade::Phase::Failed(_));
    let title = match row.remedy(now) {
        // A round that stopped and asks again on its own (ruling 283): the
        // upgrade working, said as such.
        _ if resting => format!("{who} upgrade retries later {place}"),
        // Never `… upgrade waits in tab 1` (the owner, 2026-09-28: "What is
        // this alert about 'Codex upgrade waits in tab 1'??? that is
        // confusing to me"): a row is shown only once the ladder's last rung
        // has stood for hours, and then it could not be done yet.
        Some(Remedy::Now | Remedy::Waits) => {
            placed_title(&format!("Couldn't upgrade {who}"), place, " yet")
        }
        _ => placed_title(&format!("Couldn't upgrade {who}"), place, ""),
    };
    // STILL ASKING ON ITS OWN (D5): an announced move on its own work is
    // asked again at its breaks and idle points, half an hour apart at the
    // least, and ends in a give-up, a rest and a new round — so until the
    // move clock runs out (a day behind, `MOVE_BUDGET_S`, round six F7) it is
    // the upgrade working: a record, never the glass. One a person holds (a
    // draft, a box, a hold: F6) is never asked again, and is a row.
    //
    // UNLESS THE OWNER ASKED FOR IT (2026-09-28: "when I pressed 'update' in
    // the claude version update button, nothing seemed to happen?"). Their
    // `Upgrade now` withdrew the warn row they pressed, and the answer — it
    // cannot move past the agent's own work yet — went straight to the log in
    // the same instant, so the press read as nothing. While their word stands
    // the answer stays on the glass, calm (`Info`): what it waits on, and
    // that it moves once that ends.
    let owner_asked = row.request == aterm_agent::harness::upgrade::Request::Now;
    let (severity, hold) = match (row.asks_on_its_own(now), owner_asked) {
        (true, false) => (Severity::Info, Hold::LogOnly),
        (true, true) => (Severity::Info, Hold::Standing),
        (false, _) => (Severity::Warn, Hold::Standing),
    };
    // WHAT HOLDS IT, by pid, name and age (2026-09-27: the agent's notice
    // named the shells under it, and the owner read only "its own work
    // runs"). Never a command: those are the agent's own words. A technical
    // line after the move ([`STALL_MOVE_LINE`] stays put) and before the
    // shell spelling, which stays the last (ruling 270); none when nothing
    // holds it (an empty line is left out).
    let held = row.held_words(now).map(|words| {
        let words = atpkg::progress::sanitize_for_tty(&words, aterm_messages::DETAIL_LINE_CAP);
        format!("held by {words}")
    });
    // The why at a sentence's length (day five: 64 clipped `… so the
    // upgrade…` mid-sentence); the band's excerpt is clipped on its own.
    let mut msg = Message::new(tags::HARNESS, severity, title)
        .line(atpkg::progress::sanitize_for_tty(&why, 160))
        .line(remedy)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .line(held.unwrap_or_default())
        .line(format!("the same in any shell: `{cmd} {shell_words}`"))
        .hold(hold)
        .key(&agent_upgrade_key(&row.tab));
    for intent in agent_upgrade_capsules(row, now) {
        msg = msg.action(intent);
    }
    msg
}

/// Where a held-back agent runs, as a person names it (day five, D23): `tmux
/// pane`, `screen window`, `<program> pane`, or `own terminal` when its owner
/// gave no name.
fn pane_words(owner: &str) -> String {
    match owner {
        "tmux" => "tmux pane".to_string(),
        "screen" => "screen window".to_string(),
        "" | "none" | "?" | "-" => "own terminal".to_string(),
        name => format!("{} pane", atpkg::progress::sanitize_for_tty(name, 32)),
    }
}

/// WHEN AN ANNOUNCED MOVE WAS LAST ASKED, AND WHEN IT IS ASKED AGAIN — from
/// the record, never a promise (design record 2026-09-28, §1.4 and §3.2 C8).
/// The row said "asked again every 30 minutes" as a fixed phrase, and on tab
/// #1 nothing had asked for 28 hours: the re-ask is taken only at the
/// session's next break or idle point, and a session that offers neither is
/// asked nothing, whatever the clock says.
///
/// * `noticed_at` — when the latest notice was TYPED (`Row::noticed_at`;
///   `0`: not recorded, and no "last asked" is said). Not `clock_s`: that
///   one a usage limit's episode and a void's release move on with nothing
///   asked (review of 2026-09-28: after a limit the row named the episode's
///   end as a notice's time).
/// * `clock_s` — the re-ask clock's start (`Phase::Announced::at_s`): the
///   re-ask is due [`upgrade::REASK_S`] after it.
/// * `asks` — the notices typed this round. At [`upgrade::MAX_ASKS`] the
///   step past the clock is the give-up, not a fifth notice (the same
///   review), and the words say so.
/// * `offset` — the local zone's ([`crate::presence::local_offset_s`]):
///   times said on the clock, with the day when it is not today
///   ([`clock_on_day`]: `yesterday 10:48 AM`, the review's 29-hours-old
///   notice read as this morning); `None`: said as ages.
///
/// So: `, last asked 2:04 PM, asked again at its next break or idle point
/// from 2:34 PM` while not yet due; `, last asked yesterday 2:04 PM; asks
/// again at its next break or idle point` once due — nothing can say when a
/// point comes; `, last asked 2:04 PM (the last of 4 notices); gives up at
/// its next break or idle point` on the last.
///
/// [`upgrade::REASK_S`]: aterm_agent::harness::upgrade::REASK_S
/// [`upgrade::MAX_ASKS`]: aterm_agent::harness::upgrade::MAX_ASKS
fn asked_again_words(
    noticed_at: u64,
    clock_s: u64,
    asks: u32,
    now: u64,
    offset: Option<i64>,
) -> String {
    use aterm_agent::harness::upgrade::{MAX_ASKS, REASK_S, span};
    const POINT: &str = "at its next break or idle point";
    let when = |at: u64, ago: bool| match offset {
        Some(offset) => clock_on_day(at, now, offset),
        None if ago => format!("{} ago", span(now.saturating_sub(at))),
        None => format!("in {}", span(at.saturating_sub(now))),
    };
    let last = (noticed_at != 0).then(|| when(noticed_at, true));
    let due = (clock_s != 0)
        .then(|| clock_s.saturating_add(REASK_S))
        .filter(|due| now < *due)
        .map(|due| {
            if offset.is_some() {
                format!(" from {}", when(due, false))
            } else {
                format!(" {} or later", when(due, false))
            }
        });
    if asks >= MAX_ASKS {
        let last = last.map_or_else(String::new, |l| format!(", last asked {l}"));
        let from = due.unwrap_or_default();
        return format!("{last} (the last of {MAX_ASKS} notices); gives up {POINT}{from}");
    }
    match (last, due) {
        (Some(last), Some(from)) => format!(", last asked {last}, asked again {POINT}{from}"),
        (Some(last), None) => format!(", last asked {last}; asks again {POINT}"),
        (None, from) => format!(", asked again {POINT}{}", from.unwrap_or_default()),
    }
}

/// `at` (unix seconds) on the reader's clock `offset` from UTC, with its day
/// when that is not `now`'s: `2:04 PM`, `yesterday 2:04 PM`, `tomorrow 12:10
/// AM`, else the day as a Messages header says it (`Thursday 2:04 PM`, `Fri,
/// 18 Sep, 2:04 PM`). A Messages row sits under the day it was POSTED, not
/// the day of a time it names, so a bare clock time more than a day old reads
/// as today's (review of 2026-09-28).
fn clock_on_day(at: u64, now: u64, offset: i64) -> String {
    use aterm_messages::words::{clock_words, day_heading, local_day};
    let ms = |s: u64| s.saturating_mul(1000);
    let clock = clock_words(ms(at), offset);
    let (day, today) = (local_day(ms(at), offset), local_day(ms(now), offset));
    match day.saturating_sub(today) {
        0 => clock,
        -1 => format!("yesterday {clock}"),
        1 => format!("tomorrow {clock}"),
        _ => {
            let words = day_heading(today, day, true);
            if words.contains(',') {
                format!("{words}, {clock}")
            } else {
                format!("{words} {clock}")
            }
        }
    }
}

/// When a stopped round's next one starts, on the person's clock (`4:10 PM`),
/// or `None` while it is due, not stopped, or held by the owner's word
/// ([`aterm_agent::harness::upgrade_drive::Row::next_round_in`]). A clock
/// time, not a countdown: the record says it once and it stays true.
/// `None` too where the local clock's offset is unknown: no time is said
/// on a clock that may be hours off.
fn next_round_clock(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> Option<String> {
    let secs = row.next_round_in(now).filter(|secs| *secs > 0)?;
    let offset = crate::presence::local_offset_s()?;
    Some(aterm_messages::words::clock_words(
        now.saturating_add(secs).saturating_mul(1000),
        offset,
    ))
}

/// The key a tab's upgrade rows are posted under — its stalled row
/// ([`agent_upgrade_stalled`]) and the owner's word on it
/// ([`agent_upgrade_worded`]): `harness.upgrade.<tab>`, the tab id cleaned as
/// the rows print it.
pub(crate) fn agent_upgrade_key(tab: &str) -> String {
    format!(
        "{KEY_AGENT_UPGRADE}.{}",
        atpkg::progress::sanitize_for_tty(tab, 64)
    )
}

/// The key AN AGENT'S ONE ROW for its stalled tabs is posted under
/// ([`agent_upgrade_stalled_group`]): `harness.upgrades.claude`,
/// `harness.upgrades.codex`. Never a tab's key: [`agent_upgrade_key_tab`] reads
/// `None` for it, so the per-tab sweeps leave it alone.
pub(crate) fn agent_upgrade_group_key(agent: aterm_agent::harness::upgrade::Agent) -> String {
    let name = match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "claude",
        aterm_agent::harness::upgrade::Agent::Codex => "codex",
    };
    format!("{KEY_AGENT_UPGRADE}s.{name}")
}

/// Whether `key` is an agent's one row for its stalled tabs
/// ([`agent_upgrade_group_key`]).
pub(crate) fn agent_upgrade_group_key_is(key: &str) -> bool {
    key.strip_prefix(KEY_AGENT_UPGRADE)
        .and_then(|rest| rest.strip_prefix("s."))
        .is_some_and(|agent| matches!(agent, "claude" | "codex"))
}

/// The STALLS an agent's one row named, read back from its detail's last line
/// ([`agent_upgrade_stalled_group`]: `… — tabs s-aaa@2.1.284, s-bbb@2.1.284`):
/// each tab (`s-<hex>`) and the build it was stalled on its way to. What the
/// window reads a row the owner saw through by, after a restart and in this
/// process alike, so the same stalls are not posted again — and what keeps a
/// tab in its agent's row across a restart. A tab named without a build (no
/// `@`) is read with none, which no stall matches. Empty for a detail that
/// names none.
pub(crate) fn agent_upgrade_group_members(detail: &[String]) -> Vec<(String, String)> {
    detail
        .iter()
        .rev()
        .find_map(|line| line.rsplit_once(GROUP_TABS))
        .map(|(_, tabs)| {
            tabs.split(", ")
                .filter(|t| t.starts_with("s-"))
                .map(|t| {
                    let (tab, to) = t.split_once('@').unwrap_or((t, ""));
                    (tab.to_string(), to.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One stall of an agent's one row, as its last line names it
/// ([`agent_upgrade_group_members`]): `s-aaa@2.1.284`.
pub(crate) fn agent_upgrade_group_member(tab: &str, to: &str) -> String {
    format!(
        "{}@{}",
        atpkg::progress::sanitize_for_tty(tab, 64),
        atpkg::progress::sanitize_for_tty(to, 32)
    )
}

/// What opens the list of stalls on an agent's one row's last line.
const GROUP_TABS: &str = " \u{2014} tabs ";

/// How many of an agent's stalled tabs its one row names a line each: the
/// rest are counted, and `--status` lists them (the detail holds
/// `DETAIL_LINES_CAP` lines).
const GROUP_TABS_NAMED: usize = 12;

/// THE ONE ROW FOR AN AGENT'S STALLED TABS (2026-09-28, the owner: "also fix
/// this stacking failures of claude upgrades!" — three stalled tabs were three
/// rows, the band's whole height, each "Couldn't upgrade Claude yet" with its
/// own buttons). `members`: each stalled tab's row and its place (`in window
/// 2, tab 1`), any order.
///
/// ONE TAB reads as its own row does ([`agent_upgrade_stalled`]), under this
/// key, its buttons the row's own word for that tab and its shell line naming
/// the stall it stands for. SEVERAL: the title counts them, in the most
/// serious of their words — one that could not be done over one that could
/// not be done yet (`Couldn't upgrade Claude in 3 tabs`, `… in 3 tabs yet`;
/// never `… upgrade waits in 3 tabs`, ruling 380); the first
/// line names the tabs, most behind first (`window 2, tab 1 for 26 h · window
/// 1, tab 2 for 7 h`), the second what the row's own buttons do, and behind
/// them each tab's own words — its place and move, why it stands, what holds
/// it — and the shell spelling with every stall, each tab's id and its target
/// ([`agent_upgrade_group_members`] reads them back). Each button is the word
/// for exactly the listed tabs that take it ([`Intent::AgentUpgradeTabs`]); a
/// tab's own menu has its own words.
pub(crate) fn agent_upgrade_stalled_group(
    members: &[(&aterm_agent::harness::upgrade_drive::Row, String)],
    now: u64,
) -> Option<Message> {
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    let (first, _) = members.first()?;
    let agent = first.agent;
    let key = agent_upgrade_group_key(agent);
    // ONE TAB LEFT reads as its own row, under the agent's key, with two
    // differences a person never reads (the review of 2026-09-28): its
    // buttons are the row's own word for that one tab (so a press answers
    // THIS row, never a tab's record behind it), and its shell line ends with
    // the stall it stands for (so the row is read back as the agent's after a
    // restart, never posted again as the tab's own).
    if let [(row, place)] = members {
        let mut msg = agent_upgrade_stalled(row, now, place);
        msg.key = Some(key);
        msg.actions = msg
            .actions
            .iter()
            .map(|intent| match intent {
                Intent::AgentUpgrade { tab, to, word } => Intent::AgentUpgradeTabs {
                    word: *word,
                    moves: vec![(tab.clone(), to.clone())],
                },
                other => other.clone(),
            })
            .collect();
        if let Some(shell) = msg.detail.last_mut() {
            shell.push_str(GROUP_TABS);
            shell.push_str(&agent_upgrade_group_member(&row.tab, &row.to));
        }
        return Some(msg);
    }
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let who = agent_word(agent);
    let n = members.len();
    let mut sorted: Vec<&(&aterm_agent::harness::upgrade_drive::Row, String)> =
        members.iter().collect();
    sorted.sort_by_key(|(row, place)| (row.behind_since, place.clone()));
    // The most serious of their words.
    let tier = |row: &aterm_agent::harness::upgrade_drive::Row| match row.remedy(now) {
        Some(Remedy::Now | Remedy::AskAgain) | None => 0,
        Some(Remedy::Waits) => 1,
        Some(Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => 2,
    };
    let title = match sorted.iter().map(|(row, _)| tier(row)).max().unwrap_or(0) {
        // Never `… upgrade waits in N tabs` (ruling 380, the owner's words
        // of 2026-09-28: "What is this alert about 'Codex upgrade waits in
        // tab 1'???"): a row stands only once the ladder's last rung has
        // stood for hours, and then it could not be done yet — each tab's
        // own row's words ([`agent_upgrade_stalled`]).
        0 | 1 => format!("Couldn't upgrade {who} in {n} tabs yet"),
        _ => format!("Couldn't upgrade {who} in {n} tabs"),
    };
    let bare = |place: &str| place.strip_prefix("in ").unwrap_or(place).to_string();
    let named = sorted
        .iter()
        .map(|(row, place)| match row.behind_for(now) {
            Some(b) => format!("{} for {b}", bare(place)),
            None => bare(place),
        })
        .collect::<Vec<_>>()
        .join(" \u{00b7} ");
    // Each button, for exactly the listed tabs that take its word.
    let takes = |word: UpgradeWord| -> Vec<(String, String)> {
        sorted
            .iter()
            .filter(|(row, _)| agent_upgrade_words(row, now).contains(&word))
            .map(|(row, _)| (row.tab.clone(), row.to.clone()))
            .collect()
    };
    let words: Vec<(UpgradeWord, Vec<(String, String)>)> = UpgradeWord::ALL
        .into_iter()
        .map(|word| (word, takes(word)))
        .filter(|(_, moves)| !moves.is_empty())
        .take(2)
        .collect();
    // What those buttons do, in the row's own words (ruling 288).
    let mut does: Vec<String> = Vec::new();
    for (word, moves) in &words {
        let k = moves.len();
        // Two tabs are `both`, never `all 2` (round 35, D4).
        let which = if k == n && n == 2 {
            "both".to_string()
        } else if k == n {
            format!("all {n}")
        } else if k == 1 {
            sorted
                .iter()
                .find(|(row, _)| row.tab == moves[0].0)
                .map_or_else(|| "one".to_string(), |(_, place)| bare(place))
        } else {
            format!("{k} of them")
        };
        // The tabs `Upgrade now` cannot move, in number.
        let rest = if n - k == 1 {
            "the other one moves once what holds it ends".to_string()
        } else {
            format!("the other {} move once what holds them ends", n - k)
        };
        // At their next PAUSES, as each tab's own row says it (ruling 380:
        // never "at its next turn end" — a goal-mode Codex's turns never
        // end).
        does.push(match word {
            UpgradeWord::Now if k == n => {
                format!("{} moves {which} at their next pauses", word.label())
            }
            UpgradeWord::Now if k == 1 => {
                format!("{} moves {which} at its next pause; {rest}", word.label())
            }
            UpgradeWord::Now => {
                format!(
                    "{} moves {which} at their next pauses; {rest}",
                    word.label()
                )
            }
            UpgradeWord::NotToday => format!("{} puts {which} off until tomorrow", word.label()),
            UpgradeWord::Skip => format!("{} keeps {which} where they are", word.label()),
        });
    }
    does.push("for one tab alone, use its tab's menu".to_string());
    let mut msg = Message::new(tags::HARNESS, Severity::Warn, title)
        .line(atpkg::progress::sanitize_for_tty(&named, 160))
        .line(does.join("; "));
    for (row, place) in sorted.iter().take(GROUP_TABS_NAMED) {
        let why = row
            .stall_words(now)
            .unwrap_or_else(|| "it is not moving".to_string())
            .replace('`', "");
        let held = row
            .held_words(now)
            .map_or_else(String::new, |w| format!("; held by {w}"));
        msg = msg.line(atpkg::progress::sanitize_for_tty(
            &format!(
                "{} \u{00b7} {}: {why}{held}",
                bare(place),
                clean(&row.move_words())
            ),
            aterm_messages::DETAIL_LINE_CAP,
        ));
    }
    if n > GROUP_TABS_NAMED {
        msg = msg.line(format!(
            "and {} more \u{2014} `aterm harness upgrade --status` lists them",
            n - GROUP_TABS_NAMED
        ));
    }
    let tabs: Vec<String> = sorted
        .iter()
        .map(|(row, _)| agent_upgrade_group_member(&row.tab, &row.to))
        .collect();
    msg = msg.line(format!(
        "the same in any shell: `aterm harness upgrade <tab> --now` or `--skip`{GROUP_TABS}{}",
        tabs.join(", ")
    ));
    for (word, moves) in words {
        msg = msg.action(Intent::AgentUpgradeTabs { word, moves });
    }
    Some(msg.hold(Hold::Standing).key(&key))
}

/// The tab a key names when it is one tab's [`agent_upgrade_key`] — this
/// build's or an earlier one's, which keyed the same way — or `None`: another
/// family's key, the waiting record's ([`KEY_AGENT_UPGRADE`]), the finished
/// moves' (`harness.upgrade.done`).
pub(crate) fn agent_upgrade_key_tab(key: &str) -> Option<&str> {
    key.strip_prefix(KEY_AGENT_UPGRADE)?
        .strip_prefix('.')
        .filter(|tab| !tab.is_empty() && *tab != "done")
}

/// The line of a stalled row's detail that names the tab and the move
/// ([`agent_upgrade_stalled`]: `in tab 2 · Claude Code 2.1.280 → 2.1.283`).
pub(crate) const STALL_MOVE_LINE: usize = 2;

/// THE SAME STALL, by the words of two stalled rows' details (`a`, `b`,
/// [`agent_upgrade_stalled`]) under one tab's key: the same move
/// ([`STALL_MOVE_LINE`], past its place) and the same stall — its words, an
/// overdue one's by its kind alone (`behind for …` moves with every look).
/// The place is not compared: a tab's place in its strip moves as tabs open,
/// close and move, and the key already names the tab. What keeps a stall the
/// owner already saw from being posted again after aterm restarts
/// (`upgrade_host`, the owner's report of 2026-09-27).
///
/// A ROW AN EARLIER BUILD POSTED reads the same when its words do: before
/// ruling 270 the move line held the move alone, with no place before it.
pub(crate) fn same_stall(a: &[String], b: &[String]) -> bool {
    let words = |d: &[String]| -> Option<(String, String)> {
        let (why, line) = stall_lines(d)?;
        let why = if why.starts_with("behind for ") {
            "overdue".to_string()
        } else {
            why.to_string()
        };
        let moved = strip_place(line).unwrap_or(line);
        Some((why, moved.to_string()))
    };
    matches!((words(a), words(b)), (Some(x), Some(y)) if x == y)
}

/// A stalled row's WHY and MOVE lines ([`agent_upgrade_stalled`]), found by
/// what they say, not where they stand (ruling 307): a refused press
/// restates the row with the refusal ABOVE the stall's lines (the band
/// paints `detail[0]`), which shifted every index, so a row the owner saw
/// through read as another stall after a restart and came back. The move is
/// the line that opens with a place (`in tab 2 · …`), the why two lines above
/// it; a row an earlier build posted, whose move line held no place, reads
/// at the fixed [`STALL_MOVE_LINE`]. `None` for a detail with neither.
fn stall_lines(d: &[String]) -> Option<(&str, &str)> {
    match d.iter().position(|l| strip_place(l).is_some()) {
        Some(at) => Some((d.get(at.checked_sub(2)?)?, &d[at])),
        None => Some((d.first()?, d.get(STALL_MOVE_LINE)?)),
    }
}

/// HOW MANY LEADING DETAIL LINES ARE THE SENTENCE (ruling 314, day eight
/// E2): Settings ▸ Messages sets them as a person's words and the rest as
/// technical lines. One for every record but a tab's upgrade row
/// ([`agent_upgrade_stalled`]), whose why and next step — `it asks again on
/// its own at 12:55 AM; Upgrade now asks it at its next pause; …` — stand
/// above its move line (found by its place, as [`stall_lines`] finds it, so a
/// refused press's restated row keeps its refusal, why and remedy together).
/// A record a key does not name, or one with no move line, keeps one.
pub(crate) fn sentence_lines(key: Option<&str>, detail: &[String]) -> usize {
    if key.and_then(agent_upgrade_key_tab).is_some()
        && let Some(at) = detail.iter().position(|l| strip_place(l).is_some())
    {
        return at.max(1);
    }
    // An agent's one row for its stalled tabs: its tabs, and what its buttons
    // do — one tab's reads as that tab's own row.
    if key.is_some_and(agent_upgrade_group_key_is) {
        if let Some(at) = detail.iter().position(|l| strip_place(l).is_some()) {
            return at.max(1);
        }
        return 2.min(detail.len()).max(1);
    }
    1
}

/// `line` past a PLACE at its head and ` · ` — `in tab 2`, `in window 2,
/// tab 1`, `in its tab` (`upgrade_host`'s `upgrade_place`, [`tab_place`]) —
/// or `None` when it opens with none.
fn strip_place(line: &str) -> Option<&str> {
    let (place, rest) = line.split_once(" \u{00b7} ")?;
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    let tab = |p: &str| p.strip_prefix("tab ").is_some_and(number);
    let placed = place == "in its tab"
        || place.strip_prefix("in ").is_some_and(|p| {
            tab(p)
                || p.strip_prefix("window ")
                    .and_then(|p| p.split_once(", "))
                    .is_some_and(|(w, p)| number(w) && tab(p))
        });
    placed.then_some(rest)
}

/// A tab's PLACE in the person's words (ruling 270): `in tab 2` — its place
/// in its window's strip, counted from 1 — or, where the owner has more than
/// one window (`window`, counted from 1), `in window 2, tab 1`: tabs are
/// counted per window, and the band is every window's, so two tabs first in
/// their own windows would read the same. Never a session id.
pub(crate) fn tab_place(window: Option<usize>, tab: usize) -> String {
    match window {
        Some(window) => format!("in window {window}, tab {tab}"),
        None => format!("in tab {tab}"),
    }
}

/// A GLASS TITLE THAT NAMES ITS TAB: `{head} {place}{tail}` (`Couldn't
/// upgrade Claude in tab 1 yet`) — or, where that would break the glass title
/// rule (six words, 48 characters: [`aterm_messages::text::glass_title_fault`])
/// because the place names a window too (`in window 2, tab 1` is a word
/// longer), `{head}{tail}, {place past its "in "}`: `Couldn't upgrade Claude
/// yet, window 2, tab 1` — a comma, since the title lint (ruling 261) allows
/// no parenthesis and no colon. Every title that fits keeps the words ruling
/// 270 and its successors gave it.
fn placed_title(head: &str, place: &str, tail: &str) -> String {
    let title = format!("{head} {place}{tail}");
    match place.strip_prefix("in ") {
        Some(bare)
            if bare.starts_with("window ")
                && aterm_messages::text::glass_title_fault(&title).is_some() =>
        {
            format!("{head}{tail}, {bare}")
        }
        _ => title,
    }
}

/// A STALL THAT ENDED WHILE ITS ROW WAS DOWN (review of 2026-09-27): the
/// person read or dismissed the stalled row ([`agent_upgrade_stalled`]) and
/// the stall has since ended — the upgrade moved on, finished, or its tab
/// went. A row no longer live cannot be resolved, so the log kept the
/// person's read or dismissal as the stall's last word, and when the same
/// stall came back — an overdue upgrade quieted by `--now` reading overdue
/// once more, a refusal a later round meets again after its first notice
/// went — `upgrade_host` took it for the row the person had already seen and
/// posted nothing. This RECORD, under the row's own `key`, is the stall's
/// end: the same stall coming back is news. `detail` is the stalled row's:
/// its first line (why) and its tab and move are kept.
///
/// WORDED BY WHAT HAPPENED, WITH ITS TAB (ruling 307): `Claude upgrade asks
/// again in tab 2` for a tab whose upgrade goes on (the tab still holds a
/// round), `Claude upgrade no longer waits in tab 2` for one that left —
/// never `no longer stalled`, a word the band retired, and never the stall's
/// reason in the present tense under a title saying it ended: the reason is
/// `was: …`, then the move. A restart under way that stuck and moves again
/// (ruling 327) is `Claude upgrade moves again in tab 2`: nothing asks
/// anything again, it carries on (the merge review of 2026-09-28: it read
/// `asks again`). See [`StallEnd`].
pub(crate) fn agent_upgrade_stall_over(
    agent: aterm_agent::harness::upgrade::Agent,
    key: &str,
    detail: &[String],
    place: &str,
    end: StallEnd,
) -> Message {
    let who = agent_word(agent);
    let title = match end {
        StallEnd::AsksAgain => format!("{who} upgrade asks again {place}"),
        StallEnd::MovesAgain => format!("{who} upgrade moves again {place}"),
        StallEnd::Left => format!("{who} upgrade no longer waits {place}"),
    };
    let mut msg = Message::new(tags::HARNESS, Severity::Info, title);
    if let Some((why, moved)) = stall_lines(detail) {
        msg = msg.line(format!("was: {why}")).line(moved);
    }
    msg.hold(Hold::LogOnly).key(key)
}

/// WHAT BECAME OF AN UPGRADE WHOSE STALL ENDED while its row was down
/// ([`agent_upgrade_stall_over`]'s title, ruling 307(d)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StallEnd {
    /// The tab still holds a round of the upgrade, which asks again.
    AsksAgain,
    /// A restart under way that stuck (`stuck:<what>`, ruling 327) moves
    /// again, still in flight: it carries on, and asks nothing again.
    MovesAgain,
    /// The tab left the upgrade's rows: nothing waits there now.
    Left,
}

/// The agent a row's words name: `Claude` or `Codex`.
fn agent_word(agent: aterm_agent::harness::upgrade::Agent) -> &'static str {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "Claude",
        aterm_agent::harness::upgrade::Agent::Codex => "Codex",
    }
}

/// A ROUND RE-ARMED AFTER A GIVE-UP THAT ITS AGENT'S OWN WORK HOLDS (ruling
/// 364, `Row::asks_on_its_own`'s pending arm; a usage limit, the other one,
/// waits anyway): the upgrade working, and no word the owner presses moves it
/// sooner — the move comes at the end of that work (round 35, D5).
fn own_work_round(row: &aterm_agent::harness::upgrade_drive::Row, now: u64) -> bool {
    row.phase == aterm_agent::harness::upgrade::Phase::Pending && row.asks_on_its_own(now)
}

/// THE OWNER'S WORDS THAT DO SOMETHING FOR THIS UPGRADE (gap #21), most
/// useful first — the tab menu offers them all, a row its first two (two
/// authored capsules: `MAX_ACTIONS`). None while a restart is under way or
/// done: `upgrade_drive::ask` refuses a word then. `Upgrade now` only where
/// `--now` moves it: a healthy wait (the settling window and the attended
/// guard waived), an overdue one waiting on a turn end
/// ([`Remedy::Now`](aterm_agent::harness::upgrade_drive::Remedy::Now)), one
/// that gave up (asked again) — never one waiting on what `--now` does not
/// waive, one in a pane typing cannot reach, or one refused or failed for
/// good, where it would be recorded and move nothing, or be refused — nor
/// one the owner's `--now` already hurries. `Not today` and `Skip version`
/// hold every kind (a day; until a newer build).
pub(crate) fn agent_upgrade_words(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
) -> Vec<aterm_messages::UpgradeWord> {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Remedy;
    use aterm_messages::UpgradeWord;
    if !matches!(
        row.phase,
        Phase::Pending | Phase::Announced { .. } | Phase::Failed(_)
    ) || row.tab.is_empty()
        || row.to.is_empty()
        // A goal left paused is moved by `/goal resume` alone (or, fallen
        // into a sandbox, a relaunch by hand): no word of the upgrade's is
        // offered on its row.
        || matches!(
            row.stall(now).as_deref(),
            Some("goal-paused" | "goal-sandboxed")
        )
    {
        return Vec::new();
    }
    let now_moves = match row.remedy(now) {
        // Not stalled: `--now` waives the quiet window — unless the owner's
        // `--now` is already in force, where pressing it again would only
        // arm a new round (a fresh READY marker) for a move already hurried.
        None => {
            !matches!(row.phase, Phase::Failed(_))
                && row.request != aterm_agent::harness::upgrade::Request::Now
        }
        Some(Remedy::Now) => !own_work_round(row, now),
        Some(Remedy::AskAgain) => true,
        Some(Remedy::Waits | Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => false,
    };
    // THE WORD IN FORCE IS NOT OFFERED AGAIN (ruling 270; day three: the
    // record the owner pressed `Not today` on kept `Not today` as its
    // Primary): a deferral still running drops `Not today`, a skip of this
    // very build leaves nothing to say.
    use aterm_agent::harness::upgrade::{Request, Version};
    let deferred = match &row.request {
        Request::DeferUntil(until) => now < *until,
        Request::Skip(v) if Version::parse(v) == Version::parse(&row.to) => return Vec::new(),
        Request::Skip(_) | Request::None | Request::Now => false,
    };
    let mut words = Vec::with_capacity(3);
    if now_moves {
        words.push(UpgradeWord::Now);
    }
    if !deferred {
        words.push(UpgradeWord::NotToday);
    }
    words.push(UpgradeWord::Skip);
    words
}

/// A row's capsules: the first two of [`agent_upgrade_words`], for this
/// tab and the build the row names.
pub(crate) fn agent_upgrade_capsules(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
) -> Vec<Intent> {
    agent_upgrade_words(row, now)
        .into_iter()
        .take(2)
        .map(|word| Intent::AgentUpgrade {
            tab: row.tab.clone(),
            to: row.to.clone(),
            word,
        })
        .collect()
}

/// WHAT THE OWNER'S WORD DID (gap #21), in the owner's terms — a RECORD: a
/// confirmation earns no row (ruling 76). The stalled row it was pressed on
/// takes these words as it is ANSWERED (ruling 270: `ℹ`, never the `✓` of
/// work delivered or a problem fixed — putting an upgrade off is a choice,
/// and `Upgrade now` only asks), so the entry the owner pressed is the one
/// that says what happened; with no such row, it is recorded. `row` is the
/// upgrade as the word left it; `place` names its tab (`in tab 2`).
/// `detail[0]` is the sentence, the move behind it.
///
/// `queued`: the row pressed waited on its question unread behind a full
/// queue, where `Upgrade now` types it once more (ruling 307: the answer said
/// `the wait for a quiet tab is waived`, of a session already idle).
pub(crate) fn agent_upgrade_worded(
    row: &aterm_agent::harness::upgrade_drive::Row,
    word: aterm_messages::UpgradeWord,
    place: &str,
    queued: bool,
) -> Message {
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let who = agent_word(row.agent);
    let (from, to) = (clean(&row.from), clean(&row.to));
    let (title, then) = match word {
        UpgradeWord::Now if queued => (
            format!("{who} is asked again {place}"),
            format!("the upgrade's question is typed once more; it moves when {who} reads it"),
        ),
        // The ladder's last rung (ruling 380): the first pause, never "when
        // its turn ends" — a goal-mode Codex's turns never end.
        UpgradeWord::Now => (
            format!("{who} moves at its next pause"),
            format!(
                "the wait for a quiet tab is waived; it still waits for {who} to go idle, and \
                 never moves over typing, a draft, a dialog or running work"
            ),
        ),
        UpgradeWord::NotToday => (
            format!("{who} stays on {from} until tomorrow"),
            "the upgrade asks again this time tomorrow".to_string(),
        ),
        UpgradeWord::Skip => (
            format!("{who} skips {to}"),
            format!("it stays on {from} until a build newer than {to} comes"),
        ),
    };
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(then)
        .line(format!("{place} \u{00b7} {}", clean(&row.move_words())))
        .hold(Hold::LogOnly)
        .key(&agent_upgrade_key(&row.tab))
}

/// WHAT THE OWNER'S WORD ON AN AGENT'S ONE ROW ASKED
/// ([`agent_upgrade_stalled_group`], [`Intent::AgentUpgradeTabs`]): the words
/// the row is ANSWERED with, of the `n` tabs that took the word — `Claude
/// waits until tomorrow in 3 tabs`, `Claude skips this build in 3 tabs`,
/// `Claude moves in 3 tabs when idle` — never a `✓`: a choice is not delivered
/// work, and `Upgrade now` has moved nothing yet. What each tab's word did is
/// that tab's own record ([`agent_upgrade_worded`]).
pub(crate) fn agent_upgrade_group_worded(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
) -> Message {
    use aterm_messages::UpgradeWord;
    let who = agent_word(agent);
    let tabs = if n == 1 {
        "in 1 tab".to_string()
    } else {
        format!("in {n} tabs")
    };
    let (title, then) = match word {
        UpgradeWord::Now => (
            format!("{who} moves {tabs} when idle"),
            "the wait for a quiet tab is waived; each still waits for its agent to go idle",
        ),
        UpgradeWord::NotToday => (
            format!("{who} waits until tomorrow {tabs}"),
            "the upgrade asks each again this time tomorrow",
        ),
        UpgradeWord::Skip => (
            format!("{who} skips this build {tabs}"),
            "each stays where it is until a newer build comes",
        ),
    };
    Message::new(tags::HARNESS, Severity::Info, title)
        .line(then)
        .hold(Hold::LogOnly)
        .key(&agent_upgrade_group_key(agent))
}

/// AN AGENT'S ONE ROW'S STALLS ENDED WHILE THE ROW WAS DOWN (the review of
/// 2026-09-28; [`agent_upgrade_stall_over`] for one tab's row): the owner read
/// or dismissed the row, and no tab it named stalls in it any more. The log's
/// last word under the agent's key would stay the owner's read, and the same
/// stalls coming back — the same tabs overdue again, days later — would be
/// taken for the row already seen and never shown. This RECORD, under the
/// row's own key, is their end: the same stalls back are news. `detail` is
/// the row's: its title is kept as what it was, with the tabs it named.
pub(crate) fn agent_upgrade_group_over(
    agent: aterm_agent::harness::upgrade::Agent,
    title: &str,
    detail: &[String],
) -> Message {
    let who = agent_word(agent);
    let n = agent_upgrade_group_members(detail).len().max(1);
    let tabs = if n == 1 {
        "in 1 tab".to_string()
    } else {
        format!("in {n} tabs")
    };
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("{who} upgrade wait ended {tabs}"),
    )
    .line(format!(
        "was: {}",
        atpkg::progress::sanitize_for_tty(title, 120)
    ));
    if let Some(first) = detail.first() {
        msg = msg.line(first.clone());
    }
    msg.hold(Hold::LogOnly).key(&agent_upgrade_group_key(agent))
}

/// The finished words of an agent's one row whose tabs all moved (the review
/// of 2026-09-28; day five's D26 for one tab's row,
/// [`agent_upgrade_went_through`]): `Claude upgraded in 3 tabs` — or, one tab,
/// that tab's own words — the `✓` record's title in the log, never the row's
/// `⚠ Couldn't upgrade …` beside the success records.
pub(crate) fn agent_upgrade_group_went_through(
    agent: aterm_agent::harness::upgrade::Agent,
    n: usize,
    place: Option<&str>,
) -> String {
    let who = agent_word(agent);
    match (n, place) {
        (1, Some(place)) => format!("{who} upgraded {place}"),
        (1, None) => format!("{who} upgraded in 1 tab"),
        _ => format!("{who} upgraded in {n} tabs"),
    }
}

/// THE OWNER'S WORD ON AN AGENT'S ONE ROW, REFUSED FOR EVERY TAB IT WAS SENT
/// FOR (the review of 2026-09-28): nothing was written for any of the `n`
/// tabs, and why — the first tab's refusal (`why`), in the words one tab's
/// row takes ([`agent_upgrade_word_refused`]; `to`, the build the row named,
/// for a skip's title). The row pressed takes these words above its own and
/// keeps its buttons, as one tab's row does; with no live row, one
/// gesture-failure row for the press — never a row per tab stacked under it.
pub(crate) fn agent_upgrade_group_word_refused(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
    (to, why): (&str, &str),
) -> Message {
    let tabs = if n == 1 {
        "in 1 tab".to_string()
    } else {
        format!("in {n} tabs")
    };
    let mut msg = agent_upgrade_word_refused(word, agent, ("", to), why, &tabs);
    msg.key = Some(agent_upgrade_group_key(agent));
    msg
}

/// The line an ANSWERED agent's row carries for the tabs its word was not
/// written for (`n` of them), and why — the first refusal's remedy, in the
/// words one tab's row takes ([`agent_upgrade_word_refused`]).
pub(crate) fn agent_upgrade_group_word_refused_line(
    agent: aterm_agent::harness::upgrade::Agent,
    word: aterm_messages::UpgradeWord,
    n: usize,
    why: &str,
) -> String {
    let refused = agent_upgrade_group_word_refused(agent, word, n, ("", why));
    let tabs = if n == 1 {
        "1 tab".to_string()
    } else {
        format!("{n} tabs")
    };
    match refused.detail.first() {
        Some(reason) => format!("not written for {tabs}: {reason}"),
        None => format!("not written for {tabs}"),
    }
}

/// THE OWNER'S WORD REFUSED (gap #21): nothing was written, and why — another
/// step held the lock (press it again), the upgrade moved on to a newer
/// build than the one pressed for, or `upgrade_drive::ask`'s own reason (a
/// restart under way, one stopped for good, none recorded). The person's own
/// press failed, so it is the one gesture-failure shape (ruling 270, H11):
/// Error, `HOLD_GESTURE` — a stalled row it was pressed on takes these words
/// above its own and keeps its own mark and hold.
///
/// A ROUND THAT STOPPED between the offer and the press (ruling 283) is
/// worded here from the writer's typed refusal
/// ([`aterm_agent::harness::upgrade_drive::refused_stopped`]): the tab by its
/// place, when its next round starts on the person's clock — never the CLI's
/// sentence with its raw session id, flags and stop word, nor a button the
/// entry may not carry. Any other refusal is the writer's own sentence: kept whole,
/// but behind Details (`no_excerpt`), so the band paints only the title.
pub(crate) fn agent_upgrade_word_refused(
    word: aterm_messages::UpgradeWord,
    agent: aterm_agent::harness::upgrade::Agent,
    (tab, to): (&str, &str),
    why: &str,
    place: &str,
) -> Message {
    use aterm_messages::UpgradeWord;
    let who = agent_word(agent);
    let shown = atpkg::progress::sanitize_for_tty(to, 32);
    // The tab by its place (day five, D22: `Couldn't upgrade Claude now`
    // named none). What was refused is named (ruling 307: `Couldn't put off
    // Claude` read as Claude itself being put off) — and short enough that
    // the remedy beside it is painted at 80 columns, as its siblings' is
    // (review of round 21: `Couldn't postpone Claude's upgrade in tab 2`,
    // 43 cells, painted its title alone). The tab names whose upgrade.
    let title = match word {
        UpgradeWord::Now => placed_title(&format!("Couldn't upgrade {who}"), place, " now"),
        UpgradeWord::NotToday => placed_title("Couldn't postpone the upgrade", place, ""),
        UpgradeWord::Skip => placed_title(&format!("Couldn't skip {who} {shown}"), place, ""),
    };
    // THE REMEDY FIRST, THE MECHANISM AFTER (ruling 307): at 80 columns the
    // excerpt kept `another step held the…`, and at 120 it was cut just
    // before `press it again` — the one part that changes what the person
    // does. The remedy is its own short line (the painted excerpt), the
    // mechanism the next.
    let mut again = false;
    let (reason, then, plain) = if why.starts_with("busy:") {
        again = true;
        (
            "press it again".to_string(),
            Some("another step held the upgrade's lock, so nothing was written".to_string()),
            true,
        )
    } else if let Some(now_to) = why.strip_prefix("stale:") {
        // The TARGET changed; nothing moves now (review of round 21: `it
        // moves to 2.1.283 now` promised a move the tab still waits for).
        let now_to = atpkg::progress::sanitize_for_tty(now_to, 32);
        (
            format!("a newer build: {now_to}"),
            Some(format!(
                "the upgrade is to {now_to} now, so nothing was written for {shown}"
            )),
            true,
        )
    } else if let Some((at, _)) = aterm_agent::harness::upgrade_drive::refused_stopped(why) {
        let when = match crate::presence::local_offset_s() {
            Some(offset) if at > 0 => format!(
                "at {}",
                aterm_messages::words::clock_words(at.saturating_mul(1000), offset)
            ),
            _ => "soon".to_string(),
        };
        // No button named (day five, D21): the entry this lands on carries
        // its own capsules, and a refusal of its own carries none.
        (
            format!("retries {when}"),
            Some("the upgrade stopped before the press reached it".to_string()),
            true,
        )
    } else {
        (format!("nothing was written: {why}"), None, false)
    };
    let mut msg = gesture_failure(
        tags::HARNESS,
        Severity::Error,
        &title,
        &atpkg::progress::sanitize_for_tty(&reason, 600),
    );
    if let Some(then) = then {
        msg = msg.line(then);
    }
    // A writer's own sentence names the raw tab and a CLI's flags: in a
    // person's words where its kind is known (round 21: those rows painted
    // nothing at any width, beside siblings that did), whole behind Details.
    let msg = if plain {
        msg
    } else {
        match refusal_words(why) {
            Some(words) => {
                let mut msg = msg;
                msg.detail.insert(0, words.to_string());
                msg
            }
            None => msg.no_excerpt(),
        }
    };
    // Another step held the lock: the press itself is the remedy, so the row
    // carries the very capsule that was pressed.
    if again && !tab.is_empty() && !to.is_empty() {
        msg.action(Intent::AgentUpgrade {
            tab: tab.to_string(),
            to: to.to_string(),
            word,
        })
    } else {
        msg
    }
}

/// A writer's refusal of the owner's word in a person's words, by its kind
/// (ruling 307); `None` for one this build does not know.
fn refusal_words(why: &str) -> Option<&'static str> {
    if why.contains("is already under way") {
        Some("its restart is already under way")
    } else if why.contains("stopped after its `/exit`") {
        Some("it already quit: codex resume in the tab takes it back")
    } else if why.starts_with("no upgrade is recorded") {
        Some("no upgrade waits in that tab now")
    } else if why.contains("stopped for good") || why.contains("re-arms only one that gave up") {
        Some("its upgrade stopped and was not asked again")
    } else {
        None
    }
}

/// The finished words of a stall row whose upgrade went through after all
/// (day five, D26): `Claude upgraded in tab 1` — the `✓` record's title in
/// the log, where the row's `⚠ Couldn't upgrade …` read as a failure beside
/// the success record.
pub(crate) fn agent_upgrade_went_through(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> String {
    format!("{} upgraded {place}", agent_word(row.agent))
}

/// THE LIVE AGENT UPGRADE, DONE: the owner's outcome line — the build the
/// session resumed on and the model its first answer named, or that the model
/// could not be confirmed ([`aterm_agent::harness::upgrade::restart_outcome`])
/// — as a RECORD. Until 2026-09-24 it went only to the ledger's JSONL, so a
/// model that changed across the move was news nobody was told.
pub(crate) fn agent_upgrade_done(
    row: &aterm_agent::harness::upgrade_drive::Row,
    place: &str,
) -> Message {
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 160);
    // The outcome opens `claude restarted on <to> · model …` (a Codex move's
    // `codex on <to> · <what came back>`); the title says where it moved, so
    // the detail carries the half after it.
    let model = row
        .outcome
        .split_once(" · ")
        .map_or(row.outcome.as_str(), |(_, rest)| rest);
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Success,
        format!("{} moved onto {}", row.agent.product(), clean(&row.to)),
    )
    .line(place);
    if !model.is_empty() {
        msg = msg.line(clean(model));
    }
    msg.hold(Hold::LogOnly)
        .key(&format!("{KEY_AGENT_UPGRADE}.done"))
}

/// R21 — THE FIRST-OPEN INSTALL DOCTOR: this copy runs from somewhere it can
/// neither update itself from nor put `aterm` on a new shell's PATH from,
/// and only the person can fix that. A row, then — a failure they must act
/// on — and `None` for every posture with nothing to fix (a healthy install,
/// a plain binary), whose `remedy()` is `None` too. Warn, because both
/// consequences are silent otherwise; no capsule, because the fix is a drag
/// in the Finder that no page of aterm's performs. The instruction IS the
/// title; `detail[0]` says where it runs; the remedy whole and
/// `aterm_update`'s own summary follow behind Details.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn install_posture(
    posture: aterm_update::which_copy::InstallPosture,
) -> Option<Message> {
    use aterm_update::which_copy::InstallPosture;
    let remedy = posture.remedy()?;
    let where_ = match posture {
        InstallPosture::MountedImage => "running from the disk image",
        InstallPosture::Translocated => "running from a temporary copy",
        InstallPosture::Installed | InstallPosture::NotABundle => return None,
    };
    Some(
        // Filed under `system` (ruling 309): how aterm itself is installed is
        // no ALab tool, and a person filtering for it looked there in vain.
        Message::new(tags::SYSTEM, Severity::Warn, "Move aterm to Applications")
            .line(where_)
            .line(remedy)
            .hold(Hold::For(HOLD_ASK))
            .key(KEY_INSTALL_POSTURE),
    )
}

// ---------------------------------------------------------------------------
// A person's Settings ▸ Packages verb (design ruling 224).
// ---------------------------------------------------------------------------

/// A Settings ▸ Packages verb a person pressed and is WAITING on — so its
/// pass takes the band's animated row whatever it weighs
/// ([`aterm_messages::Waiter::Person`], ruling 220 applied to the packages
/// lane). The row shares the lane's key ([`crate::toolchain_words::KEY_PASS`]):
/// once the pass writes its plan, the tailer's reads restate it with the fill,
/// the ETA and `3 of 10 programs`, exactly as a first run's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesVerb {
    /// `Check & Update Now` (`atpkg update`).
    Check,
    /// `Install ALab Tools Now` (`atpkg install --default-set`).
    Install,
    /// `Remove ALab Tools` (`atpkg uninstall --all`).
    Remove,
}

impl PackagesVerb {
    /// The row's title while the verb's own work runs, before (or without) a
    /// plan: what it is doing, in the lane's one noun.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Check => "Checking ALab tools",
            Self::Install => crate::toolchain_words::INSTALLING_ALAB,
            Self::Remove => "Removing ALab tools",
        }
    }

    /// The words its Complete echo shows when the verb ends well.
    pub(crate) const fn finished(self) -> &'static str {
        match self {
            Self::Check => crate::toolchain_words::PACKAGES_FINISHED,
            Self::Install => crate::toolchain_words::PACKAGES_INSTALLED,
            Self::Remove => "ALab tools removed",
        }
    }

    /// The words its Complete echo shows once the pass planned work — an
    /// update the Check found, the set an Install lays down.
    pub(crate) const fn finished_work(self) -> &'static str {
        match self {
            Self::Check => "ALab tools updated",
            Self::Install | Self::Remove => self.finished(),
        }
    }
}

/// The title while a person's verb is queued behind another `atpkg` pass at
/// the store lock (up to its 30-minute bound): what they are waiting for.
pub(crate) const WAITING_FOR_ALAB: &str = "Waiting for another ALab update";

/// A person's verb row before its pass plans anything: BUSY (no fraction is
/// known yet; the band adds the elapsed clock past ten seconds), keyed to the
/// lane's pass, revealed after [`aterm_messages::PROGRESS_GRACE`] so a no-op
/// Check (1–4 s, measured in `packages.log`) never reaches the glass, and
/// ending in its [`PackagesVerb::finished`] words. No capsule: the person is
/// on the page that pressed it (ruling 101). A removal declares the disk as
/// its load — deleting gigabytes of toolchains is the heavy part.
pub(crate) fn packages_verb_row(verb: PackagesVerb) -> Message {
    let (glyph, load) = match verb {
        PackagesVerb::Check => ('\u{21bb}', None),
        PackagesVerb::Install => ('\u{21e3}', None),
        // A drawn removal mark (ruling 304): the `·` read as a bullet.
        PackagesVerb::Remove => ('\u{2296}', Some(aterm_messages::Load::Disk)),
    };
    // The work's own load (ruling 246): a check or an install downloads, a
    // removal deletes — their words show only while other work is slowed.
    let primary = match verb {
        PackagesVerb::Check | PackagesVerb::Install => aterm_messages::Load::Network,
        PackagesVerb::Remove => aterm_messages::Load::Disk,
    };
    Message::new(tags::TOOLCHAIN, Severity::Info, verb.title())
        .glyph(Glyph::or_fallback(glyph))
        .loads(crate::toolchain_words::PASS_LOADS)
        .primary_load(primary)
        .no_excerpt()
        .meter(aterm_messages::Meter {
            load,
            ..aterm_messages::Meter::busy("")
        })
        .hold(Hold::Live {
            stale_after: aterm_messages::STALE_ANNOUNCE,
        })
        .key(crate::toolchain_words::KEY_PASS)
        .finished_as(verb.finished())
        .reveal_after(aterm_messages::PROGRESS_GRACE)
}

/// A person's verb QUEUED at the store lock behind another pass (atpkg's
/// `lock-waiting:`, which used to be a log line only): busy with the elapsed
/// clock, held for the wait's own bound and a margin, and restated back to
/// [`packages_verb_row`] when the lock is taken. A holder whose plan the
/// tailer reads restates it with that pass's fill — the work they wait
/// behind, and how long it has left.
pub(crate) fn packages_waiting_row(verb: PackagesVerb) -> Message {
    Message::new(tags::TOOLCHAIN, Severity::Info, WAITING_FOR_ALAB)
        .glyph(Glyph::or_fallback('\u{23f8}'))
        .loads(crate::toolchain_words::PASS_LOADS)
        .no_excerpt()
        .meter(aterm_messages::Meter::busy(""))
        .hold(Hold::Live {
            stale_after: Duration::from_secs(crate::ATPKG_WAIT_LOCK_SECS) + Duration::from_mins(5),
        })
        .key(crate::toolchain_words::KEY_PASS)
        .finished_as(verb.finished())
        .reveal_after(aterm_messages::PROGRESS_GRACE)
}

/// R22 — the first UI-made session connection (`connections::first_use_*`):
/// a DISCLOSURE — the tab's own mark and menu already show the connection
/// and its Disconnect — so a RECORD, never a row. `text` is the connections
/// module's sentence (`⇆ Session connection created — <direction>;
/// Disconnect … undoes it`): the pictogram goes (the band's glyph column is
/// the record's), the head is the title, and each `; ` clause of the tail a
/// detail line.
pub(crate) fn session_connection_created(text: &str) -> Message {
    let text = text.strip_prefix("\u{21c6} ").unwrap_or(text);
    let (title, tail) = text.split_once(" \u{2014} ").unwrap_or((text, ""));
    Message::new(tags::SESSION, Severity::Info, title)
        .lines(tail.split("; ").map(str::to_string))
        .hold(Hold::LogOnly)
}

/// One control character, made VISIBLE: the band spends exactly one cell per
/// char, so a control byte in a diagnostic neither draws anything nor
/// honestly occupies the cell it took. `paste_banner::sanitized` spells a
/// hostile clipboard's control bytes this way for the same reason; a config
/// diagnostic is the same hazard arriving from the other direction.
const CONTROL_MARK: char = '\u{00b7}';

/// The PHYSICAL rows of a (possibly multi-line) diagnostic — the port of the
/// retired banner's `notice_display_rows` (upstream e7dc1feee). Blank rows are
/// dropped (a `toml` diagram opens on a bare gutter line, and a row spent on
/// nothing is a row the caret loses), trailing whitespace goes with them, and
/// any control character that survives the split becomes [`CONTROL_MARK`].
/// Leading whitespace is KEPT: the caret row is aligned under the column it
/// points at. Every diagnostic yields at least one row.
fn diagnostic_lines(words: &str) -> Vec<String> {
    let mut rows: Vec<String> = words
        .lines()
        .map(|row| {
            row.trim_end()
                .chars()
                .map(|ch| if ch.is_control() { CONTROL_MARK } else { ch })
                .collect::<String>()
        })
        .filter(|row| !row.trim().is_empty())
        .collect();
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// A feedback sentence's head and tail: the head before the first `: ` and
/// the tail after it — `Robi was not dismissed: <error>` reads as the head
/// and the cause — or the whole sentence and nothing when it carries no
/// colon (`Message::line` drops an empty line).
fn split_sentence(words: &str) -> (&str, &str) {
    words.split_once(": ").unwrap_or((words, ""))
}

// A glass title fits well inside the engine's title cap: the cap bounds a
// record's words, the glass rule a row's.
const _: () = assert!(GLASS_TITLE_CHARS < TITLE_CAP);

#[cfg(test)]
mod tests {
    /// THE STALLED ROW NAMES THE PROCESSES THAT HOLD THE MOVE (2026-09-27: the
    /// agent's notice named the shells under it, and the owner's row said only
    /// `its own work runs` — the pids reached the owner in the give-up's
    /// ledger row alone, after four notices). A line of its own, after the
    /// tab and the move ([`super::STALL_MOVE_LINE`] stays where the host's
    /// dedupe reads it) and before the shell spelling, still the last,
    /// technical line (ruling 270): by pid, name and age — never a command,
    /// the agent's own words — within the detail line's cap. NEGATIVE
    /// CONTROL: nothing held, no line.
    #[test]
    fn a_stalled_upgrade_row_names_the_processes_that_hold_it() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::{HeldBy, Row};
        const NOW: u64 = 1_790_311_076;
        let shell = |pid: u32| HeldBy {
            pid,
            name: "zsh".into(),
            since: NOW - (5 * 86_400 + 4 * 3_600),
        };
        let row = Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: Phase::Announced {
                at_s: NOW - 3_600,
                asks: 2,
            },
            behind_since: NOW - 7 * 3_600,
            wait: "background".into(),
            held_by: vec![shell(63_492), shell(63_493)],
            ..Row::default()
        };
        let msg = super::agent_upgrade_stalled(&row, NOW, "in tab 2");
        assert_eq!(msg.detail.len(), 5, "{:?}", msg.detail);
        assert_eq!(
            msg.detail[3],
            "held by pid 63492 (zsh, 5d4h); pid 63493 (zsh, 5d4h)"
        );
        assert!(
            msg.detail[super::STALL_MOVE_LINE].starts_with("in tab 2 \u{00b7} "),
            "{:?}",
            msg.detail
        );
        assert!(
            msg.detail[4].starts_with("the same in any shell: "),
            "{:?}",
            msg.detail
        );
        let many = Row {
            held_by: (1..=9).map(|p| shell(4_000_000 + p)).collect(),
            ..row.clone()
        };
        let msg = super::agent_upgrade_stalled(&many, NOW, "in tab 2");
        assert!(msg.detail[3].ends_with("; and 4 more"), "{:?}", msg.detail);
        assert!(msg.detail[3].chars().count() <= aterm_messages::DETAIL_LINE_CAP);
        let free = Row {
            held_by: Vec::new(),
            ..row
        };
        let msg = super::agent_upgrade_stalled(&free, NOW, "in tab 2");
        assert_eq!(msg.detail.len(), 4, "{:?}", msg.detail);
        assert!(msg.detail.iter().all(|l| !l.starts_with("held by")));
    }

    /// "ASKED AGAIN EVERY 30 MINUTES" IS NEVER A FIXED PHRASE (design record
    /// 2026-09-28, §1.4): tab #1's row said it for 28 hours in which nothing
    /// asked. The words come from the record: when the latest notice was
    /// typed, with its day once that is not today; while the re-ask is not
    /// yet due, the time it becomes due, taken at the next break or idle
    /// point; once due, only that the next point asks — never an interval;
    /// and on the last notice, that the next point gives up. What would have
    /// caught the old phrase: every form below says when it was last asked
    /// and none says "every".
    #[test]
    fn the_asked_again_words_come_from_the_record_never_a_cadence() {
        use aterm_agent::harness::upgrade::{MAX_ASKS, REASK_S};
        // 2026-09-27 17:48:00 UTC; the zone at UTC and at PDT (-7 h).
        const AT: u64 = 1_790_531_280;
        let words = |noticed: u64, clock: u64, now: u64, offset: Option<i64>| {
            super::asked_again_words(noticed, clock, 2, now, offset)
        };
        assert_eq!(
            words(AT, AT, AT + 600, Some(0)),
            ", last asked 5:48 PM, asked again at its next break or idle point from 6:18 PM"
        );
        assert_eq!(
            words(AT, AT, AT + 600, Some(-7 * 3_600)),
            ", last asked 10:48 AM, asked again at its next break or idle point from 11:18 AM"
        );
        // Due: no promise of when.
        assert_eq!(
            words(AT, AT, AT + REASK_S, Some(0)),
            ", last asked 5:48 PM; asks again at its next break or idle point"
        );
        // A DAY ON (review of 2026-09-28): tab #1's notice, read 28 hours
        // later, is yesterday's — a bare `5:48 PM` under today's header read
        // as this afternoon. Further back, the day as a header says it.
        assert_eq!(
            words(AT, AT, AT + 28 * 3_600, Some(0)),
            ", last asked yesterday 5:48 PM; asks again at its next break or idle point"
        );
        assert!(
            words(AT, AT, AT + 3 * 86_400, Some(0)).starts_with(", last asked Sunday 5:48 PM;"),
            "{}",
            words(AT, AT, AT + 3 * 86_400, Some(0))
        );
        assert!(
            words(AT, AT, AT + 30 * 86_400, Some(0))
                .starts_with(", last asked Sun, 27 Sep, 5:48 PM;"),
            "{}",
            words(AT, AT, AT + 30 * 86_400, Some(0))
        );
        // No zone: said as ages.
        assert_eq!(
            words(AT, AT, AT + 600, None),
            ", last asked 10m ago, asked again at its next break or idle point in 20m or later"
        );
        assert_eq!(
            words(AT, AT, AT + 28 * 3_600, None),
            ", last asked 1d4h ago; asks again at its next break or idle point"
        );
        // An unknown notice time names none.
        assert_eq!(
            words(0, 0, AT, Some(0)),
            ", asked again at its next break or idle point"
        );
        // A LIMIT'S EPISODE MOVES THE CLOCK, NOT THE NOTICE (review of
        // 2026-09-28): typed at 17:48, the limit's episode closed at 21:58
        // and held the clock to then. The row names the notice's time, and
        // the re-ask's due time from the held clock.
        let held = AT + 4 * 3_600 + 600;
        assert_eq!(
            words(AT, held, held + 60, Some(0)),
            ", last asked 5:48 PM, asked again at its next break or idle point from 10:28 PM"
        );
        // THE LAST NOTICE (same review): the step past its clock is the
        // give-up, never a fifth notice.
        assert_eq!(
            super::asked_again_words(AT, AT, MAX_ASKS, AT + 600, Some(0)),
            ", last asked 5:48 PM (the last of 4 notices); gives up at its next break or \
             idle point from 6:18 PM"
        );
        assert_eq!(
            super::asked_again_words(AT, AT, MAX_ASKS, AT + REASK_S, Some(0)),
            ", last asked 5:48 PM (the last of 4 notices); gives up at its next break or \
             idle point"
        );
        for now in [AT + 600, AT + REASK_S, AT + 28 * 3_600] {
            for offset in [Some(0), None] {
                for asks in [1, MAX_ASKS] {
                    let w = super::asked_again_words(AT, AT, asks, now, offset);
                    assert!(!w.contains("every"), "{w}");
                    assert!(w.contains("last asked"), "{w}");
                    assert_eq!(w.contains("gives up"), asks == MAX_ASKS, "{w}");
                }
            }
        }
    }

    /// THE STALLED ROW NAMES THE REMEDY THAT MOVES ITS KIND OF STALL (review
    /// of 2026-09-25: `--now` was named for all five kinds, and it moves two —
    /// a held-back agent is never reached, a refused or failed one stays
    /// stopped). Each wording pinned, and `--now` named only where it works.
    #[test]
    fn a_stalled_upgrade_row_names_the_remedy_for_its_kind() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        let tab = "s-b5cf2faabac5ce5127bd";
        let base = Row {
            tab: tab.into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: Phase::Pending,
            behind_since: NOW - 60,
            noticed_at: NOW - 3_600,
            ..Row::default()
        };
        let remedy = |phase: Phase, wait: &str, behind: u64| {
            // Round 18 (D2/D4): a move still waiting says so; one that
            // stopped or cannot be reached could not be done. Ruling 380
            // (the owner, 2026-09-28): never `… upgrade waits in tab 2` — a
            // waiting row is shown only once the ladder's last rung has
            // stood for hours, and then it could not be done YET.
            let waits = !matches!(phase, Phase::Failed(_)) && !wait.starts_with("terminal:");
            // Ruling 283: a round that stopped once (not refused) or gave
            // up asks again on its own — it retries later.
            let resting = matches!(&phase, Phase::Failed(why)
                if why != "not-a-shell-job" && !why.starts_with("argv:"));
            let msg = super::agent_upgrade_stalled(
                &Row {
                    phase,
                    wait: wait.into(),
                    behind_since: NOW - behind,
                    ..base.clone()
                },
                NOW,
                "in tab 2",
            );
            assert_eq!(
                msg.title,
                if resting {
                    "Claude upgrade retries later in tab 2"
                } else if waits {
                    "Couldn't upgrade Claude in tab 2 yet"
                } else {
                    "Couldn't upgrade Claude in tab 2"
                },
                "{wait}"
            );
            assert_eq!(msg.detail.len(), 4, "{:?}", msg.detail);
            // Ruling 270: the tab in the person's words, the raw sid only on
            // the last, technical line.
            assert!(
                msg.detail[2].starts_with("in tab 2 \u{00b7} "),
                "{:?}",
                msg.detail
            );
            assert!(
                msg.detail[..3]
                    .iter()
                    .all(|l| !l.contains(tab) && !l.contains('`') || l.contains("codex resume")),
                "{:?}",
                msg.detail
            );
            (msg.detail[1].clone(), msg.detail[3].clone())
        };
        let shell =
            |words: &str| format!("the same in any shell: `aterm harness upgrade {tab} {words}`");
        // Overdue on a turn still running (ruling 380): hours on the ladder's
        // last rung, which `--now` is too — it is not named as moving it, and
        // never "at its next turn end" (a goal-mode session's turns never
        // end: the owner's tab, 2026-09-28).
        assert_eq!(
            remedy(Phase::Pending, "not-idle:busy", 7 * 3_600),
            (
                "Upgrade now does not move it past that wait: it moves once that ends; Skip \
                 version keeps it on 2.1.281"
                    .to_string(),
                shell("--skip")
            ),
            "overdue: the row's own buttons, the shell's spelling last"
        );
        // What `--now` still moves: the lane's own text left typed, tried
        // again at once.
        assert_eq!(
            remedy(Phase::Pending, "left-typed-backoff", 7 * 3_600),
            (
                "Upgrade now tries it again at its next pause; Skip This Version in the tab's \
                 menu keeps it on 2.1.281"
                    .to_string(),
                shell("--now` or `--skip")
            ),
            "overdue, left typed"
        );
        assert_eq!(
            remedy(Phase::Failed("unanswered".into()), "", 60),
            (
                "Upgrade now asks it again at its next pause; Skip This Version in the tab's \
                 menu keeps it on 2.1.281"
                    .to_string(),
                shell("--now` or `--skip")
            ),
            "gave up"
        );
        // A wait no rung passes (ruling 380, `blocked:*`): what is wrong, said
        // plainly, and the one way it moves — having blocked the move a
        // re-ask's interval, whatever other wait the last look found.
        let blocked = super::agent_upgrade_stalled(
            &Row {
                wait: "daemon-turn".into(),
                wait_since: NOW - 60,
                blocked: "no-shell-integration".into(),
                blocked_since: NOW - 1_800,
                agent: aterm_agent::harness::upgrade::Agent::Codex,
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert_eq!(blocked.title, "Couldn't upgrade Codex in tab 2");
        assert_eq!(
            (blocked.severity, blocked.hold),
            (Severity::Warn, Hold::Standing)
        );
        assert!(
            blocked.detail[0].starts_with(
                "its tab's shell integration is not reaching aterm (as after an aterm update)"
            ),
            "{:?}",
            blocked.detail
        );
        assert_eq!(
            blocked.detail[1],
            "to move it now, quit Codex in its tab and resume it with the `codex resume` line it \
             prints; Skip version keeps it on 2.1.281"
        );
        let unreadable = super::agent_upgrade_stalled(
            &Row {
                wait: "screen-unreadable".into(),
                wait_since: NOW - 1_800,
                blocked: "screen-unreadable".into(),
                blocked_since: NOW - 1_800,
                agent: aterm_agent::harness::upgrade::Agent::Codex,
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert_eq!(
            unreadable.detail[1],
            "to move it now, quit it in its tab and resume it there; Skip version keeps it on \
             2.1.281"
        );
        // The agent's own work under it (2026-09-26: a tab behind poll loops
        // that could never end read "it moves once that ends"). `--now` is not
        // named as moving it. The agent stopping the work does. When it was
        // last asked is the record's, and no cadence is promised (design
        // record 2026-09-28: "asked again every 30 minutes" stood over a tab
        // nothing had asked for 28 hours).
        for wait in ["background", "not-idle:shell"] {
            let working = remedy(
                Phase::Announced {
                    at_s: NOW - 3_600,
                    asks: 2,
                },
                wait,
                7 * 3_600,
            );
            let asked = super::asked_again_words(
                NOW - 3_600,
                NOW - 3_600,
                2,
                NOW,
                crate::presence::local_offset_s(),
            );
            assert!(
                asked.ends_with("; asks again at its next break or idle point"),
                "{asked}"
            );
            assert_eq!(
                working,
                (
                    format!(
                        "Upgrade now cannot move it past its own work: it moves once that ends \
                         or the agent stops it{asked}; Skip version keeps it on 2.1.281"
                    ),
                    shell("--skip")
                ),
                "{wait}"
            );
            assert!(working.0.chars().count() <= aterm_messages::DETAIL_LINE_CAP);
            assert!(!working.0.contains("now moves"), "{working:?}");
            // The old fixed phrase is gone.
            assert!(!working.0.contains("every 30 minutes"), "{working:?}");
        }
        // Before any notice, nothing is said of asking again.
        let (pending, _) = remedy(Phase::Pending, "not-idle:shell", 7 * 3_600);
        assert!(
            pending.contains("its own work") && !pending.contains("asked again"),
            "{pending}"
        );
        let pane = remedy(Phase::Pending, "terminal:tmux", 60);
        assert_eq!(
            pane,
            (
                "to move it, quit it in its tmux pane and resume it there; Skip version keeps \
                 it on 2.1.281"
                    .to_string(),
                shell("--skip")
            ),
            "held back"
        );
        for why in [
            "not-a-shell-job",
            "argv:--print",
            "no-resume",
            "resumed-elsewhere",
        ] {
            let by_hand = remedy(Phase::Failed(why.into()), "", 60);
            assert_eq!(
                by_hand,
                (
                    "the upgrade asks it again after a rest; to move it sooner, quit it and \
                     resume it by hand; Skip version keeps it on 2.1.281"
                        .to_string(),
                    shell("--skip")
                ),
                "{why}"
            );
        }
        assert!(
            !pane.0.contains("Upgrade now") && !pane.1.contains("--now"),
            "{pane:?}"
        );
    }

    /// RULING 283, THE AUDIT'S REAL CASE: a round that gave up, is resting,
    /// and is more than six hours behind reads `overdue` while its step waits
    /// `failed`. It asks again on its own, so it is a RECORD with no tab mark,
    /// it offers `Upgrade now` (which re-arms it at once), and its words are
    /// true: no "cannot move it", no bare `behind for 8h`, the next round's
    /// time where the clock is known. NEGATIVE CONTROLS: the same stop
    /// repeating is a row; a refusal is a row.
    #[test]
    fn a_resting_round_that_gave_up_is_a_record_that_offers_upgrade_now() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::{Remedy, Row};
        use aterm_messages::UpgradeWord;
        const NOW: u64 = 1_790_311_076;
        let resting = Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: Phase::Failed("unanswered".into()),
            wait: "failed".into(),
            behind_since: NOW - 8 * 3_600,
            retry_at: NOW + 3_000,
            stop_streak: 1,
            streak_why: "unanswered".into(),
            ..Row::default()
        };
        assert_eq!(resting.stall(NOW).as_deref(), Some("overdue"));
        assert_eq!(resting.remedy(NOW), Some(Remedy::AskAgain));
        assert!(resting.asks_on_its_own(NOW));
        let msg = super::agent_upgrade_stalled(&resting, NOW, "in tab 2");
        assert_eq!(msg.title, "Claude upgrade retries later in tab 2");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
        assert_eq!(
            msg.detail[0],
            "behind for 8 h: it has not agreed to the move yet, so the upgrade rests, then asks \
             again"
        );
        assert!(
            msg.detail[1].contains("Upgrade now asks it"),
            "{:?}",
            msg.detail
        );
        if crate::presence::local_offset_s().is_some() {
            assert!(
                msg.detail[1].starts_with("it asks again on its own at "),
                "{:?}",
                msg.detail
            );
        }
        // Ruling 314 (day eight E2): the why AND the next step are the
        // sentence Settings ▸ Messages sets for a person; the move and the
        // shell spelling are its technical lines. A refused press restated
        // above them joins the sentence; a record with no move line, or no
        // tab's key (the loss row, the waiting record), keeps one line.
        let key = msg.key.as_deref();
        assert_eq!(
            super::sentence_lines(key, &msg.detail),
            2,
            "{:?}",
            msg.detail
        );
        let mut refused = msg.detail.clone();
        refused.insert(0, "the upgrade no longer takes that word".to_string());
        assert_eq!(super::sentence_lines(key, &refused), 3);
        assert_eq!(super::sentence_lines(None, &msg.detail), 1, "no key");
        assert_eq!(
            super::sentence_lines(Some(KEY_AGENT_UPGRADE), &msg.detail),
            1
        );
        assert_eq!(
            super::sentence_lines(key, &msg.detail[..2]),
            1,
            "no move line"
        );
        assert!(
            msg.detail
                .iter()
                .all(|l| !l.contains("cannot move") && !l.contains("does not move")),
            "{:?}",
            msg.detail
        );
        assert!(
            msg.actions.iter().any(|a| matches!(
                a,
                Intent::AgentUpgrade {
                    word: UpgradeWord::Now,
                    ..
                }
            )),
            "{:?}",
            msg.actions
        );

        // The same stop twice in a row is the person's: a row.
        let repeating = Row {
            phase: Phase::Failed("no-resume".into()),
            wait: "failed".into(),
            stop_streak: 2,
            streak_why: "no-resume".into(),
            ..resting.clone()
        };
        let row = super::agent_upgrade_stalled(&repeating, NOW, "in tab 2");
        assert_eq!((row.severity, row.hold), (Severity::Warn, Hold::Standing));
        assert_eq!(row.title, "Couldn't upgrade Claude in tab 2");
        // Day five, D25: it says when the next round is, as the first stop's
        // record did; D24: in a person's words, never `the harness`.
        assert!(
            row.detail[1]
                .starts_with("it stopped this way 2 times in a row; the upgrade asks it again "),
            "{:?}",
            row.detail
        );
        if crate::presence::local_offset_s().is_some() {
            assert!(
                row.detail[1].contains("asks it again at "),
                "{:?}",
                row.detail
            );
        }
        assert!(
            row.detail[..3]
                .iter()
                .chain(msg.detail[..3].iter())
                .all(|l| !l.contains("harness") && !l.contains("READY")),
            "{:?} {:?}",
            row.detail,
            msg.detail
        );
        // A refusal is met again by the next round: a row from the first.
        let refused = Row {
            phase: Phase::Failed("not-a-shell-job".into()),
            stop_streak: 1,
            streak_why: "not-a-shell-job".into(),
            ..resting
        };
        let row = super::agent_upgrade_stalled(&refused, NOW, "in tab 2");
        assert_eq!(row.hold, Hold::Standing);
    }

    /// RULING 283: every way a move stops reads in a person's words — the
    /// row's first line carries no raw stop word and nothing in parentheses
    /// (it read `the move stopped (signal-refused)`).
    #[test]
    fn every_stop_reason_reads_in_a_persons_words() {
        use aterm_agent::harness::upgrade::{Agent, Phase};
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        for agent in [Agent::Claude, Agent::Codex] {
            for why in [
                "signal-refused",
                "no-resume",
                "stale-exit",
                "shell-gone",
                "exited-before-continuing",
                "relaunch-refused",
                "resumed-elsewhere",
                "some-future-reason",
            ] {
                let row = Row {
                    agent,
                    tab: "s-b5cf2faabac5ce5127bd".into(),
                    from: "2.1.281".into(),
                    to: "2.1.282".into(),
                    phase: Phase::Failed(why.into()),
                    stop_streak: 2,
                    streak_why: why.into(),
                    behind_since: NOW - 60,
                    ..Row::default()
                };
                let msg = super::agent_upgrade_stalled(&row, NOW, "in tab 2");
                let first = &msg.detail[0];
                assert!(
                    !first.contains('(') && !first.contains(why) && !first.contains('`'),
                    "{agent:?} {why}: {first}"
                );
            }
        }
    }

    /// RULING 283: a press that reaches a round which stopped between the
    /// offer and the press is worded from the writer's typed refusal — the
    /// tab by its place, the next round's time, the row's own `Skip version` —
    /// never the CLI's sentence (a raw `s-…` id, backticked flags, a stop
    /// word). NEGATIVE CONTROL: a refusal the window cannot type stays whole
    /// behind Details, never the band's excerpt.
    #[test]
    fn a_press_on_a_round_that_stopped_is_worded_without_the_clis_sentence() {
        use aterm_agent::harness::upgrade::Agent;
        use aterm_messages::UpgradeWord;
        let why = "stopped:0:the upgrade in tab s-b5cf2faabac5ce5127bd stopped (no-resume): \
                   `--now` re-arms only one that gave up";
        let msg = agent_upgrade_word_refused(
            UpgradeWord::Now,
            Agent::Claude,
            ("s-b5cf2faabac5ce5127bd", "2.1.282"),
            why,
            "in tab 2",
        );
        assert_eq!(msg.title, "Couldn't upgrade Claude in tab 2 now");
        assert!(msg.excerpt);
        // The remedy first (ruling 307).
        assert_eq!(
            msg.detail,
            [
                "retries soon",
                "the upgrade stopped before the press reached it"
            ]
        );
        for raw in ["s-b5", "`", "no-resume", "gave up", "("] {
            assert!(
                msg.detail.iter().all(|l| !l.contains(raw)),
                "{raw}: {:?}",
                msg.detail
            );
        }
        let other = agent_upgrade_word_refused(
            UpgradeWord::Now,
            Agent::Claude,
            ("s-1", "2.1.282"),
            "no upgrade is recorded for tab s-1; `aterm harness upgrade s-1 --dry-run` shows it",
            "in tab 2",
        );
        // A known kind of the writer's sentence is painted in a person's
        // words (ruling 307), the sentence whole behind them.
        assert!(other.excerpt);
        assert_eq!(other.detail[0], "no upgrade waits in that tab now");
        assert!(other.detail[1].contains("s-1"), "{:?}", other.detail);
        // NEGATIVE CONTROL: a sentence of no known kind stays behind Details.
        let unknown = agent_upgrade_word_refused(
            UpgradeWord::Now,
            Agent::Claude,
            ("s-1", "2.1.282"),
            "something else went wrong in tab s-1",
            "in tab 2",
        );
        assert!(
            !unknown.excerpt,
            "the CLI's own sentence waits behind Details"
        );
        // Another step held the lock: the row carries the press itself.
        let busy = agent_upgrade_word_refused(
            UpgradeWord::NotToday,
            Agent::Claude,
            ("s-1", "2.1.282"),
            "busy:another-sweep",
            "in tab 2",
        );
        assert_eq!(busy.title, "Couldn't postpone the upgrade in tab 2");
        assert_eq!(busy.detail[0], "press it again", "{:?}", busy.detail);
        assert_eq!(
            busy.actions,
            [Intent::AgentUpgrade {
                tab: "s-1".into(),
                to: "2.1.282".into(),
                word: UpgradeWord::NotToday,
            }]
        );
    }

    /// A REFUSED PRESS PAINTS ITS REMEDY AT 80 COLUMNS, whichever word was
    /// pressed (ruling 307(a), review of round 21: every `Couldn't postpone
    /// Claude's upgrade in tab 2` row painted its title alone at 80 while its
    /// `Couldn't upgrade Claude in tab 2 now` siblings painted the remedy).
    /// A target that changed says so and promises no move (`a newer build:
    /// 2.1.283`, never `it moves to 2.1.283 now`); a round that stopped says
    /// when it retries, at its widest clock words; a lock held elsewhere
    /// paints the pressed capsule, the press being the remedy. NEGATIVE
    /// CONTROL: the old 43-cell title paints no excerpt at that width.
    #[test]
    fn a_refused_press_paints_its_remedy_at_80_columns() {
        use aterm_agent::harness::upgrade::Agent;
        use aterm_messages::UpgradeWord;
        let painted = |msg: &Message| {
            let mut app = crate::App::headless_for_test();
            app.post_message(msg.clone());
            let row = app.band_presentation(80).rows[0].clone();
            (
                row.detail.map(|(_, d)| d),
                row.capsules
                    .iter()
                    .map(|c| c.text.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let refused = |word, why: &str| {
            agent_upgrade_word_refused(word, Agent::Claude, ("s-1", "2.1.282"), why, "in tab 2")
        };
        let stopped = "stopped:1790400000:the upgrade in tab s-1 stopped (no-resume): \
                       `--now` re-arms only one that gave up";
        for word in [UpgradeWord::Now, UpgradeWord::NotToday, UpgradeWord::Skip] {
            let stale = refused(word, "stale:2.1.283");
            assert_eq!(stale.detail[0], "a newer build: 2.1.283");
            assert!(
                stale.detail.iter().all(|l| !l.contains("moves")),
                "{:?}",
                stale.detail
            );
            assert_eq!(painted(&stale).0.as_deref(), Some("a newer build: 2.1.283"));
            let mut retry = refused(word, stopped);
            assert!(
                retry.detail[0].starts_with("retries "),
                "{:?}",
                retry.detail
            );
            "retries at 11:37 PM".clone_into(&mut retry.detail[0]);
            assert_eq!(painted(&retry).0.as_deref(), Some("retries at 11:37 PM"));
            let busy = refused(word, "busy:another-sweep");
            assert_eq!(
                painted(&busy).1.first().map(String::as_str),
                Some(word.label()),
                "{:?}",
                busy.title
            );
        }
        // NEGATIVE CONTROL: the title ruling 307 first chose.
        let mut old = refused(UpgradeWord::NotToday, "stale:2.1.283");
        "Couldn't postpone Claude's upgrade in tab 2".clone_into(&mut old.title);
        assert_eq!(painted(&old).0, None);
    }

    /// AN UPGRADE STILL ASKING ON ITS OWN IS A RECORD (round 18, day four,
    /// D5): an announced move on the agent's own work is asked again every
    /// half hour and ends in a give-up of its own, so it stands on no glass —
    /// and so, since ruling 380 (the owner, 2026-09-28), does an announced
    /// move whose turn is still running: `--now` is the ladder's last rung,
    /// where it has stood for hours, and moves it no sooner. NEGATIVE
    /// CONTROL: the same waits before any notice are rows.
    #[test]
    fn an_upgrade_still_asking_on_its_own_is_a_record() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        let row = |phase: Phase, wait: &str| Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase,
            wait: wait.into(),
            behind_since: NOW - 7 * 3_600,
            ..Row::default()
        };
        let asked = Phase::Announced {
            at_s: NOW - 600,
            asks: 1,
        };
        let msg = super::agent_upgrade_stalled(&row(asked.clone(), "background"), NOW, "in tab 2");
        assert_eq!(msg.hold, Hold::LogOnly);
        assert_eq!(msg.severity, Severity::Info);
        // A usage limit ends by itself before a notice as after one (ruling
        // 307): a record in either phase, never a warn row for days.
        for phase in [Phase::Pending, asked.clone()] {
            let limited = row(phase, "limited");
            assert!(limited.asks_on_its_own(NOW), "{:?}", limited.phase);
            let msg = super::agent_upgrade_stalled(&limited, NOW, "in tab 2");
            assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
        }
        let turn = super::agent_upgrade_stalled(&row(asked, "not-idle:busy"), NOW, "in tab 2");
        assert_eq!((turn.severity, turn.hold), (Severity::Info, Hold::LogOnly));
        for wait in ["background", "not-idle:busy"] {
            let msg = super::agent_upgrade_stalled(&row(Phase::Pending, wait), NOW, "in tab 2");
            assert_eq!(msg.hold, Hold::Standing, "{wait}");
        }
    }

    /// THE OWNER'S `Upgrade now` IS ANSWERED ON THE GLASS (2026-09-28: "when I
    /// pressed 'update' in the claude version update button, nothing seemed
    /// to happen?"). The press answers its row in place, and `--now` quiets
    /// the stall for `NOW_QUIETS_S`; after that the host posts the tab's
    /// stall again, and a move waiting on the agent's own work — which
    /// `--now` does not waive — was a record posted and folded in the same
    /// millisecond (messages.log, ids 64 and 66), withdrawing the answered
    /// row under the same key. The owner's word still stood; nothing on the
    /// glass said so. While it stands the answer is a calm standing row that
    /// says what it waits on; with no word it stays a record (D5).
    ///
    /// FAILS WITHOUT THE FIX: the hurried row is `LogOnly`, as the unasked
    /// one is.
    #[test]
    fn the_owners_upgrade_now_is_answered_on_the_glass() {
        use aterm_agent::harness::upgrade::{Phase, Request};
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        let row = |request: Request| Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.280".into(),
            to: "2.1.284".into(),
            phase: Phase::Announced {
                at_s: NOW - 600,
                asks: 3,
            },
            wait: "background".into(),
            // Inside the move clock (`MOVE_BUDGET_S`): past it the stall is a
            // row whoever asked (below).
            behind_since: NOW - 8 * 3_600,
            request,
            // Pressed longer ago than `--now` quiets a stall
            // (`NOW_QUIETS_S`, which is `REASK_S`): the stall is back, and so is
            // the host's row.
            request_at: NOW - aterm_agent::harness::upgrade::REASK_S - 60,
            ..Row::default()
        };
        let hurried = row(Request::Now);
        assert!(hurried.asks_on_its_own(NOW), "still the upgrade working");
        let msg = super::agent_upgrade_stalled(&hurried, NOW, "in tab 1");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::Standing));
        assert!(
            msg.detail[1].contains("cannot move it past its own work"),
            "it says why the press did not move it: {:?}",
            msg.detail
        );
        // NEGATIVE CONTROL: no word from the owner, the same wait — a record.
        let msg = super::agent_upgrade_stalled(&row(Request::None), NOW, "in tab 1");
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
    }

    /// THE OWNER'S PRESS IS NOT WITHDRAWN BY THE ROUND'S GIVE-UP (round six,
    /// F7): tab #1, three days behind its own never-ending work, the owner
    /// pressed `Upgrade now`, the round gave up — which spends the word
    /// (`St::give_up`) — and the next post under the tab's key was a log
    /// record that replaced the standing answer: nothing on the glass said
    /// the press was given up on, or that the tab was still behind. Past the
    /// move clock (`MOVE_BUDGET_S`) it is a warn row with its mark, asked or
    /// not.
    ///
    /// FAILS WITHOUT THE FIX: `Hold::LogOnly`.
    #[test]
    fn a_give_up_after_the_owners_press_stays_on_the_glass() {
        use aterm_agent::harness::upgrade::{GAVE_UP, Phase, Request};
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        let hurried = Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.280".into(),
            to: "2.1.284".into(),
            phase: Phase::Announced {
                at_s: NOW - 600,
                asks: 3,
            },
            wait: "background".into(),
            behind_since: NOW - 3 * 86_400,
            request: Request::Now,
            request_at: NOW - aterm_agent::harness::upgrade::REASK_S - 60,
            ..Row::default()
        };
        let msg = super::agent_upgrade_stalled(&hurried, NOW, "in tab 1");
        assert_eq!(msg.hold, Hold::Standing, "the press is answered");
        // What `St::give_up` does: the round stops, the word is spent.
        let gave_up = Row {
            phase: Phase::Failed(GAVE_UP.into()),
            streak_why: GAVE_UP.into(),
            stop_streak: 1,
            wait: "failed".into(),
            request: Request::None,
            request_at: 0,
            ..hurried
        };
        let msg = super::agent_upgrade_stalled(&gave_up, NOW, "in tab 1");
        assert_eq!(
            (msg.severity, msg.hold),
            (Severity::Warn, Hold::Standing),
            "three days behind after the owner's press stays on the glass"
        );
    }

    /// THE STALLED ROW CARRIES THE OWNER'S WORDS THAT MOVE IT (gap #21: the
    /// owner could steer an upgrade only by typing `aterm harness upgrade
    /// <tab> --now|--defer|--skip` into another shell). Two capsules, for
    /// THIS tab and THIS build: `Upgrade now` + `Not today` where `--now`
    /// moves it (gave up — and a healthy wait, the waiting record's, which it
    /// stands at the ladder's last rung), `Not today` + `Skip version` where
    /// it would not (overdue on a turn still running: hours on that rung
    /// already, ruling 380; waiting on the READY answer, in a pane, stopped for good, or already
    /// hurried by the owner's `--now` — a second press would only arm a new
    /// round); none once
    /// a restart is under way or done, where the harness refuses a word. The
    /// tab menu takes all three words, from the same table.
    #[test]
    fn a_stalled_upgrade_row_carries_the_words_that_move_its_kind() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        use aterm_messages::UpgradeWord::{self, NotToday, Now, Skip};
        const NOW: u64 = 1_790_311_076;
        let tab = "s-b5cf2faabac5ce5127bd";
        let row = |phase: Phase, wait: &str, behind: u64| Row {
            tab: tab.into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase,
            wait: wait.into(),
            behind_since: NOW - behind,
            ..Row::default()
        };
        let capsules = |words: &[UpgradeWord]| -> Vec<Intent> {
            words
                .iter()
                .map(|&word| Intent::AgentUpgrade {
                    tab: tab.into(),
                    to: "2.1.282".into(),
                    word,
                })
                .collect()
        };
        for (case, row, words) in [
            (
                "healthy",
                row(Phase::Pending, "not-idle:busy", 60),
                &[Now, NotToday, Skip][..],
            ),
            (
                "overdue",
                row(Phase::Pending, "not-idle:busy", 7 * 3_600),
                &[NotToday, Skip],
            ),
            (
                "gave up",
                row(Phase::Failed("unanswered".into()), "", 60),
                &[Now, NotToday, Skip],
            ),
            (
                "waits on READY",
                row(Phase::Pending, "awaiting-ready", 7 * 3_600),
                &[NotToday, Skip],
            ),
            (
                "in a pane",
                row(Phase::Pending, "terminal:tmux", 60),
                &[NotToday, Skip],
            ),
            (
                "stopped",
                row(Phase::Failed("no-resume".into()), "", 60),
                &[NotToday, Skip],
            ),
            (
                "hurried",
                Row {
                    request: aterm_agent::harness::upgrade::Request::Now,
                    request_at: NOW - 60,
                    ..row(Phase::Pending, "not-idle:busy", 7 * 3_600)
                },
                &[NotToday, Skip],
            ),
            // Ruling 270: the word in force is not offered again — a
            // running deferral drops `Not today`, a skip of this build
            // leaves nothing; a deferral that ran out offers it again.
            (
                "deferred",
                Row {
                    request: aterm_agent::harness::upgrade::Request::DeferUntil(NOW + 3_600),
                    request_at: NOW - 60,
                    ..row(Phase::Pending, "not-idle:busy", 7 * 3_600)
                },
                &[Now, Skip],
            ),
            (
                "deferral ran out",
                Row {
                    request: aterm_agent::harness::upgrade::Request::DeferUntil(NOW - 1),
                    ..row(Phase::Failed("no-resume".into()), "", 60)
                },
                &[NotToday, Skip],
            ),
            (
                "skipped",
                Row {
                    request: aterm_agent::harness::upgrade::Request::Skip("2.1.282".into()),
                    ..row(Phase::Failed("no-resume".into()), "", 60)
                },
                &[],
            ),
            ("under way", row(Phase::Exiting { at_s: NOW }, "", 60), &[]),
            ("done", row(Phase::Done, "", 60), &[]),
        ] {
            assert_eq!(agent_upgrade_words(&row, NOW), words, "{case}");
            assert_eq!(
                agent_upgrade_capsules(&row, NOW),
                capsules(&words[..words.len().min(2)]),
                "{case}"
            );
            if row.stall(NOW).is_some() {
                assert_eq!(
                    agent_upgrade_stalled(&row, NOW, "in tab 2").actions,
                    capsules(&words[..2]),
                    "{case}: the row's capsules"
                );
            }
        }
        // The waiting record's single session is healthy: `Upgrade now` is
        // the accent chip, `Not today` the quiet one.
        let healthy = agent_upgrade_capsules(&row(Phase::Pending, "", 60), NOW);
        assert!(healthy[0].is_consequential() && !healthy[1].is_consequential());
    }

    use super::*;

    fn sentences(msg: &Message) -> Vec<String> {
        std::iter::once(msg.title.clone())
            .chain(msg.detail.iter().cloned())
            .collect()
    }

    /// Every message a launch or reload can post, from fixtures that stand
    /// in for the real producers' words.
    fn every_reporter_message() -> Vec<Message> {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config key_sequences: skipping \"ctrl+a b\": bad chord".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\" — did you mean \"window_padding\"?"
                .into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "columns/lines applies on next launch (resize the window to change size now)".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        warns.push(
            ConfigFamily::Fonts,
            "config font_family_bold: \"Nope\" is not an admissible font (not found); ignored"
                .into(),
        );
        crate::app_config::collect_key_notices(
            &mut warns,
            "game_font = \"chunky\"\nshow_hud = true\n[packages]\nenabled = true\n\
             auto_update = false\nseed_install = true\n",
        );
        let mut all = warns.into_messages();
        // Every family's title, singular and plural, so a re-word of any of
        // them meets the attention gate (audit 2026-09-24).
        for family in [
            ConfigFamily::Keybindings,
            ConfigFamily::IgnoredKeys,
            ConfigFamily::RetiredKeys,
            ConfigFamily::UnacceptedValues,
            ConfigFamily::CursorTrail,
            ConfigFamily::Fonts,
            ConfigFamily::Assets,
            ConfigFamily::Restart,
            ConfigFamily::SecureKeyboard,
        ] {
            for n in [1, 3] {
                let mut one = ConfigWarnings::default();
                for i in 0..n {
                    one.push(family, format!("config sentence {i} for this family"));
                }
                all.extend(one.into_messages());
            }
        }
        all.push(crash_message(&crate::logging::CrashEvidence {
            path: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/crash-1-1.log.seen"),
            head: vec!["aterm-gui 0.1.0 crashed at unix 1.000".into()],
        }));
        all.push(killed_message(&crate::logging::KillEvidence {
            marker: std::path::PathBuf::from(
                "/Users/_an/Library/Logs/aterm/crash-marker-1-1-app.log.seen",
            ),
            log: std::path::PathBuf::from("/Users/_an/Library/Logs/aterm/aterm.log"),
        }));
        for class in [
            crate::crash_journal::DeathClass::Killed,
            crate::crash_journal::DeathClass::Signal,
            crate::crash_journal::DeathClass::Panic,
        ] {
            for reopened in [
                reopened_fixture(class),
                reopened_with_programs(class, &[]),
                reopened_with_programs(class, &[(1, "vim")]),
                reopened_with_programs(class, &[(0, "claude"), (1, "")]),
                reopened_with_agent(class, &[]),
                reopened_with_agent(class, &[(0, "vim")]),
            ] {
                for relaunching in [false, true] {
                    all.push(journal_reopened_message(
                        &reopened,
                        None,
                        None,
                        Some(std::path::Path::new(
                            "/Users/_an/Library/Logs/aterm/aterm.log",
                        )),
                        relaunching,
                        PanesWithoutFolder::NONE,
                    ));
                }
            }
        }
        // A pane whose folder no shell could start in (audit #7 finding 48):
        // the reopened line that counts it, and each fault's row; and a pane
        // a program held whose folder is not here (audit #8).
        for (gone, unasked) in [(1, 0), (2, 0), (0, 1)] {
            all.push(journal_reopened_message(
                &reopened_fixture(crate::crash_journal::DeathClass::Killed),
                None,
                None,
                None,
                false,
                PanesWithoutFolder { gone, unasked },
            ));
        }
        for class in [
            crate::crash_journal::DeathClass::Killed,
            crate::crash_journal::DeathClass::Signal,
        ] {
            for reopened in [
                reopened_with_programs(class, &[]),
                reopened_with_agent(class, &[]),
            ] {
                all.push(journal_reopened_message(
                    &reopened,
                    None,
                    None,
                    None,
                    true,
                    PanesWithoutFolder {
                        gone: 1,
                        unasked: 0,
                    },
                ));
            }
        }
        for fault in crate::spawn_folder::Fault::ALL {
            all.push(folder_row(fault, &named(&["/Users/_an/src/old"])));
            all.push(folder_row(fault, &named(&["/a", "/b"])));
        }
        for note in journal_note_fixtures() {
            all.push(journal_note_message(
                &note,
                Some(std::path::Path::new(
                    "/Users/_an/Library/Logs/aterm/aterm.log",
                )),
            ));
        }
        {
            use aterm_agent::harness::relaunch::Outcome;
            let missed = [
                ("in tab 2".to_string(), Outcome::Cannot("shell-gone".into())),
                ("in tab 3".to_string(), Outcome::NotYet(String::new())),
            ];
            all.push(restored_agents_not_resumed(
                &missed[..1],
                Some(std::path::Path::new(
                    "/Users/_an/Library/Logs/aterm/aterm.log",
                )),
            ));
            all.push(restored_agents_not_resumed(&missed, None));
        }
        all.push(launch_load_failure(
            "aterm.toml is not a valid configuration (expected `=` at line 3) — every \
             setting is running at its default. Fix /home/ana/.config/aterm.toml and it loads on \
             the next change.",
        ));
        all.push(cpu_renderer_no_effect(
            "background_opacity",
            "it has no translucent present path, so the window stays solid",
        ));
        all.push(backdrop_declined(
            "background_material is styling the title bar only",
            "this display stack refused the DirectComposition backdrop swapchain",
        ));
        all.push(gpu_lost());
        all.push(a11y_publisher_dead("Server GUID mismatch", true));
        all.push(a11y_publisher_dead("Server GUID mismatch", false));
        all.push(presence_not_saved(
            "presence.band",
            "Presence Band",
            "the write was refused",
            false,
        ));
        all.push(presence_not_saved(
            "presence.rim",
            "Presence Rim",
            "the write could not be verified",
            true,
        ));
        all.push(fleet_hold(true));
        all.push(fleet_hold(false));
        all.push(hold_refused("no session"));
        all.push(fabric_failure(
            "document",
            "Couldn't open the document",
            &["no such file", "~/x.md"],
        ));
        all.push(fabric_failure(
            "on",
            "Couldn't start Fabric On",
            &["cannot spawn its thread"],
        ));
        all.push(fabric_failure(
            "inbox",
            "Couldn't open the inbox",
            &["no such file"],
        ));
        all.push(fabric_failure(
            "status",
            "Couldn't start Fabric Status",
            &["cannot spawn its thread"],
        ));
        all.push(fabric_status("on", "Fabric On finished", "", true));
        all.push(fabric_status(
            "off",
            "Couldn't run Fabric Off",
            "exited 2",
            false,
        ));
        all.push(serious_mode_feedback(
            "Serious Mode was not changed: aterm.toml changed first",
        ));
        all.push(serious_mode_feedback(
            "Serious Mode was saved but could not be applied: x",
        ));
        all.push(serious_mode_feedback(
            "Serious Mode was saved, but a newer aterm.toml edit now controls it",
        ));
        all.push(serious_mode_feedback(
            "Serious Mode may have been written but could not be verified; reload before \
             retrying: the disk generation moved",
        ));
        for head in [
            "Config observation was not valid TOML: expected `]`",
            "Robi was not dismissed: the setting was not saved",
            "Config reconciliation failed; queued changes were not written: io",
            "Manual saved aterm.toml, but its exact generation could not be admitted: x",
            "Config publication could not be verified: x",
            "Config was NOT saved: x",
            "Something new went wrong: x",
        ] {
            all.push(config_lane_error(head));
        }
        // The live agent upgrade's three (gap audit 2026-09-24): waiting (a
        // record), stalled (a row), done (a record).
        all.extend(agent_upgrade_waiting(
            "Claude Code",
            &["2.1.282", "2.1.282"],
        ));
        all.extend(agent_upgrade_waiting("Claude Code", &["2.1.282"]));
        all.extend(agent_upgrade_waiting(
            "Claude Code",
            &["2.1.282", "2.1.283"],
        ));
        all.extend(agent_upgrade_waiting("Codex", &["0.157.1"]));
        // One session's record (ruling 380), by its tab and in a second
        // window, Claude Code's longer product name included.
        for (agent, from, to) in [
            (
                aterm_agent::harness::upgrade::Agent::Codex,
                "0.157.1",
                "0.158.0",
            ),
            (
                aterm_agent::harness::upgrade::Agent::Claude,
                "2.1.282",
                "2.1.283",
            ),
        ] {
            for place in ["in tab 1", "in window 2, tab 11"] {
                all.push(agent_upgrade_waiting_in(
                    &aterm_agent::harness::upgrade_drive::Row {
                        tab: "s-09205e59a0bbd30464d4".into(),
                        from: from.into(),
                        to: to.into(),
                        agent,
                        phase: aterm_agent::harness::upgrade::Phase::Pending,
                        behind_since: 1_790_572_868,
                        ..Default::default()
                    },
                    place,
                ));
            }
        }
        let upgrade = aterm_agent::harness::upgrade_drive::Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: aterm_agent::harness::upgrade::Phase::Failed("unanswered".into()),
            outcome: "claude restarted on 2.1.282 · model claude-opus-5-5".into(),
            ..Default::default()
        };
        all.push(agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2"));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                behind_since: 1_790_280_544,
                wait: "not-idle:busy".into(),
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
        // A Claude Code behind its own running work (2026-09-26), after a notice
        // and before one: the sentence guards read both remedy lines.
        for (phase, wait) in [
            (
                aterm_agent::harness::upgrade::Phase::Announced {
                    at_s: 1_790_300_000,
                    asks: 2,
                },
                "background",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "not-idle:shell",
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    behind_since: 1_790_280_544,
                    wait: wait.into(),
                    ..upgrade.clone()
                },
                1_790_311_076,
                "in tab 2",
            ));
        }
        all.push(agent_upgrade_done(&upgrade, "in tab 2"));
        // WHERE THE OWNER HAS MORE THAN ONE WINDOW the place names it (`in
        // window 2, tab 1`), a word longer: every title that names the tab —
        // a wait `Upgrade now` moves, one it does not (`… yet`), a stop, and
        // each refused word — still keeps the glass title rule, at two-digit
        // windows and tabs too.
        for place in [tab_place(Some(2), 1), tab_place(Some(12), 10)] {
            for (phase, wait) in [
                (
                    aterm_agent::harness::upgrade::Phase::Pending,
                    "not-idle:busy",
                ),
                (
                    aterm_agent::harness::upgrade::Phase::Pending,
                    "not-idle:shell",
                ),
                (
                    aterm_agent::harness::upgrade::Phase::Announced {
                        at_s: 1_790_300_000,
                        asks: 2,
                    },
                    "background",
                ),
                (
                    aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
                    "",
                ),
            ] {
                all.push(agent_upgrade_stalled(
                    &aterm_agent::harness::upgrade_drive::Row {
                        phase,
                        wait: wait.into(),
                        behind_since: 1_790_280_544,
                        ..upgrade.clone()
                    },
                    1_790_311_076,
                    &place,
                ));
            }
            for word in aterm_messages::UpgradeWord::ALL {
                all.push(agent_upgrade_word_refused(
                    word,
                    aterm_agent::harness::upgrade::Agent::Claude,
                    ("s-1", "2.1.282"),
                    "busy:another-sweep",
                    &place,
                ));
            }
            all.push(agent_upgrade_done(&upgrade, &place));
        }
        // A stall's end, recorded under its row's key once the row was down
        // (review of 2026-09-27).
        let stalled = agent_upgrade_stalled(&upgrade, 1_790_311_076, "in tab 2");
        for end in [StallEnd::AsksAgain, StallEnd::MovesAgain, StallEnd::Left] {
            all.push(agent_upgrade_stall_over(
                upgrade.agent,
                stalled.key.as_deref().expect("keyed"),
                &stalled.detail,
                "in tab 2",
                end,
            ));
        }
        // The owner's word from the band (gap #21): what each word did, and
        // each kind of refusal.
        all.push(agent_upgrade_worded(
            &upgrade,
            aterm_messages::UpgradeWord::Now,
            "in tab 2",
            true,
        ));
        for word in aterm_messages::UpgradeWord::ALL {
            all.push(agent_upgrade_worded(&upgrade, word, "in tab 2", false));
            for why in [
                "busy:another-sweep",
                "stale:2.1.283",
                "the upgrade in tab s-b5cf2faabac5ce5127bd stopped for good (no-resume): `--now` \
                 re-arms only one that gave up",
                // Ruling 283: a round that stopped between the offer and the
                // press, as the writer refuses it now (typed).
                "stopped:1790318276:the upgrade in tab s-b5cf2faabac5ce5127bd stopped \
                 (no-resume): `--now` re-arms only one that gave up",
                "stopped:0:the upgrade in tab s-b5cf2faabac5ce5127bd stopped (signal-refused): \
                 `--now` re-arms only one that gave up",
            ] {
                all.push(agent_upgrade_word_refused(
                    word,
                    aterm_agent::harness::upgrade::Agent::Claude,
                    ("s-b5cf2faabac5ce5127bd", "2.1.282"),
                    why,
                    "in tab 2",
                ));
            }
        }
        // The Codex lane's (a daemon-mode client moved, one that named no
        // thread as it exited): its words pass every reporter guard too.
        let codex = aterm_agent::harness::upgrade_drive::Row {
            agent: aterm_agent::harness::upgrade::Agent::Codex,
            session: "codex-s-b5cf2faabac5ce5127bd".into(),
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            phase: aterm_agent::harness::upgrade::Phase::Done,
            outcome: "codex on 0.157.1 · the same conversation resumed; its work never \
                      stopped, in the daemon"
                .into(),
            ..upgrade.clone()
        };
        all.push(agent_upgrade_done(&codex, "in tab 2"));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
                ..codex.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
        // The same, failed after its `/exit` ended the TUI (its record now
        // stands with no holder, and names `codex resume`), and a Codex
        // client whose daemon waits on a detached thread.
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
                exited_at: 1_790_311_000,
                ..codex.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                behind_since: 1_790_280_544,
                wait: "daemon-first:busy-thread".into(),
                ..codex.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
        // A Claude Code move that stopped after its SIGTERM ended the agent
        // (it names `claude --resume`), and moves under way that do not move
        // (stuck exiting, exited, relaunched), for both agents (2026-09-27).
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                session: "0badf00d-1111-2222-3333-444455556666".into(),
                phase: aterm_agent::harness::upgrade::Phase::Failed("stale-exit".into()),
                exited_at: 1_790_311_000,
                ..upgrade.clone()
            },
            1_790_311_076,
            "in tab 2",
        ));
        for row in [&upgrade, &codex] {
            for (phase, exited_at) in [
                (
                    aterm_agent::harness::upgrade::Phase::Exiting {
                        at_s: 1_790_310_000,
                    },
                    0,
                ),
                (
                    aterm_agent::harness::upgrade::Phase::Exiting {
                        at_s: 1_790_310_000,
                    },
                    1_790_310_010,
                ),
                (
                    aterm_agent::harness::upgrade::Phase::Relaunched {
                        at_s: 1_790_310_000,
                    },
                    1_790_310_010,
                ),
            ] {
                all.push(agent_upgrade_stalled(
                    &aterm_agent::harness::upgrade_drive::Row {
                        phase,
                        exited_at,
                        behind_since: 1_790_309_000,
                        ..row.clone()
                    },
                    1_790_311_076,
                    "in tab 2",
                ));
            }
        }
        all.extend(agent_upgrade_waiting(
            "Claude Code and Codex",
            &["2.1.282", "0.157.1"],
        ));
        for (phase, wait) in [
            (
                aterm_agent::harness::upgrade::Phase::Pending,
                "terminal:tmux",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Failed("not-a-shell-job".into()),
                "",
            ),
            (
                aterm_agent::harness::upgrade::Phase::Failed("no-resume".into()),
                "",
            ),
        ] {
            all.push(agent_upgrade_stalled(
                &aterm_agent::harness::upgrade_drive::Row {
                    phase,
                    wait: wait.into(),
                    behind_since: 1_790_311_000,
                    ..upgrade.clone()
                },
                1_790_311_076,
                "in tab 2",
            ));
        }
        all.push(log_did_not_open("/x/y.log"));
        all.push(font_family_rejected(
            "font_family: \"Nope\" is not an admissible font",
        ));
        // R14–R22, the retired toast's reporters.
        all.extend(gesture_failures());
        all.push(file_access_question());
        all.push(file_access_granted());
        for posture in [
            aterm_update::which_copy::InstallPosture::MountedImage,
            aterm_update::which_copy::InstallPosture::Translocated,
        ] {
            all.push(install_posture(posture).expect("a posture to fix"));
        }
        for kind in [
            crate::connections::ConnectedSpawnKind::Controlled,
            crate::connections::ConnectedSpawnKind::Controller,
        ] {
            all.push(session_connection_created(
                &crate::connections::first_use_notice_text(kind),
            ));
        }
        all.push(session_connection_created(
            &crate::connections::first_use_connect_notice_text(true, false),
        ));
        all
    }

    /// A GESTURE FAILURE KEEPS THE WHOLE ERROR (design ruling 64): a 600-char
    /// error — longer than one detail line — is every word behind Details, in
    /// order, never clipped at the line cap.
    #[test]
    fn a_gesture_failure_keeps_the_whole_error() {
        let error = format!(
            "posix_spawn failed: {} (see https://example.invalid/aterm/spawn for the \
             limits); errno 35",
            "resource temporarily unavailable ".repeat(16)
        );
        assert!(error.chars().count() > aterm_messages::DETAIL_LINE_CAP);
        let m = new_tab_failed(&error);
        assert!(m.detail.len() > 1, "{:?}", m.detail);
        let normalized: String = error.split_whitespace().collect::<Vec<_>>().join(" ");
        let rejoined: String = m
            .detail
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rejoined, normalized, "every word, in order");
        assert!(
            m.detail
                .iter()
                .all(|l| l.chars().count() <= aterm_messages::DETAIL_LINE_CAP)
        );
    }

    /// The folders a row names, as the fold keeps them.
    fn named(dirs: &[&str]) -> crate::spawn_folder::Named {
        crate::spawn_folder::Named {
            dirs: dirs.iter().map(|dir| (*dir).to_owned()).collect(),
            unnamed: 0,
        }
    }

    /// AUDIT #7 FINDING 48 — a shell that started in the home folder because
    /// its folder was gone is ONE warning row, keyed, in the failure grammar,
    /// saying where the shell is instead and naming the folder; several
    /// folders are counted in the title and named one per line, and past the
    /// line cap the rest are counted. A folder that could not be entered has
    /// its own row and words. On the glass by the attention rule: the shell is
    /// not where the person asked.
    #[test]
    fn a_shell_in_the_home_folder_is_one_row_naming_every_folder_per_fault() {
        use crate::spawn_folder::Fault;
        let one = folder_row(Fault::Missing, &named(&["/Users/_an/src/old"]));
        assert_eq!(one.title, "Couldn't find the folder");
        assert_eq!(one.severity, Severity::Warn);
        assert_eq!(
            one.detail,
            ["opened in your home folder instead of /Users/_an/src/old"]
        );
        assert_eq!(one.key.as_deref(), Some(KEY_FOLDER_NOT_FOUND));
        assert_eq!(attention(&one), Ok(Attention::Failure));

        let three = folder_row(Fault::Missing, &named(&["/a", "/b", "/c"]));
        assert_eq!(three.title, "Couldn't find 3 folders");
        assert_eq!(
            three.detail,
            ["opened in your home folder instead", "/a", "/b", "/c"]
        );
        assert_eq!(three.key, one.key, "one row, however many folders");
        assert_eq!(attention(&three), Ok(Attention::Failure));

        let many: Vec<String> = (0..40).map(|i| format!("/gone/{i}")).collect();
        let dirs: Vec<&str> = many.iter().map(String::as_str).collect();
        let row = folder_row(Fault::Missing, &named(&dirs));
        assert_eq!(row.title, "Couldn't find 40 folders");
        assert_eq!(row.detail.len(), DETAIL_LINES_CAP);
        assert_eq!(row.detail.last().map(String::as_str), Some("and 18 more"));

        let shut = folder_row(Fault::Shut, &named(&["/srv/locked"]));
        assert_eq!(shut.title, "Couldn't open the folder");
        assert_eq!(
            shut.detail,
            ["opened in your home folder instead of /srv/locked"]
        );
        assert_eq!(shut.key.as_deref(), Some(KEY_FOLDER_NOT_OPENED));
        assert_eq!(attention(&shut), Ok(Attention::Failure));
        let two = folder_row(Fault::Shut, &named(&["/a", "/b"]));
        assert_eq!(two.title, "Couldn't open 2 folders");
    }

    /// A ROW READS BACK AS THE FOLDERS IT NAMES — how a row carried across an
    /// update keeps them when the next folder joins it: one folder, several,
    /// past the line cap (the named ones and the count), a folder with a space
    /// and one longer than a line. The negative control is a row of another
    /// shape.
    #[test]
    fn a_folder_row_reads_back_as_the_folders_it_names() {
        use crate::spawn_folder::{Fault, Named};
        let long = format!("/deep/{}", "d".repeat(300));
        let many: Vec<String> = (0..40).map(|i| format!("/gone/{i}")).collect();
        let cases = [
            named(&["/Users/_an/src/old"]),
            named(&["/Users/_an/My Projects/old"]),
            named(&["/a", "/b", "/c"]),
            named(&[long.as_str()]),
            Named {
                dirs: many[..22].to_vec(),
                unnamed: 18,
            },
        ];
        for case in cases {
            for fault in Fault::ALL {
                assert_eq!(
                    folder_row_named(&folder_row(fault, &case)),
                    Some(case.clone())
                );
            }
        }
        let past_the_cap = named(&many.iter().map(String::as_str).collect::<Vec<_>>());
        let back = folder_row_named(&folder_row(Fault::Missing, &past_the_cap)).expect("reads");
        assert_eq!((back.dirs.len(), back.unnamed), (22, 18));
        assert_eq!(folder_row_named(&restored_tab_failed("x")), None);
    }

    /// AUDIT #7 FINDING 48 — the reopened layout's restored line COUNTS the
    /// panes that could not open their folder instead of claiming every tab
    /// came back in its folder; the rest of the row is unchanged. The
    /// negative control is the line with none. A pane a program held whose
    /// folder is not here (audit #8) is not counted, and the line then claims
    /// no folders: its shell started in the default folder.
    #[test]
    fn the_reopened_row_counts_the_panes_that_could_not_open_their_folder() {
        use crate::crash_journal::DeathClass;
        let row = |gone, unasked| {
            journal_reopened_message(
                &reopened_fixture(DeathClass::Killed),
                None,
                None,
                None,
                false,
                PanesWithoutFolder { gone, unasked },
            )
        };
        assert_eq!(
            row(0, 0).detail[1],
            "2 tabs in 1 window restored in their folders from its crash journal"
        );
        assert_eq!(
            row(1, 0).detail[1],
            "2 tabs in 1 window restored from its crash journal; 1 pane could not open its folder"
        );
        assert_eq!(
            row(2, 0).detail[1],
            "2 tabs in 1 window restored from its crash journal; 2 panes could not open their \
             folders"
        );
        assert_eq!(
            row(0, 1).detail[1],
            "2 tabs in 1 window restored from its crash journal"
        );
        assert_eq!(
            row(1, 1).detail[1],
            "2 tabs in 1 window restored from its crash journal; 1 pane could not open its folder"
        );
        let (none, some) = (row(0, 0), row(1, 0));
        assert_eq!(
            (&none.title, none.severity, &none.detail[0], &none.key),
            (&some.title, some.severity, &some.detail[0], &some.key)
        );
    }

    /// AUDIT #8 — once a pane did not come back in its folder, the loss
    /// sentence no longer says ONLY the scrollback was lost. Both shapes that said `only` — nothing ran, and
    /// only a resumed agent ran — after a kill (the quiet record) and after
    /// a crash (the row on glass). The negative control is each at 0.
    #[test]
    fn a_pane_without_its_folder_drops_only_from_the_loss_sentence() {
        use crate::crash_journal::DeathClass;
        for class in [DeathClass::Killed, DeathClass::Signal] {
            let nothing = |gone, unasked| {
                journal_reopened_message(
                    &reopened_with_programs(class, &[]),
                    None,
                    None,
                    None,
                    false,
                    PanesWithoutFolder { gone, unasked },
                )
            };
            assert_eq!(nothing(0, 0).detail[0], JOURNAL_NOTHING_RAN, "{class:?}");
            assert_eq!(
                nothing(1, 0).detail[0],
                "nothing was running in them: their scrollback did not survive",
                "{class:?}"
            );
            assert_eq!(
                nothing(0, 1).detail[0],
                JOURNAL_NOTHING_RAN_FOLDERLESS,
                "{class:?}: a held pane's folder that is not here"
            );
            assert_eq!(
                nothing(1, 0).detail[1],
                "2 tabs in 1 window restored from its crash journal; 1 pane could not open its \
                 folder",
                "{class:?}"
            );
            let resumed = |gone, unasked| {
                journal_reopened_message(
                    &reopened_with_agent(class, &[]),
                    None,
                    None,
                    None,
                    true,
                    PanesWithoutFolder { gone, unasked },
                )
            };
            assert_eq!(
                resumed(0, 0).detail[0],
                format!(
                    "aterm starts Claude again on its conversation in tab 2; \
                     {JOURNAL_ONLY_SCROLLBACK}"
                ),
                "{class:?}"
            );
            assert_eq!(
                resumed(1, 0).detail[0],
                "aterm starts Claude again on its conversation in tab 2; the scrollback did not \
                 survive",
                "{class:?}"
            );
            let (none, some) = (nothing(0, 0), nothing(1, 0));
            assert_eq!(
                (&none.title, none.severity, none.hold, &none.key),
                (&some.title, some.severity, some.hold, &some.key),
                "{class:?}: only the words move"
            );
        }
    }

    /// Every R14 row, from the sentences their sites really pass.
    fn gesture_failures() -> Vec<Message> {
        vec![
            new_tab_failed("fork: Resource temporarily unavailable (os error 35)"),
            new_window_failed("the pty could not be opened"),
            split_refused(
                "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
            ),
            split_refused("Split refused: this pane's size could not be measured"),
            split_failed("no event loop to host the new pane"),
            keystrokes_dropped(3),
            restore_stopped_early(),
            shell_lost_in_update("descriptor 9 was closed"),
            restored_tab_failed("exec failed"),
            input_refused("the paste is too large \u{2014} paste it in smaller pieces"),
        ]
    }

    /// THE COPY OF THE ONE QUESTION THIS FEATURE ASKS UNPROMPTED (the fence
    /// that held the retired card's caption, `notice.rs`, moved onto the
    /// words themselves — design §7.1). Three fences at once: the owner's
    /// restart-phrase ruling (`tools/grep_guard.sh` B12), the honesty
    /// rule that no coverage or scope claim may ship while §7 S1 and S4 are
    /// unrun, and the ruling that mitigating this annoyance is acceptable —
    /// so it may never read as elimination. The title and `detail[0]` are
    /// held under 75 characters together so the ask survives beside its two
    /// capsules on an ordinary window, and the primary capsule's label opens
    /// a pane without reading as granting anything.
    #[test]
    fn the_file_access_message_is_honest_and_fence_clean() {
        use crate::consent_card::{FDA_DETAIL, FDA_TITLE};
        let msg = file_access_question();
        assert_eq!(msg.title, FDA_TITLE, "the row carries the const");
        assert_eq!(msg.detail[0], FDA_DETAIL);
        for text in std::iter::once(&msg.title).chain(msg.detail.iter()) {
            let lower = text.to_ascii_lowercase();
            // 1. The restart-phrase fence, in the shapes B12 scans for.
            for banned in [
                "restart",
                "relaunch",
                "re-launch",
                "reopen",
                "re-open",
                "quit and",
                "next launch",
                "reboot",
                "launch aterm again",
            ] {
                assert!(!lower.contains(banned), "{banned:?} in {text:?}");
            }
            // 2. No coverage claim and no scope sentence: which folders a
            //    grant reaches is S4's measurement and how far it reaches is
            //    S1's, and NEITHER has been run.
            for unmeasured in [
                "cover",
                "documents",
                "desktop",
                "downloads",
                "volume",
                "icloud",
                "retires",
                "this process",
                "new processes",
                "every folder",
                "all folders",
            ] {
                assert!(!lower.contains(unmeasured), "{unmeasured:?} in {text:?}");
            }
            // 3. Mitigate, never eliminate (owner's ruling).
            for promise in [
                "never again",
                "no more",
                "eliminat",
                "not be interrupted",
                "never be interrupted",
                "without interruption",
                "stops the",
                "ends the",
            ] {
                assert!(!lower.contains(promise), "{promise:?} in {text:?}");
            }
        }
        // 4. The observed access, with the owner's already-enabled switch
        //    explicitly possible: a failed probe does not read the switch.
        assert!(FDA_DETAIL.contains("Full Disk Access"));
        assert!(FDA_DETAIL.contains("may already be enabled"));
        assert!(
            FDA_TITLE.chars().count() + FDA_DETAIL.chars().count() <= 75,
            "short enough to survive beside two capsules on an ordinary window"
        );
        assert!(
            FDA_TITLE.ends_with('?') && !msg.excerpt,
            "the title is the ask and the row paints it alone; the hedge is behind Details"
        );
        // 5. A decision: the primary opens a pane — its label grants nothing —
        //    and Not now answers; an Ask hold, the shared key, a quiet tone.
        assert_eq!(
            msg.actions,
            [
                Intent::OpenSystemPane {
                    pane: PANE_FULL_DISK_ACCESS.to_string()
                },
                Intent::NotNow {
                    decision: Decision::FileAccess
                }
            ]
        );
        let label = msg.actions[0].label().to_ascii_lowercase();
        for grant in ["grant", "allow", "enable", "turn on"] {
            assert!(!label.contains(grant), "{label:?} reads as granting");
        }
        assert_eq!(msg.actions[0].label(), "Open Settings");
        assert_eq!(msg.hold, Hold::Ask { for_: HOLD_ASK });
        assert_eq!(msg.key.as_deref(), Some(KEY_FILE_ACCESS));
        assert_eq!(msg.severity, Severity::Info, "a question, never an alarm");
        assert_eq!(msg.tag, tags::PRIVACY);
        // 6. The route in words is behind Details, whole.
        assert!(
            msg.detail.iter().any(|l| l
                == crate::menu::privacy_settings_path_words(
                    crate::menu::PrivacyPane::FullDiskAccess
                )),
            "{:?}",
            msg.detail
        );
    }

    /// R14 UNDER THE OWNER'S ATTENTION RULE (2026-09-23): a failure of the
    /// person's own gesture is a row — Error, a short `HOLD_GESTURE` — with a
    /// title of a few words and the WHOLE error behind Details, never cut.
    /// No key: each is an event of its own.
    #[test]
    fn a_failed_gesture_is_a_short_row_with_the_whole_error_behind_details() {
        let long = "x".repeat(200);
        let msg = new_tab_failed(&long);
        assert_eq!(msg.title, "Couldn't open a new tab");
        assert_eq!(
            msg.detail,
            std::slice::from_ref(&long),
            "the error whole, not cut at 160"
        );
        assert!(
            !msg.excerpt,
            "a spawn's error does not change what the person does"
        );
        for msg in gesture_failures() {
            assert_eq!(msg.severity, Severity::Error, "{msg:?}");
            assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "{msg:?}");
            assert!(msg.key.is_none(), "an event, not a slot: {msg:?}");
            assert!(
                msg.title.split_whitespace().count() <= 5,
                "a few words: {:?}",
                msg.title
            );
            assert!(!msg.title.contains(':'), "no clause: {:?}", msg.title);
            assert!(!msg.detail.is_empty(), "the reason is behind Details");
        }
        let refused = split_refused(
            "Split refused: this pane is 20x5 cells; a vertical split needs at least 20x7",
        );
        assert_eq!(refused.title, "Couldn't split the pane");
        assert_eq!(
            refused.detail,
            [
                "pane 20\u{00d7}5, needs 20\u{00d7}7",
                "this pane is 20x5 cells; a vertical split needs at least 20x7"
            ],
            "the sizes are the excerpt; the sentence rides whole behind them"
        );
        assert_eq!(
            split_refused("Split refused: this pane's size could not be measured").detail,
            ["this pane's size could not be measured"],
            "another shape is its own excerpt"
        );
        assert_eq!(keystrokes_dropped(3).title, "Keystrokes dropped");
        assert_eq!(
            keystrokes_dropped(3).detail[0],
            "retype the last 3 keys",
            "what to do is the excerpt"
        );
        assert_eq!(keystrokes_dropped(1).detail[0], "retype the last key");
        assert!(keystrokes_dropped(3).excerpt);
        assert!(
            !restore_stopped_early().excerpt,
            "its line restates the title: behind Details"
        );
    }

    /// THE REST OF THE RETIRED TOAST UNDER THE ATTENTION RULE: a crippled
    /// install is a row only when there is something to fix; the grant and the
    /// connection disclosure are records that never touch the glass.
    #[test]
    fn the_rest_of_the_retired_toast_is_rows_only_where_there_is_something_to_fix() {
        // R21 — a row only where the person has something to fix.
        use aterm_update::which_copy::InstallPosture;
        assert!(install_posture(InstallPosture::Installed).is_none());
        assert!(install_posture(InstallPosture::NotABundle).is_none());
        let dmg = install_posture(InstallPosture::MountedImage).expect("a fix");
        assert_eq!(
            dmg.title, "Move aterm to Applications",
            "the instruction is the title"
        );
        assert_eq!(dmg.detail[0], "running from the disk image");
        assert!(
            dmg.detail[1].starts_with("Drag aterm to Applications and open it from there")
                && dmg.detail[1].contains("cannot update itself"),
            "the remedy whole: {:?}",
            dmg.detail
        );
        assert_eq!(
            dmg.detail.len(),
            2,
            "where and the remedy; the summary would say the first line again: {:?}",
            dmg.detail
        );
        assert_eq!(dmg.severity, Severity::Warn);
        assert_eq!(dmg.tag, tags::SYSTEM, "not an ALab tool (ruling 309)");
        assert!(
            dmg.actions.is_empty(),
            "no page of aterm's performs the fix"
        );
        assert_eq!(dmg.key.as_deref(), Some(KEY_INSTALL_POSTURE));
        let moved = install_posture(InstallPosture::Translocated).expect("a fix");
        assert_eq!(moved.title, "Move aterm to Applications");
        assert_eq!(moved.detail[0], "running from a temporary copy");
        // R17, R22 — records (R20 went with the sealed seed, Phase 5).
        for record in [
            file_access_granted(),
            session_connection_created(&crate::connections::first_use_notice_text(
                crate::connections::ConnectedSpawnKind::Controlled,
            )),
        ] {
            assert_eq!(record.hold, Hold::LogOnly, "{record:?}");
        }
        assert_eq!(file_access_granted().key.as_deref(), Some(KEY_FILE_ACCESS));
        let connected = session_connection_created(&crate::connections::first_use_notice_text(
            crate::connections::ConnectedSpawnKind::Controlled,
        ));
        assert_eq!(connected.title, "Session connection created");
        assert_eq!(connected.tag, tags::SESSION);
        assert!(
            connected.detail[0].starts_with("this session can now type into"),
            "{:?}",
            connected.detail
        );
        assert!(
            connected.detail[1].starts_with("Disconnect"),
            "{:?}",
            connected.detail
        );
    }

    /// The artifact's path is a detail line (whole, absolute) behind Details,
    /// never the title — the title is the fact, the path is where to look —
    /// and never on the glass (design ruling 74: `detail[0]` says a report
    /// was saved, in words, and even that is not painted, ruling 145); the
    /// head follows it; `Open log` carries the same path.
    #[test]
    fn the_crash_row_names_the_artifact_in_its_detail_not_its_title() {
        let path = "/Users/_an/Library/Logs/aterm/crash-signal-1-1.log.seen";
        let msg = crash_message(&crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(path),
            head: vec!["aterm-gui 0.1.0 crashed".into(), "fatal signal 11".into()],
        });
        assert_eq!(msg.tag, tags::CRASH);
        assert_eq!(msg.severity, Severity::Error);
        assert_eq!(msg.title, "aterm crashed last time");
        assert!(!msg.title.contains('/'), "the title names no path");
        assert!(!msg.excerpt, "the row paints its title alone");
        assert_eq!(msg.detail[0], CRASH_EXCERPT, "the first line is words");
        assert!(
            !msg.detail[0].contains('/') && !msg.detail[0].contains(".log"),
            "the first line names no file: {}",
            msg.detail[0]
        );
        assert_eq!(msg.detail[1], format!("crash log at {path}"));
        assert_eq!(
            &msg.detail[2..],
            ["aterm-gui 0.1.0 crashed", "fatal signal 11"]
        );
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: path.to_string()
            }]
        );
        assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
    }

    /// The killed row says what happened without a path in its title, says
    /// there is no crash report (the marker is empty — `Open log` goes to the
    /// log, never to that empty file), and takes the crash row's slot.
    #[test]
    fn the_killed_row_opens_the_log_and_shares_the_crash_slot() {
        let log = "/Users/_an/Library/Logs/aterm/aterm.log";
        let marker = "/Users/_an/Library/Logs/aterm/crash-marker-9-9-app.log.seen";
        let msg = killed_message(&crate::logging::KillEvidence {
            marker: std::path::PathBuf::from(marker),
            log: std::path::PathBuf::from(log),
        });
        assert_eq!(msg.tag, tags::CRASH);
        assert_eq!(msg.severity, Severity::Warn);
        assert_eq!(msg.title, "aterm was stopped last time");
        assert_eq!(msg.detail[0], KILLED_EXCERPT);
        assert!(msg.detail.iter().any(|l| l.contains(log)));
        assert!(msg.detail.iter().any(|l| l.contains(marker)));
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: log.to_string()
            }]
        );
        assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
    }

    /// A reopened crash journal of `class`: two tabs in one window.
    fn reopened_fixture(class: crate::crash_journal::DeathClass) -> crate::crash_journal::Reopened {
        crate::crash_journal::Reopened {
            manifest: crate::crash_journal::fixtures::layout(&[("/a", "zsh"), ("/b", "vim")]),
            class,
            sources: vec![crate::crash_journal::JournalId { pid: 9, nanos: 9 }],
            writers: vec![9],
            programs: None,
            second_chance: false,
        }
    }

    /// The fixture with the journal saying which leaves ran a program (D16):
    /// `(tab, program)` in its one window.
    fn reopened_with_programs(
        class: crate::crash_journal::DeathClass,
        programs: &[(u32, &str)],
    ) -> crate::crash_journal::Reopened {
        crate::crash_journal::Reopened {
            programs: Some(
                programs
                    .iter()
                    .map(|(tab, program)| crate::crash_journal::LeafProgram {
                        window: 0,
                        tab: *tab,
                        program: (*program).to_string(),
                        agent: false,
                    })
                    .collect(),
            ),
            ..reopened_fixture(class)
        }
    }

    /// [`reopened_with_programs`] with Claude hosted in tab 2 (ruling 293):
    /// its leaf carries a relaunch record naming a conversation, and the
    /// journal says the agent ran there.
    fn reopened_with_agent(
        class: crate::crash_journal::DeathClass,
        programs: &[(u32, &str)],
    ) -> crate::crash_journal::Reopened {
        let mut reopened = reopened_with_programs(class, programs);
        let claude = crate::restore::AgentRestore {
            pid: 4242,
            start: "Sat Sep 27 01:02:03 2026".into(),
            program: "/opt/claude/bin/claude".into(),
            argv: vec!["/opt/claude/bin/claude".into()],
            session: Some("0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c".into()),
            cwd: "/b".into(),
            version: None,
            codex: None,
        };
        reopened
            .manifest
            .fill_agents(&|id| (id == 1).then(|| claude.clone()));
        if let Some(programs) = reopened.programs.as_mut() {
            programs.push(crate::crash_journal::LeafProgram {
                window: 0,
                tab: 1,
                program: "claude".to_string(),
                agent: true,
            });
        }
        reopened
    }

    /// RULING 293: a CRASH whose only program was an agent the relaunch
    /// brings back keeps its row (aterm failed), in true words — aterm
    /// starts Claude again, only the scrollback is gone; a journal that did not say what
    /// ran still says the agent resumes in today's loss sentence. The row
    /// that says restored agents did NOT come back names the tab and the
    /// reason in a person's words, never a step word, and how to resume by
    /// hand. NEGATIVE CONTROL: with the relaunch off, the crash names claude
    /// as lost.
    #[test]
    fn a_crash_that_took_only_a_resumed_agent_says_so_and_a_failed_resume_is_named() {
        use crate::crash_journal::DeathClass;
        use aterm_agent::harness::relaunch::Outcome;
        let crashed = reopened_with_agent(DeathClass::Signal, &[]);
        let row =
            journal_reopened_message(&crashed, None, None, None, true, PanesWithoutFolder::NONE);
        assert_eq!(row.severity, Severity::Error);
        assert_ne!(row.hold, Hold::LogOnly);
        assert_eq!(row.title, "Tabs restored after aterm crashed");
        assert_eq!(
            row.detail[0],
            "aterm starts Claude again on its conversation in tab 2; only the scrollback did not \
             survive"
        );
        let off =
            journal_reopened_message(&crashed, None, None, None, false, PanesWithoutFolder::NONE);
        assert_eq!(off.title, "Tabs restored, claude lost");
        let older = crate::crash_journal::Reopened {
            programs: None,
            ..crashed.clone()
        };
        let row =
            journal_reopened_message(&older, None, None, None, true, PanesWithoutFolder::NONE);
        // One sentence (ruling 314): the agent rides the loss line.
        assert_eq!(
            row.detail[0],
            format!("{JOURNAL_LOSS}; aterm starts Claude again on its conversation in tab 2")
        );
        assert!(row.detail[1..].iter().all(|l| !l.contains("starts Claude")));

        assert_eq!(
            journal_resumed_line(&[(0, 0), (0, 2)], 1),
            "aterm starts Claude again on its conversations in tabs 1 and 3"
        );
        assert_eq!(
            journal_resumed_line(&[(0, 0), (1, 0)], 2),
            "aterm starts Claude again on its conversations in tab 1 of window 1 and tab 1 of \
             window 2"
        );
        assert_eq!(
            journal_resumed_line(&[(0, 0), (0, 1), (0, 2), (0, 3)], 1),
            "aterm starts Claude again on its conversations in 4 tabs"
        );
        assert_eq!(journal_resumed_line(&[], 1), "");

        let log = std::path::Path::new("/Users/_an/Library/Logs/aterm/aterm.log");
        let one = restored_agents_not_resumed(
            &[("in tab 2".to_string(), Outcome::Cannot("shell-gone".into()))],
            Some(log),
        );
        assert_eq!(one.title, "Couldn't resume Claude in tab 2");
        assert_eq!(attention(&one), Ok(Attention::Failure));
        assert_eq!(one.tag, tags::HARNESS);
        assert_eq!(
            one.detail[0],
            "its tab's shell had ended, so its conversation did not come back after aterm \
             stopped: type claude --resume there to pick it up again"
        );
        // DAY SIX, D31/D32: an agent that started and did not pick its
        // conversation up is said as that — the tab was ready — and the
        // remedy is in the plain sentence, not the technical block. A prompt
        // that refused the line is said as that too; a tab that never came
        // to its prompt stays "not ready" (the control).
        for step in ["failed:no-resume", "wait:resume"] {
            let started = restored_agents_not_resumed(
                &[("in tab 1".to_string(), Outcome::NotYet(step.into()))],
                Some(log),
            );
            assert_eq!(
                started.detail[0],
                "Claude started in the tab but did not pick its conversation up after aterm \
                 stopped: type claude --resume there to pick it up again",
                "{step}"
            );
            assert!(
                started.detail[1..]
                    .iter()
                    .all(|l| !l.contains("--resume") && !l.contains("not ready")),
                "{:?}",
                started.detail
            );
        }
        assert!(
            not_resumed_why(&Outcome::NotYet("failed:relaunch:ERR-halted".into()))
                .starts_with("its prompt refused")
        );
        assert_eq!(
            not_resumed_why(&Outcome::NotYet("wait:shell-prompt".into())),
            "its tab was not ready for it in time"
        );
        let many = restored_agents_not_resumed(
            &[
                (
                    "in tab 1".to_string(),
                    Outcome::NotYet("wait:tab-not-live".into()),
                ),
                ("in tab 3".to_string(), Outcome::Left("not-exited".into())),
                ("in tab 4".to_string(), Outcome::Cannot("inert".into())),
            ],
            None,
        );
        assert_eq!(many.title, "Couldn't resume Claude in 3 tabs");
        assert_eq!(attention(&many), Ok(Attention::Failure));
        assert!(many.detail[0].ends_with("type claude --resume in each tab to pick one up again"));
        assert!(
            many.detail[1].starts_with("in tab 1, "),
            "{:?}",
            many.detail
        );
        for line in &many.detail {
            for word in ["wait:", "not-exited", "inert", "shell-gone", "(", "harness"] {
                assert!(!line.contains(word), "{word:?} in {line:?}");
            }
        }
    }

    fn journal_note_fixtures() -> Vec<crate::crash_journal::Note> {
        let id = crate::crash_journal::JournalId { pid: 9, nanos: 9 };
        vec![
            crate::crash_journal::Note::Unreadable {
                id,
                class: crate::crash_journal::DeathClass::Killed,
                path: std::path::PathBuf::from(
                    "/Users/_an/Library/Application Support/aterm/journal-9-9.toml",
                ),
                error: "its layout is not one this build reads".into(),
            },
            crate::crash_journal::Note::Relapsed {
                id,
                class: crate::crash_journal::DeathClass::Signal,
                windows: 1,
                tabs: 2,
                lost: Some(crate::crash_journal::Lost {
                    tabs: 1,
                    named: vec!["Python".to_string()],
                    unnamed: 0,
                }),
            },
            crate::crash_journal::Note::Relapsed {
                id,
                class: crate::crash_journal::DeathClass::Killed,
                windows: 1,
                tabs: 2,
                lost: Some(crate::crash_journal::Lost::default()),
            },
        ]
    }

    /// THE REOPENED LAYOUT'S ROW (PTY keeper P1) says what came back and what
    /// did not — the tabs and their folders, never the programs or their
    /// scrollback — in its title and its detail, takes the crash row's slot,
    /// and carries the evidence the row it replaces would have: the crash log
    /// and its head (an error), or the killed run's log (a warning).
    #[test]
    fn the_reopened_journal_row_says_what_came_back_and_what_did_not() {
        use crate::crash_journal::DeathClass;
        let path = "/Users/_an/Library/Logs/aterm/crash-7.log.seen";
        let crash = crate::logging::CrashEvidence {
            path: std::path::PathBuf::from(path),
            head: vec!["aterm-gui 0.1.0 crashed".into()],
        };
        let msg = journal_reopened_message(
            &reopened_fixture(DeathClass::Panic),
            Some(&crash),
            None,
            None,
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(msg.title, "Tabs restored, programs lost");
        assert_eq!(msg.severity, Severity::Error);
        assert_eq!(msg.key.as_deref(), Some(KEY_CRASH));
        assert_eq!(msg.tag, tags::CRASH);
        // The answer is the sentence (day five, D19); the class a detail.
        assert_eq!(msg.detail[0], JOURNAL_LOSS);
        assert_eq!(
            msg.detail[1],
            "2 tabs in 1 window restored in their folders from its crash journal"
        );
        assert_eq!(msg.detail[2], DeathClass::Panic.sentence());
        assert_eq!(msg.detail[3], format!("crash log at {path}"));
        assert_eq!(msg.detail[4], "aterm-gui 0.1.0 crashed");
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: path.to_string()
            }]
        );

        let log = "/Users/_an/Library/Logs/aterm/aterm.log";
        let killed = crate::logging::KillEvidence {
            marker: std::path::PathBuf::from("/l/crash-marker-7-7-app.log.seen"),
            log: std::path::PathBuf::from(log),
        };
        let msg = journal_reopened_message(
            &reopened_fixture(DeathClass::Killed),
            None,
            Some(&killed),
            None,
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(msg.severity, Severity::Warn);
        assert_eq!(msg.tag, tags::SESSION, "a kill is no crash");
        assert!(msg.detail.iter().any(|line| line.contains(log)));
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: log.to_string()
            }]
        );
        // A development start's kill has no kill row to carry: the log dir's
        // log is named instead.
        let msg = journal_reopened_message(
            &reopened_fixture(DeathClass::Killed),
            None,
            None,
            Some(std::path::Path::new(log)),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(
            msg.actions,
            [Intent::OpenPath {
                path: log.to_string()
            }]
        );
        assert_eq!(attention(&msg), Ok(Attention::Failure));
    }

    /// D16 (ruling 282) — A QUIET RELAUNCH: a killed run whose tabs all sat at
    /// their prompts lost nothing a person ran, so the reopened layout is a
    /// RECORD that says so, never a held warning. The negative controls: a
    /// program that ran keeps the warning and names itself, a crash keeps its
    /// row whatever ran, and a journal that did not say (an older writer's)
    /// reads exactly as before.
    #[test]
    fn a_relaunch_that_lost_nothing_is_a_record_and_a_lost_program_is_named() {
        use crate::crash_journal::DeathClass;
        let log = std::path::Path::new("/Users/_an/Library/Logs/aterm/aterm.log");
        let quiet = journal_reopened_message(
            &reopened_with_programs(DeathClass::Killed, &[]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(quiet.title, "Tabs restored after aterm stopped");
        assert_eq!(quiet.severity, Severity::Info);
        assert_eq!(quiet.hold, Hold::LogOnly);
        assert_eq!(attention(&quiet), Ok(Attention::Record));
        assert_eq!(quiet.detail[0], JOURNAL_NOTHING_RAN);
        assert_eq!(quiet.tag, tags::SESSION);
        assert!(
            quiet.detail.iter().all(|line| !line.contains("programs")),
            "{:?}",
            quiet.detail
        );

        // Negative control: one tab ran vim — the warning stands and names it.
        let lost = journal_reopened_message(
            &reopened_with_programs(DeathClass::Killed, &[(1, "vim")]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(lost.title, "Tabs restored, vim lost");
        assert_eq!(lost.severity, Severity::Warn);
        assert_eq!(lost.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(
            lost.detail[0],
            "vim was running in 1 of 2 tabs and did not survive, nor did the scrollback"
        );
        // Two tabs, one name unresolved: counted by tab, the known name kept.
        let both = journal_reopened_message(
            &reopened_with_programs(DeathClass::Killed, &[(0, "claude"), (1, "")]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(both.title, "Tabs restored, programs lost");
        assert_eq!(
            both.detail[0],
            "claude and another program were running in both tabs and did not survive, nor \
             did the scrollback"
        );
        // A leaf the reopened layout does not hold counts for nothing.
        let stray = journal_reopened_message(
            &reopened_with_programs(DeathClass::Killed, &[(7, "vim")]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(stray.hold, Hold::LogOnly);

        // A crash keeps its row (aterm failed), with true words.
        let crashed = journal_reopened_message(
            &reopened_with_programs(DeathClass::Panic, &[]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(crashed.title, "Tabs restored after aterm crashed");
        assert_eq!(crashed.severity, Severity::Error);
        assert_eq!(crashed.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(crashed.detail[0], JOURNAL_NOTHING_RAN);
        // One program whose name is unknown: counted, not named.
        let unnamed = journal_reopened_message(
            &reopened_with_programs(DeathClass::Killed, &[(0, "")]),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(unnamed.title, "Tabs restored, a program lost");

        // An older journal (no programs said): today's words, today's weight.
        let older = journal_reopened_message(
            &reopened_fixture(DeathClass::Killed),
            None,
            None,
            Some(log),
            false,
            PanesWithoutFolder::NONE,
        );
        assert_eq!(older.title, "Tabs restored, programs lost");
        assert_eq!(older.severity, Severity::Warn);
        assert_eq!(older.detail[0], JOURNAL_LOSS);
    }

    #[test]
    fn a_program_list_reads_as_a_person_would_say_it() {
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
        assert_eq!(listed(&names(&["vim"]), 0), "vim");
        assert_eq!(listed(&names(&["vim", "claude"]), 0), "vim and claude");
        assert_eq!(
            listed(&names(&["vim", "claude", "less"]), 0),
            "vim, claude and less"
        );
        assert_eq!(
            listed(&names(&["a", "b", "c", "d", "e"]), 0),
            "a, b, c and 2 other programs"
        );
        assert_eq!(listed(&names(&[]), 1), "a program");
        assert_eq!(listed(&names(&[]), 2), "2 programs");
        assert_eq!(listed(&names(&["vim"]), 1), "vim and another program");
    }

    /// A crashed run's journal that was taken and NOT reopened is said, with
    /// the reason — unreadable, or skipped by the brake — and, skipped, what it
    /// held and what ran in it, and `Open log` (day five, D18: the row called a
    /// kill a crash and named nothing). A crash files under `crash`, a second
    /// stop under `session`.
    #[test]
    fn a_journal_not_reopened_is_said_with_its_reason() {
        let log = "/Users/_an/Library/Logs/aterm/aterm.log";
        let rows: Vec<Message> = journal_note_fixtures()
            .iter()
            .map(|note| journal_note_message(note, Some(std::path::Path::new(log))))
            .collect();
        assert_eq!(rows[0].title, "Couldn't restore your last tabs");
        assert!(rows[0].detail[0].contains("could not be read"));
        assert!(
            rows[0]
                .detail
                .iter()
                .any(|line| line.contains("journal-9-9.toml"))
        );
        assert_eq!(rows[1].title, "Couldn't restore tabs after a crash");
        assert_eq!(
            rows[1].detail[0],
            "2 tabs in 1 window were not restored: aterm crashed within 90 s of restoring them, \
             so they were skipped in case they caused it"
        );
        assert_eq!(rows[1].detail[1], "Python was running in 1 of 2 tabs");
        assert_eq!(rows[1].tag, tags::CRASH);
        assert_eq!(rows[2].title, "Couldn't restore tabs after two stops");
        assert!(
            rows[2].detail[0].contains("stopped twice in a row within 90 s"),
            "{:?}",
            rows[2].detail
        );
        assert_eq!(rows[2].detail[1], "nothing was running in them");
        assert_eq!(rows[2].tag, tags::SESSION);
        assert!(
            !rows[2].title.contains("crash") && !rows[2].detail[0].contains("crash"),
            "a kill is never called a crash"
        );
        for row in &rows {
            assert_eq!(
                row.actions,
                [Intent::OpenPath {
                    path: log.to_string()
                }]
            );
            assert_eq!(row.severity, Severity::Warn);
            assert_eq!(row.key.as_deref(), Some(KEY_CRASH_JOURNAL));
            assert_eq!(attention(row), Ok(Attention::Failure));
        }
    }

    /// One message per family: a TERSE title — singular for one sentence, a
    /// count for several — the first sentence's head as `detail[0]`, every
    /// sentence whole behind it (the excerpt not repeated when it IS the
    /// sentence), a repeat folded, past the line cap the roll-up, and the
    /// family's key and the editor capsule on every one (design §10.3 C1–C9,
    /// §10.5 H9; this replaces the ruling-29 head tests).
    #[test]
    fn config_family_titles_are_terse_and_the_first_sentence_is_the_excerpt() {
        let mut warns = ConfigWarnings::default();
        assert!(
            warns.clone().into_messages().is_empty(),
            "nothing collected, nothing posted"
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::Keybindings,
            "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 3: unknown key \"windw_padding\"".into(),
        );
        warns.push(
            ConfigFamily::IgnoredKeys,
            "config line 9: unknown key \"colums\"".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        assert_eq!(
            warns.sentences().collect::<Vec<_>>(),
            [
                "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"",
                "config line 3: unknown key \"windw_padding\"",
                "config line 9: unknown key \"colums\"",
                "gpu applies on next launch (the renderer backend is chosen at startup)",
            ],
            "the echo is verbatim and in collection order, repeats folded"
        );
        assert_eq!(warns.told().len(), 4);
        let msgs = warns.into_messages();
        assert_eq!(msgs.len(), 3, "one per family");
        let kb = &msgs[0];
        assert_eq!(kb.title, "Keybinding skipped");
        assert_eq!(
            kb.detail,
            [
                "ctrl+x: no action foo",
                "\"ctrl+x\": unknown action \"foo\""
            ],
            "the chord is the subject, unquoted; the title already says skipped"
        );
        assert_eq!(kb.severity, Severity::Warn);
        assert_eq!(kb.key.as_deref(), Some("config.keybindings"));
        assert_eq!(kb.actions, [Intent::OpenConfigEditor { line: None }]);
        let keys = &msgs[1];
        assert_eq!(keys.title, "2 unknown settings");
        assert_eq!(
            keys.detail,
            [
                "line 3: unknown key \"windw_padding\"",
                "line 9: unknown key \"colums\""
            ]
        );
        let restart = &msgs[2];
        assert_eq!(restart.hold, Hold::LogOnly, "a waiting edit is a record");
        assert_eq!(restart.title, "GPU setting applies after restart");
        assert_eq!(
            restart.detail,
            ["gpu applies on next launch (the renderer backend is chosen at startup)"],
            "the sentence behind the plain title"
        );
        assert_eq!(restart.key.as_deref(), Some("config.restart"));

        // Every family's title, both forms.
        for (family, one, many) in [
            (
                ConfigFamily::Keybindings,
                "Keybinding skipped",
                "3 keybindings skipped",
            ),
            (
                ConfigFamily::IgnoredKeys,
                "Unknown setting",
                "3 unknown settings",
            ),
            (
                ConfigFamily::RetiredKeys,
                "Retired setting",
                "3 retired settings",
            ),
            (
                ConfigFamily::UnacceptedValues,
                "Couldn't use a setting's value",
                "Couldn't use 3 settings' values",
            ),
            (
                ConfigFamily::CursorTrail,
                "Couldn't apply the cursor trail",
                "3 errors in cursor trail settings",
            ),
            (
                ConfigFamily::Fonts,
                "Couldn't apply the font",
                "Couldn't apply 3 fonts",
            ),
            (
                ConfigFamily::Assets,
                "Couldn't load the image",
                "Couldn't load 3 images",
            ),
            (
                ConfigFamily::SecureKeyboard,
                "Couldn't change Secure Keyboard Entry",
                "Couldn't change Secure Keyboard Entry",
            ),
        ] {
            assert_eq!(family.title(1), one, "{family:?}");
            assert_eq!(family.title(3), many, "{family:?}");
        }

        // A font verdict's head sheds `; ignored`, and the sentence rides
        // whole behind it.
        let face = "f".repeat(20);
        let mut one = ConfigWarnings::default();
        one.push(
            ConfigFamily::Fonts,
            format!(
                "config font_family: \"{face}\" is not an admissible font (not found); ignored"
            ),
        );
        let msg = one.into_messages().remove(0);
        assert_eq!(msg.title, "Couldn't apply the font");
        // The excerpt in a person's words (ruling 306): no key, no quoting,
        // no `admissible`.
        assert_eq!(msg.detail[0], format!("no font named {face}"));
        assert_eq!(
            msg.detail[1],
            format!("font_family: \"{face}\" is not an admissible font (not found); ignored")
        );
        let font = font_family_rejected("font_family: \"Nope\" is not an admissible font");
        assert_eq!(font.title, "Couldn't apply the font");
        assert_eq!(
            font.detail,
            [
                "no font named Nope",
                "font_family: \"Nope\" is not an admissible font"
            ]
        );
        assert_eq!(font.key.as_deref(), Some("config.font-family"));

        // Past the engine's line cap the last line is the roll-up.
        let mut many = ConfigWarnings::default();
        many.extend(
            ConfigFamily::IgnoredKeys,
            (0..(DETAIL_LINES_CAP + 5)).map(|i| format!("config line {i}: unknown key \"k{i}\"")),
        );
        let msg = many.into_messages().remove(0);
        assert_eq!(
            msg.title,
            format!("{} unknown settings", DETAIL_LINES_CAP + 5)
        );
        assert_eq!(msg.detail.len(), DETAIL_LINES_CAP);
        assert_eq!(
            msg.detail[DETAIL_LINES_CAP - 1],
            format!("\u{2026} and 6 more \u{2014} {VALIDATOR_HINT}")
        );
        assert_eq!(
            msg.detail[DETAIL_LINES_CAP - 2],
            format!(
                "line {}: unknown key \"k{}\"",
                DETAIL_LINES_CAP - 2,
                DETAIL_LINES_CAP - 2
            )
        );
    }

    /// A RETIRED SPELLING IS NOT AN UNKNOWN KEY (review of the second
    /// origin/main merge, 2026-09-23; design ruling 42). atpkg still reads
    /// `[packages] auto_update = false` as `enabled = false`, and
    /// `seed_install` as `auto_install` while that is unset. They are their
    /// own family: a record that never reaches the glass, whose words say
    /// what they are. The sentences are the real ones
    /// (`app_config::collect_key_notices`, the launch's and every reload's
    /// seam), so the pin cannot drift from the validator's words.
    #[test]
    fn a_retired_spelling_is_a_record_and_never_counted_as_an_unknown_key() {
        let src = "[packages]\nauto_update = false\nseed_install = false\n";
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, src);
        let msgs = warns.into_messages();
        assert_eq!(
            msgs.len(),
            1,
            "one family, and it is not the ignored keys: {msgs:?}"
        );
        let retired = &msgs[0];
        // Both of the ignored-key family's wordings are false for a spelling that
        // is still READ: refusing only the old one would let this pass vacuously
        // the day the family was reworded.
        assert!(
            !retired.title.contains("nknown")
                && !retired.title.contains("not known to this build")
                && !retired.title.contains("no effect"),
            "a working opt-out called unknown or inert: {:?}",
            retired.title
        );
        assert_eq!(retired.title, "2 retired settings");
        assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
        assert_eq!(retired.hold, Hold::LogOnly, "a record, never on glass");
        assert_eq!(retired.severity, Severity::Info, "nothing is broken");
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("it is read as `enabled = false`")),
            "{:?}",
            retired.detail
        );
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("the old key is still read")),
            "{:?}",
            retired.detail
        );

        // Beside a typo, the deprecated `game_font` and the retired
        // `channel`: the typo is the one key the person's setting silently
        // misses, one row; `game_font` and `auto_update`, which still apply,
        // and `channel`, which a removed feature left behind, are the record
        // (design ruling 213: nothing pressed brings a removed feature back).
        let src = "game_font = \"chunky\"\nwindw_padding = 20\n\
                   [packages]\nenabled = true\nauto_update = false\nchannel = \"stable\"\n";
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, src);
        assert_eq!(
            warns.sentences().count(),
            4,
            "the stderr echo still says every one: {:?}",
            warns.told()
        );
        let msgs = warns.into_messages();
        assert_eq!(msgs.len(), 2, "{msgs:?}");
        let ignored = &msgs[0];
        assert_eq!(ignored.key.as_deref(), Some("config.ignored-keys"));
        assert_eq!(ignored.hold, Hold::Default);
        assert_eq!(ignored.severity, Severity::Warn);
        assert_eq!(ignored.title, "Misspelled setting");
        assert!(ignored.detail.iter().any(|l| l.contains("windw_padding")));
        assert!(
            ignored.detail.iter().all(|l| !l.contains("game_font")
                && !l.contains("auto_update")
                && !l.contains("packages.channel")),
            "{:?}",
            ignored.detail
        );
        let retired = &msgs[1];
        assert_eq!(retired.key.as_deref(), Some("config.retired-keys"));
        assert_eq!(retired.hold, Hold::LogOnly);
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("packages.channel has no effect")),
            "{:?}",
            retired.detail
        );
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("`game_font` is deprecated"))
        );
        assert!(
            retired
                .detail
                .iter()
                .any(|l| l.contains("it still keeps automatic updates off")),
            "{:?}",
            retired.detail
        );

        // `--validate-config` still lists every key that needs an edit.
        let all = crate::native_config_language::ignored_key_warnings(src);
        assert_eq!(all.len(), 4, "{all:?}");
    }

    /// THE COMMONEST CONFIG WARNING — one misspelled key: the title is terse
    /// and the near-miss spelling is the excerpt (the key and its suggestion,
    /// without the no-effect aside), with the whole sentence behind Details.
    /// The sentence is the real one — `app_config::ignored_key_notices` over
    /// the text a live edit would save — so the fixture cannot drift from the
    /// validator's words.
    #[test]
    fn a_lone_ignored_key_warning_titles_its_head_and_keeps_the_sentence_whole() {
        let notices = crate::app_config::ignored_key_notices("windw_padding = 20\n");
        assert_eq!(notices.len(), 1, "{notices:?}");
        let whole = notices[0].strip_prefix("config ").expect("the prefix");
        let mut warns = ConfigWarnings::default();
        warns.extend(ConfigFamily::IgnoredKeys, notices.clone());
        let msg = warns.into_messages().remove(0);
        assert_eq!(msg.title, "Misspelled setting");
        assert_eq!(
            msg.detail[0], "windw_padding \u{2192} window_padding",
            "the key and its near miss; the line number is behind Details"
        );
        assert_eq!(msg.detail[1], whole, "the sentence survives whole");
        assert_eq!(msg.key.as_deref(), Some("config.ignored-keys"));
        // Once a clean reload resolves it, the log says it was fixed, under
        // ✓ Success, the warning's title its first detail line (ruling 265).
        assert_eq!(msg.finished.as_deref(), Some("Misspelled setting fixed"));
        let now = aterm_messages::Instant::now();
        let mut center =
            aterm_messages::MessageCenter::new(aterm_messages::MessageLog::empty(), now);
        let id = center
            .post(msg.clone(), aterm_messages::WallStamp { unix_ms: 1 }, now)
            .id;
        assert_eq!(
            center.resolve_key_prefix("config.", aterm_messages::Outcome::Ok, now),
            1
        );
        let rec = center.log().get(id).unwrap();
        assert_eq!(rec.title, "Misspelled setting fixed");
        assert_eq!(
            rec.detail.first().map(String::as_str),
            Some("Misspelled setting")
        );
        assert_eq!(
            (rec.severity, rec.glyph.ch()),
            (aterm_messages::Severity::Success, '\u{2713}')
        );

        // The plain form is the bare key.
        assert_eq!(
            unknown_key_excerpt("line 4: foo_bar is unknown to this build and has no effect")
                .as_deref(),
            Some("foo_bar")
        );
        assert_eq!(
            unknown_key_excerpt("line 3: unknown key \"windw_padding\"").as_deref(),
            None,
            "neither shape: the head at a seam instead"
        );
        // A near miss sheds its no-effect aside; the head is the LONGEST that
        // fits outside a parenthesis, so it keeps the dash and the suggestion.
        let key = "k".repeat(40);
        let near = format!("line 4: {key} \u{2014} did you mean \"x\"? (no effect in this build)");
        assert_eq!(
            excerpt_head(&near, EXCERPT_CAP),
            format!("line 4: {key} \u{2014} did you mean \"x\"?")
        );
        // The plain form has no seam: it is its own head.
        let plain = format!("line 4: {key} is unknown to this build and has no effect");
        assert_eq!(excerpt_head(&plain, EXCERPT_CAP), plain);
        // A sentence with no seam inside the cap is its own excerpt.
        let wide = "x".repeat(EXCERPT_CAP + 1);
        let long = format!("{wide} (aside); tail");
        assert_eq!(excerpt_head(&long, EXCERPT_CAP), long);
        assert_eq!(excerpt_head(" (aside) only", EXCERPT_CAP), " (aside) only");
    }

    /// Every title a reporter here mints fits the engine's title cap
    /// without being clipped: a clipped title reads as a cut sentence.
    #[test]
    fn every_reporter_title_fits_the_title_cap() {
        for msg in every_reporter_message() {
            assert!(
                msg.title.chars().count() <= TITLE_CAP,
                "title over the cap: {:?}",
                msg.title
            );
            assert!(!msg.title.ends_with('\u{2026}'), "clipped: {:?}", msg.title);
            assert!(!msg.title.is_empty());
            assert!(
                msg.detail.iter().all(|l| !l.is_empty()),
                "no empty detail line: {msg:?}"
            );
        }
    }

    /// A CODEX row is named Codex, never Claude: the live upgrade's Codex
    /// lane files its rows beside Claude Code's (`codex-<tab>`), and the
    /// records were written when there was one agent.
    #[test]
    fn the_codex_lanes_records_name_codex() {
        use aterm_agent::harness::upgrade::{Agent, Phase};
        use aterm_agent::harness::upgrade_drive::Row;
        let codex = Row {
            agent: Agent::Codex,
            session: "codex-s-a".into(),
            tab: "s-a".into(),
            from: "0.157.0".into(),
            to: "0.157.1".into(),
            phase: Phase::Done,
            outcome: "codex on 0.157.1 · the same conversation resumed".into(),
            ..Row::default()
        };
        let done = agent_upgrade_done(&codex, "in tab 2");
        assert_eq!(done.title, "Codex moved onto 0.157.1");
        assert_eq!(done.detail, ["in tab 2", "the same conversation resumed"]);
        // A stop that repeats (ruling 283: a first one retries later, a record).
        let stalled = agent_upgrade_stalled(
            &Row {
                phase: Phase::Failed("no-resume-hint".into()),
                stop_streak: 2,
                streak_why: "no-resume-hint".into(),
                ..codex.clone()
            },
            1_790_311_076,
            "in tab 2",
        );
        assert_eq!(stalled.title, "Couldn't upgrade Codex in tab 2");
        assert!(
            stalled
                .detail
                .iter()
                .any(|l| l.contains("Codex 0.157.0 → 0.157.1")),
            "{:?}",
            stalled.detail
        );
        // It installs itself (ruling 380, the owner, 2026-09-28: "do I need
        // to do something? It's not clear."): nothing to do.
        let waiting = agent_upgrade_waiting("Codex", &["0.157.1"]).expect("a record");
        assert_eq!(waiting.title, "Codex 0.157.1 installs itself");
        assert!(waiting.detail[0].starts_with("nothing to do: "));
        // A mix names both products (review of 2026-09-26: "Newer agent
        // builds ready for 2 sessions" named neither), in the detail — the
        // title keeps to six words.
        let mixed = agent_upgrade_waiting("Claude Code and Codex", &["2.1.282", "0.157.1"])
            .expect("a record");
        assert_eq!(mixed.title, "Newer agent builds install themselves");
        assert_eq!(
            mixed.detail.first().map(String::as_str),
            Some("Claude Code and Codex sessions")
        );
        // A move that stopped after its `/exit` says where the conversation
        // is and what takes it back — not "quit it", which nothing runs to.
        let after_exit = agent_upgrade_stalled(
            &Row {
                phase: Phase::Failed("no-resume-hint".into()),
                exited_at: 1_790_311_000,
                ..codex.clone()
            },
            1_790_311_076,
            "in tab 2",
        );
        assert!(
            after_exit
                .detail
                .iter()
                .any(|l| l.contains("`codex resume` there takes its conversation back")),
            "{:?}",
            after_exit.detail
        );
        assert!(
            !after_exit.detail.iter().any(|l| l.contains("quit it")),
            "{:?}",
            after_exit.detail
        );
        // NEGATIVE CONTROL: a Claude row keeps every word it had.
        let claude = Row {
            agent: Agent::Claude,
            ..codex
        };
        assert_eq!(
            agent_upgrade_done(&claude, "in tab 2").title,
            "Claude Code moved onto 0.157.1"
        );
    }

    /// S1 AND S2 OF THE IN-FLIGHT REVIEW (2026-09-27). A Claude Code move that
    /// stopped after its SIGTERM ended the agent names what takes the
    /// conversation back — `claude --resume <conversation>` in the tab — never
    /// "quit it": nothing runs to quit. A move under way that does not move
    /// says it waits and that nothing is forced, offers no word (the harness
    /// refuses one while a move is under way) and names `--status` in a
    /// shell, never `--skip` or `--now`. NEGATIVE CONTROL: a Claude Code move
    /// that stopped before any exit keeps the words it had.
    #[test]
    fn a_claude_move_stopped_after_its_exit_or_stuck_under_way_says_what_moves_it() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_311_076;
        let session = "0badf00d-1111-2222-3333-444455556666";
        let tab = "s-b5cf2faabac5ce5127bd";
        let base = Row {
            session: session.into(),
            tab: tab.into(),
            from: "2.1.281".into(),
            to: "2.1.283".into(),
            behind_since: NOW - 600,
            ..Row::default()
        };
        let after_exit = agent_upgrade_stalled(
            &Row {
                phase: Phase::Failed("stale-exit".into()),
                exited_at: NOW - 60,
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert!(
            after_exit.detail.iter().any(|l| l.contains(&format!(
                "`claude --resume {session}` there takes its conversation back"
            ))),
            "{:?}",
            after_exit.detail
        );
        assert!(
            !after_exit
                .detail
                .iter()
                .any(|l| l.contains("quit it") || l.to_lowercase().contains("codex")),
            "{:?}",
            after_exit.detail
        );
        // Nothing runs in the tab to ask again: the tab's standing row, not
        // a round resting (ruling 283), and its why a person's words — no
        // raw stop word, no parenthesis, no backtick (ruling 284).
        assert_eq!(after_exit.title, "Couldn't upgrade Claude in tab 2");
        assert!(
            !after_exit.detail[0].contains("stale-exit")
                && !after_exit.detail[0].contains(['(', '`']),
            "{:?}",
            after_exit.detail
        );
        let before_exit = agent_upgrade_stalled(
            &Row {
                phase: Phase::Failed("signal-refused".into()),
                ..base.clone()
            },
            NOW,
            "in tab 2",
        );
        assert!(
            before_exit
                .detail
                .iter()
                .any(|l| l.contains("quit it and resume it by hand")),
            "{:?}",
            before_exit.detail
        );
        for phase in [
            Phase::Exiting { at_s: NOW - 400 },
            Phase::Relaunched { at_s: NOW - 400 },
        ] {
            let stuck = agent_upgrade_stalled(
                &Row {
                    phase: phase.clone(),
                    ..base.clone()
                },
                NOW,
                "in tab 2",
            );
            assert_eq!(
                stuck.detail[1],
                "no word moves it while it is under way: it goes on once what it waits on \
                 ends, and nothing is forced",
                "{phase:?}"
            );
            assert_eq!(
                stuck.detail.last(),
                Some(&format!(
                    "the same in any shell: `aterm harness upgrade {tab} --status`"
                )),
                "{phase:?}"
            );
            assert!(
                !stuck.detail[0].contains("stuck:") && !stuck.detail[0].contains(['(', '`']),
                "{phase:?}: {:?}",
                stuck.detail
            );
            assert!(stuck.actions.is_empty(), "{phase:?}: no word offered");
            assert!(
                !stuck.detail.iter().any(|l| l.contains("--skip")
                    || l.contains("--now")
                    || l.contains("Upgrade now")
                    || l.contains("Skip version")),
                "{phase:?}: {:?}",
                stuck.detail
            );
        }
    }

    /// The value-level twin of `update_words::no_update_lane_source_prompts_a_
    /// restart` over the reporters' words: no sentence asks for a restart
    /// unless it carries one of the sanctioned anchors on the same line —
    /// the restart-only keys (`columns/lines`, `gpu applies`), the GPU-lost
    /// remedy (`open a new window or restart`), the dead publisher's retry,
    /// the Windows backdrop.
    #[test]
    fn no_reporter_wording_prompts_a_restart() {
        let needles = ["restart", "relaunch", "reopen", "next launch", "reboot"];
        let anchors = [
            "columns/lines",
            "gpu applies",
            "open a new window or restart",
            "restart aterm to retry",
            "backdrop",
            // The restart-only record's title says WHEN the edit applies
            // (ruling 261: `GPU setting applies after restart`), never asks.
            "apply after restart",
            "applies after restart",
        ];
        let mut checked = 0usize;
        for msg in every_reporter_message() {
            for line in sentences(&msg) {
                checked += 1;
                let lower = line.to_lowercase();
                let prompts = needles.iter().any(|n| lower.contains(n));
                let anchored = anchors.iter().any(|a| lower.contains(a));
                assert!(!prompts || anchored, "asks for a restart: {line:?}");
            }
        }
        assert!(checked > 30, "the guard covers the reporters: {checked}");
    }

    /// The feedback rows: a terse title, the cause (or what to do) as
    /// `detail[0]`, a gesture's short hold; the pins that read the old
    /// sentences as titles ("The GPU was lost", "accessibility OFF", "has no
    /// effect on the CPU renderer" on glass) are re-pinned here.
    #[test]
    fn feedback_sentences_split_into_a_title_and_a_cause() {
        let msg = serious_mode_feedback(
            "Serious Mode was not changed: save conflict; current aterm.toml could not be admitted: x",
        );
        assert_eq!(msg.title, "Couldn't change Serious Mode");
        assert_eq!(
            msg.detail,
            [
                "save conflict",
                "current aterm.toml could not be admitted: x"
            ]
        );
        assert_eq!(msg.key.as_deref(), Some(KEY_SERIOUS_MODE));
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
        let conflict = serious_mode_feedback(
            "Serious Mode was not changed because aterm.toml changed first; its current value was kept.",
        );
        assert_eq!(conflict.detail[0], "aterm.toml changed first");
        let unverified = serious_mode_feedback(
            "Serious Mode may have been written but could not be verified; reload before retrying: gen moved",
        );
        assert_eq!(unverified.title, "Couldn't confirm Serious Mode");
        assert_eq!(unverified.detail[0], "reload before retrying");
        let msg = presence_not_saved(
            "presence.rim",
            "Presence Rim",
            "aterm.toml changed first; its current value was kept",
            false,
        );
        assert_eq!(msg.title, "Couldn't save Presence Rim");
        assert_eq!(msg.severity, Severity::Error);
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE));
        assert_eq!(
            msg.key.as_deref(),
            Some("fabric.presence-save.presence.rim")
        );
        let maybe = presence_not_saved("presence.band", "Presence Band", "io", true);
        assert_eq!(maybe.title, "Couldn't confirm Presence Band was saved");
        assert_eq!(maybe.severity, Severity::Warn);
        let msg = config_lane_error("Robi was not dismissed: the setting was not saved");
        assert_eq!(msg.title, "Couldn't dismiss Robi");
        assert_eq!(msg.detail, ["the setting was not saved"]);
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "Robi's is a gesture");
        let unknown = config_lane_error("Something new went wrong: the cause");
        assert_eq!(unknown.title, "Couldn't change a setting");
        assert_eq!(unknown.detail, ["Something new went wrong: the cause"]);
        assert!(
            !unknown.excerpt,
            "someone else's words stay behind Details (ruling 306)"
        );
        let msg = launch_load_failure(
            "aterm.toml could not be read at launch (permission denied (os error 13)) \u{2014} every setting is running at its default.",
        );
        assert_eq!(msg.title, "Couldn't read your settings");
        assert_eq!(msg.detail[0], "permission denied (os error 13)");
        // The consequence is the notice's own tail, said once (audit
        // 2026-09-24): no line of its own before the whole sentence.
        assert_eq!(
            msg.detail
                .iter()
                .filter(|l| l.contains("every setting is running at its default"))
                .count(),
            1,
            "{:?}",
            msg.detail
        );
        assert_eq!(msg.hold, Hold::For(HOLD_LAUNCH));
        assert_eq!(msg.key.as_deref(), Some(KEY_LAUNCH_LOAD));
        let invalid = launch_load_failure(
            "aterm.toml is not a valid configuration (expected `=` at line 3) \u{2014} every setting is running at its default.",
        );
        assert_eq!(invalid.title, "Couldn't load your settings");
        assert_eq!(invalid.detail[0], "a mistake on line 3", "ruling 306");
        let lineless = launch_load_failure(
            "aterm.toml is not a valid configuration (expected `=`) \u{2014} every setting is running at its default.",
        );
        assert!(!lineless.excerpt, "no line named: the title alone");
        let standing = gpu_lost();
        assert_eq!(standing.title, "GPU lost");
        assert_eq!(
            standing.detail,
            [
                "windows opened for the backdrop cannot redraw",
                "open a new window or restart aterm to get a visible one back"
            ]
        );
        assert_eq!(standing.hold, Hold::Standing);
        assert_eq!(standing.actions, [Intent::NewWindow]);
        let dead = a11y_publisher_dead("bus gone", true);
        assert_eq!(dead.title, "Screen reader access lost");
        assert_eq!(dead.hold, Hold::Standing, "a screen reader was attached");
        assert_eq!(
            a11y_publisher_dead("bus gone", false).hold,
            Hold::LogOnly,
            "nobody was listening: a record, not an FYI on the glass"
        );
        assert_eq!(
            dead.detail,
            ["restart aterm to retry", "bus gone"],
            "the retry alone on its line, the reason behind it; never the title again"
        );
        let cpu = cpu_renderer_no_effect("background_material", "no compositor");
        assert_eq!(cpu.key.as_deref(), Some("render.cpu.background_material"));
        assert_eq!(cpu.hold, Hold::LogOnly, "a disclosure is a record");
        assert_eq!(
            cpu.actions,
            [Intent::OpenSettings {
                route: "/appearance".to_string()
            }]
        );
        // A Hold press on a held session asked for what already is: a
        // record. A Release the fleet refused failed: a gesture row, titled
        // with the outcome.
        let held = fleet_hold(true);
        assert_eq!(held.title, "Session already held");
        assert_eq!(held.hold, Hold::LogOnly);
        assert_eq!(held.detail, ["the fleet holds it"]);
        let kept = fleet_hold(false);
        assert_eq!(kept.title, "Couldn't release the hold");
        assert_eq!(kept.severity, Severity::Warn);
        assert_eq!(kept.hold, Hold::For(HOLD_GESTURE));
        assert_eq!(kept.detail, ["the fleet holds it"]);
        assert!(kept.excerpt);
        assert_eq!(kept.key.as_deref(), Some(KEY_FABRIC_HOLD));
        assert_eq!(hold_refused("x").title, "Couldn't hold the session");
        assert_eq!(hold_refused("x").key.as_deref(), Some(KEY_FABRIC_HOLD));
        let failed = fabric_status("off", "Couldn't run Fabric Off", "exited 2", false);
        assert_eq!(failed.severity, Severity::Error);
        assert_eq!(failed.detail, ["exited 2"]);
        assert!(!failed.excerpt, "an exit code rides behind Details");
        let overridden = serious_mode_feedback(
            "Serious Mode was saved, but a newer aterm.toml edit now controls it.",
        );
        assert_eq!(overridden.title, "Couldn't apply Serious Mode");
        assert_eq!(
            overridden.detail,
            ["aterm.toml sets it"],
            "what overrode it"
        );
        let done = fabric_status("off", "Fabric Off finished", "", true);
        assert_eq!(done.hold, Hold::LogOnly, "a clean exit is a record");
        assert_eq!(log_did_not_open("/x/y.log").title, "Couldn't open the log");
        assert_eq!(log_did_not_open("/x/y.log").hold, Hold::For(HOLD_GESTURE));
    }

    /// A DIAGNOSTIC IS ONE PROBLEM, NOT ONE LINE (upstream e7dc1feee's
    /// `a_multi_line_diagnostic_is_painted_as_the_rows_it_actually_has`, ported
    /// onto R13). A rejected `aterm.toml` reaches the config lane as a `toml`
    /// parse error with its caret diagram: one message, a terse title, the
    /// parser's first row as the excerpt and its caret diagram behind Details.
    /// The error string comes from the REAL parser, not a literal, so the test
    /// cannot pass because the shape it assumes has changed.
    #[test]
    fn a_multi_line_diagnostic_is_one_message_with_the_rows_it_actually_has() {
        let error = "x = [bad\n"
            .parse::<aterm_toml::edit::DocumentMut>()
            .expect_err("a malformed document")
            .to_string();
        assert!(
            error.contains('\n'),
            "this test is about a multi-line diagnostic: {error:?}"
        );
        let msg = config_lane_error(&format!("Config observation was not valid TOML: {error}"));
        // ONE PROBLEM, ONE MESSAGE: a terse title, and the diagnostic's rows
        // are its detail lines — several, not one.
        assert_eq!(msg.title, "Couldn't read your settings");
        assert_eq!(msg.key.as_deref(), Some(KEY_CONFIG_LANE));
        assert!(msg.detail.len() > 1, "{:?}", msg.detail);
        // NO CONTROL CHARACTER REACHES A LINE, and no line is blank.
        for line in &msg.detail {
            assert!(
                line.chars().all(|ch| !ch.is_control()),
                "a control character took a cell: {line:?}"
            );
            assert!(!line.trim().is_empty(), "a blank row: {:?}", msg.detail);
        }
        // The band's excerpt is the finding in a person's words (ruling
        // 306), and the parser's own opening sentence and its caret row are
        // DIFFERENT lines after it, the caret row kept whole with its
        // alignment.
        assert!(
            msg.detail[0].starts_with("line 1: missing ") || msg.detail[0] == "a mistake on line 1",
            "{:?}",
            msg.detail
        );
        assert!(!msg.detail[0].contains('`'), "{:?}", msg.detail);
        assert!(
            msg.detail[1].starts_with("TOML parse error"),
            "{:?}",
            msg.detail
        );
        let caret = error
            .lines()
            .find(|row| row.contains('^'))
            .expect("the parser's caret row")
            .trim_end();
        let at = msg
            .detail
            .iter()
            .position(|line| line == caret)
            .unwrap_or_else(|| panic!("the caret row {caret:?} is no line: {:?}", msg.detail));
        assert!(at > 0, "the caret must not share a line with the sentence");
        // A control byte that survives the split is spelled, not dropped.
        let hostile = config_lane_error("Config observation was not valid TOML: bad\u{1b}[31m\tx");
        assert_eq!(hostile.detail, ["bad\u{00b7}[31m\u{00b7}x"]);
        // A one-line sentence: the terse title and its cause.
        let one = config_lane_error("Robi was not dismissed: the lane dropped it");
        assert_eq!(
            (one.title.as_str(), one.detail.as_slice()),
            (
                "Couldn't dismiss Robi",
                ["the lane dropped it".to_string()].as_slice()
            )
        );
        // Every head of design §10.3 C22's map.
        for (head, title) in [
            (
                "Config observation was not valid TOML",
                "Couldn't read your settings",
            ),
            ("Robi was not dismissed", "Couldn't dismiss Robi"),
            (
                "Config reconciliation failed; queued changes were not written",
                "Couldn't save settings changes",
            ),
            (
                "Manual saved aterm.toml, but its exact generation could not be admitted",
                "Couldn't apply saved settings",
            ),
            (
                "Config was saved, but its exact disk generation could not be admitted",
                "Couldn't apply saved settings",
            ),
            (
                "Config publication could not be verified",
                "Couldn't confirm settings saved",
            ),
            ("Config was NOT saved", "Couldn't save a settings change"),
        ] {
            assert_eq!(
                config_lane_error(&format!("{head}: the cause")).title,
                title,
                "{head}"
            );
        }
    }

    /// A PERSON'S SETTINGS ▸ PACKAGES VERB (design ruling 224): every row it
    /// can show — the verb's own, and queued at the store lock — is PROGRESS
    /// under the owner's rule (live, busy: the comet and the elapsed clock),
    /// titled in a few words in the lane's one noun, keyed to the lane's pass
    /// so the plan's reads restate it, revealed only after the progress grace
    /// (a 1–4 s no-op never flashes a row), with no capsule, and ending in
    /// its own finished words.
    #[test]
    fn a_persons_packages_rows_are_progress_after_the_grace() {
        let verbs = [
            (
                PackagesVerb::Check,
                "Checking ALab tools",
                "ALab tools checked",
            ),
            (
                PackagesVerb::Install,
                "Installing ALab tools",
                "ALab tools installed",
            ),
            (
                PackagesVerb::Remove,
                "Removing ALab tools",
                "ALab tools removed",
            ),
        ];
        for (verb, title, finished) in verbs {
            let row = packages_verb_row(verb);
            let waiting = packages_waiting_row(verb);
            assert_eq!(row.title, title);
            assert_eq!(waiting.title, WAITING_FOR_ALAB);
            for msg in [&row, &waiting] {
                assert_eq!(attention(msg), Ok(Attention::Progress), "{}", msg.title);
                assert!(aterm_messages::text::title_words(&msg.title) <= 6);
                assert_eq!(msg.reveal_after, Some(aterm_messages::PROGRESS_GRACE));
                assert_eq!(msg.key.as_deref(), Some(crate::toolchain_words::KEY_PASS));
                assert!(msg.meter.as_ref().is_some_and(|m| m.busy), "busy");
                assert!(msg.actions.is_empty(), "work in flight is no decision");
                assert!(!msg.excerpt, "the title alone");
                assert_eq!(msg.finished_title(), finished);
                assert!(matches!(msg.hold, Hold::Live { .. }));
            }
            // The wait's own bound, with a margin: the row outlives a queue
            // that could last the whole 30 minutes.
            assert!(matches!(
                waiting.hold,
                Hold::Live { stale_after } if stale_after
                    > Duration::from_secs(crate::ATPKG_WAIT_LOCK_SECS)
            ));
        }
        assert_eq!(PackagesVerb::Check.finished_work(), "ALab tools updated");
        assert_eq!(
            PackagesVerb::Install.finished_work(),
            "ALab tools installed"
        );
        // The machine apply is a sub-second local edit: no row.
        assert_eq!(
            crate::app_native::person_verb(&crate::native_app::PackagesRequest::MachineApply),
            None
        );
        assert_eq!(
            crate::app_native::person_echo(true, true),
            aterm_messages::EchoKind::Fault
        );
        assert_eq!(
            crate::app_native::person_echo(false, false),
            aterm_messages::EchoKind::Vanish
        );
        assert_eq!(
            crate::app_native::person_echo(false, true),
            aterm_messages::EchoKind::Complete
        );
    }

    /// A STOP ON WORK IN FLIGHT (design ruling 232): the paste row carries
    /// the one decision a progress row may — `Stop paste`, which stops that
    /// row's own work — and stays PROGRESS; the rewrap row carries none; any
    /// other consequential press on a moving row is refused.
    #[test]
    fn a_progress_row_may_carry_only_the_stop_of_its_own_work() {
        use aterm_messages::waits::{paste_row, rewrap_row};
        let pasting = paste_row(4, 1, 1_100_000, 4_200_000, aterm_messages::PROGRESS_GRACE);
        assert_eq!(attention(&pasting), Ok(Attention::Progress));
        assert_eq!(pasting.actions, [Intent::StopPaste { session: 4 }]);
        assert!(pasting.actions.iter().all(Intent::stops_work));
        let rewrapping = rewrap_row(4, 1_200_000, 3_400_000, aterm_messages::PROGRESS_GRACE);
        assert_eq!(attention(&rewrapping), Ok(Attention::Progress));
        assert!(rewrapping.actions.is_empty());
        let mut install_mid_flight = pasting.clone();
        install_mid_flight.actions = vec![Intent::ApplyUpdate { build: 9 }];
        assert_eq!(
            attention(&install_mid_flight),
            Err("a decision on work in flight")
        );
        // A navigation stays allowed beside work (it decides nothing).
        let mut navigation = rewrapping;
        navigation.actions = vec![Intent::OpenSettings {
            route: "/messages".into(),
        }];
        assert_eq!(attention(&navigation), Ok(Attention::Progress));
    }

    /// The band's config row at 60, 80 and 100 columns: at 60 the title and
    /// `Open aterm.toml` take the row, at 80 the correction alone fits, from
    /// 100 the whole correction.
    const PINNED_MISSPELLED_ROWS: [(&str, Option<&str>); 3] = [
        ("Misspelled setting", None),
        ("Misspelled setting", Some("\u{2192} window_padding")),
        (
            "Misspelled setting",
            Some("windw_padding \u{2192} window_padding"),
        ),
    ];

    /// THE BAND'S CONFIG ROW (ruling 261): a misspelled key reads
    /// `Misspelled setting`, its excerpt the correction — `windw_padding →
    /// window_padding` where it fits whole, the correction alone behind its
    /// arrow where it does not (ruling 306: a bare `window_padding` named the
    /// right spelling as the misspelled one), never the typo. Pinned at 60 and 80 columns, from the validator's
    /// real sentence.
    #[test]
    fn the_misspelled_setting_row_is_pinned_at_60_and_80_columns() {
        let notices = crate::app_config::ignored_key_notices("windw_padding = 20\n");
        let mut warns = ConfigWarnings::default();
        warns.extend(ConfigFamily::IgnoredKeys, notices);
        let msg = warns.into_messages().remove(0);
        assert_eq!(msg.title, "Misspelled setting");
        let mut app = crate::App::headless_for_test();
        app.post_message(msg);
        let row = |cols: usize| {
            let p = app.band_presentation(cols);
            let r = p.rows[0].clone();
            (r.title.1, r.detail.map(|(_, d)| d))
        };
        assert_eq!(
            vec![row(60), row(80), row(100)],
            PINNED_MISSPELLED_ROWS
                .iter()
                .map(|(t, d)| (t.to_string(), d.map(str::to_string)))
                .collect::<Vec<_>>()
        );
    }

    /// THE TITLE LINT (design ruling 261): the owner's "short plain English,
    /// no jargon", held over every title aterm's own builders write — this
    /// module's, the toolchain and update lanes', the paste and rewrap rows and
    /// the strain row. A glass title is at most six words (ruling 76) and a
    /// record's at most eight (the log reads it whole; a record may name a
    /// measured span); every title starts with a capital or a count (the brand
    /// `aterm` may lead in its own case), and carries no parenthesis, no colon
    /// and no file extension (`aterm.toml` is "your settings"). A Warn or Error
    /// that is not a question or a decision says what went wrong in ONE grammar:
    /// `Couldn't …`, or it names the loss (`GPU lost`, `Keystrokes dropped`,
    /// `Misspelled setting`).
    #[test]
    fn every_title_is_short_plain_and_states_what_happened() {
        const LOSS: [&str; 10] = [
            "lost",
            "dropped",
            "crashed",
            "skipped",
            "misspelled",
            "unknown",
            "errors",
            "full",
            "slowed",
            "stopped",
        ];
        let mut all = every_reporter_message();
        use crate::toolchain_words as tw;
        all.extend([
            tw::announced("installing 10 ALab program(s) over the network (about 3 GB on disk)"),
            tw::deferred("the network is metered"),
            tw::installed("claude 2.1.280"),
            tw::ended("3 programs updated", true),
            tw::failed("trust: disk full"),
            tw::install_failed("trust: disk full"),
            tw::not_installed("nothing to install"),
            tw::first_run_short(tw::FirstRunShort::Failed, "No space left on device"),
            tw::first_run_short(tw::FirstRunShort::Nothing, "seed unusable"),
            tw::appnotice(
                tags::TOOLCHAIN,
                "aterm pkg install claude: 2.1.281 installed",
            ),
            tw::managed_current(
                "claude 2.1.280 (Anthropic latest)",
                0,
                true,
                crate::toolchain_words::HookDialect::Zsh,
            )
            .expect("a record"),
            tw::managed_current(
                "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)",
                0,
                true,
                crate::toolchain_words::HookDialect::Zsh,
            )
            .expect("a record"),
        ]);
        all.extend(tw::machine_settings(&format!(
            "spotlight-noindex 73 dir(s) migrated; {}",
            atpkg::machine::UNIVERSAL_CONTROL_ENTRY
        )));
        for verb in [
            PackagesVerb::Check,
            PackagesVerb::Install,
            PackagesVerb::Remove,
        ] {
            all.push(packages_verb_row(verb));
            all.push(packages_waiting_row(verb));
        }
        all.push(aterm_messages::waits::paste_row(
            3,
            1,
            1_100_000,
            4_200_000,
            Duration::ZERO,
        ));
        all.push(aterm_messages::waits::rewrap_row(
            3,
            1_200_000,
            3_400_000,
            Duration::ZERO,
        ));
        // The update lane's rows too (ruling 309: `Update didn't finish` and
        // `Update didn't install` read apart from every `Couldn't …` beside
        // them, and no lint read them) — the health warnings included since
        // ruling 311 keyed their heal and latch on their kind, not their words.
        all.extend(
            every_lane_message()
                .into_iter()
                .filter(|m| m.tag == tags::UPDATE),
        );
        let mut failures = Vec::new();
        for msg in &all {
            let t = msg.title.as_str();
            let words = aterm_messages::text::title_words(t);
            let cap = if msg.hold == Hold::LogOnly { 8 } else { 6 };
            let mut fault = Vec::new();
            if words > cap {
                fault.push(format!("{words} words (at most {cap})"));
            }
            let first = t.chars().next().unwrap_or(' ');
            if !(first.is_uppercase() || first.is_ascii_digit() || t.starts_with("aterm ")) {
                fault.push("does not start with a capital".to_string());
            }
            if t.contains(['(', ')']) {
                fault.push("a parenthesis".to_string());
            }
            if t.contains(':') {
                fault.push("a colon".to_string());
            }
            let bytes = t.as_bytes();
            let extension = t.match_indices('.').any(|(at, _)| {
                at > 0
                    && bytes[at - 1].is_ascii_alphanumeric()
                    && t[at + 1..]
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .next()
                        .is_some_and(|ext| {
                            (2..=8).contains(&ext.len())
                                && ext.bytes().all(|b| b.is_ascii_lowercase())
                        })
            });
            if extension {
                fault.push("a file extension".to_string());
            }
            // A question, a decision, work in flight — or a row whose title
            // IS its remedy (`Move aterm to Applications`): what to do,
            // which the failure grammar would bury.
            let decision = msg.is_ask()
                || msg.actions.iter().any(Intent::is_consequential)
                || matches!(msg.hold, Hold::Live { .. })
                || t.starts_with("Move ");
            let lower = t.to_lowercase();
            if matches!(msg.severity, Severity::Warn | Severity::Error)
                && !decision
                && !t.starts_with("Couldn't ")
                && !lower
                    .split(|c: char| !c.is_alphanumeric())
                    .any(|w| LOSS.contains(&w))
            {
                fault.push("a failure that neither says Couldn't nor names a loss".to_string());
            }
            if !fault.is_empty() {
                failures.push(format!("{t:?} ({:?}): {}", msg.severity, fault.join(", ")));
            }
        }
        assert!(failures.is_empty(), "titles:\n{}", failures.join("\n"));
    }

    /// THE OWNER'S RULE, OVER EVERY BUILDER (design §10.1, ruling 76): every
    /// message a reporter here posts is progress, a decision, a failure or a
    /// record — never a confirmation, an FYI or progress with no indicator on
    /// the glass — and every glass title is terse.
    #[test]
    fn every_glass_message_earns_its_row() {
        let mut classes = std::collections::BTreeMap::new();
        let all = every_reporter_message();
        // A key a removed feature left behind (`show_hud`) is a RECORD in
        // the retired family, never the ignored-key row (design ruling 213).
        let removed = all
            .iter()
            .find(|m| {
                m.detail
                    .iter()
                    .any(|l| l.contains("show_hud has no effect"))
            })
            .expect("the removed key is reported");
        assert_eq!(
            removed.key.as_deref(),
            Some(ConfigFamily::RetiredKeys.key())
        );
        assert_eq!(attention(removed), Ok(Attention::Record));
        // A record is never painted, so its first sentence is not repeated
        // as a cut excerpt above itself (audit 2026-09-24).
        let restart = all
            .iter()
            .find(|m| m.key.as_deref() == Some(ConfigFamily::Restart.key()))
            .expect("the restart record");
        assert_eq!(
            restart.detail.first().map(String::as_str),
            Some("columns/lines applies on next launch (resize the window to change size now)"),
            "{:?}",
            restart.detail
        );
        for msg in all {
            let class = attention(&msg)
                .unwrap_or_else(|why| panic!("{why}: {:?} ({:?})", msg.title, msg.hold));
            *classes.entry(format!("{class:?}")).or_insert(0usize) += 1;
        }
        // This module's builders are decisions, failures and records; its one
        // progress row (the admin install) went with the OS-installer protocols
        // (2026-09-24), and the live-work rows are `toolchain_words`', whose
        // own test holds them to the rule as `Progress` — as
        // `a_persons_packages_rows_are_progress_after_the_grace` holds the
        // Settings verbs' rows (ruling 224).
        for class in ["Decision", "Failure", "Record"] {
            assert!(
                classes.contains_key(class),
                "the fixture exercises every class it builds: {classes:?}"
            );
        }
        assert!(!classes.contains_key("Progress"), "{classes:?}");
        // The rule refuses what it must.
        let fyi = Message::new(tags::SYSTEM, Severity::Info, "Something happened");
        assert_eq!(attention(&fyi), Err("an FYI on glass"));
        let done = Message::new(tags::SYSTEM, Severity::Success, "Done");
        assert_eq!(attention(&done), Err("a confirmation on glass"));
        let blind = Message::new(tags::SYSTEM, Severity::Info, "Working").hold(Hold::Live {
            stale_after: Duration::from_secs(30),
        });
        assert_eq!(attention(&blind), Err("progress with no indicator"));
        let clause = Message::new(tags::SYSTEM, Severity::Warn, "It broke: here is why");
        assert_eq!(attention(&clause), Err("a clause in a glass title"));
        let long = Message::new(
            tags::SYSTEM,
            Severity::Warn,
            "one two three four five six seven",
        );
        assert_eq!(attention(&long), Err("a glass title over six words"));
        let sentence = Message::new(tags::SYSTEM, Severity::Warn, "It broke.");
        assert_eq!(attention(&sentence), Err("a sentence for a glass title"));
        // Words, not tokens: a route's `▸` and a name's `&` spend none.
        assert_eq!(
            aterm_messages::text::title_words("Open Privacy & Security \u{25b8} Full Disk Access"),
            6
        );
        // A measured LEVEL is Progress on the strain row alone (ruling 208).
        let level = |tag: Tag, key: &str| {
            Message::new(tag, Severity::Info, "Typing slowed by yes in tab 2")
                .key(key)
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_STRAIN,
                })
                .meter(aterm_messages::Meter::level(900, "7.1 of 8 cores"))
        };
        assert_eq!(
            attention(&level(tags::SYSTEM, aterm_messages::STRAIN_KEY)),
            Ok(Attention::Progress)
        );
        assert_eq!(
            attention(&level(tags::UPDATE, aterm_messages::STRAIN_KEY)),
            Err("a level off the strain row")
        );
        assert_eq!(
            attention(&level(tags::SYSTEM, "system.other")),
            Err("a level off the strain row")
        );
    }

    /// Every lane's rows beside this module's: the update flow, its records
    /// and outcomes, the toolchain lane's rows and records, a person's
    /// Settings ▸ Packages verbs and the session waits — the census the
    /// marks are held over (ruling 302).
    fn every_lane_message() -> Vec<Message> {
        use crate::toolchain_words as tw;
        use crate::update_words as uw;
        use aterm_update::Progress as P;
        let mut all = every_reporter_message();
        all.extend([
            uw::progress(
                &P::Downloading {
                    version: "0.95.0".into(),
                    bytes_done: 37_000_000,
                    bytes_total: 74_000_000,
                },
                None,
                "",
            ),
            uw::progress(
                &P::Verifying {
                    version: "0.95.0".into(),
                },
                None,
                "",
            ),
            uw::staged("0.95.0", 7, Some(uw::ApplyPosture::Automatic)),
            uw::staged("0.95.0", 7, Some(uw::ApplyPosture::ManualByConfig)),
            uw::download_failed("0.95.0", "zip sha256 mismatch"),
            uw::download_postponed("on battery"),
            uw::installing("0.95.0"),
            uw::finishing("0.95.0"),
            uw::landed("0.95.0", 7, 0, None),
            uw::switch_started(Some("0.95.0"), "0.94.0"),
            uw::switch_stopped(Some("0.95.0"), "0.94.0", true, "typing"),
            uw::switch_stopped(Some("0.95.0"), "0.94.0", false, "child died"),
            uw::outcome("Update waiting", "x", Severity::Info),
            uw::outcome("Couldn't finish the update", "x", Severity::Warn),
            uw::needs_install(
                &crate::app_update_screen::update_installed_title(Some("0.95.0")),
                uw::INSTALL_FROM_MENU,
                Severity::Info,
                7,
            ),
            uw::needs_install("Couldn't install aterm v0.95.0", "x", Severity::Warn, 7),
            uw::needs_install(
                "Couldn't install aterm v0.95.0",
                uw::TRIES_AGAIN,
                Severity::Warn,
                7,
            ),
            uw::failed("Couldn't finish the update", "the helper exited 3", false),
            uw::health_warning(
                uw::HealthKind::Download,
                aterm_update::health_failing_title("pipeline"),
                "3 checks",
            ),
            uw::health_warning(
                uw::HealthKind::Stalled,
                uw::CHECKER_STALLED_TITLE,
                "the update check stopped answering 50 min ago while checking for a new \
                 version; aterm started a fresh one in its place.",
            ),
            uw::health_recovered("Couldn't download updates", "since Aug 27"),
            uw::scrollback_lost(12_345, 3),
            tw::announced("installing 10 ALab program(s) over the network (about 3 GB on disk)"),
            tw::deferred("the network is metered"),
            tw::installed("claude 2.1.280"),
            tw::ended("3 programs updated", true),
            tw::failed("trust: disk full"),
            tw::install_failed("trust: disk full"),
            tw::not_installed("nothing to install"),
            aterm_messages::waits::paste_row(3, 1, 1_100_000, 4_200_000, Duration::ZERO),
            aterm_messages::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO),
        ]);
        all.extend(tw::machine_settings("spotlight-noindex 73 dir(s) migrated"));
        for verb in [
            PackagesVerb::Check,
            PackagesVerb::Install,
            PackagesVerb::Remove,
        ] {
            all.push(packages_verb_row(verb));
            all.push(packages_waiting_row(verb));
        }
        all
    }

    /// THE MARK SAYS WHAT THE ENTRY IS (design rulings 302–304), over every
    /// lane's builders: a RECORD and a FAILURE wear their severity's mark —
    /// `✕` for an Error (round 21: `aterm crashed last time` wore the
    /// warning's `⚠` in an error's red), `⚠` for a Warn, `ℹ` and `✓` for the
    /// quiet two — never the working `↻` of a row still moving (a finished
    /// `Tabs restored after aterm stopped` read as a restore under way); a
    /// DECISION that warns wears its severity's mark, and an Info one `ℹ`, or
    /// `⇣` for the one build waiting on `Install now` (ruling 303: `aterm vX
    /// is ready` wore `✓` from one builder and a still `↻` from the other);
    /// WORK IN FLIGHT wears its activity's mark from one table — `⇣` a thing
    /// moving in, `↻` a check or a rewrap, `⏸` a wait, `⊖` a removal (ruling
    /// 304: the `·` read as a bullet), `↑` a thing going out.
    #[test]
    fn every_mark_says_what_the_entry_is() {
        let faults: Vec<String> = every_lane_message()
            .into_iter()
            .filter(|msg| msg.key.as_deref() != Some(aterm_messages::STRAIN_KEY))
            .filter(|msg| !mark_fits(msg))
            .map(|msg| {
                format!(
                    "{:?} ({:?}, {:?}): {}",
                    msg.title,
                    msg.severity,
                    msg.hold,
                    msg.glyph.ch()
                )
            })
            .collect();
        assert!(faults.is_empty(), "marks:\n{}", faults.join("\n"));
        // NEGATIVE CONTROLS, through the lint's own predicate (review of
        // round 21: the control built a row and never asked the rule): each
        // class the rulings name, wearing the mark they retired.
        let crash = Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
            .glyph(Glyph::or_fallback('\u{26a0}'));
        assert!(!mark_fits(&crash), "a crash in a warning's triangle");
        assert!(mark_fits(
            &crash.clone().glyph(Severity::Error.default_glyph())
        ));
        let relaunch = Message::new(tags::CRASH, Severity::Info, "aterm reopened quietly")
            .hold(Hold::LogOnly)
            .glyph(Glyph::or_fallback('\u{21bb}'));
        assert!(
            !mark_fits(&relaunch),
            "a finished record wearing the working mark"
        );
        assert!(mark_fits(
            &relaunch.clone().glyph(Severity::Info.default_glyph())
        ));
        let rewrap = aterm_messages::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO);
        assert!(mark_fits(&rewrap), "the control's row as built");
        let still = rewrap.glyph(Severity::Info.default_glyph());
        assert!(!mark_fits(&still), "work in flight wearing a record's mark");
    }

    /// The mark lint's rule for one entry ([`every_mark_says_what_the_entry_is`]).
    fn mark_fits(msg: &Message) -> bool {
        const MOVING: [char; 5] = ['\u{21e3}', '\u{21bb}', '\u{23f8}', '\u{2296}', '\u{2191}'];
        let mark = msg.glyph.ch();
        let own = msg.severity.default_glyph().ch();
        let moving = matches!(msg.hold, Hold::Live { .. })
            && msg
                .meter
                .as_ref()
                .is_some_and(|m| m.busy || m.fill_permille.is_some());
        let decision = msg.is_ask() || msg.actions.iter().any(Intent::is_consequential);
        if moving {
            MOVING.contains(&mark)
        } else if msg.hold == Hold::LogOnly || msg.severity != Severity::Info || !decision {
            mark == own
        } else {
            mark == '\u{2139}'
                || (mark == '\u{21e3}'
                    && msg
                        .actions
                        .iter()
                        .any(|i| matches!(i, Intent::ApplyUpdate { .. })))
        }
    }

    /// The restart-only family and the renderer's disclosures are RECORDS
    /// (design §10.3 C8, C13, C14): nothing broke, nothing presses.
    #[test]
    fn the_restart_family_and_the_render_notes_are_records() {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::Restart,
            "columns/lines applies on next launch (resize the window to change size now)".into(),
        );
        warns.push(
            ConfigFamily::Restart,
            "gpu applies on next launch (the renderer backend is chosen at startup)".into(),
        );
        let restart = warns.into_messages().remove(0);
        assert_eq!(restart.hold, Hold::LogOnly);
        assert_eq!(attention(&restart), Ok(Attention::Record));
        for record in [
            cpu_renderer_no_effect("background_opacity", "no translucent present path"),
            backdrop_declined(
                "background_material is styling the title bar only",
                "this display stack refused the DirectComposition backdrop swapchain",
            ),
        ] {
            assert_eq!(record.hold, Hold::LogOnly, "{}", record.title);
        }
    }

    /// C9's DEFECT FIX: the Secure Keyboard Entry refusal was one sentence with
    /// an 18-space run where a line continuation was lost; it is one clean
    /// sentence now, and its excerpt is the state the person set.
    #[test]
    fn secure_keyboard_refusal_has_no_space_run() {
        let mut warns = ConfigWarnings::default();
        warns.push(
            ConfigFamily::SecureKeyboard,
            "secure_keyboard_entry: the OS refused the change (OSStatus -25293) \u{2014} \
             Secure Keyboard Entry is NOT on"
                .into(),
        );
        let msg = warns.into_messages().remove(0);
        assert_eq!(msg.title, "Couldn't change Secure Keyboard Entry");
        assert_eq!(msg.detail[0], "Secure Keyboard Entry is NOT on");
        assert!(msg.detail[1].contains("OSStatus -25293"));
        assert!(
            msg.detail.iter().all(|l| !l.contains("  ")),
            "{:?}",
            msg.detail
        );
    }

    /// A glued row is the sentence and the diagram sharing one line — the shape
    /// the engine's `\n`-deleting sanitizer produced: `…column 11  |1 | font_px =`.
    fn glued(line: &str) -> bool {
        line.contains("column") && line.contains('|')
    }

    /// THE LAUNCH PATH KEEPS THE ROWS TOO. An `aterm.toml` that is invalid AT
    /// LAUNCH — every setting running at its default, the highest-stakes case —
    /// went through one `Message::line`, so the band, the Details page and the
    /// screen reader all read the caret diagram glued inline and the 240-char cut
    /// fell mid-word in the consequence. The lane path kept e7dc1feee's split;
    /// this one lost it when the banner became messages. Real parser, real notice.
    #[test]
    fn an_invalid_aterm_toml_at_launch_keeps_its_caret_diagram_in_rows() {
        let path = std::path::Path::new("/home/someone/.config/aterm/aterm.toml");
        let error = aterm_toml::from_str::<crate::app_config::Config>("font_px = \n")
            .err()
            .expect("a malformed aterm.toml");
        let notice = crate::app_config::launch_config_notice(
            path,
            crate::app_config::LaunchConfigProblem::Invalid(&error),
        );
        assert!(
            notice.contains('\n'),
            "this is about a multi-line notice: {notice:?}"
        );

        let msg = launch_load_failure(&notice);
        assert_eq!(
            msg.title, "Couldn't load your settings",
            "terse, and says it is broken"
        );
        assert!(msg.detail.len() > 1, "{:?}", msg.detail);
        for line in &msg.detail {
            assert!(!glued(line), "the diagram is glued to the words: {line:?}");
            assert!(line.chars().all(|ch| !ch.is_control()), "{line:?}");
        }
        assert!(
            msg.detail
                .iter()
                .any(|line| line.starts_with(char::is_whitespace) && line.contains('^')),
            "the caret row keeps its leading whitespace, so Details sets it as a diagram: {:?}",
            msg.detail
        );
        assert!(
            msg.detail
                .last()
                .is_some_and(|line| line.ends_with("and it loads on the next change.")),
            "the consequence arrives whole, not cut mid-word: {:?}",
            msg.detail
        );
        // A one-line problem is unchanged: the notice is its only line.
        let unreadable = crate::app_config::launch_config_notice(
            path,
            crate::app_config::LaunchConfigProblem::Unreadable(&"permission denied"),
        );
        let unreadable_msg = launch_load_failure(&unreadable);
        assert_eq!(unreadable_msg.title, "Couldn't read your settings");
        assert_eq!(
            unreadable_msg.detail.last(),
            Some(&unreadable),
            "the notice is its own last line: {:?}",
            unreadable_msg.detail
        );
    }

    /// A TRAIL PACK THAT DOES NOT PARSE KEEPS ITS ROWS, alone or in a crowd. Its
    /// sentence embeds `aterm_toml`'s caret diagram, and the config families
    /// handed it to one `Message::line` too. In a count, rows are what the
    /// budget spends: counting one diagram as one line let the engine's cap drop
    /// the roll-up, and a list that looks complete while it is not is the
    /// defect the roll-up exists to prevent.
    #[test]
    fn a_trail_pack_parse_error_keeps_its_rows_and_the_roll_up_survives() {
        let error = aterm_toml::from_str::<aterm_toml::Value>("trail = [\n")
            .expect_err("a malformed manifest")
            .to_string();
        assert!(error.contains('\n'), "{error:?}");
        let sentence = format!(
            "cursor_trail_packs[0] \"sparkle.toml\" invalid (TOML schema error: {error}); skipping"
        );

        // Alone: the family's short title (ruling 146), the head as the
        // excerpt, and every row of the sentence behind it.
        let lone =
            config_family_message(ConfigFamily::CursorTrail, std::slice::from_ref(&sentence));
        assert_eq!(lone.title, "Couldn't apply the cursor trail");
        assert_eq!(
            lone.detail[0],
            "cursor_trail_packs[0] \"sparkle.toml\" invalid"
        );
        assert!(lone.detail.len() > 2, "{:?}", lone.detail);
        for line in &lone.detail {
            assert!(!glued(line), "the diagram is glued to the words: {line:?}");
        }
        assert!(
            lone.detail
                .iter()
                .any(|line| line.trim_start().starts_with('|') && line.contains('^')),
            "the caret row is a line of its own: {:?}",
            lone.detail
        );

        // Enough of them that their ROWS overflow a message, though their count
        // (eight sentences) would not have.
        let many: Vec<String> = (0..8).map(|_| sentence.clone()).collect();
        let msg = config_family_message(ConfigFamily::CursorTrail, &many);
        assert!(msg.detail.len() <= DETAIL_LINES_CAP, "{}", msg.detail.len());
        assert!(
            msg.detail
                .last()
                .is_some_and(|line| line.starts_with("\u{2026} and ")),
            "the roll-up must survive the rows it rolls up: {:?}",
            msg.detail.last()
        );
        for line in &msg.detail {
            assert!(!glued(line), "{line:?}");
        }
    }

    /// TWO REMOVED KEYS ARE NOT "UNKNOWN". A removed feature's keys once
    /// rode the ignored-key family, and its COUNT title said "not known to
    /// this build" — so the owner's `show_scene_hud`, joined by one more
    /// Scene key, put the forward-compatibility story back in the title of a
    /// row whose every line says "was removed" (3164e20c0 fixed the
    /// sentences; the count reinstated the contradiction one level up). They
    /// are records now, in the retired family (design ruling 213): no row at
    /// every launch for a feature nothing can bring back.
    #[test]
    fn two_removed_keys_are_counted_as_having_no_effect_not_as_unknown() {
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(
            &mut warns,
            "show_scene_hud = true\nscene_rows = 3\n",
        );
        let msgs = warns.into_messages();
        assert!(
            msgs.iter()
                .all(|m| m.key.as_deref() != Some("config.ignored-keys")),
            "no ignored-key row: {msgs:?}"
        );
        let ignored = msgs
            .iter()
            .find(|m| m.key.as_deref() == Some("config.retired-keys"))
            .unwrap_or_else(|| panic!("the retired family: {msgs:?}"));
        assert_eq!(ignored.title, "2 retired settings");
        assert_eq!(ignored.hold, Hold::LogOnly, "a record");
        assert!(!ignored.title.contains("nknown"), "{:?}", ignored.title);
        assert!(
            ignored
                .detail
                .iter()
                .filter(|line| line.contains("Scene HUD was removed"))
                .count()
                >= 2,
            "{:?}",
            ignored.detail
        );
    }

    /// UNDER ONE TAB'S KEY, THE SAME STALL IS THE SAME BY ITS WORDS (the
    /// owner's report of 2026-09-27: a restart posted two tabs' stalls the
    /// owner had already read): wherever that tab now stands (the place on
    /// the move line is not compared), and an overdue one whatever its age.
    /// Two tabs' stalls are told apart by their titles (ruling 275: `…in tab
    /// 2`), never the sid. NEGATIVE CONTROLS: another stall and another
    /// target are other stalls; so is a row before ruling 270 (the move
    /// alone on its line) whose words differ, while one whose words are the
    /// same is the same stall.
    #[test]
    fn a_stall_is_the_same_by_its_words_wherever_its_tab_now_stands() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        const NOW: u64 = 1_790_529_236;
        // A refusal: a stall a new round meets again. (A round that gave up
        // is no stall since main's 2026-09-27 rest-and-re-arm.)
        let refused = |tab: &str| Row {
            tab: tab.into(),
            from: "2.1.280".into(),
            to: "2.1.283".into(),
            phase: Phase::Failed("not-a-shell-job".into()),
            ..Row::default()
        };
        let (a, b) = ("s-c543f4e0edd3439e5791", "s-0df27d3bf4b3b704621e");
        let row_a = agent_upgrade_stalled(&refused(a), NOW, "in tab 2");
        let row_b = agent_upgrade_stalled(&refused(b), NOW, "in tab 5");
        assert_ne!(row_a.title, row_b.title, "the band tells them apart");
        assert_eq!(row_a.detail[0], row_b.detail[0], "the same stall's words");
        assert!(
            row_a.detail[0].contains("foreground job"),
            "{:?}",
            row_a.detail
        );
        assert!(
            row_a.detail[..STALL_MOVE_LINE + 1]
                .iter()
                .all(|l| !l.contains(a)),
            "never the sid there: {:?}",
            row_a.detail
        );
        assert_eq!(
            row_a.detail[STALL_MOVE_LINE],
            "in tab 2 \u{00b7} Claude Code 2.1.280 \u{2192} 2.1.283"
        );

        // THE SAME STALL, wherever its tab now stands.
        let moved = agent_upgrade_stalled(&refused(a), NOW, "in its tab");
        assert!(same_stall(&row_a.detail, &moved.detail));
        // AND WHEREVER A REFUSED PRESS PUT ITS LINES (ruling 307): the row is
        // restated with the refusal above the stall's lines, and the record it
        // retires into is what a restart reads back.
        let mut restated = vec!["press it again: another step held the upgrade's lock".to_string()];
        restated.extend(row_a.detail.iter().cloned());
        assert!(same_stall(&restated, &row_a.detail), "{restated:?}");
        assert!(same_stall(&row_a.detail, &restated));
        let over = agent_upgrade_stall_over(
            aterm_agent::harness::upgrade::Agent::Claude,
            "harness.upgrade.s-a",
            &restated,
            "in tab 2",
            StallEnd::AsksAgain,
        );
        assert_eq!(over.title, "Claude upgrade asks again in tab 2");
        assert_eq!(
            over.detail,
            [
                format!("was: {}", row_a.detail[0]),
                row_a.detail[STALL_MOVE_LINE].clone()
            ]
        );
        // …in another window too: the place names the window once the owner
        // has two, and two tabs first in their own windows read apart.
        let windowed = |tab: &str, window: usize| {
            agent_upgrade_stalled(&refused(tab), NOW, &tab_place(Some(window), 1))
        };
        assert!(same_stall(&row_a.detail, &windowed(a, 2).detail));
        assert!(same_stall(&windowed(a, 1).detail, &windowed(a, 12).detail));
        assert_eq!(
            windowed(a, 2).detail[STALL_MOVE_LINE],
            "in window 2, tab 1 \u{00b7} Claude Code 2.1.280 \u{2192} 2.1.283"
        );
        assert_eq!(
            windowed(b, 2).title,
            "Couldn't upgrade Claude in window 2, tab 1"
        );
        assert_ne!(windowed(a, 1).title, windowed(b, 2).title);
        let overdue = |age: u64| {
            agent_upgrade_stalled(
                &Row {
                    phase: Phase::Pending,
                    behind_since: NOW - age,
                    wait: "not-idle:busy".into(),
                    ..refused(a)
                },
                NOW,
                "in tab 2",
            )
        };
        assert!(
            overdue(7 * 3_600).detail[0].starts_with("behind for "),
            "{:?}",
            overdue(7 * 3_600).detail
        );
        assert!(same_stall(
            &overdue(7 * 3_600).detail,
            &overdue(9 * 3_600).detail
        ));
        // NEGATIVE CONTROLS.
        assert!(
            !same_stall(&row_a.detail, &overdue(7 * 3_600).detail),
            "another stall"
        );
        let newer = agent_upgrade_stalled(
            &Row {
                to: "2.1.284".into(),
                ..refused(a)
            },
            NOW,
            "in tab 2",
        );
        assert!(!same_stall(&row_a.detail, &newer.detail), "another target");

        // A ROW BEFORE RULING 270: the move alone on its line.
        let unplaced = |detail: &[String]| -> Vec<String> {
            let mut d = detail.to_vec();
            d[STALL_MOVE_LINE] = d[STALL_MOVE_LINE]
                .split_once(" \u{00b7} ")
                .map(|(_, moved)| moved.to_string())
                .expect("placed");
            d
        };
        let old = unplaced(&row_a.detail);
        assert!(same_stall(&old, &row_a.detail), "{old:?}");
        assert!(same_stall(&row_a.detail, &old));
        assert!(same_stall(
            &unplaced(&overdue(7 * 3_600).detail),
            &overdue(9 * 3_600).detail
        ));
        // NEGATIVE CONTROLS: another stall, another target, in that layout
        // too; and words that differ.
        assert!(!same_stall(
            &unplaced(&overdue(7 * 3_600).detail),
            &row_a.detail
        ));
        assert!(!same_stall(&unplaced(&newer.detail), &row_a.detail));
        let mut reworded = old.clone();
        reworded[0] = "the move stopped (not-a-shell-job)".to_string();
        assert!(!same_stall(&reworded, &row_a.detail), "{reworded:?}");
        // A move line whose head is no place is compared whole.
        let mut odd = row_a.detail.clone();
        odd[STALL_MOVE_LINE] = format!("tab two \u{00b7} {}", old[STALL_MOVE_LINE]);
        assert!(!same_stall(&odd, &row_a.detail), "{odd:?}");
        for head in [
            "in window two, tab 1",
            "in window 2 tab 1",
            "in window 2, tab",
        ] {
            let mut odd = row_a.detail.clone();
            odd[STALL_MOVE_LINE] = format!("{head} \u{00b7} {}", old[STALL_MOVE_LINE]);
            assert!(!same_stall(&odd, &row_a.detail), "{odd:?}");
        }
    }

    /// ONE TAB'S UPGRADE KEY, AND NOTHING ELSE (review of 2026-09-27): the
    /// carried-row sweep (`upgrade_host::post_upgrade_stalls`) withdraws
    /// every live row this filter matches that its view does not own, so a
    /// match too wide would take other families' rows down. A tab's key —
    /// this build's, cleaned as it prints, or an earlier build's — names its
    /// tab; the waiting record's bare key, an empty tab, the finished moves'
    /// `done`, a key that only begins with the family's word, and every
    /// other family's key name none.
    #[test]
    fn a_tabs_upgrade_key_names_its_tab_and_no_other_key_does() {
        assert_eq!(agent_upgrade_key_tab("harness.upgrade.s-x"), Some("s-x"));
        let key = agent_upgrade_key("s-c543f4e0edd3439e5791");
        assert_eq!(agent_upgrade_key_tab(&key), Some("s-c543f4e0edd3439e5791"));
        for other in [
            KEY_AGENT_UPGRADE,
            "harness.upgrade.",
            "harness.upgrade.done",
            "harness.upgradex.s-x",
            "harness.upgrades.s-x",
            "harness.s-x",
            "harness.relaunch.s-x",
            "fabric.x",
            "",
        ] {
            assert_eq!(agent_upgrade_key_tab(other), None, "{other:?}");
        }
    }

    /// A PLACE THAT NAMES THE WINDOW (the owner with more than one window:
    /// `in window 2, tab 1`) keeps every title that names the tab within the
    /// glass title rule — six words, 48 characters. A title the window's word
    /// would push past it names the place after a comma (`Couldn't upgrade
    /// Claude yet, window 2, tab 1`: the title lint allows no brackets); every
    /// title that fits keeps its words. NEGATIVE CONTROLS: the one-window place
    /// never takes the comma form, and the same words placed plainly break the
    /// rule.
    #[test]
    fn a_place_that_names_the_window_keeps_the_titles_short() {
        use aterm_agent::harness::upgrade::{Agent, Phase};
        use aterm_agent::harness::upgrade_drive::Row;
        use aterm_messages::UpgradeWord;
        const NOW: u64 = 1_790_529_236;
        let row = |phase: Phase, wait: &str| Row {
            tab: "s-c543f4e0edd3439e5791".into(),
            from: "2.1.280".into(),
            to: "2.1.283".into(),
            phase,
            wait: wait.into(),
            behind_since: NOW - 7 * 3_600,
            ..Row::default()
        };
        // Behind the agent's own work before any notice: a row, not a record.
        let waits = row(Phase::Pending, "not-idle:shell");
        assert!(!waits.asks_on_its_own(NOW), "a row");
        let place = tab_place(Some(2), 1);
        assert_eq!(place, "in window 2, tab 1");
        assert_eq!(tab_place(None, 3), "in tab 3");
        let title = |row: &Row, place: &str| agent_upgrade_stalled(row, NOW, place).title;
        assert_eq!(
            title(&waits, "in tab 1"),
            "Couldn't upgrade Claude in tab 1 yet"
        );
        assert_eq!(
            title(&waits, &place),
            "Couldn't upgrade Claude yet, window 2, tab 1"
        );
        assert_eq!(
            title(&row(Phase::Failed("not-a-shell-job".into()), ""), &place),
            "Couldn't upgrade Claude in window 2, tab 1"
        );
        let refused = |word| {
            agent_upgrade_word_refused(word, Agent::Claude, ("s-1", "2.1.283"), "busy:x", &place)
                .title
        };
        assert_eq!(
            refused(UpgradeWord::Now),
            "Couldn't upgrade Claude now, window 2, tab 1"
        );
        assert_eq!(
            refused(UpgradeWord::NotToday),
            "Couldn't postpone the upgrade, window 2, tab 1"
        );
        assert_eq!(
            refused(UpgradeWord::Skip),
            "Couldn't skip Claude 2.1.283 in window 2, tab 1"
        );
        for title in [
            title(&waits, &place),
            refused(UpgradeWord::Now),
            refused(UpgradeWord::NotToday),
            refused(UpgradeWord::Skip),
            title(&waits, &tab_place(Some(12), 10)),
        ] {
            assert_eq!(
                aterm_messages::text::glass_title_fault(&title),
                None,
                "{title:?}"
            );
        }
        // NEGATIVE CONTROLS.
        assert_eq!(
            title(&waits, "in tab 12"),
            "Couldn't upgrade Claude in tab 12 yet"
        );
        assert_eq!(
            title(&waits, "in its tab"),
            "Couldn't upgrade Claude in its tab yet"
        );
        assert!(
            aterm_messages::text::glass_title_fault(&format!(
                "Couldn't upgrade Claude {place} yet"
            ))
            .is_some(),
            "placed plainly, the window's word breaks the rule"
        );
    }

    /// ONE SESSION'S UPGRADE, WAITING, IS A RECORD WORDED BY WHAT HOLDS IT
    /// (ruling 380; the owner, 2026-09-28: "What is this alert about 'Codex
    /// upgrade waits in tab 1'??? that is confusing to me. do I need to do
    /// something? It's not clear. I want upgrades to be applied
    /// automatically." — and the review of that day: the record promised "the
    /// first pause in your typing" of the incident's goal, which its daemon's
    /// turns held). The incident's own row, ten minutes behind: titled by its
    /// tab, `Info` in the log with no glass and no tab mark. Where only the
    /// ladder's comfort holds it (its screen settling), its sentence is the
    /// answer — nothing to do, when it starts, and from when a moment with the
    /// agent idle and nobody typing is enough (the ladder's Land rung on the
    /// person's clock, or `two hours behind` where the clock is not known);
    /// where this tab's own goal holds it, that goal and when it moves; where
    /// a turn aterm cannot place holds it, a turn that may be this tab's own
    /// (a subagent of this conversation) or another Codex session's — never
    /// presumed either, never a goal to pause here — said whole, unclipped. Then the tab and the move. Never `waits`,
    /// never `next turn end`, never the pause in the person's typing. A place
    /// that would break the title rule leaves the title (the move line keeps
    /// it). NEGATIVE CONTROL: the same session six hours behind on a turn
    /// still running is no longer healthy — a stall, and its row says it could
    /// not be done yet; six hours on another session's turn, its row names
    /// that and no goal to pause.
    #[test]
    fn one_waiting_upgrade_is_a_record_worded_by_what_holds_it() {
        use aterm_agent::harness::upgrade::{Agent, Phase};
        use aterm_agent::harness::upgrade_drive::Row;
        const PENDING: u64 = 1_790_572_868;
        let now = PENDING + 600;
        let row = Row {
            tab: "s-09205e59a0bbd30464d4".into(),
            from: "0.157.1".into(),
            to: "0.158.0".into(),
            agent: Agent::Codex,
            phase: Phase::Pending,
            behind_since: PENDING,
            wait: "settling".into(),
            wait_since: PENDING,
            ..Row::default()
        };
        assert_eq!(row.stall(now), None, "the ladder still has rungs to go");
        assert_eq!(row.lands_by(), Some(PENDING + 7_200));
        let msg = agent_upgrade_waiting_in(&row, "in tab 1");
        assert_eq!(msg.title, "Codex 0.158.0 installs itself in tab 1");
        assert_eq!(aterm_messages::text::glass_title_fault(&msg.title), None);
        assert_eq!((msg.severity, msg.hold), (Severity::Info, Hold::LogOnly));
        let from = crate::presence::local_offset_s().map_or_else(
            || "two hours behind".to_string(),
            |offset| aterm_messages::words::clock_words((PENDING + 7_200) * 1000, offset),
        );
        assert_eq!(
            msg.detail,
            vec![
                format!(
                    "nothing to do: it starts on its own at a quiet moment in the tab, and from \
                     {from} as soon as it is idle and nobody has typed there for 20 seconds"
                ),
                "in tab 1 \u{00b7} Codex 0.157.1 \u{2192} 0.158.0".to_string(),
            ]
        );
        // This tab's own goal (the kernel named its thread): what holds it,
        // and when it moves.
        let goal = agent_upgrade_waiting_in(
            &Row {
                wait: "goal".into(),
                ..row.clone()
            },
            "in tab 1",
        );
        // The goal pause (the owner's decision of 2026-09-28): aterm pauses
        // the goal for a moment at the Land rung and resumes it; nothing to
        // do.
        assert_eq!(
            goal.detail[0],
            format!(
                "a goal is running in this tab's Codex; from {from} aterm pauses the goal for a \
                 moment to install the new Codex, and resumes it right after: nothing to do"
            )
        );
        // The goal paused for the move: said so, nothing to do.
        let held = agent_upgrade_waiting_in(
            &Row {
                wait: "goal-held".into(),
                goal_held_since: PENDING + 7_300,
                ..row.clone()
            },
            "in tab 1",
        );
        assert_eq!(
            held.detail[0],
            "aterm paused its goal for a moment to install the new Codex, and resumes it right \
             after: nothing to do"
        );
        // A turn aterm cannot place: this conversation's subagent or another
        // session's, never presumed either, never a goal here.
        let unplaced = agent_upgrade_waiting_in(
            &Row {
                wait: "daemon-busy".into(),
                ..row.clone()
            },
            "in tab 1",
        );
        assert!(
            unplaced.detail[0].starts_with(
                "a turn is running on its Codex daemon that may be this tab's own (a subagent of \
                 this conversation) or another Codex session's"
            ),
            "{:?}",
            unplaced.detail
        );
        for msg in [&msg, &goal, &held, &unplaced] {
            for line in std::iter::once(&msg.title).chain(&msg.detail) {
                assert!(
                    !line.contains("waits")
                        && !line.contains("turn end")
                        && !line.contains("pause in your typing"),
                    "{line}"
                );
            }
        }
        assert!(!unplaced.detail[0].contains("goal") && !unplaced.detail[0].contains("pause"));
        // A place that breaks the title rule leaves the title, not the record.
        let far = agent_upgrade_waiting_in(
            &Row {
                agent: Agent::Claude,
                from: "2.1.282".into(),
                to: "2.1.283".into(),
                ..row.clone()
            },
            "in window 2, tab 11",
        );
        assert_eq!(far.title, "Claude Code 2.1.283 installs itself");
        assert!(far.detail[1].starts_with("in window 2, tab 11 \u{00b7} "));
        // NEGATIVE CONTROL: six hours behind, the turn still running.
        let late = PENDING + 6 * 3_600;
        let overdue = Row {
            wait: "not-idle:busy".into(),
            ..row.clone()
        };
        assert_eq!(overdue.stall(late).as_deref(), Some("overdue"));
        let stalled = agent_upgrade_stalled(&overdue, late, "in tab 1");
        assert_eq!(stalled.title, "Couldn't upgrade Codex in tab 1 yet");
        assert_eq!(
            stalled.detail[0],
            "behind for 6 h: its turn is still running"
        );
        // Six hours on a turn aterm cannot place: named so — a subagent of
        // this conversation or another session's, neither presumed — no goal
        // to pause.
        let other = agent_upgrade_stalled(
            &Row {
                wait: "daemon-busy".into(),
                ..row.clone()
            },
            late,
            "in tab 1",
        );
        assert!(
            other.detail[0].contains("a subagent of this conversation")
                && other.detail[0].ends_with("or another Codex session's")
                && !other.detail[0].contains("most often"),
            "{:?}",
            other.detail
        );
        for line in &other.detail {
            assert!(!line.contains("goal") && !line.contains("pause"), "{line}");
        }
        // Six hours on its own goal (something kept the pause off): aterm
        // pauses it once nothing else holds the tab, and pausing it here
        // moves it now.
        let own = agent_upgrade_stalled(
            &Row {
                wait: "goal".into(),
                ..row.clone()
            },
            late,
            "in tab 1",
        );
        assert!(
            own.detail[1].starts_with(
                "aterm pauses the goal for a moment to move it once nothing else holds the tab, \
                 and resumes it after; pausing it in this tab moves it now"
            ),
            "{:?}",
            own.detail
        );
        // A GOAL ATERM PAUSED AND HAS NOT RESUMED: a row of its own, worded as
        // the wait it is, with the one hand step, and no word of the
        // upgrade's offered. NEGATIVE CONTROL: the move done and nothing left
        // paused — no row.
        let left = Row {
            phase: aterm_agent::harness::upgrade::Phase::Done,
            goal_left_since: PENDING + 9_000,
            ..row.clone()
        };
        assert_eq!(left.stall(late).as_deref(), Some("goal-paused"));
        let msg = agent_upgrade_stalled(&left, late, "in tab 1");
        assert_eq!(msg.title, "Codex goal still paused in tab 1");
        assert_eq!((msg.severity, msg.hold), (Severity::Warn, Hold::Standing));
        assert!(
            msg.detail[0].contains("has not been able to resume it yet")
                && msg.detail[1] == "type /goal resume in the tab to go on with the goal now",
            "{:?}",
            msg.detail
        );
        assert!(agent_upgrade_words(&left, late).is_empty());
        assert!(agent_upgrade_capsules(&left, late).is_empty());
        // LEFT PAUSED ON PURPOSE, its thread fallen into a sandbox: its own
        // words, the relaunch its hand step, no word of the upgrade's.
        let sandboxed = Row {
            wait: "goal-sandboxed".into(),
            ..left.clone()
        };
        assert_eq!(sandboxed.stall(late).as_deref(), Some("goal-sandboxed"));
        let msg = agent_upgrade_stalled(&sandboxed, late, "in tab 1");
        assert_eq!(msg.title, "Codex goal left paused in tab 1");
        assert!(
            msg.detail[0].contains("fell into a sandbox")
                && msg.detail[1].contains("--dangerously-bypass-approvals-and-sandbox"),
            "{:?}",
            msg.detail
        );
        assert!(agent_upgrade_words(&sandboxed, late).is_empty());
        assert!(agent_upgrade_capsules(&sandboxed, late).is_empty());
        let done = Row {
            phase: aterm_agent::harness::upgrade::Phase::Done,
            ..row
        };
        assert_eq!(done.stall(late), None);
    }

    /// A NOTICE WAITING BEHIND A FULL QUEUE IS TOLD HOW IT MOVES (review of
    /// 2026-09-27: the row said `Upgrade now` does not move it and that it
    /// "moves once that ends", of a limit over by every word, while nothing of
    /// the upgrade's own ever ended the wait). An overdue upgrade waiting
    /// `queued`: its words name the wait, its remedy says the agent's next
    /// run moves it — typing in its tab, or `Upgrade now`, which types the
    /// question once more — and promises no period of its own (ruling 307);
    /// `Upgrade now` is
    /// offered (Remedy::Now), a row of the person's, not a record, titled as
    /// one that could not be done yet (ruling 380). NEGATIVE CONTROL: waiting on the READY
    /// answer, `Upgrade now` is not offered and the remedy says it does not
    /// move it.
    #[test]
    fn a_full_queues_row_says_the_next_turn_or_upgrade_now_moves_it() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::{Remedy, Row};
        use aterm_messages::UpgradeWord;
        const NOW: u64 = 1_790_529_236;
        let waiting = |wait: &str| Row {
            tab: "s-c543f4e0edd3439e5791".into(),
            from: "2.1.280".into(),
            to: "2.1.283".into(),
            phase: Phase::Announced { at_s: NOW, asks: 1 },
            behind_since: NOW - 30 * 86_400,
            wait: wait.into(),
            ..Row::default()
        };
        let queued = waiting("queued");
        assert_eq!(queued.remedy(NOW), Some(Remedy::Now));
        assert!(!queued.asks_on_its_own(NOW), "the person's to move: a row");
        let msg = agent_upgrade_stalled(&queued, NOW, "in tab 2");
        // Never `… upgrade waits in tab 2` (ruling 380, the owner's words of
        // 2026-09-28): it could not be done yet.
        assert_eq!(msg.title, "Couldn't upgrade Claude in tab 2 yet");
        assert_eq!(msg.hold, Hold::Standing);
        // A person's words, not the upgrade's own (ruling 307: `its notice
        // waits unread` read as a band row, and `30d` as a code).
        assert_eq!(
            msg.detail[0],
            "behind for 30 days: it has not read the upgrade's question yet"
        );
        let remedy = &msg.detail[1];
        for words in [
            "moves when Claude next runs",
            "type in its tab",
            UpgradeWord::Now.label(),
            "once more",
            "waiting longer each time, up to a day",
        ] {
            assert!(remedy.contains(words), "{words}: {remedy}");
        }
        assert!(!remedy.contains("once that ends"), "{remedy}");
        // A person's words (ruling 307): never the session's own; and no one
        // interval it does not keep (main's growing rest).
        assert!(!remedy.contains("session"), "{remedy}");
        assert!(!remedy.contains("every 2h30m"), "no one interval: {remedy}");
        assert!(remedy.len() <= aterm_messages::DETAIL_LINE_CAP, "{remedy}");
        assert!(agent_upgrade_words(&queued, NOW).contains(&UpgradeWord::Now));
        // ITS ANSWER SAYS WHAT THE PRESS DID (ruling 307): the question typed
        // once more — never `the wait for a quiet tab is waived`, of a session
        // already idle. NEGATIVE CONTROL: any other wait keeps those words.
        let asked = agent_upgrade_worded(&queued, UpgradeWord::Now, "in tab 2", true);
        assert_eq!(asked.title, "Claude is asked again in tab 2");
        assert!(asked.detail[0].contains("once more"), "{:?}", asked.detail);
        assert!(!asked.detail[0].contains("waived"), "{:?}", asked.detail);
        let hurried = agent_upgrade_worded(&queued, UpgradeWord::Now, "in tab 2", false);
        assert!(hurried.detail[0].contains("waived"), "{:?}", hurried.detail);
        // NEGATIVE CONTROL.
        let ready = waiting("not-ready");
        assert_eq!(ready.remedy(NOW), Some(Remedy::Waits));
        let msg = agent_upgrade_stalled(&ready, NOW, "in tab 2");
        assert!(
            msg.detail[1].contains("does not move it"),
            "{:?}",
            msg.detail
        );
        assert!(!agent_upgrade_words(&ready, NOW).contains(&UpgradeWord::Now));
    }
}
