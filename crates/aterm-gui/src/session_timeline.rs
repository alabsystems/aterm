// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-session USER METADATA + EVENT TIMELINE (session-metadata stage 1).
//!
//! Two small, session-scoped stores that live on [`crate::SessionCtx`] (the one
//! per-session context every holder — the owning `Session`, the registry
//! [`crate::session_store::SessionHandle`], and each control connection — already
//! shares by `Arc`):
//!
//! * [`SessionMeta`] — the USER-settable identity a driver (human or agent)
//!   stamps on a session over the control socket (`meta set title|description|
//!   icon|role|attention …`): a display title that OUTRANKS the OSC 0/2 title
//!   in tab labels, a free-text description, an icon token, a typed `role`
//!   (`operator` designates the fleet operator), and a typed `attention`
//!   escalation message (non-empty ⇒ the menu-bar status item badges).
//!   Orthogonal to the engine's OSC title (which programs keep rewriting):
//!   this is what the OPERATOR calls the session, not what the running
//!   program does.
//! * [`SessionTimeline`] — a bounded, drop-oldest ring of lifecycle events
//!   (`spawned`, `state-change`, `title-change`, `cwd-change`, `meta-change`),
//!   modeled on [`crate::turn_ledger::TurnLedger`] (same cap, same monotonic-ms
//!   clock, same clamp discipline). Read back by the `timeline` verb and scanned
//!   by the `subscribe … events` digest for `EVENT <sid> meta …` pushes.
//!
//! ## Why `SessionCtx`, not `SessionHandle`
//!
//! A `SessionHandle` is a CLONE living in the registry — cloned out per control
//! request under the store lock. State stored as plain handle fields would fork
//! on every clone and would force every hot-path reader (the per-frame tab-label
//! refill) through a `Store` read lock. On `SessionCtx` there is exactly ONE
//! copy per session, reachable lock-disjointly from the pool session (tab
//! labels), the registry handle (`sessions`/handoff projection), and the control
//! thread (`meta`/`timeline` verbs) — no store lock on any hot path, and the
//! recorders in `SessionStore` mutators reach it through the handle's `ctx` Arc
//! they already hold. Both locks are LEAVES: taken briefly, never across a
//! `Terminal` or `Store` lock.

use std::collections::VecDeque;

use aterm_grapheme::GraphemeClusters;

use crate::turn_ledger::now_ms;

/// Byte cap for `meta set title` (after trim). Small: it is a tab label.
pub(crate) const META_TITLE_MAX: usize = 120;
/// Byte cap for `meta set description` (after trim) — a paragraph, not a doc.
pub(crate) const META_DESCRIPTION_MAX: usize = 1024;
/// Byte cap for `meta set icon` (after trim) — an emoji / short token.
pub(crate) const META_ICON_MAX: usize = 64;
/// Byte cap for `meta set role` (after trim) — a short role token. The one
/// recognized value today is `operator` (the menu-bar status item keys on it);
/// other values are stored verbatim for future roles.
pub(crate) const META_ROLE_MAX: usize = 64;
/// Byte cap for `meta set attention` (after trim) — a one-line needs-human
/// message. NON-EMPTY means the session is escalating: the status item badges
/// the menu bar and lists the message. Unset it once the human has acted.
pub(crate) const META_ATTENTION_MAX: usize = 256;

/// True for characters that must never reach native/window chrome from USER
/// metadata. `char::is_control` covers C0/C1 (including every ASCII line break
/// and tab); the explicit format characters cover Unicode line separators,
/// bidi overrides/isolation, tags/fillers, and spoof-relevant default-ignorables.
/// ZWJ/ZWNJ and the standardized variation-selector blocks U+FE00..FE0F and
/// U+E0100..E01EF are deliberately allowed so ordinary emoji, joining scripts,
/// and ideographic variants survive.
fn is_forbidden_metadata_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{00ad}' // SOFT HYPHEN
                | '\u{034f}' // COMBINING GRAPHEME JOINER
                | '\u{061c}' // ARABIC LETTER MARK
                | '\u{115f}'..='\u{1160}' // Hangul fillers
                | '\u{17b4}'..='\u{17b5}' // Khmer inherent-vowel controls
                | '\u{180b}'..='\u{180f}' // Mongolian selectors / vowel separator
                | '\u{200b}' // ZERO WIDTH SPACE
                | '\u{200e}'..='\u{200f}' // directional marks
                | '\u{2028}'..='\u{202e}' // line/paragraph separators + bidi embedding/override
                | '\u{2060}'..='\u{206f}' // invisible operators + bidi isolates/deprecated controls
                | '\u{3164}' // HANGUL FILLER
                | '\u{feff}' // BOM / ZERO WIDTH NO-BREAK SPACE
                | '\u{ffa0}' // HALFWIDTH HANGUL FILLER
                | '\u{fff0}'..='\u{fff8}' // reserved specials
                | '\u{13430}'..='\u{1343f}' // Egyptian hieroglyph format controls
                | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
                | '\u{1d173}'..='\u{1d17a}' // musical symbol controls
                | '\u{e0000}'..='\u{e007f}' // language/tag characters
        )
}

