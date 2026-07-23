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
//!   icon …`): a display title that OUTRANKS the OSC 0/2 title in tab labels, a
//!   free-text description, and an icon token. Orthogonal to the engine's OSC
//!   title (which programs keep rewriting): this is what the OPERATOR calls the
//!   session, not what the running program does.
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

/// The user-settable per-session metadata (`meta set`/`meta unset`). All three
/// are `None` until a driver sets them; `user_title` (when set + non-empty)
/// outranks the live OSC title in tab labels and stays until unset.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct SessionMeta {
    /// Operator-chosen display title — the TOP rung of the tab-label chain.
    pub user_title: Option<String>,
    /// Free-text purpose/notes for the session (agents leave context here).
    pub description: Option<String>,
    /// Icon token (emoji or short name); reserved for the strip/UI stage.
    pub icon: Option<String>,
}

impl SessionMeta {
    /// Whether ANY field is set — the `sessions` listing's `meta=<1|0>` bit.
    #[must_use]
    pub fn any_set(&self) -> bool {
        self.user_title.is_some() || self.description.is_some() || self.icon.is_some()
    }

    /// The named field's current value (`None` for an unknown field name —
    /// callers validate names before writing; reading is total).
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&str> {
        match field {
            "title" => self.user_title.as_deref(),
            "description" => self.description.as_deref(),
            "icon" => self.icon.as_deref(),
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

    /// A canonical bounded copy suitable for restore/handoff persistence.
    #[must_use]
    pub(crate) fn sanitized(&self) -> Self {
        Self {
            user_title: self.presentation_value("title"),
            description: self.presentation_value("description"),
            icon: self.presentation_value("icon"),
        }
    }

    /// The byte cap for a named field, or `None` for an unknown field name.
    #[must_use]
    pub fn cap(field: &str) -> Option<usize> {
        match field {
            "title" => Some(META_TITLE_MAX),
            "description" => Some(META_DESCRIPTION_MAX),
            "icon" => Some(META_ICON_MAX),
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
    pub fn since(&self, after: Option<u64>) -> impl Iterator<Item = &TimelineEvent> {
        self.events
            .iter()
            .filter(move |e| after.is_none_or(|a| e.id > a))
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
        assert_eq!(SessionMeta::cap("colour"), None);
    }
}
