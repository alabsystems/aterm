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
const VALIDATOR_HINT: &str = "run `aterm --validate-config` for the full list";

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
/// is unknown to this aterm build; …` — the title already says unknown, and
/// the line number is the sentence's, behind Details. `None` for a sentence
/// in neither shape.
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

/// The longest head of `sentence` that ends at a clause seam — before a
/// parenthesised aside (` (`), a semicolon (`; `) or a dash (` — `), OUTSIDE
/// any parenthesis (a seam inside an aside cuts the aside open, not the
/// sentence) — and fits `cap` characters; the whole sentence when none does
/// (the band's width law shapes it from there). The seams are the ones the
/// config sentences use: the near-miss key keeps its `did you mean` and sheds
/// the forward-compatibility aside, the plain unknown key keeps `is unknown to
/// this aterm build` and sheds `it will be preserved …`, a font sentence keeps
/// its verdict and `(not found)` and sheds `; ignored`.
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
    Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .retrospective()
        .glyph(Glyph::or_fallback('\u{26a0}'))
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
    let error = notice
        .split_once(" (")
        .and_then(|(_, rest)| rest.rsplit_once(") \u{2014} "))
        .and_then(|(error, _)| diagnostic_lines(error).into_iter().next())
        .filter(|row| !row.is_empty());
    let mut msg = Message::new(tags::CONFIG, Severity::Error, title);
    if let Some(error) = error {
        msg = msg.line(error);
    }
    // The notice's own tail says the consequence (`— every setting is
    // running at its default`): no line of its own before it.
    msg.lines(diagnostic_lines(notice))
        .action(Intent::OpenConfigEditor { line: None })
        .hold(Hold::For(HOLD_LAUNCH))
        .key(KEY_LAUNCH_LOAD)
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
    let msg = rows.fold(msg.line(excerpt), Message::line);
    let msg = msg
        .action(Intent::OpenConfigEditor { line: None })
        .key(KEY_CONFIG_LANE);
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

/// The live agent upgrade's supersede key: each new waiting record replaces
/// the last one recorded under it.
pub(crate) const KEY_AGENT_UPGRADE: &str = "harness.upgrade";

/// THE LIVE AGENT UPGRADE, WAITING (gap audit 2026-09-24: two sessions sat one
/// and two releases behind for 8h22m and nothing said so). A RECORD, never a
/// row — waiting for a turn end is the upgrade working, and the band carries
/// only what the person acts on (ruling e83d7d233) — so it is on Settings ▸
/// Messages, `messages.log` and `appstatus` as `kind=harness`. `product` is
/// the agent the waiting sessions run (`Claude Code`, `Codex`; `Claude Code
/// and Codex` for a mix, named in the detail under the generic title),
/// `waiting` the versions they move to, one per session; `None` when none
/// waits.
pub(crate) fn agent_upgrade_waiting(product: &str, waiting: &[&str]) -> Option<Message> {
    let first = *waiting.first()?;
    let n = waiting.len();
    let sessions = if n == 1 { "session" } else { "sessions" };
    // A MIX of products (`Claude Code and Codex`) is named in the detail:
    // the title keeps to the eight-word rule with the generic word.
    let (titled, mixed) = if product.contains(" and ") {
        ("agent", Some(product))
    } else {
        (product, None)
    };
    let target = if waiting.iter().all(|v| *v == first) {
        format!("{titled} {}", atpkg::progress::sanitize_for_tty(first, 24))
    } else {
        format!("Newer {titled} builds")
    };
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("{target} ready for {n} {sessions}"),
    );
    if let Some(both) = mixed {
        msg = msg.line(format!("{both} sessions, each on its own lane"));
    }
    msg = msg.line(if n == 1 {
        "it moves onto it at its next turn end"
    } else {
        "each moves onto it at its next turn end"
    });
    Some(msg.no_excerpt().hold(Hold::LogOnly).key(KEY_AGENT_UPGRADE))
}