#[must_use]
pub(crate) fn metadata_has_forbidden_formatting(value: &str) -> bool {
    value.chars().any(is_forbidden_metadata_char)
}

/// Canonical single-line presentation sanitizer shared by restored metadata
/// and every USER-metadata chrome boundary. Unsafe controls are removed and
/// the byte cap is applied only between grapheme clusters, so a combining mark
/// or ZWJ emoji is never bisected into malformed-looking chrome.
#[must_use]
pub(crate) fn sanitize_presentation_line(value: &str, max_bytes: usize) -> String {
    let filtered: String = value
        .chars()
        .filter(|ch| !is_forbidden_metadata_char(*ch))
        .collect();
    let value = filtered.trim();
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut sanitized = String::with_capacity(max_bytes);
    for grapheme in value.graphemes() {
        if sanitized.len().saturating_add(grapheme.len()) > max_bytes {
            break;
        }
        sanitized.push_str(grapheme);
    }
    let trimmed_len = sanitized.trim_end().len();
    sanitized.truncate(trimmed_len);
    sanitized
}

/// Sanitize one named USER metadata field for presentation/persistence.
/// Unknown fields and values that become empty are represented as unset.
#[must_use]
pub(crate) fn sanitize_metadata_value(field: &str, value: &str) -> Option<String> {
    let cap = SessionMeta::cap(field)?;
    let value = sanitize_presentation_line(value, cap);
    (!value.is_empty()).then_some(value)
}

/// How many timeline events a session retains (drop-oldest past this), sized
/// like the turn ledger: a long session stays readable, the ring never grows.
pub(crate) const TIMELINE_CAP: usize = 512;

/// Byte cap for one event's stored payload — the payload is a short `k=v` wire
/// token string (values already pct-encoded), so 256B holds every real event;
/// the clamp only guards a pathological title/cwd from bloating the ring.
const MAX_PAYLOAD: usize = 256;

/// The user-settable per-session metadata (`meta set`/`meta unset`). All
/// fields are `None` until a driver sets them; `user_title` (when set +
/// non-empty) outranks the live OSC title in tab labels and stays until unset.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct SessionMeta {
    /// Operator-chosen display title — the TOP rung of the tab-label chain.
    pub user_title: Option<String>,
    /// Free-text purpose/notes for the session (agents leave context here).
    pub description: Option<String>,
    /// Icon token (emoji or short name); reserved for the strip/UI stage.
    pub icon: Option<String>,
    /// TYPED role token. `operator` designates the fleet operator to the
    /// menu-bar status item (which falls back to the legacy `operator: …`
    /// title convention only when no session carries the typed role).
    pub role: Option<String>,
    /// TYPED needs-human escalation: non-empty ⇒ this session wants a human,
    /// and the value is the one-line reason shown in the status-item menu.
    /// Replaces the legacy `⚠`-title convention (still honored as fallback).
    pub attention: Option<String>,
}

impl SessionMeta {
    /// Whether ANY field is set — the `sessions` listing's `meta=<1|0>` bit.
    #[must_use]
    pub fn any_set(&self) -> bool {
        self.user_title.is_some()
            || self.description.is_some()
            || self.icon.is_some()
            || self.role.is_some()
            || self.attention.is_some()
    }

    /// The named field's current value (`None` for an unknown field name —
    /// callers validate names before writing; reading is total).
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&str> {
        match field {
            "title" => self.user_title.as_deref(),
            "description" => self.description.as_deref(),
            "icon" => self.icon.as_deref(),
            "role" => self.role.as_deref(),
            "attention" => self.attention.as_deref(),
            _ => None,
        }
    }

    /// The named field after the canonical single-line presentation policy.
    /// This is defensive even though the control setter validates input: a
    /// restore manifest or an older in-process caller may predate that gate.
    #[must_use]
    pub(crate) fn presentation_value(&self, field: &str) -> Option<String> {
        self.get(field)
            .and_then(|value| sanitize_metadata_value(field, value))
    }

    /// [`Self::presentation_value`] without the allocation: writes the canonical
    /// value into `out` and returns whether the field is set (and non-empty
    /// after the policy). `out` is left UNTOUCHED when the answer is `false`, so
    /// a caller may pass a resident slot holding a stale value it wants kept —
    /// which is exactly what the tab-strip refill's try-lock fallback needs.
    ///
    /// This exists for the render path: `presentation_value` allocates TWO
    /// `String`s (the char filter's collect plus the trim's `to_string`) per set
    /// field per tab, on a function that runs before the redraw early-out.
    pub(crate) fn presentation_value_into(&self, field: &str, out: &mut String) -> bool {
        let Some(cap) = Self::cap(field) else {
            return false;
        };
        let Some(value) = self.get(field) else {
            return false;
        };
        // Fixpoint of `sanitize_presentation_line`: nothing to filter (so the
        // `collect` would reproduce `value`), nothing to trim, and within the
        // cap (so the early `to_string` arm returns `value` verbatim). Every
        // production writer goes through `set`, which sanitizes, so this is the
        // steady-state path — one scan, zero allocations.
        if value.len() <= cap
            && value.trim().len() == value.len()
            && !metadata_has_forbidden_formatting(value)
        {
            if value.is_empty() {
                return false;
            }
            out.clear();
            out.push_str(value);
            return true;
        }
        // Non-canonical (an older in-process caller, a hand-edited manifest):
        // the exact sanitizer as before, byte-for-byte. This arm is the
        // bidi-override / invisible-character guard and must not be dropped.
        match sanitize_metadata_value(field, value) {
            Some(sanitized) => {
                out.clear();
                out.push_str(&sanitized);
                true
            }
            None => false,
        }
    }

