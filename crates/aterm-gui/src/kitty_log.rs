// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The KITTY LOG (sparkle-words v2.2, design §F4): a durable, machine-owned
//! collection-book ledger of every kitty sighting the cat renderer reports —
//! aggregated by `(shown type, magic bucket, primary language)`, never by raw
//! genome words, so the file is bounded by the vocabulary, not by usage.
//!
//! The shape is a clone of the updater's Health ledger
//! (`crates/aterm-update/src/health.rs`, the repo's one durable-counter idiom):
//! an all-`#[serde(default)]` TOML struct (absent/corrupt reads as EMPTY —
//! fail-open: the log is observability, never a gate), saturating lifetime
//! counters, RFC3339 first/last timestamps, a best-effort sibling `.lock`
//! around every read→merge→write, an atomic pid+seq-unique temp + rename
//! write, and a no-op skip when there is nothing to flush.
//!
//! Legacy sightings and a rollback-readable collectible mirror live at
//! `config_path().parent()/kitty-log.toml`; the authoritative collectible set
//! and its published mirror baseline live in sibling `kitty-collectibles.toml`.
//! A collectible-aware rollback can keep discovering into the mirror, while a
//! pre-collectibles binary may erase only that mirror, never the sidecar.
//! Neither file lives inside the user's hand-edited
//! `aterm.toml`, whose 500 ms watcher would turn machine count-bumps into config
//! reload + lexicon-recompile storms.
//!
//! Multi-process safety: every flush is a READ-MERGE-WRITE under the sibling
//! lock (summed counts, min `first_seen`, max `last_seen`, unioned language
//! chips) — a debounced dump of in-memory totals would last-writer-win a
//! second aterm's sightings away. The crash-loss window is exactly the deltas
//! since the last flush (≤ the 30 s debounce + whatever the exit flush
//! misses on a hard kill) — documented, accepted.
//! The semantic unlock set is lossless across supported rollback chains. A
//! mirror erased and recreated within the same timestamp second (or across a
//! backward wall-clock jump) is information-theoretically indistinguishable
//! from a stale replica, so its encounter count falls back to conservative
//! max-merge; identity/unlock durability is unaffected.
//!
//! Containment mode denies reads/writes under `~/.config/aterm`
//! (`config_watcher.rs`): every IO here is best-effort and never panics, so
//! the log silently degrades to in-memory-only there — the settings page
//! still shows this session's sightings, nothing persists.
//!
//! [`KittyLogHost`] is the App-side state: the in-memory totals the settings
//! page renders (settings-open does NO synchronous IO), the unflushed delta,
//! the `(session, ident)` dedupe ring (multi-window shared sessions count a
//! cat once; a vim round-trip's grace-expiry recount is absorbed for
//! [`RING_TTL`]), and the drain-time debounce that hands the delta to a
//! detached short-lived writer thread (the `config_watcher` thread precedent
//! — no new wakes or timers, never the render thread).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_effects::cat_glyphs_gen::{GLYPH_IDS, GLYPHS, GlyphKind};
use aterm_effects::kitty_registry::{
    KittyLook, KittyShownAs, KittySighting, TRAIT_BLAZE, TRAIT_BOW, TRAIT_CROWN, TRAIT_EAR_NICK,
    TRAIT_HETEROCHROMIA, TRAIT_SHY, TRAIT_SUNGLASSES, TRAIT_WITCH_HAT, age_from_key, age_key,
    glyph_from_key, glyph_key,
};
use aterm_lexicon::{Lexicon, primary_lang};
use serde::{Deserialize, Serialize};

/// The ledger's filename, a sibling of `aterm.toml` (see the module doc for
/// why it must never live INSIDE the user's config file).
const KITTY_LOG_FILE: &str = "kitty-log.toml";

/// Authoritative collectible identities and the embedded-replica baseline are
/// isolated from the legacy ledger so a destructive rollback cannot drop them.
const KITTY_COLLECTIBLES_FILE: &str = "kitty-collectibles.toml";

fn collectibles_path(legacy_path: &Path) -> PathBuf {
    legacy_path.with_file_name(KITTY_COLLECTIBLES_FILE)
}

/// Dedupe-ring capacity: how many recently-logged `(session, ident)` pairs are
/// remembered. 256 = 2× `MAX_OCCURRENCES` across a couple of shared-session
/// windows — a screenful of cats cannot evict itself.
const RING_SLOTS: usize = 256;

/// How long a ring entry keeps absorbing recounts of the SAME episode ident
/// (alt-screen vim round-trips, config reloads, grace expiry + re-appearance).
/// Refreshed on every hit, so a cat that stays on screen never double-counts.
const RING_TTL: Duration = Duration::from_secs(600);

/// Drain-time flush debounce: a dirty delta is handed to the writer thread at
/// most this often (plus once more on exit). Checked only at the existing tick
/// drains — no new wakes, no timers, zero idle cost.
const FLUSH_DEBOUNCE: Duration = Duration::from_secs(30);

/// One aggregated collection-book row, keyed `(kitty_type, magic, lang)` —
/// the registry `config_key()` spellings and the primary language CODE
/// (`"en"`, never a raw `LangId`: ids are lexicon-build-scoped).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KittyEntry {
    /// [`KittyType::config_key`] of the shown type.
    #[serde(default)]
    pub(crate) kitty_type: String,
    /// [`KittyMagic::config_key`] of the magic bucket.
    #[serde(default)]
    pub(crate) magic: String,
    /// The PRIMARY language code of the matched surface (first-appearance
    /// member of the match's language set; `"unknown"` for an empty set).
    #[serde(default)]
    pub(crate) lang: String,
    /// Lifetime sighting count for this cell (saturating).
    #[serde(default)]
    pub(crate) count: u64,
    /// RFC3339 UTC of the first sighting of this cell.
    #[serde(default)]
    pub(crate) first_seen: String,
    /// RFC3339 UTC of the most recent sighting of this cell.
    #[serde(default)]
    pub(crate) last_seen: String,
    /// EVERY language code claiming a sighted surface (the settings page's
    /// language chips) — the full `LangSet`, not just the primary.
    #[serde(default)]
    pub(crate) langs: Vec<String>,
}

/// One actually unlockable cat-art collectible. The key is a semantic authored
/// glyph id (`s1_03`, `spec_maneki`, `acc_bow`), so the album denominator is
/// exactly the generated roster rather than the retired v2 type×magic matrix.
/// For an accessory unlock, `look` persists the composed cat that wore it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KittyCollectible {
    #[serde(default)]
    pub(crate) key: String,
    #[serde(default)]
    pub(crate) variant: String,
    #[serde(default)]
    pub(crate) accessory: String,
    #[serde(default)]
    pub(crate) coat: u8,
    #[serde(default)]
    pub(crate) iris: u8,
    #[serde(default)]
    pub(crate) age: String,
    #[serde(default)]
    pub(crate) count: u64,
    #[serde(default)]
    pub(crate) first_seen: String,
    #[serde(default)]
    pub(crate) last_seen: String,
    #[serde(default)]
    pub(crate) langs: Vec<String>,
}

impl KittyCollectible {
    fn look(&self) -> Option<KittyLook> {
        let collectible = glyph_from_key(&self.key)?;
        let default = KittyLook::default();
        let mut look = KittyLook {
            variant: glyph_from_key(&self.variant).unwrap_or(default.variant),
            accessory: (!self.accessory.is_empty())
                .then(|| glyph_from_key(&self.accessory))
                .flatten(),
            coat: self.coat,
            iris: self.iris,
            age: age_from_key(&self.age),
        }
        .normalized();

        // `key` is the durable identity of this row. Recover an edited or old
        // composition from that authoritative semantic key rather than from a
        // malformed base field.
        match GLYPHS[collectible as usize].kind {
            GlyphKind::Head => look.variant = collectible,
            GlyphKind::Special => {
                look.variant = collectible;
                look.accessory = None;
            }
            GlyphKind::Accessory => {
                if GLYPHS[look.variant as usize].kind != GlyphKind::Head {
                    look.variant = default.variant;
                }
                look.accessory = Some(collectible);
            }
        }
        Some(look.normalized())
    }

    /// Rewrite the untrusted serialized composition to its canonical semantic
    /// keys. This is used only on ledger load/merge, never on the render path.
    fn canonicalize(&mut self) -> bool {
        let Some(look) = self.look() else {
            return false;
        };
        self.variant = glyph_key(look.variant).to_string();
        self.accessory = look.accessory.map(glyph_key).unwrap_or("").to_string();
        self.coat = look.coat;
        self.iris = look.iris;
        self.age = age_key(look.age).to_string();
        true
    }
}

/// The rollback-safe collectible sidecar. `legacy_mirror` is the roster last
/// published as the embedded-replica baseline. A collectible-aware rollback
/// may advance that replica; the next current build imports only its positive
/// per-key count delta. Because sidecar commits precede mirror writes, a hard
/// kill may leave the replica behind the baseline; lower counts are ignored,
/// never inflated. A pre-collectibles rollback may erase the replica, but can
/// never subtract from the authoritative `collectibles` rows here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct KittyCollectibleStore {
    #[serde(default)]
    collectibles: Vec<KittyCollectible>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_mirror: Option<Vec<KittyCollectible>>,
}

impl KittyCollectibleStore {
    fn mirrored(collectibles: &[KittyCollectible]) -> Self {
        Self {
            collectibles: collectibles.to_vec(),
            legacy_mirror: Some(collectibles.to_vec()),
        }
    }
}

/// The durable ledger. All fields default so an absent/corrupt file reads as
/// EMPTY (fail-open — the log is observability, never a gate).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KittyLog {
    /// Lifetime sighting total (saturating; one per deduped episode).
    #[serde(default)]
    pub(crate) sightings: u64,
    /// RFC3339 UTC of the very first recorded sighting.
    #[serde(default)]
    pub(crate) first_seen: String,
    /// RFC3339 UTC of the most recent recorded sighting.
    #[serde(default)]
    pub(crate) last_seen: String,
    /// HOW sightings rendered — the fallback CAUSE tallied as its own
    /// dimension (§F4.1: cat vs the three paw-fallback causes, never
    /// conflated with the shown type).
    #[serde(default)]
    pub(crate) shown_cat: u64,
    #[serde(default)]
    pub(crate) shown_paw_fallback_floor: u64,
    #[serde(default)]
    pub(crate) shown_paw_fallback_overflow: u64,
    #[serde(default)]
    pub(crate) shown_paw_style: u64,
    /// DISPLAYED-trait tallies (§F3.3: counted only when a head pose actually
    /// drew/applied the trait that episode).
    #[serde(default)]
    pub(crate) trait_heterochromia: u64,
    #[serde(default)]
    pub(crate) trait_ear_nick: u64,
    #[serde(default)]
    pub(crate) trait_blaze: u64,
    #[serde(default)]
    pub(crate) trait_shy: u64,
    /// v3 §2.1 accessory tallies (worn only by plain — non-magic — cats;
    /// counted when the accessory was actually drawn that episode). All
    /// serde-default like every field, so a v2 ledger reads forward and a v3
    /// ledger read by v2 keeps its unknown keys fail-open.
    #[serde(default)]
    pub(crate) accessory_bow: u64,
    #[serde(default)]
    pub(crate) accessory_sunglasses: u64,
    #[serde(default)]
    pub(crate) accessory_witch_hat: u64,
    #[serde(default)]
    pub(crate) accessory_crown: u64,
    /// The aggregated collection cells, in first-discovery order.
    #[serde(default)]
    pub(crate) entries: Vec<KittyEntry>,
    /// Cat-art v4's bounded, achievable collection roster. This embedded copy
    /// remains a deliberate rollback-compatible mirror: the first collectible
    /// build can discover into it, while the sidecar remains authoritative
    /// against destructive rewrites by older, pre-collectibles builds.
    #[serde(default)]
    pub(crate) collectibles: Vec<KittyCollectible>,
}