/// THE LIVE AGENT UPGRADE, STALLED: a ROW, because only the person can move it
/// — it gave up, was refused, runs where typing into the tab cannot reach, or
/// has been behind for six hours ([`aterm_agent::harness::upgrade_drive::Row::stall`]).
/// `Standing`: it stays until the stall ends (the host resolves it) or the
/// person reads it. `detail[0]` is why; then what moves it, spelled for a
/// shell — any shell, never the stalled tab's own (its foreground is Claude,
/// and a line typed there is a prompt). Keyed per tab.
///
/// THE REMEDY IS THE ONE THAT WORKS FOR THIS KIND OF STALL (review of
/// 2026-09-25: it named `--now` for every kind, and `--now` moves only two).
/// Overdue: `--now` moves it at its next turn end — unless it waits on what
/// `--now` does not waive (the READY answer, a draft, a box, a hold, work under
/// the agent), which is named instead. Gave up: `--now` asks it
/// again. Held back in a pane: typing into the tab cannot reach it, so no word
/// moves it — quit it in its pane and resume it there, or `--skip`. Refused or
/// failed: the harness will not move it — quit and resume it by hand, or
/// `--skip`. (Neither says "restart": the reporters' restart guard,
/// `no_reporter_wording_prompts_a_restart`, reads every line.)
pub(crate) fn agent_upgrade_stalled(
    row: &aterm_agent::harness::upgrade_drive::Row,
    now: u64,
) -> Message {
    use aterm_agent::harness::upgrade_drive::Remedy;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let why = row
        .stall_words(now)
        .unwrap_or_else(|| "it is not moving".to_string());
    let tab = clean(&row.tab);
    let from = clean(&row.from);
    let cmd = format!("aterm harness upgrade {tab}");
    let remedy = match row.remedy(now) {
        Some(Remedy::Now) | None => format!(
            "in any shell, `{cmd} --now` moves it at its next turn end; `--skip` keeps it on \
             {from}"
        ),
        Some(Remedy::Waits) => format!(
            "`--now` does not move it past what it waits on ({}): it moves once that ends; in \
             any shell, `{cmd} --skip` keeps it on {from}",
            clean(&row.wait)
        ),
        Some(Remedy::AskAgain) => format!(
            "in any shell, `{cmd} --now` asks it again at its next turn end; `--skip` keeps it \
             on {from}"
        ),
        Some(Remedy::InItsPane) => format!(
            "typing into the tab cannot reach it, so no word moves it: quit it in its pane and \
             resume it there, or `{cmd} --skip` keeps it on {from}"
        ),
        Some(Remedy::ByHand) => format!(
            "the harness will not move it: quit it and resume it by hand, or `{cmd} --skip` \
             keeps it on {from}"
        ),
        Some(Remedy::ResumeInTab) => format!(
            "it no longer runs in the tab and the harness will not bring it back: `codex resume` \
             there takes its conversation back, or `{cmd} --skip` keeps this record quiet"
        ),
    };
    let who = agent_word(row.agent);
    let mut msg = Message::new(
        tags::HARNESS,
        Severity::Warn,
        format!("Couldn't upgrade {who}"),
    )
    .line(clean(&why))
    .line(format!("tab {tab} · {}", clean(&row.move_words())))
    .line(remedy)
    .hold(Hold::Standing)
    .key(&format!("{KEY_AGENT_UPGRADE}.{tab}"));
    for intent in agent_upgrade_capsules(row, now) {
        msg = msg.action(intent);
    }
    msg
}