    /// A canonical bounded copy suitable for restore/handoff persistence.
    #[must_use]
    pub(crate) fn sanitized(&self) -> Self {
        Self {
            user_title: self.presentation_value("title"),
            description: self.presentation_value("description"),
            icon: self.presentation_value("icon"),
            role: self.presentation_value("role"),
            attention: self.presentation_value("attention"),
        }
    }

    /// The byte cap for a named field, or `None` for an unknown field name.
    #[must_use]
    pub fn cap(field: &str) -> Option<usize> {
        match field {
            "title" => Some(META_TITLE_MAX),
            "description" => Some(META_DESCRIPTION_MAX),
            "icon" => Some(META_ICON_MAX),
            "role" => Some(META_ROLE_MAX),
            "attention" => Some(META_ATTENTION_MAX),
            _ => None,
        }
    }

    /// Set (or with `None`, unset) a named field. Returns `Some(changed)` for a
    /// known field (`changed` = the stored value actually moved, so callers only
    /// record/notify/repaint on a REAL change), `None` for an unknown name.
    pub fn set(&mut self, field: &str, value: Option<String>) -> Option<bool> {
        let slot = match field {
            "title" => &mut self.user_title,
            "description" => &mut self.description,
            "icon" => &mut self.icon,
            "role" => &mut self.role,
            "attention" => &mut self.attention,
            _ => return None,
        };
        // Callers exposed to the user reject unsafe/over-cap values so the
        // rejection is visible. This second gate protects older/internal
        // callers by storing only the same canonical representation chrome
        // consumes.
        let value = value.and_then(|value| sanitize_metadata_value(field, &value));
        let changed = *slot != value;
        *slot = value;
        Some(changed)
    }
}

/// The USER-metadata fields as a CLOSED type. Everything past the wire
/// PARSE boundary carries this instead of a `&str` name, which removes two
/// hazards by construction: the unknown-field case stops being reachable, and
/// the `meta-change` record no longer has to re-map a borrowed name onto a
/// `'static` token before printing it into a payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaField {
    /// `title` — the operator's display title, TOP rung of the label chain.
    Title,
    /// `description` — free-text purpose/notes an agent leaves behind.
    Description,
    /// `icon` — an emoji / short token for the strip.
    Icon,
    /// `role` — typed role token (`operator` is the recognized value).
    Role,
    /// `attention` — typed needs-human escalation message (non-empty ⇒ badge).
    Attention,
}

impl MetaField {
    /// Parse a wire field token, or `None` for an unknown name — the ONE door
    /// from the string vocabulary into the typed one.
    #[must_use]
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "title" => Self::Title,
            "description" => Self::Description,
            "icon" => Self::Icon,
            "role" => Self::Role,
            "attention" => Self::Attention,
            _ => return None,
        })
    }

    /// The stable token [`SessionMeta::set`]/[`SessionMeta::get`] key on and the
    /// `meta-change` payload prints. Safe to print verbatim: a closed
    /// vocabulary, never free text (only the VALUE is user-supplied).
    #[must_use]
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::Icon => "icon",
            Self::Role => "role",
            Self::Attention => "attention",
        }
    }

    /// This field's byte cap — applied to the TRIMMED value, measured in bytes.
    #[must_use]
    pub(crate) const fn cap(self) -> usize {
        match self {
            Self::Title => META_TITLE_MAX,
            Self::Description => META_DESCRIPTION_MAX,
            Self::Icon => META_ICON_MAX,
            Self::Role => META_ROLE_MAX,
            Self::Attention => META_ATTENTION_MAX,
        }
    }
}

/// What a metadata write INTENDS. `Clear` is first-class rather than a `Set("")`
/// spelling: both end with a stored `None`, but they record DIFFERENT timeline
/// payloads (`value=-` is the documented cleared marker; `value=` is not), so an
/// `events` consumer must be able to tell them apart. Typing the intent stops
/// `Set("")` from silently masquerading as a clear at the mutation boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaEdit<'a> {
    /// Store this value, after the full validation ladder.
    Set(&'a str),
    /// Unset the field — labels fall back down the chain.
    Clear,
}