/// Min of two RFC3339 UTC stamps, ignoring empties (the fixed-width `…Z`
/// format makes lexicographic order chronological order).
fn min_ts(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => if a <= b { a } else { b }.to_string(),
    }
}

/// Max of two RFC3339 UTC stamps, ignoring empties.
fn max_ts(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => if a >= b { a } else { b }.to_string(),
    }
}

/// Chronological order for first-seen stamps. Missing legacy stamps sort before
/// known instants so they can never masquerade as the latest discovery; the
/// semantic key breaks timestamp ties deterministically across processes whose
/// second-resolution wall clocks coincide.
fn collectible_order(a: &KittyCollectible, b: &KittyCollectible) -> std::cmp::Ordering {
    match (a.first_seen.is_empty(), b.first_seen.is_empty()) {
        (false, true) => std::cmp::Ordering::Greater,
        (true, false) => std::cmp::Ordering::Less,
        _ => a
            .first_seen
            .cmp(&b.first_seen)
            .then_with(|| a.key.cmp(&b.key)),
    }
}

/// Whether `candidate` owns the earlier first-discovery composition. Equal or
/// missing timestamps use the serialized look tuple as a deterministic tie
/// break, making duplicate normalization independent of flush order.
fn collectible_look_precedes(candidate: &KittyCollectible, current: &KittyCollectible) -> bool {
    let by_time = match (
        candidate.first_seen.is_empty(),
        current.first_seen.is_empty(),
    ) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => candidate.first_seen.cmp(&current.first_seen),
    };
    if by_time != std::cmp::Ordering::Equal {
        return by_time == std::cmp::Ordering::Less;
    }
    (
        candidate.variant.as_str(),
        candidate.accessory.as_str(),
        candidate.coat,
        candidate.iris,
        candidate.age.as_str(),
    ) < (
        current.variant.as_str(),
        current.accessory.as_str(),
        current.coat,
        current.iris,
        current.age.as_str(),
    )
}

impl KittyLog {
    /// Read the ledger; absent/corrupt ⇒ empty default (fail-open, exactly
    /// like `Health::read` — under Containment the read is denied and this
    /// yields the same empty default).
    pub(crate) fn read(path: &Path) -> Self {
        let mut log: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        log.normalize_collectibles();
        log
    }

    /// Read and reconcile the embedded replica with the authoritative sidecar.
    /// The sidecar stores the last mirror baseline, so a collectible-aware
    /// rollback contributes only positive per-key deltas. With a pre-baseline
    /// sidecar, replica counts merge by `max` once, which preserves new keys
    /// without counting identical copies twice. Sidecar is written first, then
    /// the reconciled mirror, so every crash prefix preserves unlocks and the
    /// ledger's documented hard-kill window never becomes count inflation.
    fn read_with_sidecar(path: &Path) -> Self {
        let sidecar_path = collectibles_path(path);
        // Every new writer takes locks in this order. A rollback binary takes
        // only the first lock, so it cannot erase transitional rows while this
        // process is moving them to the sidecar.
        let _legacy_lock = lock(path);
        let _sidecar_lock = lock(&sidecar_path);
        let mut log = Self::read(path);
        let embedded = std::mem::take(&mut log.collectibles);
        let persisted = Self::read_collectible_store(&sidecar_path);
        let reconciled = Self::reconcile_collectible_replicas(
            persisted
                .as_ref()
                .map(|store| store.collectibles.as_slice()),
            persisted
                .as_ref()
                .and_then(|store| store.legacy_mirror.as_deref()),
            &embedded,
        );
        let desired = KittyCollectibleStore::mirrored(&reconciled);
        let sidecar_current = persisted.as_ref() == Some(&desired);
        let sidecar_safe = sidecar_current
            || (reconciled.is_empty() && persisted.is_none())
            || Self::write_collectible_store_state(&sidecar_path, &desired);
        log.collectibles = reconciled;
        if sidecar_safe && embedded != log.collectibles {
            log.write(path);
        }
        log
    }