/// The agent a row's words name: `Claude` or `Codex`.
fn agent_word(agent: aterm_agent::harness::upgrade::Agent) -> &'static str {
    match agent {
        aterm_agent::harness::upgrade::Agent::Claude => "Claude",
        aterm_agent::harness::upgrade::Agent::Codex => "Codex",
    }
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
        Some(Remedy::Now | Remedy::AskAgain) => true,
        Some(Remedy::Waits | Remedy::InItsPane | Remedy::ByHand | Remedy::ResumeInTab) => false,
    };
    let mut words = Vec::with_capacity(3);
    if now_moves {
        words.push(UpgradeWord::Now);
    }
    words.extend([UpgradeWord::NotToday, UpgradeWord::Skip]);
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
/// takes these words as it is resolved, so the entry the owner pressed is
/// the one that says what happened; with no such row, it is recorded. `row`
/// is the upgrade as the word left it.
pub(crate) fn agent_upgrade_worded(
    row: &aterm_agent::harness::upgrade_drive::Row,
    word: aterm_messages::UpgradeWord,
) -> Message {
    use aterm_messages::UpgradeWord;
    let clean = |s: &str| atpkg::progress::sanitize_for_tty(s, 64);
    let who = agent_word(row.agent);
    let (from, to, tab) = (clean(&row.from), clean(&row.to), clean(&row.tab));
    let (title, then) = match word {
        UpgradeWord::Now => (
            format!("{who} moves when its turn ends"),
            format!("the wait for a quiet tab is waived; it still waits for {who} to go idle"),
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
        .line(format!("tab {tab} · {}", clean(&row.move_words())))
        .line(then)
        .hold(Hold::LogOnly)
        .key(&format!("{KEY_AGENT_UPGRADE}.{tab}"))
}

/// THE OWNER'S WORD REFUSED (gap #21): nothing was written, and why — another
/// step held the lock (press it again), the upgrade moved on to a newer
/// build than the one pressed for, or `upgrade_drive::ask`'s own reason (a
/// restart under way, one stopped for good, none recorded).
pub(crate) fn agent_upgrade_word_refused(
    word: aterm_messages::UpgradeWord,
    agent: aterm_agent::harness::upgrade::Agent,
    to: &str,
    why: &str,
) -> Message {
    use aterm_messages::UpgradeWord;
    let who = agent_word(agent);
    let shown = atpkg::progress::sanitize_for_tty(to, 32);
    let title = match word {
        UpgradeWord::Now => format!("Couldn't upgrade {who} now"),
        UpgradeWord::NotToday => format!("Couldn't put off the {who} upgrade"),
        UpgradeWord::Skip => format!("Couldn't skip {who} {shown}"),
    };
    let reason = if why.starts_with("busy:") {
        "another step held the upgrade's lock, so nothing was written: press it again".to_string()
    } else if let Some(now_to) = why.strip_prefix("stale:") {
        format!(
            "the upgrade moved on to {}, so nothing was written for {shown}",
            atpkg::progress::sanitize_for_tty(now_to, 32)
        )
    } else {
        format!("nothing was written: {why}")
    };
    Message::new(tags::HARNESS, Severity::Warn, title)
        .sentence(atpkg::progress::sanitize_for_tty(&reason, 600))
}

/// THE LIVE AGENT UPGRADE, DONE: the owner's outcome line — the build the
/// session resumed on and the model its first answer named, or that the model
/// could not be confirmed ([`aterm_agent::harness::upgrade::restart_outcome`])
/// — as a RECORD. Until 2026-09-24 it went only to the ledger's JSONL, so a
/// model that changed across the move was news nobody was told.
pub(crate) fn agent_upgrade_done(row: &aterm_agent::harness::upgrade_drive::Row) -> Message {
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
    .line(format!("tab {}", clean(&row.tab)));
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
        Message::new(tags::PACKAGES, Severity::Warn, "Move aterm to Applications")
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
        PackagesVerb::Remove => ('\u{00b7}', Some(aterm_messages::Load::Disk)),
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
            ..Row::default()
        };
        let remedy = |phase: Phase, wait: &str, behind: u64| {
            let msg = super::agent_upgrade_stalled(
                &Row {
                    phase,
                    wait: wait.into(),
                    behind_since: NOW - behind,
                    ..base.clone()
                },
                NOW,
            );
            assert_eq!(msg.title, "Couldn't upgrade Claude");
            assert_eq!(msg.detail.len(), 3, "{:?}", msg.detail);
            msg.detail[2].clone()
        };
        assert_eq!(
            remedy(Phase::Pending, "not-idle:busy", 7 * 3_600),
            format!(
                "in any shell, `aterm harness upgrade {tab} --now` moves it at its next turn \
                 end; `--skip` keeps it on 2.1.281"
            ),
            "overdue"
        );
        assert_eq!(
            remedy(Phase::Failed("unanswered".into()), "", 60),
            format!(
                "in any shell, `aterm harness upgrade {tab} --now` asks it again at its next \
                 turn end; `--skip` keeps it on 2.1.281"
            ),
            "gave up"
        );
        let pane = remedy(Phase::Pending, "terminal:tmux", 60);
        assert_eq!(
            pane,
            format!(
                "typing into the tab cannot reach it, so no word moves it: quit it in its pane \
                 and resume it there, or `aterm harness upgrade {tab} --skip` keeps it on 2.1.281"
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
                format!(
                    "the harness will not move it: quit it and resume it by hand, or \
                     `aterm harness upgrade {tab} --skip` keeps it on 2.1.281"
                ),
                "{why}"
            );
            assert!(!by_hand.contains("--now"), "{why}: {by_hand}");
        }
        assert!(!pane.contains("--now"), "{pane}");
    }

    /// THE STALLED ROW CARRIES THE OWNER'S WORDS THAT MOVE IT (gap #21: the
    /// owner could steer an upgrade only by typing `aterm harness upgrade
    /// <tab> --now|--defer|--skip` into another shell). Two capsules, for
    /// THIS tab and THIS build: `Upgrade now` + `Not today` where `--now`
    /// moves it (overdue on a turn end, gave up — and a healthy wait, the
    /// waiting record's), `Not today` + `Skip version` where it would not
    /// (waiting on the READY answer, in a pane, stopped for good, or already
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
                &[Now, NotToday, Skip],
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
                    agent_upgrade_stalled(&row, NOW).actions,
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
            "Robi was not dismissed: the settings lane dropped the request",
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
        let upgrade = aterm_agent::harness::upgrade_drive::Row {
            tab: "s-b5cf2faabac5ce5127bd".into(),
            from: "2.1.281".into(),
            to: "2.1.282".into(),
            phase: aterm_agent::harness::upgrade::Phase::Failed("unanswered".into()),
            outcome: "claude restarted on 2.1.282 · model claude-opus-5-5".into(),
            ..Default::default()
        };
        all.push(agent_upgrade_stalled(&upgrade, 1_790_311_076));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                behind_since: 1_790_280_544,
                wait: "not-idle:busy".into(),
                ..upgrade.clone()
            },
            1_790_311_076,
        ));
        all.push(agent_upgrade_done(&upgrade));
        // The owner's word from the band (gap #21): what each word did, and
        // each kind of refusal.
        for word in aterm_messages::UpgradeWord::ALL {
            all.push(agent_upgrade_worded(&upgrade, word));
            for why in [
                "busy:another-sweep",
                "stale:2.1.283",
                "the upgrade in tab s-b5cf2faabac5ce5127bd stopped for good (no-resume): `--now` \
                 re-arms only one that gave up",
            ] {
                all.push(agent_upgrade_word_refused(
                    word,
                    aterm_agent::harness::upgrade::Agent::Claude,
                    "2.1.282",
                    why,
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
        all.push(agent_upgrade_done(&codex));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Failed("no-resume-hint".into()),
                ..codex.clone()
            },
            1_790_311_076,
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
        ));
        all.push(agent_upgrade_stalled(
            &aterm_agent::harness::upgrade_drive::Row {
                phase: aterm_agent::harness::upgrade::Phase::Pending,
                behind_since: 1_790_280_544,
                wait: "daemon-first:busy-thread".into(),
                ..codex.clone()
            },
            1_790_311_076,
        ));
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
        ]
    }

    /// THE COPY OF THE ONE QUESTION THIS FEATURE ASKS UNPROMPTED (the fence
    /// that held the retired card's caption, `notice.rs`, moved onto the
    /// words themselves — design §7.1). Three fences at once: the owner's
    /// restart-phrase ruling (`tools/grep_guard.sh` B10/B12), the honesty
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
        assert_eq!(
            msg.detail[0],
            format!("font_family: \"{face}\" is not an admissible font (not found)")
        );
        assert_eq!(
            msg.detail[1],
            format!("font_family: \"{face}\" is not an admissible font (not found); ignored")
        );
        let font = font_family_rejected("font_family: \"Nope\" is not an admissible font");
        assert_eq!(font.title, "Couldn't apply the font");
        assert_eq!(
            font.detail,
            ["font_family: \"Nope\" is not an admissible font"]
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
    /// and the near-miss spelling is the excerpt (the head up to the
    /// forward-compatibility aside), with the whole sentence behind Details.
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
            unknown_key_excerpt(
                "line 4: foo_bar is unknown to this aterm build; it will be preserved for \
                 forward compatibility"
            )
            .as_deref(),
            Some("foo_bar")
        );
        assert_eq!(
            unknown_key_excerpt("line 3: unknown key \"windw_padding\"").as_deref(),
            None,
            "neither shape: the head at a seam instead"
        );
        // A plain form sheds its forward-compatibility clause at the
        // semicolon; the head is the LONGEST that fits outside a parenthesis.
        let key = "k".repeat(40);
        let plain = format!(
            "line 4: {key} is unknown to this aterm build; it will be preserved for forward \
             compatibility"
        );
        assert_eq!(
            excerpt_head(&plain, EXCERPT_CAP),
            format!("line 4: {key} is unknown to this aterm build")
        );
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
        let done = agent_upgrade_done(&codex);
        assert_eq!(done.title, "Codex moved onto 0.157.1");
        assert_eq!(done.detail, ["tab s-a", "the same conversation resumed"]);
        let stalled = agent_upgrade_stalled(
            &Row {
                phase: Phase::Failed("no-resume-hint".into()),
                ..codex.clone()
            },
            1_790_311_076,
        );
        assert_eq!(stalled.title, "Couldn't upgrade Codex");
        assert!(
            stalled
                .detail
                .iter()
                .any(|l| l.contains("Codex 0.157.0 → 0.157.1")),
            "{:?}",
            stalled.detail
        );
        let waiting = agent_upgrade_waiting("Codex", &["0.157.1"]).expect("a record");
        assert_eq!(waiting.title, "Codex 0.157.1 ready for 1 session");
        // A mix names both products (review of 2026-09-26: "Newer agent
        // builds ready for 2 sessions" named neither), in the detail — the
        // title keeps to eight words.
        let mixed = agent_upgrade_waiting("Claude Code and Codex", &["2.1.282", "0.157.1"])
            .expect("a record");
        assert_eq!(mixed.title, "Newer agent builds ready for 2 sessions");
        assert_eq!(
            mixed.detail.first().map(String::as_str),
            Some("Claude Code and Codex sessions, each on its own lane")
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
            agent_upgrade_done(&claude).title,
            "Claude Code moved onto 0.157.1"
        );
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
        let msg =
            config_lane_error("Robi was not dismissed: the settings lane dropped the request");
        assert_eq!(msg.title, "Couldn't dismiss Robi");
        assert_eq!(msg.detail, ["the settings lane dropped the request"]);
        assert_eq!(msg.hold, Hold::For(HOLD_GESTURE), "Robi's is a gesture");
        let unknown = config_lane_error("Something new went wrong: the cause");
        assert_eq!(unknown.title, "Couldn't change a setting");
        assert_eq!(unknown.detail, ["Something new went wrong: the cause"]);
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
        assert_eq!(invalid.detail[0], "expected `=` at line 3");
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
        // The diagnostic's own opening sentence and its caret row are DIFFERENT
        // lines, the caret row kept whole with its alignment.
        assert!(
            msg.detail[0].starts_with("TOML parse error"),
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
        ("Misspelled setting", Some("window_padding")),
        (
            "Misspelled setting",
            Some("windw_padding \u{2192} window_padding"),
        ),
    ];

    /// THE BAND'S CONFIG ROW (ruling 261): a misspelled key reads
    /// `Misspelled setting`, its excerpt the correction — `windw_padding →
    /// window_padding` where it fits whole, the correction alone where it does
    /// not, never the typo. Pinned at 60 and 80 columns, from the validator's
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
        // The literal as the compiler reads it: a `\` line continuation eats
        // the newline and the next line's indentation.
        let src = include_str!("app_config.rs");
        let at = src
            .find("\"secure_keyboard_entry: the OS refused the change")
            .expect("the refusal's literal");
        let end = src[at + 1..].find("\",").expect("its end") + at + 1;
        let mut literal = String::new();
        let mut rest = &src[at + 1..end];
        while let Some(cut) = rest.find("\\\n") {
            literal.push_str(&rest[..cut]);
            rest = rest[cut + 2..].trim_start();
        }
        literal.push_str(rest);
        assert!(
            literal.contains("\\u{2014} Secure Keyboard Entry is NOT"),
            "{literal:?}"
        );
        assert!(
            !literal.contains("  "),
            "a space run in the sentence: {literal:?}"
        );
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
}