/// Why a metadata write was REFUSED. Every variant is user-visible: the control
/// arm renders it as its existing `ERR` line, a GUI caller as an inline
/// rejection. Refusal is deliberate — a value is never silently truncated or
/// stripped, because the caller must know its label did not land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaWriteError {
    /// The value was empty (or whitespace-only). `Set` cannot clear.
    Empty,
    /// Control, bidi, or invisible formatting characters were present.
    ForbiddenFormatting,
    /// Over the field's byte cap (which is carried so the caller can name it).
    TooLong {
        /// The exceeded [`MetaField::cap`].
        cap: usize,
    },
}

/// PURE validation: the whole `meta set` ladder minus the store — trim, empty
/// rejection, forbidden-formatting rejection, byte cap, then canonicalization to
/// the STORED representation. No locks and no ctx, so any caller can run it
/// before it owns anything.
///
/// `Ok(None)` is produced ONLY by [`MetaEdit::Clear`]: a `Set` that survives the
/// ladder always has a canonical value, because rejection already removed every
/// input that could sanitize away to nothing.
pub(crate) fn validated_meta_value(
    field: MetaField,
    edit: MetaEdit<'_>,
) -> Result<Option<String>, MetaWriteError> {
    let MetaEdit::Set(value) = edit else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(MetaWriteError::Empty);
    }
    if metadata_has_forbidden_formatting(value) {
        return Err(MetaWriteError::ForbiddenFormatting);
    }
    let cap = field.cap();
    if value.len() > cap {
        return Err(MetaWriteError::TooLong { cap });
    }
    Ok(Some(
        sanitize_metadata_value(field.wire_name(), value)
            .expect("non-empty validated metadata has a canonical value"),
    ))
}

/// Apply one already-validated metadata mutation and — on an ACTUAL change —
/// record the `meta-change` timeline event (`field=<f> value=<pct|->`). Returns
/// whether the stored value moved, which is the caller's gate for the wake +
/// subscriber fan-out (see the `meta` dispatch arm and the GUI rename commit).
///
/// ATOMICITY: the timeline record happens WHILE the meta guard is still held —
/// the one sanctioned meta→timeline nesting (documented on `SessionCtx`). The
/// control socket runs concurrent worker threads, so two authorized `meta set`s
/// racing on one session must not interleave between the store and the record:
/// dropping the meta guard first lets the pair invert (store A,B — record B,A),
/// leaving every `subscribe … events` watcher and the `timeline` verb with a
/// LAST event that names the LOSING value while the stored meta, the bare
/// `meta` readout, and the tab label all show the winner. Holding the guard
/// across the record makes event-stream order match store order by
/// construction. Deadlock-free: no other site takes these two nested, and
/// timeline is a leaf everywhere (nothing locks meta under timeline).
///
/// GUARDS ARE RELEASED ON RETURN, and that is load-bearing rather than tidy: a
/// SAME-THREAD GUI caller refreshes the tab chrome immediately after this
/// returns, and that refresh re-takes `ctx.meta` once per tab inside
/// `App::tab_titles`. `std::sync::Mutex` is not reentrant, so a refresh run
/// from inside the mutation would self-deadlock the event loop.
pub(crate) fn apply_meta_value(
    ctx: &crate::SessionCtx,
    field: MetaField,
    value: Option<String>,
) -> bool {
    let payload_value = value
        .as_deref()
        .map_or_else(|| "-".to_string(), crate::control::pct_encode);
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    let changed = meta.set(field.wire_name(), value).unwrap_or(false);
    if changed {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .record(
                "meta-change",
                format!("field={} value={payload_value}", field.wire_name()),
            );
    }
    drop(meta);
    changed
}

/// Validate THEN apply — the entry point a NON-wire caller uses so it cannot
/// skip a rung. The wire handler splits the two only because it must render
/// each refusal as its own byte-exact `ERR` line.
pub(crate) fn write_session_meta(
    ctx: &crate::SessionCtx,
    field: MetaField,
    edit: MetaEdit<'_>,
) -> Result<bool, MetaWriteError> {
    let value = validated_meta_value(field, edit)?;
    Ok(apply_meta_value(ctx, field, value))
}

/// One recorded lifecycle event. `kind` is a closed vocabulary (`spawned`,
/// `state-change`, `title-change`, `cwd-change`, `meta-change`); `payload` is a
/// short space-separated `k=v` token string whose free-text values are ALREADY
/// pct-encoded at record time, so the `timeline` verb and the events digest can
/// print it verbatim as the line tail (one line per event, always).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEvent {
    /// Per-session monotonic id (1-based), the `since=<id>` resume key.
    pub id: u64,
    /// Milliseconds since the process epoch ([`now_ms`] — monotonic, ordering
    /// + alignment against the turn ledger's clock, not wall time).
    pub t_ms: u64,
    /// The event kind token.
    pub kind: &'static str,
    /// Pre-encoded `k=v` tail, clamped to [`MAX_PAYLOAD`] bytes.
    pub payload: String,
}