    fn read_collectible_store(path: &Path) -> Option<KittyCollectibleStore> {
        let mut store: KittyCollectibleStore = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())?;
        store.collectibles = Self::normalized_collectibles(store.collectibles);
        store.legacy_mirror = store.legacy_mirror.map(Self::normalized_collectibles);
        Some(store)
    }

    #[cfg(test)]
    fn write_collectible_store(path: &Path, collectibles: &[KittyCollectible]) -> bool {
        Self::write_collectible_store_state(path, &KittyCollectibleStore::mirrored(collectibles))
    }

    fn write_collectible_store_state(path: &Path, store: &KittyCollectibleStore) -> bool {
        atomic_write_toml(path, store)
    }

    fn normalized_collectibles(collectibles: Vec<KittyCollectible>) -> Vec<KittyCollectible> {
        let mut normalized = Self {
            collectibles,
            ..Self::default()
        };
        normalized.normalize_collectibles();
        normalized.collectibles
    }

    /// Reconcile two replicas without treating the embedded mirror as a second
    /// event stream. With a baseline, only counts above the last mirror are new
    /// events. Without one (old sidecar schema), `max` is the only conservative
    /// policy: it imports rollback-only keys/count advances without inflation.
    fn reconcile_collectible_replicas(
        authoritative: Option<&[KittyCollectible]>,
        baseline: Option<&[KittyCollectible]>,
        embedded: &[KittyCollectible],
    ) -> Vec<KittyCollectible> {
        let mut merged = Self {
            collectibles: authoritative.unwrap_or_default().to_vec(),
            ..Self::default()
        };
        merged.normalize_collectibles();
        if authoritative.is_none() {
            merged.collectibles.clear();
            for candidate in embedded {
                merged.merge_collectible(candidate.clone());
            }
        } else if let Some(baseline) = baseline {
            for candidate in embedded {
                let mut delta = candidate.clone();
                let baseline_row = baseline.iter().find(|item| item.key == candidate.key);
                delta.count = Self::collectible_replica_delta(candidate, baseline_row);
                merged.merge_collectible(delta);
            }
        } else {
            for candidate in embedded {
                merged.merge_collectible_replica(candidate.clone());
            }
        }
        merged.collectibles.sort_by(collectible_order);
        merged.collectibles
    }

    /// Count the events represented by a rollback-written replica row. A
    /// normal collectible-aware rollback preserves the baseline `first_seen`,
    /// so only its counter advance is new. If a pre-collectibles rewrite erased
    /// the mirror first, the old collectible build recreates the row after the
    /// baseline's `last_seen`; all of that recreated row is post-baseline
    /// activity. The strict temporal order avoids re-adding a stale mirror when
    /// a delayed flush merely moved the authoritative `first_seen` earlier.
    fn collectible_replica_delta(
        candidate: &KittyCollectible,
        baseline: Option<&KittyCollectible>,
    ) -> u64 {
        let Some(baseline) = baseline else {
            return candidate.count;
        };
        if !candidate.first_seen.is_empty()
            && !baseline.last_seen.is_empty()
            && candidate.first_seen > baseline.last_seen
        {
            candidate.count
        } else {
            candidate.count.saturating_sub(baseline.count)
        }
    }

    /// Restore the generated-key set invariant after deserializing an
    /// untrusted, hand-editable ledger. This is cold-path load work: invalid
    /// rows are dropped, duplicate semantic keys merge, and the vector can
    /// never exceed the generated roster even if the TOML was oversized.
    fn normalize_collectibles(&mut self) {
        let persisted = std::mem::take(&mut self.collectibles);
        self.collectibles
            .reserve(persisted.len().min(GLYPH_IDS.len()));
        for candidate in persisted {
            self.merge_collectible(candidate);
        }
        self.collectibles.sort_by(collectible_order);
    }

    /// Merge one untrusted/persisted row into the bounded semantic set.
    fn merge_collectible(&mut self, mut candidate: KittyCollectible) {
        self.merge_collectible_with(&mut candidate, true);
    }

    /// Merge a second physical copy of the same logical ledger. Counts take
    /// `max` rather than sum because neither replica is an independent stream.
    fn merge_collectible_replica(&mut self, mut candidate: KittyCollectible) {
        self.merge_collectible_with(&mut candidate, false);
    }

    fn merge_collectible_with(&mut self, candidate: &mut KittyCollectible, additive: bool) {
        if !candidate.canonicalize() {
            return;
        }
        if let Some(item) = self
            .collectibles
            .iter_mut()
            .find(|item| item.key == candidate.key)
        {
            let candidate_owns_look = collectible_look_precedes(candidate, item);
            item.count = if additive {
                item.count.saturating_add(candidate.count)
            } else {
                item.count.max(candidate.count)
            };
            item.first_seen = min_ts(&item.first_seen, &candidate.first_seen);
            item.last_seen = max_ts(&item.last_seen, &candidate.last_seen);
            for code in std::mem::take(&mut candidate.langs) {
                if !item.langs.iter().any(|present| present == &code) {
                    item.langs.push(code);
                }
            }
            if candidate_owns_look {
                item.variant = std::mem::take(&mut candidate.variant);
                item.accessory = std::mem::take(&mut candidate.accessory);
                item.coat = candidate.coat;
                item.iris = candidate.iris;
                item.age = std::mem::take(&mut candidate.age);
            }
        } else if self.collectibles.len() < GLYPH_IDS.len() {
            self.collectibles.push(candidate.clone());
        }
    }

    /// Whether there is nothing recorded (the flush no-op-skip predicate).
    pub(crate) fn is_empty(&self) -> bool {
        self.sightings == 0 && self.entries.is_empty() && self.collectibles.is_empty()
    }

    /// Count one generated glyph unlock/encounter. The semantic roster key is
    /// validated before insert, making the persisted vector structurally
    /// bounded by `GLYPH_IDS.len()` even when merging an edited/corrupt file.
    fn record_collectible(
        &mut self,
        key: &str,
        look: KittyLook,
        s: &KittySighting,
        lexicon: &Lexicon,
        now: &str,
    ) -> bool {
        if glyph_from_key(key).is_none() {
            return false;
        }
        let mut discovered = false;
        let item = match self.collectibles.iter().position(|item| item.key == key) {
            Some(i) => &mut self.collectibles[i],
            None if self.collectibles.len() < GLYPH_IDS.len() => {
                discovered = true;
                let mut candidate = KittyCollectible {
                    key: key.to_string(),
                    variant: glyph_key(look.variant).to_string(),
                    accessory: look.accessory.map(glyph_key).unwrap_or("").to_string(),
                    coat: look.coat,
                    iris: look.iris,
                    age: age_key(look.age).to_string(),
                    ..KittyCollectible::default()
                };
                if !candidate.canonicalize() {
                    return false;
                }
                self.collectibles.push(candidate);
                self.collectibles.last_mut().expect("just pushed")
            }
            None => return false,
        };
        item.count = item.count.saturating_add(1);
        item.first_seen = min_ts(&item.first_seen, now);
        item.last_seen = max_ts(&item.last_seen, now);
        for id in s.langs.iter() {
            let code = lexicon.lang_code(id);
            if !item.langs.iter().any(|c| c == code) {
                item.langs.push(code.to_string());
            }
        }
        discovered
    }

    /// Record ONE deduped sighting at RFC3339 stamp `now`. Language codes are
    /// resolved HERE, with the same lexicon build that produced the match —
    /// `LangId`s are build-scoped and must never be persisted (§F4).
    pub(crate) fn record(&mut self, s: &KittySighting, lexicon: &Lexicon, now: &str) -> bool {
        self.sightings = self.sightings.saturating_add(1);
        self.first_seen = min_ts(&self.first_seen, now);
        self.last_seen = max_ts(&self.last_seen, now);
        match s.shown_as {
            KittyShownAs::Cat => self.shown_cat = self.shown_cat.saturating_add(1),
            KittyShownAs::PawFallbackFloor => {
                self.shown_paw_fallback_floor = self.shown_paw_fallback_floor.saturating_add(1);
            }
            KittyShownAs::PawFallbackOverflow => {
                self.shown_paw_fallback_overflow =
                    self.shown_paw_fallback_overflow.saturating_add(1);
            }
            KittyShownAs::PawStyle => {
                self.shown_paw_style = self.shown_paw_style.saturating_add(1);
            }
        }
        if s.traits & TRAIT_HETEROCHROMIA != 0 {
            self.trait_heterochromia = self.trait_heterochromia.saturating_add(1);
        }
        if s.traits & TRAIT_EAR_NICK != 0 {
            self.trait_ear_nick = self.trait_ear_nick.saturating_add(1);
        }
        if s.traits & TRAIT_BLAZE != 0 {
            self.trait_blaze = self.trait_blaze.saturating_add(1);
        }
        if s.traits & TRAIT_SHY != 0 {
            self.trait_shy = self.trait_shy.saturating_add(1);
        }
        if s.traits & TRAIT_BOW != 0 {
            self.accessory_bow = self.accessory_bow.saturating_add(1);
        }
        if s.traits & TRAIT_SUNGLASSES != 0 {
            self.accessory_sunglasses = self.accessory_sunglasses.saturating_add(1);
        }
        if s.traits & TRAIT_WITCH_HAT != 0 {
            self.accessory_witch_hat = self.accessory_witch_hat.saturating_add(1);
        }
        if s.traits & TRAIT_CROWN != 0 {
            self.accessory_crown = self.accessory_crown.saturating_add(1);
        }
        let kitty_type = s.kitty_type.config_key();
        let magic = s.magic.config_key();
        let lang = lexicon.lang_code(primary_lang(s.langs));
        let e = match self
            .entries
            .iter_mut()
            .find(|e| e.kitty_type == kitty_type && e.magic == magic && e.lang == lang)
        {
            Some(e) => e,
            None => {
                self.entries.push(KittyEntry {
                    kitty_type: kitty_type.to_string(),
                    magic: magic.to_string(),
                    lang: lang.to_string(),
                    ..KittyEntry::default()
                });
                self.entries.last_mut().expect("just pushed")
            }
        };
        e.count = e.count.saturating_add(1);
        e.first_seen = min_ts(&e.first_seen, now);
        e.last_seen = max_ts(&e.last_seen, now);
        for id in s.langs.iter() {
            let code = lexicon.lang_code(id);
            if !e.langs.iter().any(|c| c == code) {
                e.langs.push(code.to_string());
            }
        }
        let look = s.look.normalized();
        let base_new = self.record_collectible(glyph_key(look.variant), look, s, lexicon, now);
        let accessory_new = look.accessory.is_some_and(|accessory| {
            self.record_collectible(glyph_key(accessory), look, s, lexicon, now)
        });
        base_new || accessory_new
    }

    /// Fold `other` into `self`: summed saturating counts, min `first_seen` /
    /// max `last_seen`, per-cell merge by `(type, magic, lang)`, unioned
    /// language chips. PURE — the multi-process read-merge-write core.
    pub(crate) fn merge_from(&mut self, other: &KittyLog) {
        self.sightings = self.sightings.saturating_add(other.sightings);
        self.first_seen = min_ts(&self.first_seen, &other.first_seen);
        self.last_seen = max_ts(&self.last_seen, &other.last_seen);
        self.shown_cat = self.shown_cat.saturating_add(other.shown_cat);
        self.shown_paw_fallback_floor = self
            .shown_paw_fallback_floor
            .saturating_add(other.shown_paw_fallback_floor);
        self.shown_paw_fallback_overflow = self
            .shown_paw_fallback_overflow
            .saturating_add(other.shown_paw_fallback_overflow);
        self.shown_paw_style = self.shown_paw_style.saturating_add(other.shown_paw_style);
        self.trait_heterochromia = self
            .trait_heterochromia
            .saturating_add(other.trait_heterochromia);
        self.trait_ear_nick = self.trait_ear_nick.saturating_add(other.trait_ear_nick);
        self.trait_blaze = self.trait_blaze.saturating_add(other.trait_blaze);
        self.trait_shy = self.trait_shy.saturating_add(other.trait_shy);
        self.accessory_bow = self.accessory_bow.saturating_add(other.accessory_bow);
        self.accessory_sunglasses = self
            .accessory_sunglasses
            .saturating_add(other.accessory_sunglasses);
        self.accessory_witch_hat = self
            .accessory_witch_hat
            .saturating_add(other.accessory_witch_hat);
        self.accessory_crown = self.accessory_crown.saturating_add(other.accessory_crown);
        for oe in &other.entries {
            match self
                .entries
                .iter_mut()
                .find(|e| e.kitty_type == oe.kitty_type && e.magic == oe.magic && e.lang == oe.lang)
            {
                Some(e) => {
                    e.count = e.count.saturating_add(oe.count);
                    e.first_seen = min_ts(&e.first_seen, &oe.first_seen);
                    e.last_seen = max_ts(&e.last_seen, &oe.last_seen);
                    for code in &oe.langs {
                        if !e.langs.iter().any(|c| c == code) {
                            e.langs.push(code.clone());
                        }
                    }
                }
                None => self.entries.push(oe.clone()),
            }
        }
        for candidate in &other.collectibles {
            self.merge_collectible(candidate.clone());
        }
        self.collectibles.sort_by(collectible_order);
    }

    /// Flush `delta` into the ledger at `path`: READ-MERGE-WRITE under the
    /// sibling `.lock` (see the module doc — a dump of in-memory totals would
    /// last-writer-win a second process's sightings). No-op for an empty
    /// delta. Best-effort everywhere: a denied lock proceeds unlocked, a
    /// denied write is dropped (Containment ⇒ silent in-memory).
    pub(crate) fn flush_merge(path: &Path, delta: &KittyLog) {
        if delta.is_empty() {
            return; // no-op skip: never rewrite (or create) the file for nothing
        }
        let sidecar_path = collectibles_path(path);
        let _legacy_lock = lock(path);
        let _sidecar_lock = lock(&sidecar_path);
        let mut merged = Self::read(path);
        let embedded = std::mem::take(&mut merged.collectibles);
        let persisted = Self::read_collectible_store(&sidecar_path);
        let sidecar_was_valid = persisted.is_some();
        let mut collection = Self {
            collectibles: Self::reconcile_collectible_replicas(
                persisted
                    .as_ref()
                    .map(|store| store.collectibles.as_slice()),
                persisted
                    .as_ref()
                    .and_then(|store| store.legacy_mirror.as_deref()),
                &embedded,
            ),
            ..Self::default()
        };
        for candidate in &delta.collectibles {
            collection.merge_collectible(candidate.clone());
        }
        collection.collectibles.sort_by(collectible_order);
        let desired = KittyCollectibleStore::mirrored(&collection.collectibles);

        // Persist/reconcile the authoritative sidecar BEFORE rewriting its
        // embedded mirror. If the first sidecar write fails, leave the only
        // durable legacy copy untouched. With an existing sidecar, a failed
        // update may still be mirrored safely: the next read imports its
        // positive delta against the previous baseline.
        let sidecar_dirty = (!collection.collectibles.is_empty() || persisted.is_some())
            && persisted.as_ref() != Some(&desired);
        let sidecar_safe = if sidecar_dirty {
            Self::write_collectible_store_state(&sidecar_path, &desired) || sidecar_was_valid
        } else {
            sidecar_was_valid || collection.collectibles.is_empty()
        };
        if !sidecar_safe {
            return;
        }
        merged.merge_from(delta);
        merged.collectibles = collection.collectibles;
        merged.write(path);
    }

    /// Best-effort atomic write: create-parent, pid+seq-unique sibling temp,
    /// rename (mirrors `Health::write` + `save_prefs_edits`). Never panics.
    fn write(&self, path: &Path) {
        let _ = atomic_write_toml(path, self);
    }
}

fn atomic_write_toml(path: &Path, value: &impl Serialize) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};

    let Ok(text) = toml::to_string(value) else {
        return false;
    };
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false; // unreachable config dir (Containment) — stay in-memory
    }
    // Unique per INVOCATION (pid + a process-wide counter): the exit flush and
    // writer thread must never stage through the same temp path.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, text).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if std::fs::rename(&tmp, path).is_ok() {
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

/// Best-effort sibling lock (`kitty-log.toml.lock`) guarding a whole
/// read→merge→write. `None` on failure — the caller proceeds unlocked rather
/// than drop the flush (the ledger is observability, never a gate). Held for
/// the guard's lifetime; `flock` is released by the kernel on drop/exit.
fn lock(path: &Path) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // A lock file is a rendezvous, not data — never clobber its contents.
        .truncate(false)
        .open(path.with_extension("toml.lock"))
        .ok()?;
    f.lock().ok()?;
    Some(f)
}

/// Best-effort RFC3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) — the ledger's
/// stamp format, computed in-process (the `aterm-update::install` idiom; no
/// `/bin/date` fork). Empty string on a pre-epoch clock.
fn now_rfc3339() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format_rfc3339(d.as_secs()),
        Err(_) => String::new(),
    }
}