/// A session's bounded event timeline, newest-last, drop-oldest at
/// [`TIMELINE_CAP`], with per-session monotonic ids minted here.
#[derive(Default)]
pub struct SessionTimeline {
    events: VecDeque<TimelineEvent>,
    next_id: u64,
    /// The last cwd a `cwd-change` was recorded for — the dedup watermark that
    /// makes [`Self::record_cwd_change`] idempotent per actual change (the
    /// observer runs once per output wake, not once per `cd`).
    last_cwd: Option<String>,
}

impl SessionTimeline {
    /// Append one event, evicting the oldest past the cap. Returns its id.
    pub fn record(&mut self, kind: &'static str, payload: String) -> u64 {
        self.next_id += 1;
        if self.events.len() == TIMELINE_CAP {
            self.events.pop_front();
        }
        self.events.push_back(TimelineEvent {
            id: self.next_id,
            t_ms: now_ms(),
            kind,
            payload: clamp_payload(payload),
        });
        self.next_id
    }

    /// Record a `cwd-change` IFF `cwd` differs from the last one recorded (the
    /// GUI observes cwd drift on every output wake via the title-epoch path, so
    /// the dedup lives here, not at the observer). A `None`/empty cwd (OSC 7
    /// cleared) records `cwd=-`.
    pub fn record_cwd_change(&mut self, cwd: Option<&str>) {
        let cwd = cwd.filter(|c| !c.is_empty());
        if self.last_cwd.as_deref() == cwd {
            return;
        }
        self.last_cwd = cwd.map(str::to_string);
        let payload = match cwd {
            Some(c) => format!("cwd={}", crate::control::pct_encode(c)),
            None => "cwd=-".to_string(),
        };
        self.record("cwd-change", payload);
    }

    /// Events with `id > after`, oldest-first (ids only ever append increasing,
    /// so this is a suffix). `None` = all retained events.
    ///
    /// Double-ended so a caller that only wants the newest few can walk backwards
    /// instead of materializing the whole retained deque; the concrete iterator
    /// (a `VecDeque::range`) is, and so was the filtered `iter` before it.
    /// `DoubleEndedIterator: Iterator`, so every existing caller is unaffected.
    ///
    /// SEEK, DON'T FILTER — the turn-ledger twin. `next_id` is bumped per record
    /// and records are pushed to the BACK, so `id <= after` is monotone across
    /// the deque and `partition_point` lands on the first event past the
    /// watermark. The subscribe `events` digest calls this on every 250 ms
    /// liveness tick per watched target purely to learn that nothing changed;
    /// filtering made that O([`TIMELINE_CAP`]), seeking makes it
    /// O(log n + matched). A watermark below the retained low-water still yields
    /// everything (`partition_point` returns 0).
    pub fn since(&self, after: Option<u64>) -> impl DoubleEndedIterator<Item = &TimelineEvent> {
        let start = match after {
            None => 0,
            Some(a) => self.events.partition_point(|e| e.id <= a),
        };
        self.events.range(start..)
    }

    /// The highest recorded event id, or `None` when empty — the events digest
    /// seeds its watermark here so only post-subscription events push.
    pub fn high_id(&self) -> Option<u64> {
        self.events.back().map(|e| e.id)
    }

    /// Retained event count.
    #[allow(dead_code)] // used by tests; the verb frames via `since(None)`
    pub fn len(&self) -> usize {
        self.events.len()
    }
}

/// Clamp a payload to [`MAX_PAYLOAD`] bytes without bisecting a UTF-8 char OR a
/// `%XX` percent-escape triple.
///
/// The char-boundary walk alone is NOT enough: recorded values are pct-encoded
/// BEFORE recording (`meta-change`/`cwd-change` payloads), and a pct-encoded
/// string is pure ASCII — every byte is a char boundary, so a naive cut can
/// land mid-triple and leave a dangling `%` or `%X` tail. A legitimately
/// accepted value overruns the cap easily (the meta caps are on RAW bytes and
/// encoding expands up to 3x), and both the `timeline` verb and the
/// `EVENT <sid> meta …` subscribe push emit the stored payload verbatim — a
/// strict client pct-decoder must never receive a truncated escape. So after
/// the char-boundary walk, if a `%` in the last two bytes before the cut starts
/// a GENUINE escape (two hex digits follow in the full string), back the cut
/// off to that `%`. A literal `%` that happens to precede hex loses ≤2 extra
/// bytes of an already-lossy clamp — harmless; correctness of the escape stream
/// wins. `%` is ASCII, so the backed-off cut is still a char boundary.
fn clamp_payload(s: String) -> String {
    if s.len() <= MAX_PAYLOAD {
        return s;
    }
    let mut end = MAX_PAYLOAD;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = s.as_bytes();
    let escape_starts_at = |i: usize| {
        bytes[i] == b'%'
            && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
    };
    // A triple bisected by the cut has its `%` at end-1 (`…%|XX`) or end-2
    // (`…%X|X`); a `%` at end-3 or earlier fits whole and needs no back-off.
    for back in 1..=2 {
        if end >= back && escape_starts_at(end - back) {
            end -= back;
            break;
        }
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_sanitizer_is_single_line_bidi_safe_and_grapheme_bounded() {
        let hostile = "  alpha\nbe\u{0085}ta\u{202e}spoof\u{2066}\u{feff}  ";
        assert!(metadata_has_forbidden_formatting(hostile));
        assert_eq!(
            sanitize_metadata_value("title", hostile).as_deref(),
            Some("alphabetaspoof")
        );
        for invisible in [
            '\u{00ad}',
            '\u{034f}',
            '\u{180b}',
            '\u{115f}',
            '\u{3164}',
            '\u{ffa0}',
            '\u{1bca0}',
            '\u{1d173}',
            '\u{e0001}',
        ] {
            assert!(
                metadata_has_forbidden_formatting(&format!("a{invisible}b")),
                "spoof-relevant default-ignorable U+{:04X} is rejected",
                u32::from(invisible)
            );
        }

        // ZWJ is presentation data, not a bidi/control primitive: keep the
        // family cluster intact for icon metadata.
        let family = "👨‍👩‍👧‍👦";
        assert!(!metadata_has_forbidden_formatting(family));
        assert_eq!(
            sanitize_metadata_value("icon", family).as_deref(),
            Some(family)
        );
        for legitimate in ["a\u{200c}b", "✈\u{fe0f}", "漢\u{e0100}"] {
            assert!(
                !metadata_has_forbidden_formatting(legitimate),
                "joiners/standard variation selectors remain available: {legitimate:?}"
            );
            assert_eq!(
                sanitize_metadata_value("title", legitimate).as_deref(),
                Some(legitimate)
            );
        }

        // `e + COMBINING ACUTE` is three bytes but one grapheme. Forty fit the
        // 120-byte title cap exactly; the trailing grapheme is dropped whole.
        let cluster = "e\u{301}";
        let expected = cluster.repeat(META_TITLE_MAX / cluster.len());
        let over_cap = format!("{expected}z");
        let sanitized = sanitize_metadata_value("title", &over_cap).expect("non-empty");
        assert_eq!(sanitized, expected);
        assert_eq!(sanitized.len(), META_TITLE_MAX);
    }

    #[test]
    fn session_meta_defensively_canonicalizes_internal_callers() {
        let mut meta = SessionMeta::default();
        assert_eq!(
            meta.set("title", Some(" safe\n\u{202e}title ".to_string())),
            Some(true)
        );
        assert_eq!(meta.user_title.as_deref(), Some("safetitle"));

        // A raw legacy/restore assignment is still sanitized at the chrome /
        // persistence boundary even if it bypassed `set`.
        meta.description = Some("one\u{2029}two".to_string());
        meta.icon = Some("\u{2066}🚀\u{2069}".to_string());
        let canonical = meta.sanitized();
        assert_eq!(canonical.description.as_deref(), Some("onetwo"));
        assert_eq!(canonical.icon.as_deref(), Some("🚀"));
    }

    #[test]
    fn timeline_ids_are_monotonic_and_ring_drops_oldest() {
        let mut tl = SessionTimeline::default();
        for _ in 0..(TIMELINE_CAP + 3) {
            tl.record("state-change", "state=alive".to_string());
        }
        assert_eq!(tl.len(), TIMELINE_CAP, "capped");
        assert_eq!(tl.high_id(), Some(TIMELINE_CAP as u64 + 3));
        // The three oldest were evicted; ids stay strictly increasing and
        // `since` is a suffix keyed on them.
        let ids: Vec<u64> = tl.since(None).map(|e| e.id).collect();
        assert_eq!(ids[0], 4, "oldest three evicted");
        assert!(ids.windows(2).all(|w| w[1] == w[0] + 1), "monotonic ids");
        let tail: Vec<u64> = tl
            .since(Some(TIMELINE_CAP as u64 + 1))
            .map(|e| e.id)
            .collect();
        assert_eq!(tail, vec![TIMELINE_CAP as u64 + 2, TIMELINE_CAP as u64 + 3]);
        // Timestamps never move backward (monotonic clock).
        let ts: Vec<u64> = tl.since(None).map(|e| e.t_ms).collect();
        assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    }

    /// DIFFERENTIAL: the `partition_point` seek agrees with the linear filter it
    /// replaced at EVERY watermark, including the two the events digest stands
    /// on — below the retained low-water (yield everything) and at the high
    /// (yield nothing). The timeline's ids ARE contiguous, so this fixture also
    /// probes the boundaries either side of each retained id rather than only
    /// the gaps a sparse ledger would have.
    #[test]
    fn since_seek_matches_the_linear_filter_for_every_watermark() {
        let mut tl = SessionTimeline::default();
        for _ in 0..(TIMELINE_CAP + 5) {
            tl.record("state-change", "state=alive".to_string());
        }
        let low = tl.since(None).next().expect("non-empty").id;
        let high = tl.high_id().expect("non-empty");
        assert!(
            low > 1,
            "the fixture must have evicted, or the below-low arm is vacuous"
        );

        let reference = |after: Option<u64>| -> Vec<u64> {
            tl.events
                .iter()
                .filter(|e| after.is_none_or(|a| e.id > a))
                .map(|e| e.id)
                .collect()
        };
        let observed = |after: Option<u64>| -> Vec<u64> { tl.since(after).map(|e| e.id).collect() };

        let mut probes: Vec<Option<u64>> = vec![None, Some(0), Some(1)];
        for id in [low - 1, low, low + 1, high - 1, high, high + 1, high + 100] {
            probes.push(Some(id));
        }
        for after in probes {
            assert_eq!(
                observed(after),
                reference(after),
                "since({after:?}) diverged"
            );
        }
        assert_eq!(
            observed(Some(low - 1)).len(),
            TIMELINE_CAP,
            "below low-water = all retained"
        );
        assert!(
            observed(Some(high)).is_empty(),
            "at the high-water = nothing new"
        );
        // Still double-ended: the reverse walk the newest-few readers use.
        let newest: Vec<u64> = tl.since(None).rev().take(3).map(|e| e.id).collect();
        assert_eq!(newest, vec![high, high - 1, high - 2]);
    }

    #[test]
    fn cwd_change_records_only_actual_changes_and_clears_to_dash() {
        let mut tl = SessionTimeline::default();
        tl.record_cwd_change(Some("/a"));
        tl.record_cwd_change(Some("/a")); // reprompt: no new event
        tl.record_cwd_change(Some("/b b")); // space -> pct-encoded
        tl.record_cwd_change(None); // OSC 7 cleared
        tl.record_cwd_change(None); // still cleared: no new event
        let got: Vec<(&str, &str)> = tl
            .since(None)
            .map(|e| (e.kind, e.payload.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("cwd-change", "cwd=/a"),
                ("cwd-change", "cwd=/b%20b"),
                ("cwd-change", "cwd=-"),
            ]
        );
    }

    #[test]
    fn payload_clamps_on_a_char_boundary() {
        let mut tl = SessionTimeline::default();
        let long = "é".repeat(400); // 800 bytes
        tl.record("meta-change", long);
        let e = tl.since(None).next().unwrap();
        assert!(e.payload.len() <= MAX_PAYLOAD);
        assert!(e.payload.chars().all(|c| c == 'é'), "boundary-safe clamp");
    }

    /// The clamp must never bisect a `%XX` escape: recorded values are
    /// pct-encoded BEFORE recording, so the payload is pure ASCII and every
    /// byte is a char boundary — the char walk alone would happily cut `%E4`
    /// into `%E`. Slide the cut across all three in-triple offsets (via a
    /// 0/1/2-byte ASCII prefix) and require every stored `%` to still head a
    /// complete, decodable triple.
    #[test]
    fn payload_clamp_never_bisects_a_pct_escape() {
        for pad in 0..3usize {
            let encoded = crate::control::pct_encode(&"中".repeat(MAX_PAYLOAD)); // %E4%B8%AD…
            let payload = format!("{}{encoded}", "x".repeat(pad));
            let mut tl = SessionTimeline::default();
            tl.record("meta-change", payload);
            let stored = &tl.since(None).next().unwrap().payload;
            assert!(stored.len() <= MAX_PAYLOAD, "cap holds (pad {pad})");
            let b = stored.as_bytes();
            for (i, &c) in b.iter().enumerate() {
                if c == b'%' {
                    assert!(
                        i + 2 < b.len()
                            && b[i + 1].is_ascii_hexdigit()
                            && b[i + 2].is_ascii_hexdigit(),
                        "dangling escape at byte {i} of {stored:?} (pad {pad})"
                    );
                }
            }
        }
    }

    #[test]
    fn meta_set_reports_change_caps_and_unknown_fields() {
        let mut m = SessionMeta::default();
        assert!(!m.any_set());
        assert_eq!(m.set("title", Some("build agent".into())), Some(true));
        assert_eq!(m.get("title"), Some("build agent"));
        assert!(m.any_set());
        // Same value again: known field, NOT a change.
        assert_eq!(m.set("title", Some("build agent".into())), Some(false));
        // Unset flips back; a second unset is a known-field no-change.
        assert_eq!(m.set("title", None), Some(true));
        assert_eq!(m.set("title", None), Some(false));
        assert!(!m.any_set());
        // Unknown fields are rejected (None), not silently stored.
        assert_eq!(m.set("colour", Some("red".into())), None);
        assert_eq!(m.get("colour"), None);
        assert_eq!(SessionMeta::cap("title"), Some(META_TITLE_MAX));
        assert_eq!(SessionMeta::cap("description"), Some(META_DESCRIPTION_MAX));
        assert_eq!(SessionMeta::cap("icon"), Some(META_ICON_MAX));
        assert_eq!(SessionMeta::cap("role"), Some(META_ROLE_MAX));
        assert_eq!(SessionMeta::cap("attention"), Some(META_ATTENTION_MAX));
        assert_eq!(SessionMeta::cap("colour"), None);
        // The typed keys round-trip like the original three and flip any_set.
        assert_eq!(m.set("role", Some("operator".into())), Some(true));
        assert_eq!(m.get("role"), Some("operator"));
        assert!(m.any_set());
        assert_eq!(
            m.set("attention", Some("needs human: approval".into())),
            Some(true)
        );
        assert_eq!(m.get("attention"), Some("needs human: approval"));
        assert_eq!(m.set("role", None), Some(true));
        assert_eq!(m.set("attention", None), Some(true));
        assert!(!m.any_set());
    }
}

/// Proofs for the TYPED metadata write API — the one path both the control
/// socket (`meta set`/`meta unset`) and the GUI rename affordance take.
#[cfg(test)]
mod write_api_tests {
    use super::{MetaEdit, MetaField, MetaWriteError, apply_meta_value, validated_meta_value};

    /// The validation ladder refuses rather than repairs: an empty `Set` is a
    /// refusal (never a clear), forbidden formatting and an over-cap value are
    /// refusals (never a strip or a truncation), and a legal value comes back
    /// canonicalized. `Clear` is the ONLY way to reach `Ok(None)`.
    #[test]
    fn the_ladder_refuses_rather_than_repairs() {
        use MetaField::{Icon, Title};
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("   ")),
            Err(MetaWriteError::Empty),
            "an empty Set is a refusal, not a clear"
        );
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("build\u{202e}agent")),
            Err(MetaWriteError::ForbiddenFormatting)
        );
        let over = "x".repeat(super::META_TITLE_MAX + 1);
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set(&over)),
            Err(MetaWriteError::TooLong {
                cap: super::META_TITLE_MAX
            }),
            "over-cap is refused, never truncated"
        );
        assert_eq!(
            validated_meta_value(Title, MetaEdit::Set("  build agent  ")),
            Ok(Some("build agent".to_string())),
            "interior whitespace survives; the edges are trimmed"
        );
        assert_eq!(validated_meta_value(Title, MetaEdit::Clear), Ok(None));
        assert_eq!(validated_meta_value(Icon, MetaEdit::Clear), Ok(None));
        assert_eq!(Icon.cap(), super::META_ICON_MAX);
        assert_eq!(MetaField::parse("colour"), None);
    }

    /// A CLEAR records the documented cleared marker (`value=-`), which is what
    /// distinguishes it from the unrepresentable `Set("")` — both would store
    /// `None`, but only one of them says so in the event stream.
    #[test]
    fn a_clear_records_the_cleared_marker_and_only_on_a_real_change() {
        let ctx = crate::stub_session(0).ctx.clone();
        assert!(apply_meta_value(
            &ctx,
            MetaField::Title,
            Some("agent".into())
        ));
        assert!(
            !apply_meta_value(&ctx, MetaField::Title, Some("agent".into())),
            "a no-op re-set reports unchanged so the caller stays silent"
        );
        assert!(apply_meta_value(&ctx, MetaField::Title, None));
        assert!(
            !apply_meta_value(&ctx, MetaField::Title, None),
            "clearing an unset field is a no-op"
        );
        let tl = ctx.timeline.lock().unwrap();
        let payloads: Vec<&str> = tl
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.as_str())
            .collect();
        assert_eq!(
            payloads,
            vec!["field=title value=agent", "field=title value=-"],
            "exactly one record per REAL change, and a clear is `-`"
        );
    }
}