/// Format seconds-since-Unix-epoch as an RFC3339 UTC instant, via Howard
/// Hinnant's branch-free civil-from-days (the exact `aterm-update::install`
/// helper — that fn is crate-private there). Pure and total for all inputs.
fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day-of-era      [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year (Mar 1 = 0)
    let mp = (5 * doy + 2) / 153; // month, shifted so Mar = 0  [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day-of-month  [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month  [1, 12]
    let y = yoe + era * 400 + i64::from(m <= 2); // Jan/Feb belong to the next year
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// The settings overlay's SNAPSHOT of the log (§F4.6): taken on open /
/// category switch / drain-while-open, so the card painter stays a pure
/// function of `SettingsState` (no live App reads from the painter).
/// `revision` is the change stamp `SettingsState::fingerprint` folds while
/// the Kitty Log category is active.
#[derive(Clone)]
pub(crate) struct KittyLogView {
    /// The host's revision counter at snapshot time (bumps once per recorded
    /// sighting — a cheap staleness check and the repaint fingerprint term).
    pub(crate) revision: u64,
    /// The in-memory totals (startup file read + this session's sightings).
    pub(crate) log: KittyLog,
}

impl Default for KittyLogView {
    /// The NEVER-SYNCED sentinel: `revision = u64::MAX` can never equal the
    /// host's counter (which starts at 0 and bumps once per sighting), so a
    /// freshly opened overlay always takes its first snapshot — even when the
    /// host holds only the startup file read at revision 0.
    fn default() -> Self {
        Self {
            revision: u64::MAX,
            log: KittyLog::default(),
        }
    }
}

/// A ring slot: one recently-logged `(session, ident)` episode.
#[derive(Clone, Copy)]
struct RingSlot {
    session: u64,
    ident: u64,
    at: Instant,
}

/// The App-side Kitty Log state: in-memory totals, the unflushed delta, the
/// dedupe ring, and the flush debounce. See the module doc.
pub(crate) struct KittyLogHost {
    /// The ledger path (`…/kitty-log.toml`), or `None` for in-memory-only
    /// (no config dir — tests use this too, so they never touch the user's
    /// real ledger).
    path: Option<PathBuf>,
    /// Display totals: the startup file read plus every sighting recorded
    /// this session (flushed or not) — what the settings page renders,
    /// memory-only, no IO on the interaction path.
    mem: KittyLog,
    /// Sightings recorded since the last flush (the read-merge-write delta).
    delta: KittyLog,
    /// Bumps once per recorded sighting; snapshot staleness + repaint stamp.
    revision: u64,
    /// Most recently discovered visual identity, resolved once on load/update.
    /// Cursor frames read this `Copy` value in O(1), never scan the ledger.
    companion: Option<KittyLook>,
    /// Recently-logged `(session, ident)` episodes (≤ [`RING_SLOTS`], TTL
    /// [`RING_TTL`], stamp refreshed on hit): shared-session multi-window
    /// drains and vim-round-trip recounts collapse to one count.
    ring: Vec<RingSlot>,
    /// Next ring slot to overwrite once the ring is full (FIFO).
    ring_next: usize,
    /// When the delta was last handed to the writer.
    last_flush: Option<Instant>,
    /// The single long-lived flush writer. A persistent host pre-arms it during
    /// [`Self::load`], before the event loop can enter a render/present; an
    /// in-memory-only host starts no thread. Deltas are handed over a bounded
    /// channel with a NON-blocking `try_send`, so the render thread never
    /// creates/joins a thread or blocks on disk IO (TYPING-5); [`flush_exit`]
    /// sends the final delta and joins. `None` only for an in-memory host or a
    /// failed pre-arm (which falls back to the exit-path inline flush).
    ///
    /// [`flush_exit`]: Self::flush_exit
    writer: Option<KittyWriter>,
}

/// The single background flush thread + its bounded delivery channel. One
/// long-lived worker parked on `recv` (0 CPU while idle) does every
/// read-merge-write, so the UI/render thread only ever `try_send`s a delta —
/// it never spawns a thread nor joins a previous one on the hot path (the old
/// per-flush spawn+inline-join could stall a render frame on a slow/networked
/// config dir; TYPING-5).
struct KittyWriter {
    /// Bounded so a pathologically slow disk can't grow an unbounded backlog;
    /// the sender `try_send`s and coalesces on full (see `maybe_flush`).
    tx: std::sync::mpsc::SyncSender<KittyLog>,
    /// The worker; joined only by `flush_exit` (after the sender is dropped, so
    /// `recv` returns `Err` and the loop exits cleanly).
    handle: std::thread::JoinHandle<()>,
}

impl KittyWriter {
    /// Spawn the worker bound to `path`. `None` if the thread can't be spawned
    /// (best-effort, matching the old `.ok()` on spawn — the delta is dropped
    /// inside the documented crash-loss window).
    fn spawn(path: PathBuf) -> Option<Self> {
        // Depth 1: the worker can hold one in-flight batch while another queues;
        // a third arriving before the first drains coalesces back into `delta`.
        let (tx, rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let handle = std::thread::Builder::new()
            .name("kitty-log-flush".into())
            .spawn(move || {
                // Each delta is a quick read-merge-write; the loop ends when the
                // last sender drops (recv → Err), i.e. at `flush_exit`.
                while let Ok(delta) = rx.recv() {
                    KittyLog::flush_merge(&path, &delta);
                }
            })
            .ok()?;
        Some(Self { tx, handle })
    }
}

impl KittyLogHost {
    /// Host state persisting beside the given CONFIG path (the ledger is the
    /// `kitty-log.toml` sibling). `None` — or a parentless path — degrades to
    /// in-memory-only. The one startup read is fail-open (absent / corrupt /
    /// Containment-denied ⇒ empty).
    pub(crate) fn load(config_path: Option<PathBuf>) -> Self {
        Self::load_with_writer_spawn(config_path, KittyWriter::spawn)
    }

    /// Construction seam for the pre-armed writer. Keeping the spawner scoped
    /// to `load` makes it impossible for [`Self::observe`] / [`Self::maybe_flush`]
    /// to create a thread on the render path; tests inject a failed pre-arm and
    /// pin that no hot-path retry can creep back in.
    fn load_with_writer_spawn(
        config_path: Option<PathBuf>,
        spawn: impl FnOnce(PathBuf) -> Option<KittyWriter>,
    ) -> Self {
        let path = config_path
            .as_deref()
            .and_then(Path::parent)
            .map(|d| d.join(KITTY_LOG_FILE));
        let mem = path
            .as_deref()
            .map(KittyLog::read_with_sidecar)
            .unwrap_or_default();
        let companion = mem
            .collectibles
            .iter()
            .rev()
            .find_map(KittyCollectible::look);
        // Pre-arm outside `observe`/present. A parked receiver consumes no CPU;
        // the first sighting now performs only the bounded `try_send` below.
        let writer = path.clone().and_then(spawn);
        Self {
            path,
            mem,
            delta: KittyLog::default(),
            revision: 0,
            companion,
            ring: Vec::new(),
            ring_next: 0,
            last_flush: None,
            writer,
        }
    }

    /// An in-memory-only host (no ledger file) — the headless-test App, which
    /// must never read or write the developer's real ledger.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self::load(None)
    }

    /// The in-memory totals (the `controls prefs` closed-overlay fallback).
    pub(crate) fn log(&self) -> &KittyLog {
        &self.mem
    }

    /// The current change stamp (bumps once per recorded sighting).
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// The collected identity used by the cursor companion (O(1), no I/O).
    pub(crate) fn companion_look(&self) -> Option<KittyLook> {
        self.companion
    }

    /// Snapshot for the settings overlay (memory only — no IO).
    pub(crate) fn view(&self) -> KittyLogView {
        KittyLogView {
            revision: self.revision,
            log: self.mem.clone(),
        }
    }

    /// Drain-site entry point: dedupe, record, and debounce-flush this tick's
    /// sightings. `enabled=false` (`[sparkle_words.feline] log = false`)
    /// drains-and-drops — the effects-side recorder always runs (§F4.7), the
    /// host gate is here. `now` is the tick's existing frame Instant (no new
    /// clock reads on the render path); the RFC3339 stamp is taken once per
    /// RECORDED sighting (rare), not per tick.
    pub(crate) fn observe<I>(
        &mut self,
        session: u64,
        sightings: I,
        lexicon: &Lexicon,
        now: Instant,
        enabled: bool,
    ) -> Option<KittyLook>
    where
        I: IntoIterator<Item = KittySighting>,
    {
        let mut discovery = None;
        for s in sightings {
            if !enabled {
                continue; // still consume the drain: the vec must empty either way
            }
            if self.ring_note(session, s.ident, now) {
                continue; // recently logged (shared window / vim round-trip)
            }
            let stamp = now_rfc3339();
            if self.mem.record(&s, lexicon, &stamp) {
                let look = s.look.normalized();
                self.companion = Some(look);
                discovery = Some(look);
            }
            let _ = self.delta.record(&s, lexicon, &stamp);
            self.revision = self.revision.wrapping_add(1);
        }
        self.maybe_flush(now);
        discovery
    }

    /// Note `(session, ident)` in the dedupe ring. Returns `true` when the
    /// episode was logged within [`RING_TTL`] (⇒ suppress), refreshing its
    /// stamp so a cat that STAYS on screen keeps absorbing its own recounts
    /// (alt-screen round-trips, config reloads, grace expiry). An expired or
    /// absent entry is (re-)stamped and NOT suppressed.
    fn ring_note(&mut self, session: u64, ident: u64, now: Instant) -> bool {
        for slot in &mut self.ring {
            if slot.session == session && slot.ident == ident {
                let fresh = now.saturating_duration_since(slot.at) <= RING_TTL;
                slot.at = now;
                return fresh;
            }
        }
        let slot = RingSlot {
            session,
            ident,
            at: now,
        };
        if self.ring.len() < RING_SLOTS {
            self.ring.push(slot);
        } else {
            self.ring[self.ring_next] = slot;
            self.ring_next = (self.ring_next + 1) % RING_SLOTS;
        }
        false
    }

    /// Drain-time debounce: hand the delta to the single long-lived writer at
    /// most once per [`FLUSH_DEBOUNCE`], via a NON-blocking `try_send`. The
    /// worker does the whole read-merge-write; the render thread only performs a
    /// bounded `try_send`, so a slow/networked config dir can no longer stall a
    /// render frame (TYPING-5). If the worker is still busy with a prior
    /// batch (bounded channel full), the delta is COALESCED back into `self.delta`
    /// and retried on the next observe — no counts are lost. If construction-time
    /// pre-arm failed, the delta remains resident for [`Self::flush_exit`]'s
    /// inline durability fallback; render never retries thread creation.
    fn maybe_flush(&mut self, now: Instant) {
        if self.delta.is_empty() {
            return;
        }
        if self
            .last_flush
            .is_some_and(|t| now.saturating_duration_since(t) < FLUSH_DEBOUNCE)
        {
            return;
        }
        if self.path.is_none() {
            return; // in-memory-only: totals stay in `mem`, the delta is moot
        }
        let Some(writer) = self.writer.as_ref() else {
            // Pre-arm failed. Keep the accumulated delta for `flush_exit`'s
            // inline fallback; never retry thread creation from a render frame.
            return;
        };
        self.last_flush = Some(now);
        // `self.delta` is untouched between here and the next `observe`, so on a
        // full channel we can restore it verbatim (no merge needed).
        let delta = std::mem::take(&mut self.delta);
        match writer.tx.try_send(delta) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(delta)) => {
                // Worker still writing a prior batch (slow disk): coalesce and
                // retry promptly rather than block the render thread.
                self.delta = delta;
                self.last_flush = None;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(delta)) => {
                // Worker died. Preserve the batch and detach the already-dead
                // handle without joining from render; exit-time inline flush is
                // now the sole durability owner.
                self.delta = delta;
                self.last_flush = None;
                let _ = self.writer.take();
            }
        }
    }

    /// Exit-path flush: hand the writer any remaining delta with a BLOCKING send
    /// (we are past the event loop — blocking is fine), then drop the sender so
    /// the worker drains its queue, sees `recv → Err`, and exits, and join it —
    /// so every batch lands before quit with no last-writer-wins race. With no
    /// writer (in-memory-only, or nothing ever flushed) fall back to an inline
    /// merge of whatever accumulated.
    pub(crate) fn flush_exit(&mut self) {
        let delta = std::mem::take(&mut self.delta);
        match self.writer.take() {
            Some(KittyWriter { tx, handle }) => {
                // Deliver the tail only if there's something to write; then drop
                // the sender — the worker still drains any already-queued batch
                // (buffered items survive the sender drop) before `recv → Err`.
                let unsent = if delta.is_empty() {
                    None
                } else {
                    tx.send(delta).err().map(|error| error.0)
                };
                drop(tx);
                let _ = handle.join();
                if let Some(delta) = unsent
                    && let Some(path) = &self.path
                {
                    KittyLog::flush_merge(path, &delta);
                }
            }
            None => {
                // No worker ever started (in-memory-only, or nothing flushed).
                if !delta.is_empty()
                    && let Some(path) = &self.path
                {
                    KittyLog::flush_merge(path, &delta);
                }
            }
        }
    }
}

// ---- The collection book (settings §F4.6) -------------------------------------------

/// One rendered collection-book row — the SHARED model behind the settings
/// painter, `SettingsState::controls_lines`, and the `controls prefs`
/// closed-overlay fallback (screen == introspection by construction).
pub(crate) struct KittyBookRow {
    /// Rarity tier: `legendary` / `rare` / `traits` / `common`.
    pub(crate) tier: &'static str,
    /// The registry `config_key` (or the trait key) of this cell.
    pub(crate) key: &'static str,
    /// The human label ([`KittyType::label`] / [`KittyMagic::label`] / trait).
    pub(crate) label: &'static str,
    /// Whether it has been sighted at all (`false` paints the `???` row).
    pub(crate) seen: bool,
    /// Encounter count for an individual item; distinct discoveries for an
    /// aggregate progress row.
    pub(crate) count: u64,
    /// Number of designs represented by this row (`1` for individual art).
    pub(crate) goal: usize,
    /// Language chips: every code that has sighted this cell.
    pub(crate) langs: Vec<String>,
    /// RFC3339 UTC of the first sighting (empty when unseen).
    pub(crate) first_seen: String,
    /// RFC3339 UTC of the last sighting (empty when unseen).
    pub(crate) last_seen: String,
}

/// The header stats + rows of the collection book.
pub(crate) struct KittyBook {
    /// Lifetime sighting total.
    pub(crate) sightings: u64,
    /// Distinct authored glyphs discovered.
    pub(crate) collected: usize,
    /// The generated, actually reachable art roster.
    pub(crate) denominator: usize,
    /// The primary-language codes seen, in first-discovery order.
    pub(crate) languages: Vec<String>,
    /// Label of the newest collected special (or accessory fallback), or `None`.
    pub(crate) rarest: Option<&'static str>,
    /// The book rows, grouped by tier in display order.
    pub(crate) rows: Vec<KittyBookRow>,
}

fn glyph_label(key: &str) -> &'static str {
    match key {
        "spec_fluffy" => "Cloud Puff",
        "spec_maneki" => "Lucky Bean",
        "spec_sleeping" => "Cinnamon Roll",
        "spec_stretch" => "Toastbyte",
        "spec_tabbybell" => "Biscuit",
        "spec_tuxedo" => "Sir Socks",
        "spec_witch" => "Moon Mochi",
        "spec_yarn" => "Tangle",
        "acc_bell" => "Golden Bell",
        "acc_bow" => "Red Bow",
        "acc_crown" => "Crown",
        _ => "Cat Character",
    }
}

fn collectible_row(log: &KittyLog, index: usize, tier: &'static str) -> KittyBookRow {
    let def = &GLYPHS[index];
    let found = log.collectibles.iter().find(|item| item.key == def.id);
    KittyBookRow {
        tier,
        key: def.id,
        label: glyph_label(def.id),
        seen: found.is_some(),
        count: found.map_or(0, |item| item.count),
        goal: 1,
        langs: found.map_or_else(Vec::new, |item| item.langs.clone()),
        first_seen: found.map_or_else(String::new, |item| item.first_seen.clone()),
        last_seen: found.map_or_else(String::new, |item| item.last_seen.clone()),
    }
}

/// Build the collection book from the generated, reachable v4 art roster.
/// Eight full-cat specials and three overlay accessories get individual rows;
/// the 25 head variants are summarized in one row so the page remains compact.
pub(crate) fn kitty_book(log: &KittyLog) -> KittyBook {
    let mut languages: Vec<String> = Vec::new();
    for e in &log.entries {
        if !languages.contains(&e.lang) {
            languages.push(e.lang.clone());
        }
    }
    let denominator = GLYPH_IDS.len();
    // Full-body specials outrank attachments; within a class, use discovery
    // order from the durable ledger so this is meaningful rather than an
    // accidental consequence of generated enum ordering.
    let rarest = [GlyphKind::Special, GlyphKind::Accessory]
        .into_iter()
        .find_map(|kind| {
            log.collectibles.iter().rev().find_map(|item| {
                glyph_from_key(&item.key)
                    .filter(|id| GLYPHS[*id as usize].kind == kind)
                    .map(|id| glyph_label(GLYPHS[id as usize].id))
            })
        });
    let mut rows: Vec<KittyBookRow> = Vec::with_capacity(12);
    for (i, def) in GLYPHS.iter().enumerate() {
        match def.kind {
            GlyphKind::Special => rows.push(collectible_row(log, i, "specials")),
            GlyphKind::Accessory => rows.push(collectible_row(log, i, "accessories")),
            GlyphKind::Head => {}
        }
    }
    let head_items: Vec<&KittyCollectible> = log
        .collectibles
        .iter()
        .filter(|item| {
            glyph_from_key(&item.key).is_some_and(|id| GLYPHS[id as usize].kind == GlyphKind::Head)
        })
        .collect();
    let mut head_row = KittyBookRow {
        tier: "heads",
        key: "heads",
        label: "Head variants",
        seen: !head_items.is_empty(),
        count: head_items.len() as u64,
        goal: GLYPHS
            .iter()
            .filter(|def| def.kind == GlyphKind::Head)
            .count(),
        langs: Vec::new(),
        first_seen: String::new(),
        last_seen: String::new(),
    };
    for item in head_items {
        head_row.first_seen = min_ts(&head_row.first_seen, &item.first_seen);
        head_row.last_seen = max_ts(&head_row.last_seen, &item.last_seen);
        for code in &item.langs {
            if !head_row.langs.iter().any(|c| c == code) {
                head_row.langs.push(code.clone());
            }
        }
    }
    rows.push(head_row);
    KittyBook {
        sightings: log.sightings,
        collected: GLYPHS
            .iter()
            .filter(|def| log.collectibles.iter().any(|item| item.key == def.id))
            .count(),
        denominator,
        languages,
        rarest,
        rows,
    }
}