/// Concurrency proof for [`apply_meta_value`]: the meta store and the timeline
/// record are ATOMIC with respect to racing `meta set`s (the record is taken
/// while the meta guard is held), so the event stream can never invert against
/// the finally-stored value. The control socket runs a pool of worker threads;
/// before the fix, two writers could interleave store(A) store(B) record(B)
/// record(A) — every `subscribe … events` watcher and the `timeline` verb then
/// ended on an event naming A while the store, the bare `meta` readout, and
/// the tab label all showed B.
#[cfg(test)]
mod meta_atomicity_tests {
    use super::{MetaField, apply_meta_value};

    /// Hammer one session's title from several threads, then require the LAST
    /// recorded `meta-change` event to name exactly the value the store ended
    /// on. Deterministically true with the guard-held record; reliably flaky
    /// without it (the race window was the whole guard-drop → record gap).
    #[test]
    fn racing_meta_sets_keep_the_last_event_matching_final_state() {
        let ctx = crate::stub_session(0).ctx.clone();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    for j in 0..50 {
                        apply_meta_value(&ctx, MetaField::Title, Some(format!("t{i}-{j}")));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let final_title = ctx
            .meta
            .lock()
            .unwrap()
            .get("title")
            .expect("some writer won")
            .to_string();
        let tl = ctx.timeline.lock().unwrap();
        let last = tl
            .since(None)
            .rfind(|e| e.kind == "meta-change")
            .expect("changes were recorded");
        assert_eq!(
            last.payload,
            format!(
                "field=title value={}",
                crate::control::pct_encode(&final_title)
            ),
            "the final meta-change event names the finally-stored value"
        );
    }
}