/// Serialize the book as `kittylog …` introspection lines — consumed by BOTH
/// `SettingsState::controls_lines` (open overlay) and the `read_aux_controls`
/// closed-overlay fallback, from the same [`kitty_book`] model the painter
/// renders (screen == introspection).
pub(crate) fn book_lines(log: &KittyLog) -> Vec<String> {
    let book = kitty_book(log);
    let mut out = Vec::with_capacity(book.rows.len() + 1);
    out.push(format!(
        "kittylog sightings={} collected={} denominator={} languages=[{}] rarest={}",
        book.sightings,
        book.collected,
        book.denominator,
        book.languages.join(","),
        book.rarest.map_or("none", |l| l).to_lowercase(),
    ));
    for r in &book.rows {
        out.push(format!(
            "kittylog tier={} key={} label={:?} seen={} count={} goal={} langs=[{}] first={:?} last={:?}",
            r.tier,
            r.key,
            r.label,
            r.seen,
            r.count,
            r.goal,
            r.langs.join(","),
            r.first_seen,
            r.last_seen,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_effects::cat_glyphs_gen::CatGlyphId;
    use aterm_effects::kitty_registry::{KittyMagic, KittyType};
    use aterm_lexicon::LangSet;
    use aterm_spec::derive::{kitty_collectibles_model, kitty_sidecar_durability_model};
    use aterm_spec::verify;
    use std::collections::BTreeMap;

    /// Per-TEST scratch dir (tests share one process, so a pid-keyed dir would
    /// race across the parallel test threads) — the health.rs test idiom.
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aterm-kitty-log-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join(KITTY_LOG_FILE)
    }

    fn sighting(ident: u64) -> KittySighting {
        KittySighting {
            kitty_type: KittyType::HeadPeek,
            magic: KittyMagic::None,
            shown_as: KittyShownAs::Cat,
            langs: LangSet::EMPTY,
            traits: TRAIT_SHY,
            look: KittyLook::default(),
            ident,
        }
    }

    fn write_transitional_embedded(path: &Path, log: &KittyLog) {
        std::fs::write(path, toml::to_string(log).expect("serialize legacy ledger"))
            .expect("write transitional ledger");
    }

    fn write_precollectibles_rewrite(path: &Path, log: &KittyLog) {
        let mut old = log.clone();
        old.collectibles.clear();
        old.write(path);
    }

    /// Both durable files round-trip and expose the same rollback-readable
    /// collectible replica.
    #[test]
    fn store_round_trips_through_toml() {
        let p = tmp("roundtrip");
        let lex = Lexicon::builtin();
        let mut log = KittyLog::default();
        log.record(&sighting(1), lex, "2026-07-01T10:00:00Z");
        let crowned = KittySighting {
            traits: TRAIT_SHY | TRAIT_CROWN,
            ..sighting(2)
        };
        log.record(&crowned, lex, "2026-07-02T10:00:00Z");
        log.write(&p);
        assert!(KittyLog::write_collectible_store(
            &collectibles_path(&p),
            &log.collectibles,
        ));
        let back = KittyLog::read(&p);
        assert_eq!(back, log, "legacy mirror write → read is identity");
        assert!(
            std::fs::read_to_string(&p)
                .expect("legacy ledger")
                .contains("collectibles"),
            "collectible-aware rollback must see the embedded mirror"
        );
        let full = KittyLog::read_with_sidecar(&p);
        assert_eq!(full, log, "combined legacy + sidecar read is identity");
        assert_eq!(back.sightings, 2);
        assert_eq!(back.trait_shy, 2);
        assert_eq!(back.accessory_crown, 1, "v3 accessory counter persists");
        assert_eq!(back.shown_cat, 2);
        assert_eq!(back.entries.len(), 1, "same (type, magic, lang) cell");
        assert_eq!(back.entries[0].count, 2);
        assert_eq!(back.entries[0].first_seen, "2026-07-01T10:00:00Z");
        assert_eq!(back.entries[0].last_seen, "2026-07-02T10:00:00Z");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// Absent and corrupt files both read as the empty default (fail-open —
    /// the log is observability, never a gate), and a partial/unknown-key
    /// file keeps whatever fields it does carry.
    #[test]
    fn corrupt_or_absent_file_reads_empty() {
        let p = tmp("corrupt");
        assert!(KittyLog::read(&p).is_empty(), "absent ⇒ empty");
        std::fs::write(&p, "not = [valid").unwrap();
        assert!(KittyLog::read(&p).is_empty(), "corrupt ⇒ empty");
        std::fs::write(&p, "sightings = 3\nfuture_key = true\n").unwrap();
        let l = KittyLog::read(&p);
        assert_eq!(l.sightings, 3, "known fields survive unknown keys");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// Flushing merges with what another process already wrote (summed
    /// counts, min first / max last, unioned chips) instead of clobbering it;
    /// an empty delta is a no-op that never creates the file.
    #[test]
    fn flush_is_read_merge_write() {
        let p = tmp("merge");
        KittyLog::flush_merge(&p, &KittyLog::default());
        assert!(!p.exists(), "empty delta must not create the ledger");

        let lex = Lexicon::builtin();
        // "Another process" recorded first.
        let mut theirs = KittyLog::default();
        theirs.record(&sighting(1), lex, "2026-07-01T09:00:00Z");
        theirs.entries[0].langs = vec!["en".to_string()];
        theirs.write(&p);
        // Our delta overlaps the same cell and adds a new one.
        let mut delta = KittyLog::default();
        delta.record(&sighting(2), lex, "2026-07-03T09:00:00Z");
        delta.entries[0].langs = vec!["ms".to_string()];
        let other = KittySighting {
            magic: KittyMagic::Sakura,
            ..sighting(3)
        };
        delta.record(&other, lex, "2026-07-02T09:00:00Z");
        KittyLog::flush_merge(&p, &delta);

        let merged = KittyLog::read(&p);
        assert_eq!(merged.sightings, 3, "counts sum");
        assert_eq!(merged.entries.len(), 2, "cells merge by key");
        let cell = &merged.entries[0];
        assert_eq!(cell.count, 2);
        assert_eq!(cell.first_seen, "2026-07-01T09:00:00Z", "min first_seen");
        assert_eq!(cell.last_seen, "2026-07-03T09:00:00Z", "max last_seen");
        assert_eq!(cell.langs, ["en", "ms"], "chips union");
        assert_eq!(merged.last_seen, "2026-07-03T09:00:00Z");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn transitional_collectibles_migrate_once_and_survive_a_rollback_rewrite() {
        let p = tmp("rollback-sidecar");
        let lex = Lexicon::builtin();
        let mut transitional = KittyLog::default();
        transitional.record(&sighting(1), lex, "2026-07-01T09:00:00Z");
        write_transitional_embedded(&p, &transitional);
        assert!(!collectibles_path(&p).exists());

        let migrated = KittyLog::read_with_sidecar(&p);
        assert_eq!(migrated.collectibles.len(), 1);
        assert_eq!(migrated.collectibles[0].count, 1);
        assert!(collectibles_path(&p).exists(), "load creates the sidecar");

        // A pre-v4 process reads known legacy fields and serializes that old
        // schema back, removing the embedded table while leaving the sibling
        // sidecar outside its reach.
        let mut rollback_view = KittyLog::read(&p);
        rollback_view.sightings = rollback_view.sightings.saturating_add(1);
        write_precollectibles_rewrite(&p, &rollback_view);
        assert!(KittyLog::read(&p).collectibles.is_empty());

        let after_rollback = KittyLog::read_with_sidecar(&p);
        assert_eq!(after_rollback.collectibles.len(), 1);
        assert_eq!(
            after_rollback.collectibles[0].count, 1,
            "the embedded migration must not be counted again"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn pre_baseline_sidecar_unions_rollback_key_with_replica_max_counts() {
        let p = tmp("pre-baseline-sidecar");
        let lex = Lexicon::builtin();
        let sample = sighting(88);
        let look_a = sample.look.normalized();
        let mut sidecar_a = KittyLog::default();
        assert!(sidecar_a.record_collectible(
            glyph_key(look_a.variant),
            look_a,
            &sample,
            lex,
            "2026-07-09T12:00:00Z",
        ));
        assert!(KittyLog::write_collectible_store_state(
            &collectibles_path(&p),
            &KittyCollectibleStore {
                collectibles: sidecar_a.collectibles.clone(),
                legacy_mirror: None,
            },
        ));

        // The old collectible-aware build has the replicated A and discovers
        // B. A sidecar written by the interim one-way implementation has no
        // baseline, so reconciliation uses per-key max rather than sum.
        let mut rollback = sidecar_a;
        let look_b = KittyLook {
            variant: CatGlyphId::S101,
            ..look_a
        };
        assert!(rollback.record_collectible(
            glyph_key(look_b.variant),
            look_b,
            &sample,
            lex,
            "2026-07-09T12:00:01Z",
        ));
        rollback.write(&p);

        let upgraded = KittyLog::read_with_sidecar(&p);
        assert_eq!(upgraded.collectibles.len(), 2, "A+B are unioned");
        assert_eq!(
            upgraded
                .collectibles
                .iter()
                .map(|row| row.count)
                .sum::<u64>(),
            2,
            "the physical A replica is not counted twice"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn replica_delta_distinguishes_reset_rows_from_stale_first_seen() {
        let baseline = KittyCollectible {
            count: 3,
            first_seen: "2026-07-01T00:00:00Z".into(),
            last_seen: "2026-07-03T00:00:00Z".into(),
            ..KittyCollectible::default()
        };
        let stale_legacy = KittyCollectible {
            count: 2,
            // A delayed authoritative flush moved first_seen earlier, while
            // the mirror write failed and retained this later historical row.
            first_seen: "2026-07-02T00:00:00Z".into(),
            last_seen: "2026-07-02T00:00:00Z".into(),
            ..KittyCollectible::default()
        };
        assert_eq!(
            KittyLog::collectible_replica_delta(&stale_legacy, Some(&baseline)),
            0,
            "a stale later first_seen is not evidence of post-reset activity"
        );

        let recreated = KittyCollectible {
            count: 2,
            first_seen: "2026-07-04T00:00:00Z".into(),
            last_seen: "2026-07-05T00:00:00Z".into(),
            ..KittyCollectible::default()
        };
        assert_eq!(
            KittyLog::collectible_replica_delta(&recreated, Some(&baseline)),
            2,
            "a row born after the baseline window is entirely new"
        );
    }

    /// Tier-1 conformance for the real bidirectional two-file protocol. The
    /// trace discovers A in the current build, discovers B and repeats A in a
    /// collectible-aware rollback, reconciles both positive deltas exactly
    /// once, then survives and repairs a pre-collectibles destructive rewrite.
    /// The negative control omits authoritative sidecar rows and reproduces the
    /// rollback loss admitted solely by `Buggy=1`.
    #[test]
    fn sidecar_reconciles_rollback_discovery_and_repeat_without_double_counting() {
        let model = kitty_sidecar_durability_model();
        let project = |path: &Path, known: i64, events: i64| -> BTreeMap<_, _> {
            let count = |len: usize| i64::try_from(len).expect("bounded roster fits i64");
            let event_count = |rows: &[KittyCollectible]| {
                i64::try_from(rows.iter().map(|row| row.count).sum::<u64>())
                    .expect("bounded test event count fits i64")
            };
            let base_rows = KittyLog::read(path).collectibles;
            let persisted = KittyLog::read_collectible_store(&collectibles_path(path));
            let sidecar_rows = persisted
                .as_ref()
                .map(|store| store.collectibles.as_slice())
                .unwrap_or_default();
            let baseline = persisted
                .as_ref()
                .and_then(|store| store.legacy_mirror.as_deref());
            let durable_rows = KittyLog::reconcile_collectible_replicas(
                persisted
                    .as_ref()
                    .map(|store| store.collectibles.as_slice()),
                baseline,
                &base_rows,
            );
            let pending = base_rows
                .iter()
                .filter(|row| !sidecar_rows.iter().any(|item| item.key == row.key))
                .count();
            let pending_events = base_rows
                .iter()
                .map(|row| {
                    let baseline_row =
                        baseline.and_then(|rows| rows.iter().find(|item| item.key == row.key));
                    KittyLog::collectible_replica_delta(row, baseline_row)
                })
                .sum::<u64>();
            [
                ("known", known),
                ("base", count(base_rows.len())),
                ("sidecar", count(sidecar_rows.len())),
                ("pending", count(pending)),
                (
                    "pending_events",
                    i64::try_from(pending_events).expect("bounded pending events fit i64"),
                ),
                ("durable", count(durable_rows.len())),
                ("events", events),
                ("base_events", event_count(&base_rows)),
                ("sidecar_events", event_count(sidecar_rows)),
                ("mirror_events", baseline.map_or(0, event_count)),
                ("durable_events", event_count(&durable_rows)),
            ]
            .into_iter()
            .collect()
        };

        let path = tmp("sidecar-conformance");
        let initial = project(&path, 0, 0);
        let mut spec = model.init_state();
        assert_eq!(initial, spec);
        let sample = sighting(7000);
        let look = sample.look.normalized();
        let mut discovery = KittyLog::default();
        assert!(discovery.record_collectible(
            glyph_key(look.variant),
            look,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:00Z",
        ));
        KittyLog::flush_merge(&path, &discovery);
        let discovered = project(&path, 1, 1);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &initial,
            &discovered,
            Some("Discover"),
            "real sidecar collectible discovery",
        );
        assert!(ok, "model rejected sidecar discovery: {why}");
        assert!(model.fire("Discover", &mut spec));
        assert_eq!(discovered, spec);

        // The first collectible-aware build knows only kitty-log.toml. It sees
        // mirrored A, discovers B there, and leaves the sidecar untouched.
        let mut rollback_view = KittyLog::read(&path);
        let rollback_look = KittyLook {
            variant: CatGlyphId::S101,
            ..look
        };
        assert!(rollback_view.record_collectible(
            glyph_key(CatGlyphId::S101),
            rollback_look,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:01Z",
        ));
        rollback_view.write(&path);
        let rollback_discovered = project(&path, 2, 2);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &discovered,
            &rollback_discovered,
            Some("OldDiscover"),
            "collectible-aware rollback discovers B",
        );
        assert!(ok, "model rejected rollback discovery: {why}");
        assert!(model.fire("OldDiscover", &mut spec));
        assert_eq!(rollback_discovered, spec);

        let reconciled = KittyLog::read_with_sidecar(&path);
        assert_eq!(reconciled.collectibles.len(), 2, "A+B survive re-upgrade");
        let reupgraded = project(&path, 2, 2);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &rollback_discovered,
            &reupgraded,
            Some("Reconcile"),
            "re-upgrade unions rollback-only B",
        );
        assert!(ok, "model rejected rollback reconciliation: {why}");
        assert!(model.fire("Reconcile", &mut spec));
        assert_eq!(reupgraded, spec);

        // The rollback now repeats A. Re-upgrade must import count +1, not add
        // the complete mirrored A+B ledger to the authoritative copy.
        let mut rollback_repeat = KittyLog::read(&path);
        assert!(!rollback_repeat.record_collectible(
            glyph_key(look.variant),
            look,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:02Z",
        ));
        rollback_repeat.write(&path);
        let repeated = project(&path, 2, 3);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &reupgraded,
            &repeated,
            Some("OldRepeat"),
            "collectible-aware rollback repeats A",
        );
        assert!(ok, "model rejected rollback repeat: {why}");
        assert!(model.fire("OldRepeat", &mut spec));
        assert_eq!(repeated, spec);

        let final_rows = KittyLog::read_with_sidecar(&path).collectibles;
        assert_eq!(final_rows.iter().map(|row| row.count).sum::<u64>(), 3);
        assert_eq!(
            final_rows
                .iter()
                .find(|row| row.key == glyph_key(look.variant))
                .expect("A")
                .count,
            2,
            "mirrored A is not counted twice"
        );
        assert_eq!(
            KittyLog::read_with_sidecar(&path)
                .collectibles
                .iter()
                .map(|row| row.count)
                .sum::<u64>(),
            3,
            "a second re-upgrade load is idempotent"
        );
        let repeat_reconciled = project(&path, 2, 3);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &repeated,
            &repeat_reconciled,
            Some("Reconcile"),
            "re-upgrade imports one repeat delta",
        );
        assert!(ok, "model rejected repeat reconciliation: {why}");
        assert!(model.fire("Reconcile", &mut spec));
        assert_eq!(repeat_reconciled, spec);

        let precollectibles_view = KittyLog::read(&path);
        write_precollectibles_rewrite(&path, &precollectibles_view);
        let survived = project(&path, 2, 3);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &repeat_reconciled,
            &survived,
            Some("OldRewrite"),
            "pre-collectibles destructive rewrite",
        );
        assert!(ok, "model rejected rollback-safe rewrite: {why}");
        assert!(model.fire("OldRewrite", &mut spec));
        assert_eq!(survived, spec);
        assert!(model.check_invariant("NoUnlockRollback", &survived));
        assert!(model.check_invariant("NoCountRollback", &survived));

        // With the mirror gone, the collectible-aware rollback can still start
        // a fresh book and discover C. Re-upgrade must union that new key with
        // sidecar-only A+B rather than treating the empty mirror as final.
        let mut empty_rollback = KittyLog::read(&path);
        assert!(empty_rollback.collectibles.is_empty());
        let look_c = KittyLook {
            variant: CatGlyphId::S102,
            ..look
        };
        assert!(empty_rollback.record_collectible(
            glyph_key(look_c.variant),
            look_c,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:03Z",
        ));
        empty_rollback.write(&path);
        let empty_rollback_discovered = project(&path, 3, 4);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &survived,
            &empty_rollback_discovered,
            Some("OldDiscover"),
            "collectible-aware rollback discovers C after mirror erasure",
        );
        assert!(ok, "model rejected empty-mirror rollback discovery: {why}");
        assert!(model.fire("OldDiscover", &mut spec));
        assert_eq!(empty_rollback_discovered, spec);

        let reunioned_rows = KittyLog::read_with_sidecar(&path).collectibles;
        assert_eq!(
            reunioned_rows.len(),
            3,
            "sidecar A+B unions rollback-only C"
        );
        assert_eq!(reunioned_rows.iter().map(|row| row.count).sum::<u64>(), 4);
        let reunioned = project(&path, 3, 4);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &empty_rollback_discovered,
            &reunioned,
            Some("Reconcile"),
            "re-upgrade reconciles a discovery from an erased mirror",
        );
        assert!(ok, "model rejected empty-mirror reconciliation: {why}");
        assert!(model.fire("Reconcile", &mut spec));
        assert_eq!(reunioned, spec);

        let precollectibles_view = KittyLog::read(&path);
        write_precollectibles_rewrite(&path, &precollectibles_view);
        let erased_again = project(&path, 3, 4);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &reunioned,
            &erased_again,
            Some("OldRewrite"),
            "second pre-collectibles destructive rewrite",
        );
        assert!(ok, "model rejected second rollback-safe rewrite: {why}");
        assert!(model.fire("OldRewrite", &mut spec));
        assert_eq!(erased_again, spec);

        // The information-losing chain can also rediscover an already-known
        // semantic key. The recreated row has a new first_seen stamp, which
        // marks its whole count as post-baseline rather than subtracting the
        // now-larger sidecar count and silently losing the encounter.
        let mut reset_repeat = KittyLog::read(&path);
        assert!(reset_repeat.collectibles.is_empty());
        assert!(reset_repeat.record_collectible(
            glyph_key(look.variant),
            look,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:04Z",
        ));
        reset_repeat.write(&path);
        let repeated_after_reset = project(&path, 3, 5);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &erased_again,
            &repeated_after_reset,
            Some("OldRepeatReset"),
            "collectible-aware rollback repeats A after mirror erasure",
        );
        assert!(ok, "model rejected reset-mirror repeat: {why}");
        assert!(model.fire("OldRepeatReset", &mut spec));
        assert_eq!(repeated_after_reset, spec);

        let reset_repeat_rows = KittyLog::read_with_sidecar(&path).collectibles;
        assert_eq!(reset_repeat_rows.len(), 3);
        assert_eq!(
            reset_repeat_rows.iter().map(|row| row.count).sum::<u64>(),
            5
        );
        let reset_repeat_reconciled = project(&path, 3, 5);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &repeated_after_reset,
            &reset_repeat_reconciled,
            Some("Reconcile"),
            "re-upgrade imports same-key repeat from a recreated mirror",
        );
        assert!(ok, "model rejected reset-mirror reconciliation: {why}");
        assert!(model.fire("Reconcile", &mut spec));
        assert_eq!(reset_repeat_reconciled, spec);

        let precollectibles_view = KittyLog::read(&path);
        write_precollectibles_rewrite(&path, &precollectibles_view);
        let erased_final = project(&path, 3, 5);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &reset_repeat_reconciled,
            &erased_final,
            Some("OldRewrite"),
            "final pre-collectibles destructive rewrite",
        );
        assert!(ok, "model rejected final rollback-safe rewrite: {why}");
        assert!(model.fire("OldRewrite", &mut spec));
        assert_eq!(erased_final, spec);

        let restored_rows = KittyLog::read_with_sidecar(&path).collectibles;
        assert_eq!(restored_rows.len(), 3);
        assert_eq!(restored_rows.iter().map(|row| row.count).sum::<u64>(), 5);
        let restored = project(&path, 3, 5);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &erased_final,
            &restored,
            Some("RestoreMirror"),
            "re-upgrade restores rollback-readable mirror",
        );
        assert!(ok, "model rejected mirror restore: {why}");
        assert!(model.fire("RestoreMirror", &mut spec));
        assert_eq!(restored, spec);

        let buggy_path = tmp("sidecar-negative");
        let mut embedded_only = KittyLog::default();
        assert!(embedded_only.record_collectible(
            glyph_key(look.variant),
            look,
            &sample,
            Lexicon::builtin(),
            "2026-07-09T12:00:00Z",
        ));
        write_transitional_embedded(&buggy_path, &embedded_only);
        assert!(KittyLog::write_collectible_store_state(
            &collectibles_path(&buggy_path),
            &KittyCollectibleStore {
                collectibles: Vec::new(),
                legacy_mirror: Some(embedded_only.collectibles.clone()),
            },
        ));
        let mut buggy_discovered = project(&buggy_path, 1, 1);
        // Former base-only code considered its own row durable and had no
        // concept of a pending cross-replica key.
        buggy_discovered.insert("pending", 0);
        buggy_discovered.insert("durable", 1);
        buggy_discovered.insert("durable_events", 1);
        let (bug_admits, bug_why) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 1)],
            &model.init_state(),
            &buggy_discovered,
            Some("Discover"),
            "base-only collectible negative control",
        );
        assert!(
            bug_admits,
            "Buggy=1 must admit base-only storage: {bug_why}"
        );
        let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
            &model,
            &[],
            &model.init_state(),
            &buggy_discovered,
            Some("Discover"),
            "base-only collectible healthy rejection",
        );
        assert!(
            !healthy_admits,
            "production model admitted base-only storage: {healthy_why}"
        );

        let old_view = KittyLog::read(&buggy_path);
        write_precollectibles_rewrite(&buggy_path, &old_view);
        let lost = project(&buggy_path, 1, 1);
        let (bug_admits, bug_why) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 1)],
            &buggy_discovered,
            &lost,
            Some("OldRewrite"),
            "base-only rollback loss",
        );
        assert!(
            bug_admits,
            "Buggy=1 must reproduce rollback loss: {bug_why}"
        );
        assert!(!model.check_invariant("NoUnlockRollback", &lost));
        assert!(!model.check_invariant("NoCountRollback", &lost));

        let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
        let _ = std::fs::remove_dir_all(buggy_path.parent().expect("scratch parent"));
    }

    #[test]
    fn interleaved_flushes_preserve_first_discovery_look_and_chronology() {
        let p = tmp("collectible-order");
        let lex = Lexicon::builtin();
        let sample = sighting(90);
        let delta = |key: CatGlyphId, coat: u8, at: &str| {
            let mut log = KittyLog::default();
            let look = KittyLook {
                variant: key,
                coat,
                ..KittyLook::default()
            };
            assert!(log.record_collectible(glyph_key(key), look, &sample, lex, at));
            log
        };

        // Process B flushes the later discovery before delayed process A.
        let late = delta(CatGlyphId::SpecSleeping, 12, "2026-07-02T00:00:00Z");
        let early = delta(CatGlyphId::SpecWitch, 3, "2026-07-01T00:00:00Z");
        KittyLog::flush_merge(&p, &late);
        KittyLog::flush_merge(&p, &early);

        // The same semantic key also arrives out of order. Its collectible
        // appearance must come from the earliest discovery, not the process
        // that happened to acquire the file lock first.
        let same_late = delta(CatGlyphId::SpecYarn, 14, "2026-07-03T00:00:00Z");
        let same_early = delta(CatGlyphId::SpecYarn, 1, "2026-06-30T00:00:00Z");
        KittyLog::flush_merge(&p, &same_late);
        KittyLog::flush_merge(&p, &same_early);

        let loaded = KittyLog::read_with_sidecar(&p);
        assert_eq!(
            loaded
                .collectibles
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["spec_yarn", "spec_witch", "spec_sleeping"],
            "disk order follows first_seen, independent of flush order"
        );
        let yarn = &loaded.collectibles[0];
        assert_eq!(yarn.count, 2);
        assert_eq!(yarn.coat, 1, "earliest discovery owns the saved look");
        assert_eq!(yarn.first_seen, "2026-06-30T00:00:00Z");
        assert_eq!(yarn.last_seen, "2026-07-03T00:00:00Z");

        let host = KittyLogHost::load(Some(p.clone()));
        assert_eq!(
            host.companion_look().map(|look| look.variant),
            Some(CatGlyphId::SpecSleeping),
            "startup companion is the chronologically latest discovery"
        );
        assert_eq!(
            kitty_book(host.log()).rarest,
            Some("Cinnamon Roll"),
            "rarest selection consumes chronological ledger order"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// TYPING-5 negative control: even when the one construction-time writer
    /// pre-arm fails, observing the first sighting must not retry a thread spawn
    /// from the render path. The delta stays resident and `flush_exit` preserves
    /// the existing inline durability fallback.
    #[test]
    fn observe_never_spawns_after_writer_prearm_failure() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer-prearm-failure");
        let spawn_calls = std::cell::Cell::new(0usize);
        let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |path| {
            spawn_calls.set(spawn_calls.get() + 1);
            assert_eq!(path, ledger);
            None
        });
        assert_eq!(
            spawn_calls.get(),
            1,
            "construction makes one pre-arm attempt"
        );
        assert!(host.writer.is_none(), "the injected pre-arm failed");

        host.observe(7, [sighting(11)], lex, Instant::now(), true);
        assert_eq!(
            spawn_calls.get(),
            1,
            "first-sighting observe must be try-send-only, never spawn"
        );
        assert!(
            host.writer.is_none(),
            "observe must not bypass the construction spawner and create a worker"
        );

        host.flush_exit();
        assert_eq!(KittyLog::read_with_sidecar(&ledger).sightings, 1);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    #[test]
    fn disconnected_prearmed_writer_preserves_delta_for_exit_flush() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer-disconnected");
        let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |_| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            drop(rx);
            let handle = std::thread::spawn(|| {});
            Some(KittyWriter { tx, handle })
        });

        host.observe(7, [sighting(11)], lex, Instant::now(), true);
        assert!(
            host.writer.is_none(),
            "a disconnected render-side sender is detached, never joined"
        );
        assert_eq!(host.delta.sightings, 1, "the rejected batch stays owned");
        host.flush_exit();
        assert_eq!(KittyLog::read_with_sidecar(&ledger).sightings, 1);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The long-lived writer path end-to-end (TYPING-5): a real-path host
    /// pre-arms ONE background worker before any sighting; `observe` hands its
    /// delta over the bounded channel without replacing/spawning a worker. A
    /// second sighting is debounced and accumulates; `flush_exit` delivers the
    /// tail, drops the sender, and JOINS the worker, so every count is durable
    /// before quit.
    #[test]
    fn background_writer_persists_and_drains_on_exit() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer"); // load() derives the same sibling ledger path
        let mut host = KittyLogHost::load(Some(ledger.clone()));
        let writer_id = host
            .writer
            .as_ref()
            .expect("persistent host pre-arms its writer")
            .handle
            .thread()
            .id();
        let t0 = Instant::now();
        // First sighting → passes the debounce → handed to the parked worker.
        host.observe(7, [sighting(11)], lex, t0, true);
        assert_eq!(
            host.writer
                .as_ref()
                .map(|writer| writer.handle.thread().id()),
            Some(writer_id),
            "observe reuses the pre-armed worker"
        );
        // Second (new session) within FLUSH_DEBOUNCE → debounced, stays in `delta`.
        host.observe(8, [sighting(22)], lex, t0 + Duration::from_secs(1), true);
        // Exit drains the tail and joins the worker — both counts must be on disk.
        host.flush_exit();
        let saved = KittyLog::read_with_sidecar(&ledger);
        assert_eq!(
            saved.sightings, 2,
            "both sightings persisted across the background writer + exit drain"
        );
        assert_eq!(saved.collectibles.len(), 1);
        assert_eq!(saved.collectibles[0].count, 2);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The (session, ident) ring: a shared-session second window and a
    /// vim-round-trip recount both collapse to ONE count; a DIFFERENT session
    /// or an expired entry counts again; `enabled = false` drains-and-drops.
    #[test]
    fn dedupe_ring_absorbs_recounts() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let t0 = Instant::now();
        host.observe(7, [sighting(42)], lex, t0, true);
        assert_eq!(host.log().sightings, 1);
        let rev = host.revision();
        // Second window draining the SAME session + the vim recount.
        host.observe(7, [sighting(42)], lex, t0, true);
        host.observe(7, [sighting(42)], lex, t0 + Duration::from_secs(60), true);
        assert_eq!(host.log().sightings, 1, "ring absorbs the recounts");
        assert_eq!(host.revision(), rev, "no revision bump when suppressed");
        // A different SESSION with the same ident is a different cat.
        host.observe(8, [sighting(42)], lex, t0, true);
        assert_eq!(host.log().sightings, 2);
        // TTL expiry (stamp was refreshed by the 60 s recount) ⇒ counts anew.
        host.observe(
            7,
            [sighting(42)],
            lex,
            t0 + Duration::from_secs(60) + RING_TTL + Duration::from_secs(1),
            true,
        );
        assert_eq!(host.log().sightings, 3, "an expired episode recounts");
        // Recorder off: drained and dropped, revision untouched.
        let rev = host.revision();
        host.observe(9, [sighting(1)], lex, t0, false);
        assert_eq!(host.log().sightings, 3, "log=false drains-and-drops");
        assert_eq!(host.revision(), rev);
    }

    /// Tier-1 conformance for the real semantic-key set primitive. Every
    /// generated key drives one `Unlock`, the first repeat drives `Repeat`, and
    /// an invalid key is rejected without a state transition. The exact model
    /// capacity is pinned to codegen so adding cat art cannot drift the proof.
    #[test]
    fn generated_collectible_set_conforms_and_rejects_duplicate_growth() {
        let model = kitty_collectibles_model();
        let roster_cap = model
            .consts
            .iter()
            .find_map(|(name, value)| (*name == "RosterCap").then_some(*value))
            .expect("model declares RosterCap");
        assert_eq!(
            usize::try_from(roster_cap).expect("non-negative roster cap"),
            GLYPH_IDS.len(),
            "generated art and its verified capacity must change together"
        );

        let lex = Lexicon::builtin();
        let sample = sighting(700);
        let look = sample.look.normalized();
        let mut log = KittyLog::default();
        let mut calls = 0i64;
        let mut discoveries = 0i64;
        let mut spec = model.init_state();
        let project = |log: &KittyLog, calls: i64, discoveries: i64| -> BTreeMap<_, _> {
            [
                (
                    "unlocked",
                    i64::try_from(log.collectibles.len()).expect("bounded roster fits i64"),
                ),
                ("discoveries", discoveries),
                ("sightings", calls),
                ("duplicates", calls - discoveries),
            ]
            .into_iter()
            .collect()
        };

        for &glyph in GLYPH_IDS {
            let prev = project(&log, calls, discoveries);
            assert_eq!(prev, spec);
            let is_new = log.record_collectible(
                glyph_key(glyph),
                look,
                &sample,
                lex,
                "2026-07-09T12:00:00Z",
            );
            assert!(is_new, "first semantic-key insertion must discover");
            calls += 1;
            discoveries += 1;
            let next = project(&log, calls, discoveries);
            let (ok, why) = verify::validate_transition_tiered(
                &model,
                &[],
                &prev,
                &next,
                Some("Unlock"),
                "real generated collectible unlock",
            );
            assert!(ok, "model rejected real unique insertion: {why}");
            assert!(model.fire("Unlock", &mut spec));
            assert_eq!(next, spec);
        }
        assert_eq!(log.collectibles.len(), GLYPH_IDS.len());

        let prev = project(&log, calls, discoveries);
        let first_key = glyph_key(GLYPH_IDS[0]);
        let first_count = log.collectibles[0].count;
        let repeat_discovered =
            log.record_collectible(first_key, look, &sample, lex, "2026-07-09T12:00:01Z");
        assert!(!repeat_discovered);
        assert_eq!(log.collectibles[0].count, first_count + 1);
        calls += 1;
        let next = project(&log, calls, discoveries);
        let (ok, why) = verify::validate_transition_tiered(
            &model,
            &[],
            &prev,
            &next,
            Some("Repeat"),
            "real generated collectible repeat",
        );
        assert!(ok, "model rejected real duplicate insertion: {why}");
        assert!(model.fire("Repeat", &mut spec));
        assert_eq!(next, spec);

        let unchanged = log.clone();
        assert!(!log.record_collectible(
            "not_a_generated_cat",
            look,
            &sample,
            lex,
            "2026-07-09T12:00:02Z",
        ));
        assert_eq!(log, unchanged, "unknown keys cannot grow the set");

        // Negative control: an append-list implementation would count an
        // already-present key as both a new unlock and a duplicate. That exact
        // step is admitted by Buggy=1 and rejected by the production model.
        let mut duplicate_prev = model.init_state();
        assert!(model.fire("Unlock", &mut duplicate_prev));
        let mut duplicate_growth = duplicate_prev.clone();
        duplicate_growth.insert("unlocked", 2);
        duplicate_growth.insert("discoveries", 2);
        duplicate_growth.insert("sightings", 2);
        duplicate_growth.insert("duplicates", 1);
        let (bug_admits, bug_why) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 1)],
            &duplicate_prev,
            &duplicate_growth,
            Some("Repeat"),
            "collectible duplicate-growth negative control",
        );
        assert!(
            bug_admits,
            "Buggy=1 must reproduce duplicate growth: {bug_why}"
        );
        let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
            &model,
            &[],
            &duplicate_prev,
            &duplicate_growth,
            Some("Repeat"),
            "collectible duplicate-growth healthy rejection",
        );
        assert!(
            !healthy_admits,
            "production model accepted duplicate growth: {healthy_why}"
        );

        // The disk boundary is untrusted: edited TOML may duplicate every row
        // or append unknown keys, but load restores a valid bounded set.
        let path = tmp("collectible-normalize");
        let mut edited = log.clone();
        edited.collectibles.extend(log.collectibles.clone());
        edited.collectibles.push(KittyCollectible {
            key: "future_unknown".to_string(),
            variant: "future_unknown".to_string(),
            ..KittyCollectible::default()
        });
        assert!(KittyLog::write_collectible_store(
            &collectibles_path(&path),
            &edited.collectibles,
        ));
        let loaded = KittyLog::read_with_sidecar(&path);
        assert_eq!(loaded.collectibles.len(), GLYPH_IDS.len());
        let mut expected = GLYPH_IDS
            .iter()
            .map(|&glyph| glyph_key(glyph))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(
            loaded
                .collectibles
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            expected,
            "equal timestamps use semantic-key order"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
    }

    #[test]
    fn persisted_collectibles_repair_accessory_bases_from_their_keys() {
        let path = tmp("collectible-look-repair");
        let edited = KittyLog {
            collectibles: vec![
                KittyCollectible {
                    key: "acc_bow".to_string(),
                    variant: "acc_bow".to_string(),
                    accessory: "acc_crown".to_string(),
                    coat: u8::MAX,
                    iris: u8::MAX,
                    age: "future-age".to_string(),
                    count: 1,
                    ..KittyCollectible::default()
                },
                KittyCollectible {
                    key: "s1_00".to_string(),
                    variant: "acc_crown".to_string(),
                    accessory: "acc_bow".to_string(),
                    count: 1,
                    ..KittyCollectible::default()
                },
                KittyCollectible {
                    key: "spec_sleeping".to_string(),
                    variant: "acc_bow".to_string(),
                    accessory: "acc_crown".to_string(),
                    count: 1,
                    ..KittyCollectible::default()
                },
            ],
            ..KittyLog::default()
        };
        assert!(KittyLog::write_collectible_store(
            &collectibles_path(&path),
            &edited.collectibles,
        ));

        let loaded = KittyLog::read_with_sidecar(&path);
        assert_eq!(loaded.collectibles.len(), 3);

        let bow = &loaded.collectibles[0];
        assert_eq!(bow.variant, glyph_key(KittyLook::default().variant));
        assert_eq!(bow.accessory, "acc_bow");
        assert_eq!((bow.coat, bow.iris), (15, 7));
        assert_eq!(bow.age, "adult");
        let bow_look = bow.look().expect("canonical bow look");
        assert_eq!(GLYPHS[bow_look.variant as usize].kind, GlyphKind::Head);
        assert_eq!(bow_look.accessory, Some(CatGlyphId::AccBow));

        let head = &loaded.collectibles[1];
        assert_eq!(head.variant, "s1_00");
        assert_eq!(head.accessory, "");
        assert_eq!(
            head.look().expect("canonical head look").variant,
            CatGlyphId::S100
        );

        let special = &loaded.collectibles[2];
        assert_eq!(special.variant, "spec_sleeping");
        assert_eq!(special.accessory, "");
        assert_eq!(
            special.look().expect("canonical special look").variant,
            CatGlyphId::SpecSleeping
        );

        let _ = std::fs::remove_dir_all(path.parent().expect("scratch parent"));
    }

    /// The book: generated-art denominator, reachable grouping, `???`
    /// (unseen) rows, rarest pick, and the introspection serialization.
    #[test]
    fn book_groups_by_tier_with_completeness() {
        let lex = Lexicon::builtin();
        let mut log = KittyLog::default();
        log.record(&sighting(1), lex, "2026-07-01T00:00:00Z");
        let sleeping = KittySighting {
            magic: KittyMagic::Sakura,
            look: KittyLook {
                variant: CatGlyphId::SpecSleeping,
                ..KittyLook::default()
            },
            ..sighting(2)
        };
        log.record(&sleeping, lex, "2026-07-02T00:00:00Z");
        // A bow-wearing plain cat (v3 §2.1): the accessory chip row lights up.
        let bowed = KittySighting {
            traits: TRAIT_SHY | TRAIT_BOW,
            look: KittyLook {
                accessory: Some(CatGlyphId::AccBow),
                ..KittyLook::default()
            },
            ..sighting(3)
        };
        log.record(&bowed, lex, "2026-07-03T00:00:00Z");
        let book = kitty_book(&log);
        assert_eq!(book.sightings, 3);
        assert_eq!(book.collected, 3, "head + full cat + accessory");
        assert_eq!(book.denominator, GLYPH_IDS.len(), "generated roster");
        assert_eq!(book.languages.len(), 1);
        assert_eq!(book.rarest, Some("Cinnamon Roll"));
        assert_eq!(book.rows.len(), 8 + 3 + 1, "specials + accessories + heads");
        let sleep = book.rows.iter().find(|r| r.key == "spec_sleeping").unwrap();
        assert!(sleep.seen && sleep.tier == "specials" && sleep.count == 1);
        let witch = book.rows.iter().find(|r| r.key == "spec_witch").unwrap();
        assert!(!witch.seen, "unseen special renders the ??? row");
        let bow = book.rows.iter().find(|r| r.key == "acc_bow").unwrap();
        assert!(bow.tier == "accessories" && bow.seen && bow.count == 1);
        let crown = book.rows.iter().find(|r| r.key == "acc_crown").unwrap();
        assert!(
            crown.tier == "accessories" && !crown.seen && crown.count == 0,
            "unworn accessory renders the ??? chip"
        );
        let heads = book.rows.iter().find(|r| r.key == "heads").unwrap();
        assert!(heads.seen && heads.tier == "heads" && heads.count == 1);
        assert_eq!(heads.goal, 25, "aggregate tracks distinct head designs");
        // The serialization mirrors the same model (screen == introspection).
        let lines = book_lines(&log);
        assert_eq!(lines.len(), 1 + book.rows.len());
        assert!(
            lines[0].contains("sightings=3")
                && lines[0].contains("collected=3")
                && lines[0].contains(&format!("denominator={}", GLYPH_IDS.len()))
                && lines[0].contains("rarest=cinnamon roll"),
            "{}",
            lines[0]
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=specials key=spec_sleeping") && l.contains("seen=true")),
            "the sleeping-special row serializes"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=accessories key=acc_bow") && l.contains("count=1")),
            "the bow chip serializes"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("tier=heads key=heads") && l.contains("count=1 goal=25")),
            "head progress serializes as distinct designs"
        );
        // An EMPTY log still advertises the finite generated roster.
        let empty = kitty_book(&KittyLog::default());
        assert_eq!(empty.denominator, GLYPH_IDS.len());
        assert!(empty.rows.iter().all(|r| !r.seen));
        assert_eq!(empty.rarest, None);
    }
}
