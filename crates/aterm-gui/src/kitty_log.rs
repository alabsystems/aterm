// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Kitty Log: a durable, machine-owned collection-book ledger of every kitty sighting the cat renderer reports, aggregated by `(shown type, magic bucket, primary language)` rather than raw words, so its size is bounded by the vocabulary, not by usage.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_effects::cat_glyphs_gen::{GLYPH_IDS, GLYPHS, GlyphKind};
use aterm_effects::kitty_registry::{
    KittyLook, KittyMagic, KittyShownAs, KittySighting, KittyType, TRAIT_BLAZE, TRAIT_BOW,
    TRAIT_CROWN, TRAIT_EAR_NICK, TRAIT_HETEROCHROMIA, TRAIT_SHY, TRAIT_SUNGLASSES, TRAIT_WITCH_HAT,
    age_from_key, age_key, glyph_from_key, glyph_key,
};
use aterm_lexicon::{LangSet, Lexicon, primary_lang};
use serde::{Deserialize, Serialize};

use crate::native_document_host::HeldAdvisoryLock;

/// The ledger's filename, a sibling of `aterm.toml` (the module doc says why it
/// never lives inside the user's config file).
const KITTY_LOG_FILE: &str = "kitty-log.toml";

/// Authoritative collectible identities and the embedded-replica baseline are
/// isolated from the legacy ledger so a destructive rollback cannot drop them.
const KITTY_COLLECTIBLES_FILE: &str = "kitty-collectibles.toml";

/// Maximum accepted and emitted size of either Kitty Log TOML file: a generous
/// 2 KiB for every possible row (every shown type × magic bucket × `LangSet` slot,
/// plus the sidecar's two roster copies).
const MAX_KITTY_LEDGER_ROWS: usize =
    KittyType::ALL.len() * KittyMagic::ALL.len() * LangSet::CAPACITY + (2 * GLYPH_IDS.len());
const MAX_KITTY_LEDGER_BYTES: usize = MAX_KITTY_LEDGER_ROWS * 2 * 1024;

fn collectibles_path(legacy_path: &Path) -> PathBuf {
    legacy_path.with_file_name(KITTY_COLLECTIBLES_FILE)
}

/// Purpose-specific admission boundary shared by the legacy ledger and its
/// sidecar.
fn read_kitty_ledger_text(path: &Path) -> Option<String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(path, MAX_KITTY_LEDGER_BYTES).ok()
}

/// Dedupe-ring capacity: how many recently-logged `(session, ident)` pairs are
/// remembered.
const RING_SLOTS: usize = 256;

/// How long a ring entry keeps absorbing recounts of the same episode ident
/// (alt-screen vim round-trips, config reloads, grace expiry + re-appearance).
const RING_TTL: Duration = Duration::from_secs(600);

/// Drain-time flush debounce: a dirty delta is handed to the writer thread at
/// most this often (plus once more on exit).
const FLUSH_DEBOUNCE: Duration = Duration::from_secs(30);
/// A retained batch retries after transient lock contention even when no new
/// sighting arrives.
const FLUSH_RETRY_DELAY: Duration = Duration::from_secs(1);

/// A contended best-effort ledger must never turn application quit into an
/// unbounded join.
const EXIT_LOCK_RETRIES: usize = 4;
const EXIT_LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);
/// Main-thread quit waits only this long for the best-effort ledger worker.
const EXIT_JOIN_BUDGET: Duration = Duration::from_millis(100);
const EXIT_JOIN_POLL: Duration = Duration::from_millis(1);

/// One aggregated collection-book row, keyed `(kitty_type, magic, lang)`: the registry `config_key()` spellings and the primary language code (`"en"`, never a raw `LangId`, which is lexicon-build-scoped).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KittyEntry {
    /// [`KittyType::config_key`] of the shown type.
    #[serde(default)]
    pub(crate) kitty_type: String,
    /// [`KittyMagic::config_key`] of the magic bucket.
    #[serde(default)]
    pub(crate) magic: String,
    /// The primary language code of the matched surface (first-appearance member of the match's language set; `"unknown"` for an empty set).
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
    /// Every language code claiming a sighted surface (the settings page's language chips): the full `LangSet`, not just the primary.
    #[serde(default)]
    pub(crate) langs: Vec<String>,
}

/// One unlockable cat-art collectible.
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
    /// RFC3339 UTC of the moment the user favourited this cat; empty for an automatic discovery.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) favourite: String,
    #[serde(default)]
    pub(crate) langs: Vec<String>,
}

impl KittyCollectible {
    /// The composition this row wears, recovered from its durable semantic key.
    pub(crate) fn look(&self) -> Option<KittyLook> {
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
    /// keys.
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
/// published as the embedded-replica baseline.
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

/// The durable ledger.
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
    /// How sightings rendered: the fallback cause, tallied apart from the shown type (cat vs the three paw-fallback causes).
    #[serde(default)]
    pub(crate) shown_cat: u64,
    #[serde(default)]
    pub(crate) shown_paw_fallback_floor: u64,
    #[serde(default)]
    pub(crate) shown_paw_fallback_overflow: u64,
    #[serde(default)]
    pub(crate) shown_paw_style: u64,
    /// Displayed-trait tallies (counted only when a head pose drew the trait that episode).
    #[serde(default)]
    pub(crate) trait_heterochromia: u64,
    #[serde(default)]
    pub(crate) trait_ear_nick: u64,
    #[serde(default)]
    pub(crate) trait_blaze: u64,
    #[serde(default)]
    pub(crate) trait_shy: u64,
    /// Accessory tallies (worn only by plain, non-magic cats; counted when the accessory was drawn that episode).
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
    /// The bounded collection roster.
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

/// Whether `candidate`'s composition wins over `current`'s for the same
/// semantic key.
fn collectible_owns_look(candidate: &KittyCollectible, current: &KittyCollectible) -> bool {
    match (candidate.favourite.as_str(), current.favourite.as_str()) {
        (c, u) if c == u => {}
        ("", _) => return false,
        (_, "") => return true,
        (c, u) => return c > u,
    }
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
    /// Read the ledger; absent or corrupt ⇒ empty default (fail-open, like `Health::read`; a Containment-denied read yields the same).
    pub(crate) fn read(path: &Path) -> Self {
        let mut log: Self = read_kitty_ledger_text(path)
            .and_then(|t| aterm_toml::from_str(&t).ok())
            .unwrap_or_default();
        log.normalize_collectibles();
        log
    }

    /// Read and reconcile the embedded replica with the authoritative sidecar.
    fn read_with_sidecar(path: &Path) -> Self {
        let sidecar_path = collectibles_path(path);
        let locks = lock_pair(path, &sidecar_path);
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
        let sidecar_safe = locks.is_some()
            && (sidecar_current
                || (reconciled.is_empty() && persisted.is_none())
                || Self::write_collectible_store_state(&sidecar_path, &desired));
        log.collectibles = reconciled;
        if sidecar_safe && embedded != log.collectibles {
            let _ = log.write(path);
        }
        log
    }

    fn read_collectible_store(path: &Path) -> Option<KittyCollectibleStore> {
        let mut store: KittyCollectibleStore =
            read_kitty_ledger_text(path).and_then(|text| aterm_toml::from_str(&text).ok())?;
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
    /// event stream.
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
    /// so only its counter advance is new.
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
    /// untrusted, hand-editable ledger.
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
            let candidate_owns_look = collectible_owns_look(candidate, item);
            item.count = if additive {
                item.count.saturating_add(candidate.count)
            } else {
                item.count.max(candidate.count)
            };
            item.first_seen = min_ts(&item.first_seen, &candidate.first_seen);
            item.last_seen = max_ts(&item.last_seen, &candidate.last_seen);
            item.favourite = max_ts(&item.favourite, &candidate.favourite);
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
    pub(crate) const fn is_empty(&self) -> bool {
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

    /// Stamp an existing roster row as the user's favourite, with the pinned
    /// composition.
    fn favourite_collectible(&mut self, look: KittyLook, now: &str) {
        let look = look.normalized();
        let key = glyph_key(look.variant);
        let Some(item) = self.collectibles.iter_mut().find(|item| item.key == key) else {
            return;
        };
        item.variant = key.to_string();
        item.accessory = look.accessory.map(glyph_key).unwrap_or("").to_string();
        item.coat = look.coat;
        item.iris = look.iris;
        item.age = age_key(look.age).to_string();
        item.favourite = max_ts(&item.favourite, now);
    }

    /// Wear a cat the user already has: stamp the row named by `key` as the
    /// favourite and return it.
    fn wear(&mut self, key: &str, now: &str) -> Option<KittyCollectible> {
        let item = self.collectibles.iter_mut().find(|item| item.key == key)?;
        item.favourite = max_ts(&item.favourite, now);
        item.look()?;
        Some(item.clone())
    }

    /// Carry a wear made by [`Self::wear`] on another copy of this ledger into this
    /// one, without counting a sighting.
    fn adopt_wear(&mut self, row: &KittyCollectible) {
        let mut carried = row.clone();
        carried.count = 0;
        self.merge_collectible(carried);
    }

    /// The roster, in collection-book order: the menu of [`Self::wear`]'s `key`.
    pub(crate) fn roster(&self) -> &[KittyCollectible] {
        &self.collectibles
    }

    /// The pinned companion: the roster row with the greatest favourite stamp
    /// (the semantic key breaks a same-second tie deterministically across
    /// processes, exactly as [`collectible_order`] does for discoveries).
    fn favourite_look(&self) -> Option<KittyLook> {
        self.collectibles
            .iter()
            .filter(|item| !item.favourite.is_empty())
            .max_by(|a, b| {
                a.favourite
                    .cmp(&b.favourite)
                    .then_with(|| a.key.cmp(&b.key))
            })
            .and_then(KittyCollectible::look)
    }

    /// Record one deduped sighting at RFC3339 stamp `now`.
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

    /// Fold `other` into `self`: summed saturating counts, min `first_seen` / max `last_seen`, per-cell merge by `(type, magic, lang)`, unioned language chips.
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

    /// Flush `delta` into the ledger at `path`: read-merge-write under the sibling `.lock` (see the module doc).
    pub(crate) fn flush_merge(path: &Path, delta: &KittyLog) -> bool {
        if delta.is_empty() {
            // no-op skip: never rewrite (or create) the file for nothing
            return true;
        }
        if !prepare_ledger_parent(path) {
            return false;
        }
        let sidecar_path = collectibles_path(path);
        let Some((_legacy_lock, _sidecar_lock)) = lock_pair(path, &sidecar_path) else {
            return false;
        };
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

        let sidecar_dirty = (!collection.collectibles.is_empty() || persisted.is_some())
            && persisted.as_ref() != Some(&desired);
        let sidecar_safe = if sidecar_dirty {
            Self::write_collectible_store_state(&sidecar_path, &desired) || sidecar_was_valid
        } else {
            sidecar_was_valid || collection.collectibles.is_empty()
        };
        if !sidecar_safe {
            return false;
        }
        merged.merge_from(delta);
        merged.collectibles = collection.collectibles;
        merged.write(path)
    }

    /// Best-effort atomic write: create-parent, pid+seq-unique sibling temp,
    /// rename (mirrors `Health::write`).
    fn write(&self, path: &Path) -> bool {
        atomic_write_toml(path, self)
    }
}

/// Create and validate the ledger's immediate parent before lock acquisition.
fn prepare_ledger_parent(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(parent) else {
        return false;
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn atomic_write_toml(path: &Path, value: &impl Serialize) -> bool {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    if !ledger_destination_is_safe(path) {
        return false;
    }
    let Ok(text) = aterm_toml::to_string(value) else {
        return false;
    };
    if text.len() > MAX_KITTY_LEDGER_BYTES {
        return false;
    }
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        // unreachable config dir (Containment) — stay in-memory
        return false;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let write_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if !ledger_destination_is_safe(path) {
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

/// Whether an atomic ledger commit may create or replace this final path.
fn ledger_destination_is_safe(path: &Path) -> bool {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn lock_not_regular() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "Kitty Log lock is not a regular non-link file",
    )
}

/// Open one lock rendezvous without following or waiting on a hostile final
/// component.
#[cfg(unix)]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(lock_not_regular());
    }
    Ok(file)
}

#[cfg(windows)]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(lock_not_regular());
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => return Err(lock_not_regular()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(lock_not_regular());
    }
    Ok(file)
}

/// Best-effort sibling lock (`kitty-log.toml.lock`) guarding a whole
/// read→merge→write.
fn try_lock(path: &Path) -> Option<HeldAdvisoryLock> {
    let lock_path = path.with_extension("toml.lock");
    let held = HeldAdvisoryLock::try_lock(open_lock(&lock_path).ok()?).ok()?;
    #[cfg(all(test, unix))]
    crate::parked_fork::fork_if_armed();
    Some(held)
}

/// Both ledger locks, legacy first (the order every writer takes them in). A
/// refused sidecar lock drops the legacy guard already taken, which releases it
/// by `LOCK_UN` too.
fn lock_pair(
    legacy_path: &Path,
    sidecar_path: &Path,
) -> Option<(HeldAdvisoryLock, HeldAdvisoryLock)> {
    let legacy = try_lock(legacy_path)?;
    let sidecar = try_lock(sidecar_path)?;
    Some((legacy, sidecar))
}

/// Best-effort RFC3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`), computed in-process;
/// empty on a pre-epoch clock.
fn now_rfc3339() -> String {
    aterm_time::unix_secs().map_or_else(String::new, format_rfc3339)
}

use aterm_types::rfc3339::format_rfc3339;

/// A ring slot: one recently-logged `(session, ident)` episode.
#[derive(Clone, Copy)]
struct RingSlot {
    session: u64,
    ident: u64,
    at: Instant,
}

/// Where a batch of sightings came from, which decides what a first-ever discovery
/// may do: words in output text never activate the cursor companion, and no
/// discovery re-dresses it (only a pinned favourite outranks the launch kitty).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SightingSource {
    /// The committed keystream (`record_typed_kitty`): a first-ever discovery is
    /// returned as the Kitty Log's accounting of an unlock; no caller presents it,
    /// and it never repoints the companion.
    Typed,
    /// The grid scanner's peeking word-cats: whatever a program happened to
    /// print.
    Ambient,
}

/// The App-side Kitty Log state: in-memory totals, the unflushed delta, the dedupe ring, and the flush debounce.
pub(crate) struct KittyLogHost {
    /// The ledger path (`…/kitty-log.toml`), or `None` for in-memory-only
    /// (no config dir — tests use this too, so they never touch the user's
    /// real ledger).
    path: Option<PathBuf>,
    /// Display totals: the asynchronously admitted startup ledger plus every
    /// sighting recorded this session (flushed or not) — what the settings
    /// page renders, memory-only, no IO on the interaction path.
    mem: KittyLog,
    /// Sightings recorded since the last flush (the read-merge-write delta).
    delta: KittyLog,
    /// Bumps once per recorded sighting; snapshot staleness + repaint stamp.
    revision: u64,
    /// The pinned favourite's look, resolved on load/update (the ledger's pin stamp,
    /// or a memory-only pin under `log = false`), so cursor frames read a `Copy`
    /// value instead of scanning the ledger.
    favourite: Option<KittyLook>,
    /// The user pinned a favourite in this process. The one-shot startup import
    /// ([`Self::absorb_initial`]) can land after such a press, and its election must
    /// leave the explicit pick alone: under `log = false` a memory-only pin has no
    /// roster row to win the election on its own.
    pinned_this_session: bool,
    /// Recently-logged `(session, ident)` episodes (≤ [`RING_SLOTS`], TTL
    /// [`RING_TTL`], stamp refreshed on hit): shared-session multi-window
    /// drains and vim-round-trip recounts collapse to one count.
    ring: Vec<RingSlot>,
    /// Next ring slot to overwrite once the ring is full (FIFO).
    ring_next: usize,
    /// When the delta was last handed to the writer.
    last_flush: Option<Instant>,
    /// The single long-lived flush writer, pre-armed by a persistent host in
    /// [`Self::load`] before the event loop can render; an in-memory host starts
    /// none.
    writer: Option<KittyWriter>,
}

/// The single background flush thread and its bounded delivery channel. One
/// long-lived worker parked on `recv` (no CPU while idle) does every
/// read-merge-write, so the UI thread only `try_send`s a delta and never spawns or
/// joins a thread on the hot path.
struct KittyWriter {
    /// Bounded so a pathologically slow disk can't grow an unbounded backlog;
    /// the sender `try_send`s and coalesces on full (see `maybe_flush`).
    tx: std::sync::mpsc::SyncSender<KittyLog>,
    /// Dedicated one-shot exit lane. This queue is empty for the worker's
    /// entire runtime and receives at most the final debounced tail, so a full
    /// ordinary queue can never make shutdown silently discard that tail.
    exit_tx: std::sync::mpsc::SyncSender<KittyLog>,
    /// One-shot startup admission produced by this same background worker.
    initial: Option<std::sync::mpsc::Receiver<KittyLog>>,
    /// The worker; joined only when it finishes within the quit deadline. A
    /// completed worker returns any batch that exhausted its finite exit-flush
    /// budget so the host, rather than thread teardown, retains ownership.
    handle: std::thread::JoinHandle<KittyLog>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExitFlushResult {
    persisted: bool,
    attempts: usize,
}

fn flush_pending_at_exit(path: &Path, pending: &KittyLog) -> ExitFlushResult {
    if pending.is_empty() {
        return ExitFlushResult {
            persisted: true,
            attempts: 0,
        };
    }
    for attempt in 1..=EXIT_LOCK_RETRIES {
        if KittyLog::flush_merge(path, pending) {
            return ExitFlushResult {
                persisted: true,
                attempts: attempt,
            };
        }
        if attempt < EXIT_LOCK_RETRIES {
            std::thread::sleep(EXIT_LOCK_RETRY_DELAY);
        }
    }
    ExitFlushResult {
        persisted: false,
        attempts: EXIT_LOCK_RETRIES,
    }
}

fn offer_exit_tail(
    exit_tx: &std::sync::mpsc::SyncSender<KittyLog>,
    tail: KittyLog,
) -> Option<KittyLog> {
    match exit_tx.try_send(tail) {
        Ok(()) => None,
        Err(
            std::sync::mpsc::TrySendError::Full(tail)
            | std::sync::mpsc::TrySendError::Disconnected(tail),
        ) => Some(tail),
    }
}

fn run_kitty_writer(
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<KittyLog>,
    exit_rx: std::sync::mpsc::Receiver<KittyLog>,
    initial_tx: Option<std::sync::mpsc::SyncSender<KittyLog>>,
) -> KittyLog {
    // Startup ledger admission can stall on a remote/dead mount; keep it
    // entirely off the UI thread. It is sampled before this worker accepts new
    // deltas, so merging the one-shot result with session memory cannot
    // double-count a flush.
    if let Some(initial_tx) = initial_tx {
        let _ = initial_tx.try_send(KittyLog::read_with_sidecar(&path));
    }
    let mut pending = KittyLog::default();
    loop {
        if pending.is_empty() {
            let Ok(delta) = rx.recv() else { break };
            pending.merge_from(&delta);
        } else {
            match rx.recv_timeout(FLUSH_RETRY_DELAY) {
                Ok(delta) => pending.merge_from(&delta),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        if KittyLog::flush_merge(&path, &pending) {
            pending = KittyLog::default();
        }
    }
    if let Ok(tail) = exit_rx.recv() {
        pending.merge_from(&tail);
    }
    if flush_pending_at_exit(&path, &pending).persisted {
        KittyLog::default()
    } else {
        pending
    }
}

impl KittyWriter {
    /// Spawn the worker bound to `path`, or `None` if the thread cannot be spawned:
    /// the host then stays memory-only and retains later deltas rather than doing
    /// filesystem work from the render or exit path.
    fn spawn(path: PathBuf) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let (initial_tx, initial_rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        let handle = std::thread::Builder::new()
            .name("kitty-log-flush".into())
            .spawn(move || run_kitty_writer(path, rx, exit_rx, Some(initial_tx)))
            .ok()?;
        Some(Self {
            tx,
            exit_tx,
            initial: Some(initial_rx),
            handle,
        })
    }
}

impl KittyLogHost {
    /// Host state persisting beside the given config path (the ledger is its `kitty-log.toml` sibling).
    pub(crate) fn load(config_path: Option<PathBuf>) -> Self {
        Self::load_with_writer_spawn(config_path, KittyWriter::spawn)
    }

    /// Construction seam for the pre-armed writer. Only `load` can spawn, so
    /// [`Self::observe`] / [`Self::maybe_flush`] cannot create a thread on the
    /// render path; tests inject a failed pre-arm.
    fn load_with_writer_spawn(
        config_path: Option<PathBuf>,
        spawn: impl FnOnce(PathBuf) -> Option<KittyWriter>,
    ) -> Self {
        let path = config_path
            .as_deref()
            .and_then(Path::parent)
            .map(|d| d.join(KITTY_LOG_FILE));
        let writer = path.clone().and_then(spawn);
        Self {
            path,
            mem: KittyLog::default(),
            delta: KittyLog::default(),
            revision: 0,
            favourite: None,
            pinned_this_session: false,
            ring: Vec::new(),
            ring_next: 0,
            last_flush: None,
            writer,
        }
    }

    /// An in-memory-only host (no ledger file) — the headless-test App, which must never read or write the developer's real ledger.
    #[cfg(any(test, feature = "bench-support"))]
    pub(crate) fn in_memory() -> Self {
        Self::load(None)
    }

    /// The in-memory totals exposed to model-level regression tests.
    #[cfg(test)]
    pub(crate) fn log(&self) -> &KittyLog {
        &self.mem
    }

    /// Drain the delta the way a real flush does, so a test can start from the state every process is in for a cat collected in an earlier run.
    #[cfg(test)]
    pub(crate) fn take_delta_for_test(&mut self) {
        self.delta = KittyLog::default();
    }

    /// The unflushed delta: what a flush would put on disk, which `mem` cannot show.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> &KittyLog {
        &self.delta
    }

    #[cfg(test)]
    fn await_initial_load(&mut self) {
        let received = self
            .writer
            .as_mut()
            .and_then(|writer| writer.initial.take())
            .map(|receiver| receiver.recv());
        if let Some(Ok(loaded)) = received {
            self.absorb_initial(loaded);
        }
    }

    /// Import the worker's one-shot startup ledger without waiting. The worker
    /// samples before processing any session delta; combining that immutable
    /// base with `mem` therefore preserves sightings recorded while admission
    /// was in flight without duplicating a persisted batch.
    fn poll_initial_load(&mut self) {
        let loaded = self.writer.as_mut().and_then(|writer| {
            let receiver = writer.initial.as_ref()?;
            match receiver.try_recv() {
                Ok(loaded) => {
                    writer.initial = None;
                    Some(loaded)
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    writer.initial = None;
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            }
        });
        let Some(loaded) = loaded else { return };
        self.absorb_initial(loaded);
    }

    /// Merge one startup ledger in and re-run the restart election. Shared by the
    /// non-blocking render-path poll and the blocking test wait, so the two cannot
    /// drift.
    fn absorb_initial(&mut self, loaded: KittyLog) {
        let mut loaded = loaded;
        loaded.merge_from(&self.mem);
        if loaded == self.mem {
            return;
        }
        self.mem = loaded;
        if !self.pinned_this_session {
            self.favourite = self.mem.favourite_look();
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// The current change stamp (bumps once per recorded sighting).
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// The pinned favourite, if any: the ledger's one claim on the cursor companion (O(1), no I/O), read by `aterm_effects::companion_look::companion_precedence` (favourite > tenured program > launch kitty).
    pub(crate) fn favourite_look(&mut self) -> Option<KittyLook> {
        self.poll_initial_load();
        self.favourite
    }

    /// [`Self::favourite_look`] as far as the ledger has loaded, for a reader that holds `&self` (the Settings preview's companion): it never polls, so before the first load lands it claims nothing.
    pub(crate) const fn favourite_look_loaded(&self) -> Option<KittyLook> {
        self.favourite
    }

    /// Typed drain-site entry point (`record_typed_kitty`): dedupe, record and debounce-flush this tick's sightings.
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
        self.observe_from(
            session,
            sightings,
            lexicon,
            now,
            enabled,
            SightingSource::Typed,
        )
    }

    /// Ambient drain-site entry point: the grid scanner's peeking word-cats (`cat` in a man page, not a keystroke).
    pub(crate) fn observe_ambient<I>(
        &mut self,
        session: u64,
        sightings: I,
        lexicon: &Lexicon,
        now: Instant,
        enabled: bool,
    ) where
        I: IntoIterator<Item = KittySighting>,
    {
        let _ = self.observe_from(
            session,
            sightings,
            lexicon,
            now,
            enabled,
            SightingSource::Ambient,
        );
    }

    /// The shared drain core. `source` is the one difference between the entry
    /// points: only [`SightingSource::Typed`] returns a discovery (neither repoints
    /// the companion).
    fn observe_from<I>(
        &mut self,
        session: u64,
        sightings: I,
        lexicon: &Lexicon,
        now: Instant,
        enabled: bool,
        source: SightingSource,
    ) -> Option<KittyLook>
    where
        I: IntoIterator<Item = KittySighting>,
    {
        self.poll_initial_load();
        let mut discovery = None;
        for s in sightings {
            if !enabled {
                // still consume the drain: the vec must empty either way
                continue;
            }
            if self.ring_note(session, s.ident, now) {
                // recently logged (shared window / vim round-trip)
                continue;
            }
            let stamp = now_rfc3339();
            if self.mem.record(&s, lexicon, &stamp) && source == SightingSource::Typed {
                // A typed discovery reports itself but never re-dresses the
                // companion (only a pin changes the cat); an ambient one gets
                // nothing back.
                discovery = Some(s.look.normalized());
            }
            let _ = self.delta.record(&s, lexicon, &stamp);
            self.revision = self.revision.wrapping_add(1);
        }
        self.maybe_flush(now);
        discovery
    }

    /// Promote one look into the durable registry and pin it as the companion.
    pub(crate) fn favourite(
        &mut self,
        s: &KittySighting,
        lexicon: &Lexicon,
        now: Instant,
        enabled: bool,
    ) {
        self.poll_initial_load();
        let look = s.look.normalized();
        self.favourite = Some(look);
        self.pinned_this_session = true;
        if !enabled {
            return;
        }
        let stamp = now_rfc3339();
        let _ = self.mem.record(s, lexicon, &stamp);
        self.mem.favourite_collectible(look, &stamp);
        let _ = self.delta.record(s, lexicon, &stamp);
        self.delta.favourite_collectible(look, &stamp);
        self.revision = self.revision.wrapping_add(1);
        self.last_flush = None;
        self.maybe_flush(now);
    }

    /// Wear the roster row called `key`: pin it and return the look now on the cursor, or `None` when no row has that key.
    pub(crate) fn wear(&mut self, key: &str, now: Instant, enabled: bool) -> Option<KittyLook> {
        self.poll_initial_load();
        let stamp = now_rfc3339();
        let row = self.mem.wear(key, &stamp)?;
        let look = row.look()?;
        self.favourite = Some(look);
        self.pinned_this_session = true;
        if !enabled {
            return Some(look);
        }
        self.delta.adopt_wear(&row);
        self.revision = self.revision.wrapping_add(1);
        self.last_flush = None;
        self.maybe_flush(now);
        Some(look)
    }

    /// The collection as the wear menu sees it: the key currently worn (if any), then every roster row in collection-book order; each row's `key` is what [`Self::wear`] takes.
    pub(crate) fn wearable(&mut self) -> (Option<String>, Vec<KittyCollectible>) {
        self.poll_initial_load();
        let rows: Vec<KittyCollectible> = self.mem.roster().to_vec();
        let worn = self.favourite.and_then(|look| {
            rows.iter()
                .find(|row| row.look() == Some(look))
                .map(|row| row.key.clone())
        });
        (worn, rows)
    }

    /// Whether `look` is the pinned favourite (the palette checkmark).
    pub(crate) fn is_favourite(&self, look: KittyLook) -> bool {
        self.favourite == Some(look.normalized())
    }

    /// Note `(session, ident)` in the dedupe ring: `true` (suppress) when the episode
    /// was logged within [`RING_TTL`], refreshing its stamp so a cat that stays on
    /// screen keeps absorbing its recounts.
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

    /// Drain-time debounce: hand the delta to the long-lived writer at most once per
    /// [`FLUSH_DEBOUNCE`] by a non-blocking `try_send`, so a slow or networked config
    /// dir never stalls a render frame.
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
            // in-memory-only: totals stay in `mem`, the delta is moot
            return;
        }
        let Some(writer) = self.writer.as_ref() else {
            // Pre-arm failed. Keep the accumulated in-memory delta; never retry
            // thread creation from a render frame or perform synchronous exit IO.
            return;
        };
        self.last_flush = Some(now);
        let delta = std::mem::take(&mut self.delta);
        match writer.tx.try_send(delta) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(delta)) => {
                self.delta = delta;
                self.last_flush = None;
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(delta)) => {
                self.delta = delta;
                self.last_flush = None;
                let _ = self.writer.take();
            }
        }
    }

    /// Exit-path flush: offer the writer any remaining delta without waiting, close its channel, and join only if the worker finishes within [`EXIT_JOIN_BUDGET`].
    pub(crate) fn flush_exit(&mut self) {
        self.poll_initial_load();
        if let Some(KittyWriter {
            tx,
            exit_tx,
            initial: _,
            handle,
        }) = self.writer.take()
        {
            let delta = std::mem::take(&mut self.delta);
            if !delta.is_empty()
                && let Some(delta) = offer_exit_tail(&exit_tx, delta)
            {
                // A disconnected worker cannot take ownership: keep the batch.
                self.delta = delta;
            }
            drop(tx);
            drop(exit_tx);
            let deadline = Instant::now() + EXIT_JOIN_BUDGET;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(EXIT_JOIN_POLL);
            }
            if handle.is_finished()
                && let Ok(unpersisted) = handle.join()
            {
                // Retry exhaustion is not an ownership sink: keep the batch, so it
                // stays visibly not durably accepted.
                self.delta.merge_from(&unpersisted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_effects::cat_glyphs_gen::CatGlyphId;
    use aterm_models_core::kitty_pin_merge_model;
    use aterm_models_effects::{
        kitty_collectibles_model, kitty_flush_worker_model, kitty_sidecar_durability_model,
    };
    use aterm_spec::derive::Model;
    use aterm_spec::verify;
    use std::collections::BTreeMap;

    /// A per-test scratch dir in a process root: [`KittyLogHost::flush_exit`] may
    /// detach a writer that re-creates it after the test, and the root goes at exit.
    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_scratch::process_root("aterm-kitty-log-test-").join(name);
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
        std::fs::write(
            path,
            aterm_toml::to_string(log).expect("serialize legacy ledger"),
        )
        .expect("write transitional ledger");
    }

    fn write_precollectibles_rewrite(path: &Path, log: &KittyLog) {
        let mut old = log.clone();
        old.collectibles.clear();
        old.write(path);
    }

    fn exact_limit_toml(prefix: &str) -> String {
        assert!(prefix.len() < MAX_KITTY_LEDGER_BYTES);
        let mut text = String::with_capacity(MAX_KITTY_LEDGER_BYTES);
        text.push_str(prefix);
        text.push('#');
        text.push_str(&"x".repeat(MAX_KITTY_LEDGER_BYTES - text.len()));
        assert_eq!(text.len(), MAX_KITTY_LEDGER_BYTES);
        text
    }

    /// Startup plus exit on a thread of its own, waited for a minute: a hang
    /// detector, not a latency budget.
    fn host_startup_and_exit(config_path: PathBuf) -> (u64, usize) {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let lex = Lexicon::builtin();
            let mut host = KittyLogHost::load(Some(config_path));
            host.await_initial_load();
            let startup = (host.log().sightings, host.log().collectibles.len());
            host.observe(91, [sighting(91)], lex, Instant::now(), true);
            host.flush_exit();
            let _ = done_tx.send(startup);
        });
        done_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("host startup plus background flush/exit must be bounded")
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

    /// Absent and corrupt files read as the empty default, and a file with unknown
    /// keys keeps the fields it does carry.
    #[test]
    fn corrupt_or_absent_file_reads_empty() {
        let p = tmp("corrupt");
        assert!(KittyLog::read(&p).is_empty(), "absent ⇒ empty");
        std::fs::write(&p, "not = [valid").unwrap();
        assert!(KittyLog::read(&p).is_empty(), "corrupt ⇒ empty");
        std::fs::write(&p, [0xff, 0xfe]).unwrap();
        assert!(KittyLog::read(&p).is_empty(), "non-UTF-8 ⇒ empty");
        std::fs::write(&p, "sightings = 3\nfuture_key = true\n").unwrap();
        let l = KittyLog::read(&p);
        assert_eq!(l.sightings, 3, "known fields survive unknown keys");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn ledger_cap_accepts_exact_limit_and_rejects_oversized_sparse_files() {
        let legacy = tmp("bounded-legacy");
        std::fs::write(&legacy, exact_limit_toml("sightings = 7\n"))
            .expect("write exact-limit legacy ledger");
        assert_eq!(
            KittyLog::read(&legacy).sightings,
            7,
            "the documented limit is inclusive"
        );

        let sidecar = collectibles_path(&legacy);
        std::fs::write(
            &sidecar,
            exact_limit_toml("collectibles = []\nlegacy_mirror = []\n"),
        )
        .expect("write exact-limit sidecar");
        assert_eq!(
            KittyLog::read_collectible_store(&sidecar),
            Some(KittyCollectibleStore {
                collectibles: Vec::new(),
                legacy_mirror: Some(Vec::new()),
            }),
            "the same inclusive boundary applies to the sidecar"
        );

        let oversized_legacy = tmp("oversized-sparse-legacy");
        std::fs::File::create(&oversized_legacy)
            .and_then(|file| file.set_len((MAX_KITTY_LEDGER_BYTES + 1) as u64))
            .expect("create oversized sparse legacy ledger");
        assert!(
            KittyLog::read(&oversized_legacy).is_empty(),
            "oversized legacy ledger fails open without reading its body"
        );

        let oversized_sidecar = collectibles_path(&oversized_legacy);
        std::fs::File::create(&oversized_sidecar)
            .and_then(|file| file.set_len((MAX_KITTY_LEDGER_BYTES + 1) as u64))
            .expect("create oversized sparse collectible sidecar");
        assert_eq!(
            KittyLog::read_collectible_store(&oversized_sidecar),
            None,
            "oversized sidecar is the absent/default state"
        );

        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
        let _ = std::fs::remove_dir_all(oversized_legacy.parent().unwrap());
    }

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path CString");
        // SAFETY: `path` is a live NUL-terminated pathname and `mkfifo` retains no pointer after returning.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    /// A hostile target planted at the legacy ledger, the collectible sidecar
    /// or the lock — a writerless FIFO, or a final symlink to a victim that a
    /// followed read would admit — is admitted as empty (or fails the lock at
    /// once): startup and exit stay bounded, the target is neither followed
    /// nor replaced, the victim is never written through, and an unsafe
    /// sidecar or an untaken lock gates the mirror write.
    #[cfg(unix)]
    #[test]
    fn a_hostile_target_at_any_ledger_path_is_not_followed_and_cannot_block_startup_or_exit() {
        use std::os::unix::fs::{FileTypeExt as _, symlink};

        let mut seeded = KittyLog::default();
        seeded.record(&sighting(1), Lexicon::builtin(), "2026-07-01T00:00:00Z");
        let store_text =
            aterm_toml::to_string(&KittyCollectibleStore::mirrored(&seeded.collectibles))
                .expect("serialize sidecar victim");
        for (case, gates_legacy) in [("legacy", false), ("sidecar", true), ("lock", true)] {
            for fifo in [true, false] {
                let legacy = tmp(&format!("hostile-{case}-{fifo}"));
                let target = match case {
                    "legacy" => legacy.clone(),
                    "sidecar" => collectibles_path(&legacy),
                    _ => legacy.with_extension("toml.lock"),
                };
                let victim = legacy.with_file_name(format!("{case}-victim.toml"));
                let victim_text = if case == "sidecar" {
                    store_text.clone()
                } else {
                    "sightings = 99\n".to_string()
                };
                if fifo {
                    make_fifo(&target);
                } else {
                    std::fs::write(&victim, &victim_text).expect("write victim");
                    symlink(&victim, &target).expect("plant final symlink");
                }
                assert_eq!(
                    host_startup_and_exit(legacy.with_file_name("aterm.toml")),
                    (0, 0),
                    "{case} fifo={fifo}: bounded, and admitted as empty"
                );
                let kind = std::fs::symlink_metadata(&target)
                    .expect("the hostile target remains")
                    .file_type();
                assert!(
                    if fifo {
                        kind.is_fifo()
                    } else {
                        kind.is_symlink()
                    },
                    "{case} fifo={fifo}: best-effort flush must not replace a hostile target"
                );
                if !fifo {
                    assert_eq!(
                        std::fs::read_to_string(&victim).expect("read victim"),
                        victim_text,
                        "{case}: never written through"
                    );
                }
                if gates_legacy {
                    assert!(
                        !legacy.exists(),
                        "{case} fifo={fifo}: the mirror write is gated"
                    );
                }
                let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
            }
        }
    }

    #[test]
    fn held_sibling_lock_never_parks_flush_exit() {
        let legacy = tmp("held-lock");
        let held = try_lock(&legacy).expect("hold sibling lock");

        assert_eq!(
            host_startup_and_exit(legacy.with_file_name("aterm.toml")),
            (0, 0),
            "startup plus exit stays bounded while another process owns the lock"
        );
        assert!(!legacy.exists(), "a contended flush must not race unlocked");
        drop(held);
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    /// Both ledger locks are free when a flush ends, even while a child forked under
    /// them has yet to exec.
    #[cfg(unix)]
    #[test]
    fn a_flush_frees_both_locks_while_a_child_forked_under_them_has_yet_to_exec() {
        let legacy = tmp("fork-window");
        let lex = Lexicon::builtin();
        let mut first = KittyLog::default();
        first.record(&sighting(21), lex, "2026-07-21T00:00:00Z");
        let armed = crate::parked_fork::arm_at(2);
        assert!(KittyLog::flush_merge(&legacy, &first), "uncontended flush");
        let child = armed.child();
        assert!(child.still_parked(), "forked under the first flush's locks");

        let mut second = KittyLog::default();
        second.record(&sighting(22), lex, "2026-07-21T00:00:01Z");
        assert!(
            KittyLog::flush_merge(&legacy, &second),
            "the next single-try flush takes both locks"
        );
        assert!(child.still_parked(), "…while the child had yet to exec");
        child.release();
        assert_eq!(KittyLog::read_with_sidecar(&legacy).sightings, 2);
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    /// A refused sidecar lock drops the legacy lock by `LOCK_UN`, so a child forked
    /// while it was held does not keep it and the next pair take succeeds.
    #[cfg(unix)]
    #[test]
    fn a_refused_sidecar_frees_the_legacy_lock_while_a_child_forked_under_it_has_yet_to_exec() {
        let legacy = tmp("fork-window-half");
        let sidecar = collectibles_path(&legacy);
        let peer = HeldAdvisoryLock::try_lock(
            open_lock(&sidecar.with_extension("toml.lock")).expect("open"),
        )
        .expect("a peer holds the sidecar lock");
        let armed = crate::parked_fork::arm_at(1);
        assert!(lock_pair(&legacy, &sidecar).is_none(), "sidecar refused");
        let child = armed.child();
        drop(peer);
        assert!(child.still_parked(), "forked under the legacy lock");
        assert!(
            lock_pair(&legacy, &sidecar).is_some(),
            "the next pair take finds the legacy lock free"
        );
        assert!(child.still_parked(), "…while the child had yet to exec");
        child.release();
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    #[test]
    fn finite_exit_lock_retries_conform_and_reject_dropped_pending_batch() {
        let model = kitty_flush_worker_model();
        let project = |accepted,
                       normal_lane,
                       host_tail,
                       exit_lane,
                       pending,
                       persisted,
                       exiting,
                       retries,
                       joined| {
            [
                ("accepted", accepted),
                ("normal_lane", normal_lane),
                ("host_tail", host_tail),
                ("exit_lane", exit_lane),
                ("pending", pending),
                ("persisted", persisted),
                ("exiting", exiting),
                ("retries", retries),
                ("joined", joined),
                ("stalled", 0),
                ("deadline", 0),
                ("detached", 0),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
        };
        let validate = |action: &str,
                        before: &BTreeMap<&'static str, i64>,
                        after: &BTreeMap<&'static str, i64>| {
            let (ok, why) = verify::validate_transition_tiered(
                &model,
                &[],
                before,
                after,
                Some(action),
                "real Kitty Log finite exit flush",
            );
            assert!(ok, "model rejected {action}: {why}");
        };

        let initial = model.init_state();
        let queued = project(1, 1, 0, 0, 0, 0, 0, 0, 0);
        validate("QueueNormal", &initial, &queued);
        let drained = project(1, 0, 0, 0, 1, 0, 0, 0, 0);
        validate("DrainNormal", &queued, &drained);
        assert!(
            !model.action_enabled("Contend", &drained),
            "active-runtime contention must not consume the fresh exit retry budget"
        );
        let exiting = project(1, 0, 0, 0, 1, 0, 1, 0, 0);
        validate("BeginExit", &drained, &exiting);

        let legacy = tmp("exit-retry-conformance");
        let held = try_lock(&legacy).expect("hold sibling lock");
        let mut delta = KittyLog::default();
        delta.record(&sighting(8), Lexicon::builtin(), "2026-07-21T00:00:00Z");
        let result = flush_pending_at_exit(&legacy, &delta);
        assert_eq!(
            result,
            ExitFlushResult {
                persisted: false,
                attempts: EXIT_LOCK_RETRIES,
            },
            "the shipping exit helper consumes exactly the finite retry budget"
        );
        assert!(
            !legacy.exists(),
            "contention must retain rather than race the batch"
        );

        let mut before = exiting;
        for retry in 1..=result.attempts {
            let after = project(1, 0, 0, 0, 1, 0, 1, retry as i64, 0);
            validate("Contend", &before, &after);
            before = after;
        }
        assert!(!model.action_enabled("Flush", &before));
        assert!(!model.action_enabled("StallIo", &before));
        let joined = project(1, 0, 0, 0, 1, 0, 1, EXIT_LOCK_RETRIES as i64, 1);
        validate("Join", &before, &joined);

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
        tx.send(delta)
            .expect("the worker lane has room for one batch");
        drop(tx);
        drop(exit_tx);
        assert_eq!(
            run_kitty_writer(legacy.clone(), rx, exit_rx, None).sightings,
            1,
            "retry exhaustion must return the worker-owned batch to its joiner"
        );
        assert!(
            !legacy.exists(),
            "the genuine worker must not claim a contended batch was persisted"
        );
        drop(held);

        let dropped = project(1, 0, 0, 0, 0, 0, 1, 1, 0);
        assert!(
            !model.check_invariant("AcceptedConserved", &dropped),
            "negative control: dropping a contended batch must violate conservation"
        );
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
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
        let mut theirs = KittyLog::default();
        theirs.record(&sighting(1), lex, "2026-07-01T09:00:00Z");
        theirs.entries[0].langs = vec!["en".to_string()];
        theirs.write(&p);
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
    fn first_flush_bootstraps_a_missing_config_directory_before_locking() {
        let seed = tmp("missing-parent");
        let root = seed.parent().unwrap().to_path_buf();
        std::fs::remove_dir_all(&root).unwrap();
        let path = root.join("fresh/profile").join(KITTY_LOG_FILE);
        let lex = Lexicon::builtin();
        let mut delta = KittyLog::default();
        delta.record(&sighting(1), lex, "2026-07-01T09:00:00Z");

        assert!(KittyLog::flush_merge(&path, &delta));
        assert_eq!(KittyLog::read_with_sidecar(&path).sightings, 1);
        assert!(path.with_extension("toml.lock").is_file());
        assert!(
            collectibles_path(&path)
                .with_extension("toml.lock")
                .is_file()
        );
        let _ = std::fs::remove_dir_all(root);
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

    /// Tier-1 conformance for the two-file protocol: discover A, then in a
    /// collectible-aware rollback discover B and repeat A, reconcile both positive
    /// deltas exactly once, and survive and repair a pre-collectibles destructive
    /// rewrite.
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

        let step = |prev: &mut BTreeMap<&'static str, i64>,
                    spec: &mut BTreeMap<&'static str, i64>,
                    after: BTreeMap<&'static str, i64>,
                    action: &str,
                    what: &str| {
            let (ok, why) =
                verify::validate_transition_tiered(&model, &[], prev, &after, Some(action), what);
            assert!(ok, "model rejected {what}: {why}");
            assert!(model.fire(action, spec), "{what}");
            assert_eq!(&after, spec, "{what}");
            *prev = after;
        };
        let rows = |path: &Path| KittyLog::read_with_sidecar(path).collectibles;
        let total = |rows: &[KittyCollectible]| rows.iter().map(|row| row.count).sum::<u64>();

        let path = tmp("sidecar-conformance");
        let mut prev = project(&path, 0, 0);
        let mut spec = model.init_state();
        assert_eq!(prev, spec);
        let sample = sighting(7000);
        let look = sample.look.normalized();
        let record = |log: &mut KittyLog, variant: CatGlyphId, at: &str| {
            log.record_collectible(
                glyph_key(variant),
                KittyLook { variant, ..look },
                &sample,
                Lexicon::builtin(),
                at,
            )
        };
        let mut discovery = KittyLog::default();
        assert!(record(&mut discovery, look.variant, "2026-07-09T12:00:00Z"));
        KittyLog::flush_merge(&path, &discovery);
        step(
            &mut prev,
            &mut spec,
            project(&path, 1, 1),
            "Discover",
            "discover A",
        );

        let mut rollback_view = KittyLog::read(&path);
        assert!(record(
            &mut rollback_view,
            CatGlyphId::S101,
            "2026-07-09T12:00:01Z"
        ));
        rollback_view.write(&path);
        step(
            &mut prev,
            &mut spec,
            project(&path, 2, 2),
            "OldDiscover",
            "rollback discovers B",
        );
        assert_eq!(rows(&path).len(), 2, "A+B survive re-upgrade");
        step(
            &mut prev,
            &mut spec,
            project(&path, 2, 2),
            "Reconcile",
            "re-upgrade unions B",
        );

        let mut rollback_repeat = KittyLog::read(&path);
        assert!(!record(
            &mut rollback_repeat,
            look.variant,
            "2026-07-09T12:00:02Z"
        ));
        rollback_repeat.write(&path);
        step(
            &mut prev,
            &mut spec,
            project(&path, 2, 3),
            "OldRepeat",
            "rollback repeats A",
        );
        let final_rows = rows(&path);
        assert_eq!(total(&final_rows), 3);
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
            total(&rows(&path)),
            3,
            "a second re-upgrade load is idempotent"
        );
        step(
            &mut prev,
            &mut spec,
            project(&path, 2, 3),
            "Reconcile",
            "re-upgrade imports one repeat",
        );

        write_precollectibles_rewrite(&path, &KittyLog::read(&path));
        step(
            &mut prev,
            &mut spec,
            project(&path, 2, 3),
            "OldRewrite",
            "pre-collectibles rewrite",
        );
        assert!(model.check_invariant("NoUnlockRollback", &prev));
        assert!(model.check_invariant("NoCountRollback", &prev));

        let mut empty_rollback = KittyLog::read(&path);
        assert!(empty_rollback.collectibles.is_empty());
        assert!(record(
            &mut empty_rollback,
            CatGlyphId::S102,
            "2026-07-09T12:00:03Z"
        ));
        empty_rollback.write(&path);
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 4),
            "OldDiscover",
            "rollback discovers C",
        );
        let reunioned = rows(&path);
        assert_eq!((reunioned.len(), total(&reunioned)), (3, 4), "A+B union C");
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 4),
            "Reconcile",
            "re-upgrade unions C",
        );
        write_precollectibles_rewrite(&path, &KittyLog::read(&path));
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 4),
            "OldRewrite",
            "second rewrite",
        );

        let mut reset_repeat = KittyLog::read(&path);
        assert!(reset_repeat.collectibles.is_empty());
        assert!(record(
            &mut reset_repeat,
            look.variant,
            "2026-07-09T12:00:04Z"
        ));
        reset_repeat.write(&path);
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 5),
            "OldRepeatReset",
            "repeat A after erasure",
        );
        let reset_rows = rows(&path);
        assert_eq!((reset_rows.len(), total(&reset_rows)), (3, 5));
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 5),
            "Reconcile",
            "re-upgrade imports the reset repeat",
        );
        write_precollectibles_rewrite(&path, &KittyLog::read(&path));
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 5),
            "OldRewrite",
            "final rewrite",
        );
        let restored = rows(&path);
        assert_eq!((restored.len(), total(&restored)), (3, 5));
        step(
            &mut prev,
            &mut spec,
            project(&path, 3, 5),
            "RestoreMirror",
            "re-upgrade restores the mirror",
        );

        let buggy_path = tmp("sidecar-negative");
        let mut embedded_only = KittyLog::default();
        assert!(record(
            &mut embedded_only,
            look.variant,
            "2026-07-09T12:00:00Z"
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

        let late = delta(CatGlyphId::SpecSleeping, 12, "2026-07-02T00:00:00Z");
        let early = delta(CatGlyphId::SpecWitch, 3, "2026-07-01T00:00:00Z");
        let flush = |delta: &KittyLog| {
            let result = flush_pending_at_exit(&p, delta);
            assert!(
                result.persisted,
                "the semantic ordering check requires an accepted durable flush"
            );
        };
        flush(&late);
        flush(&early);

        let same_late = delta(CatGlyphId::SpecYarn, 14, "2026-07-03T00:00:00Z");
        let same_early = delta(CatGlyphId::SpecYarn, 1, "2026-06-30T00:00:00Z");
        flush(&same_late);
        flush(&same_early);

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

        let mut host = KittyLogHost::load(Some(p.clone()));
        host.await_initial_load();
        assert_eq!(
            host.favourite_look(),
            None,
            "an unpinned ledger claims no companion on startup — the launch \
             kitty rides (the startup import no longer elects the latest \
             discovery; owner ruling, 2026-08-17)"
        );
        host.flush_exit();
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A sighting carrying an explicit look, as the favourite path mints it (no
    /// matched surface, so no displayed-trait bits).
    fn look_sighting(ident: u64, look: KittyLook) -> KittySighting {
        KittySighting {
            look,
            traits: 0,
            ..sighting(ident)
        }
    }

    /// A head glyph with a chosen coat, to tell two compositions of one key apart.
    fn coated(variant: CatGlyphId, coat: u8) -> KittyLook {
        KittyLook {
            variant,
            coat,
            ..KittyLook::default()
        }
        .normalized()
    }

    /// A wear reaches the delta (with count 0), since the flush sends `delta`, not
    /// `mem`, and the row a wear stamps is usually one an earlier run collected.
    #[test]
    fn a_wear_reaches_the_delta_or_it_does_not_survive_a_restart() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let now = Instant::now();
        let old = coated(CatGlyphId::S100, 3);
        let new = coated(CatGlyphId::S101, 7);

        host.observe(4, [look_sighting(11, old)], lex, now, true);
        host.observe(4, [look_sighting(12, new)], lex, now, true);
        let keys: Vec<String> = host
            .log()
            .roster()
            .iter()
            .map(|row| row.key.clone())
            .collect();
        assert_eq!(keys.len(), 2, "fixture: two heads collected");
        host.take_delta_for_test();
        assert!(
            host.pending().roster().is_empty(),
            "fixture: the delta is drained, as it is after any flush"
        );

        let worn = host
            .wear(&keys[0], now, true)
            .expect("a collected cat can be worn");
        assert_eq!(host.favourite_look(), Some(worn), "it is on the cursor");

        let carried = host
            .pending()
            .roster()
            .iter()
            .find(|row| row.key == keys[0])
            .expect("the worn row reached the delta");
        assert!(
            !carried.favourite.is_empty(),
            "the delta row carries the pin stamp, which is the whole point"
        );
        assert_eq!(carried.count, 0, "a wear is not an observation");
    }

    /// The logical-clock tick `n` as the RFC3339 stamp a wear writes. Fixed
    /// width, so lexicographic order is the clock's order — exactly the
    /// property `max_ts` rests on.
    fn pin_stamp(n: i64) -> String {
        format!("2026-09-25T00:00:{n:02}Z")
    }

    /// The tick a replica's row for `key` is pinned at (`0` = never pinned).
    fn pin_tick(log: &KittyLog, key: &str) -> i64 {
        let row = log
            .roster()
            .iter()
            .find(|row| row.key == key)
            .expect("both cats are collected on both replicas");
        if row.favourite.is_empty() {
            return 0;
        }
        row.favourite
            .strip_prefix("2026-09-25T00:00:")
            .and_then(|rest| rest.strip_suffix('Z'))
            .and_then(|tick| tick.parse().ok())
            .unwrap_or_else(|| panic!("a stamp this test did not write: {}", row.favourite))
    }

    /// One replica merging another's ledger in: the real pure read-merge core
    /// (`merge_from`, which `flush_merge` runs), or a fault injected after it.
    type PinFlush = fn(&mut KittyLog, &KittyLog);

    fn join_flush(dst: &mut KittyLog, src: &KittyLog) {
        dst.merge_from(src);
    }

    /// The model's `Buggy = 1` merge on the real ledger: the real merge, then every
    /// favourite stamp taken from the incoming copy (last writer by arrival).
    fn take_flush(dst: &mut KittyLog, src: &KittyLog) {
        dst.merge_from(src);
        for row in &mut dst.collectibles {
            row.favourite = src
                .collectibles
                .iter()
                .find(|incoming| incoming.key == row.key)
                .map(|incoming| incoming.favourite.clone())
                .unwrap_or_default();
        }
    }

    /// Walk `model`'s reachable space on two real replicas. A wear is the real
    /// [`KittyLog::wear`] at the next clock stamp; a flush is `flush` of one replica
    /// into the other.
    fn pin_merge_walk(model: &Model, flush: PinFlush) -> Result<(usize, usize), String> {
        let lex = Lexicon::builtin();
        let mut replica = KittyLog::default();
        for (ident, look) in [
            (11, coated(CatGlyphId::S100, 3)),
            (12, coated(CatGlyphId::S101, 7)),
        ] {
            replica.record(&look_sighting(ident, look), lex, "2026-09-24T00:00:00Z");
        }
        let keys: Vec<String> = replica.roster().iter().map(|row| row.key.clone()).collect();
        assert_eq!(keys.len(), 2, "fixture: two cats collected");
        let (one, two) = (keys[0].as_str(), keys[1].as_str());
        let project = |a: &KittyLog, b: &KittyLog, book: &aterm_spec::interp::State| {
            let mut state = book.clone();
            state.insert("a1", pin_tick(a, one));
            state.insert("a2", pin_tick(a, two));
            state.insert("b1", pin_tick(b, one));
            state.insert("b2", pin_tick(b, two));
            state
        };

        let init = model.init_state();
        if project(&replica, &replica, &init) != init {
            return Err(format!("the fixture does not project to {init:?}"));
        }
        let mut seen = std::collections::BTreeSet::from([init.clone()]);
        let mut queue = std::collections::VecDeque::from([(init, replica.clone(), replica)]);
        let (mut states, mut edges) = (0usize, 0usize);
        while let Some((state, a, b)) = queue.pop_front() {
            states += 1;
            for action in ["WearOneOnA", "WearTwoOnB", "FlushAToB", "FlushBToA"] {
                let expected = model.successors(action, &state);
                if expected.is_empty() {
                    continue;
                }
                let (mut a, mut b, mut book) = (a.clone(), b.clone(), state.clone());
                *book.get_mut("steps").expect("steps") += 1;
                let hi = |book: &mut aterm_spec::interp::State, hi: &'static str, pre: &str| {
                    let now = book[hi].max(state[pre]);
                    book.insert(hi, now);
                };
                match action {
                    "WearOneOnA" | "WearTwoOnB" => {
                        let stamp = pin_stamp(state["clock"] + 1);
                        let worn = if action == "WearOneOnA" {
                            a.wear(one, &stamp)
                        } else {
                            hi(&mut book, "b2_hi", "b2");
                            b.wear(two, &stamp)
                        };
                        worn.expect("a collected cat can be worn");
                        *book.get_mut("clock").expect("clock") += 1;
                        book.insert("ab", 0);
                    }
                    "FlushAToB" => {
                        hi(&mut book, "b1_hi", "b1");
                        hi(&mut book, "b2_hi", "b2");
                        flush(&mut b, &a);
                        book.insert("ab", 1);
                    }
                    _ => flush(&mut a, &b),
                }
                let after = project(&a, &b, &book);
                if expected != vec![after.clone()] {
                    return Err(format!(
                        "{}: real {action} took {state:?} to {after:?}, the model's {action} \
                         to {expected:?}",
                        model.name
                    ));
                }
                edges += 1;
                if seen.insert(after.clone()) {
                    queue.push_back((after, a, b));
                }
            }
        }
        Ok((states, edges))
    }

    /// Tier-1 conformance for `KittyPinMerge`
    /// (`aterm_models_core::kitty_pin_merge_model`): wearing a cat and folding
    /// one replica's ledger into another, on two REAL `KittyLog`s, is exactly
    /// the model's join — over the model's whole reachable space, both flush
    /// directions in every order.
    #[test]
    fn wear_and_merge_conform_to_the_kitty_pin_merge_model() {
        let model = kitty_pin_merge_model();
        let (states, edges) =
            pin_merge_walk(&model, join_flush).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(
            Ok(states),
            aterm_spec::interp::bmc(&model),
            "the real replicas must reach every state the model can"
        );
        assert!(edges > states, "every state is left more than one way");

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let rejected = pin_merge_walk(&model, take_flush)
            .expect_err("the healthy model must reject an assignment merge");
        assert!(
            rejected.contains("the model's FlushAToB")
                || rejected.contains("the model's FlushBToA"),
            "the assignment merge must be rejected at a flush: {rejected}"
        );
        pin_merge_walk(&buggy, take_flush)
            .unwrap_or_else(|why| panic!("the buggy model must describe the take: {why}"));
        pin_merge_walk(&buggy, join_flush)
            .expect_err("the shipped join must not conform to the assignment merge");
    }

    /// The explicit pin moves the composition of an already-collected head and
    /// becomes the companion (routed through `observe`, it would no-op: `record`
    /// reports "new" only for an unseen glyph key).
    #[test]
    fn favourite_pins_an_already_collected_head() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let stumbled = coated(CatGlyphId::S100, 3);
        let pinned = coated(CatGlyphId::S100, 9);
        let now = Instant::now();

        host.observe(4, [look_sighting(11, stumbled)], lex, now, true);
        assert_eq!(
            host.favourite_look(),
            None,
            "a stumbled discovery collects but claims nothing"
        );

        host.favourite(&look_sighting(12, pinned), lex, now, true);

        assert_eq!(
            host.favourite_look(),
            Some(pinned),
            "the pin wins even though this glyph was already collected"
        );
        let rows: Vec<_> = host
            .log()
            .collectibles
            .iter()
            .filter(|item| item.key == glyph_key(CatGlyphId::S100))
            .collect();
        assert_eq!(rows.len(), 1, "a favourite re-stamps, it never duplicates");
        assert_eq!(rows[0].coat, 9, "the pin takes composition ownership");
        assert!(!rows[0].favourite.is_empty(), "and is stamped durably");
    }

    /// A later automatic discovery still reports itself but does not steal a pin.
    #[test]
    fn a_later_discovery_does_not_steal_a_pinned_favourite() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let pinned = coated(CatGlyphId::S100, 5);
        let newcomer = coated(CatGlyphId::SpecWitch, 12);
        let now = Instant::now();

        host.favourite(&look_sighting(21, pinned), lex, now, true);
        let discovery = host.observe(4, [look_sighting(22, newcomer)], lex, now, true);

        assert_eq!(
            discovery,
            Some(newcomer),
            "a genuine unlock still reports itself, so its hello still plays"
        );
        assert_eq!(
            host.favourite_look(),
            Some(pinned),
            "…but the pin is untouched"
        );
    }

    /// Words in output text never activate the companion: an ambient first-ever
    /// discovery mints its collectible row, but the companion stays put and
    /// `observe_ambient` returns nothing a call site could hand to `on_collect`.
    #[test]
    fn an_ambient_discovery_collects_without_touching_the_companion() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let newcomer = coated(CatGlyphId::SpecWitch, 12);
        let now = Instant::now();

        host.observe_ambient(4, [look_sighting(41, newcomer)], lex, now, true);

        assert_eq!(
            host.favourite_look(),
            None,
            "watching output earns no companion"
        );
        assert!(
            host.log()
                .collectibles
                .iter()
                .any(|item| item.key == glyph_key(CatGlyphId::SpecWitch)),
            "…but the collectible row still mints"
        );
        assert_eq!(host.log().sightings, 1, "and the ledger still counts");

        let pinned = coated(CatGlyphId::S100, 3);
        host.favourite(&look_sighting(51, pinned), lex, now, true);
        let printed = coated(CatGlyphId::S101, 12);
        host.observe_ambient(4, [look_sighting(52, printed)], lex, now, true);
        assert_eq!(
            host.favourite_look(),
            Some(pinned),
            "output text never re-dresses the companion"
        );
    }

    /// In both merge orders a favourite owns the composition against an earlier
    /// unfavourited discovery, without rewriting chronology.
    #[test]
    fn the_favourite_wins_the_merge_against_an_earlier_discovery() {
        let key = glyph_key(CatGlyphId::S100).to_string();
        let stumbled = KittyCollectible {
            key: key.clone(),
            variant: key,
            coat: 3,
            age: age_key(KittyLook::default().age).to_string(),
            count: 1,
            first_seen: "2026-07-01T00:00:00Z".to_string(),
            last_seen: "2026-07-01T00:00:00Z".to_string(),
            ..KittyCollectible::default()
        };
        let pinned = KittyCollectible {
            coat: 9,
            first_seen: "2026-07-05T00:00:00Z".to_string(),
            last_seen: "2026-07-05T00:00:00Z".to_string(),
            favourite: "2026-07-05T00:00:00Z".to_string(),
            ..stumbled.clone()
        };
        let merged = |first: &KittyCollectible, second: &KittyCollectible| {
            let mut log = KittyLog {
                collectibles: vec![first.clone()],
                ..KittyLog::default()
            };
            log.merge_from(&KittyLog {
                collectibles: vec![second.clone()],
                ..KittyLog::default()
            });
            log.collectibles
        };

        for rows in [merged(&stumbled, &pinned), merged(&pinned, &stumbled)] {
            assert_eq!(rows.len(), 1, "one semantic key, one row");
            assert_eq!(
                rows[0].coat, 9,
                "the favourite owns the look, not the earliest discovery"
            );
            assert_eq!(rows[0].favourite, "2026-07-05T00:00:00Z");
            assert_eq!(
                rows[0].first_seen, "2026-07-01T00:00:00Z",
                "the pin never rewrites when the cat was first met"
            );
        }
    }

    /// A pin survives restart and outranks the latest discovery.
    #[test]
    fn a_favourite_survives_the_startup_import() {
        let lex = Lexicon::builtin();
        let p = tmp("favourite-restart");
        let pinned = coated(CatGlyphId::S100, 7);
        let later = coated(CatGlyphId::S101, 2);
        assert!(glyph_key(CatGlyphId::S100) < glyph_key(CatGlyphId::S101));

        let mut host = KittyLogHost::load_with_writer_spawn(Some(p.clone()), |_| None);
        let now = Instant::now();
        host.favourite(&look_sighting(31, pinned), lex, now, true);
        host.observe(4, [look_sighting(32, later)], lex, now, true);
        assert!(
            KittyLog::flush_merge(&p, &host.delta),
            "the synchronous flush must land for the restart to mean anything"
        );

        let mut restarted = KittyLogHost::load(Some(p.clone()));
        restarted.await_initial_load();
        assert_eq!(
            restarted.favourite_look(),
            Some(pinned),
            "the pin outranks the chronologically latest discovery on restart"
        );
        restarted.flush_exit();
        let _ = std::fs::remove_dir_all(p.parent().expect("scratch parent"));
    }

    /// The favourite path bypasses the recount ring, shown with a reused ident that
    /// `observe` would drop for `RING_TTL`.
    #[test]
    fn a_favourite_is_never_absorbed_by_the_recount_ring() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let look = coated(CatGlyphId::S100, 4);
        let now = Instant::now();
        let before = host.revision();

        host.favourite(&look_sighting(77, look), lex, now, true);
        host.favourite(&look_sighting(77, look), lex, now, true);

        assert_eq!(
            host.log().sightings,
            2,
            "the cat appeared again because the user asked again"
        );
        assert_eq!(
            host.revision(),
            before.wrapping_add(2),
            "each press is its own change stamp"
        );
    }

    /// `[sparkle_words.feline] log = false` closes the durable tier only: the
    /// session keeps the cat it asked for, nothing is written.
    #[test]
    fn favourite_with_recording_off_is_memory_only() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let look = coated(CatGlyphId::S100, 6);

        host.favourite(&look_sighting(88, look), lex, Instant::now(), false);

        assert_eq!(
            host.favourite_look(),
            Some(look),
            "the pick still holds for this session"
        );
        assert_eq!(host.log().sightings, 0, "the ledger tier stayed closed");
        assert!(
            host.log().collectibles.is_empty(),
            "and no roster row was minted"
        );
        assert!(
            host.is_favourite(look),
            "the palette checkmark reads the same pin the verdict wears — even \
             a memory-only pin the roster never sees"
        );
    }

    /// A pin taken in this process before the startup import lands survives the
    /// import's election, shown with a memory-only pin (`log = false`, no roster
    /// row); the control shows the import still elects a disk pin otherwise.
    #[test]
    fn a_session_pin_is_not_clobbered_by_a_late_startup_import() {
        let lex = Lexicon::builtin();
        let now = Instant::now();
        let session_pick = coated(CatGlyphId::S100, 6);
        let disk_pin = coated(CatGlyphId::S101, 2);
        let mut on_disk = KittyLog::default();
        let _ = on_disk.record(&look_sighting(1, disk_pin), lex, "2026-08-01T00:00:00Z");
        on_disk.favourite_collectible(disk_pin, "2026-08-01T00:00:00Z");
        assert_eq!(
            on_disk.favourite_look(),
            Some(disk_pin),
            "fixture: the disk carries a pin"
        );

        let mut untouched = KittyLogHost::in_memory();
        untouched.absorb_initial(on_disk.clone());
        assert_eq!(untouched.favourite_look(), Some(disk_pin));

        let mut host = KittyLogHost::in_memory();
        host.favourite(&look_sighting(88, session_pick), lex, now, false);
        host.absorb_initial(on_disk);
        assert_eq!(
            host.favourite_look(),
            Some(session_pick),
            "the session's explicit pick outranks the imported pin"
        );
        assert!(host.is_favourite(session_pick), "and the checkmark agrees");
    }

    /// After a failed construction-time pre-arm, `observe` never retries a spawn from
    /// the render path; the delta stays resident and `flush_exit` does no
    /// synchronous filesystem fallback.
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
        assert!(
            !ledger.exists(),
            "a failed pre-arm degrades observability instead of doing exit IO"
        );
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    #[test]
    fn disconnected_prearmed_writer_keeps_delta_in_memory_without_exit_io() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer-disconnected");
        let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |_| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
            drop(rx);
            drop(exit_rx);
            let handle = std::thread::spawn(KittyLog::default);
            Some(KittyWriter {
                tx,
                exit_tx,
                initial: None,
                handle,
            })
        });

        host.observe(7, [sighting(11)], lex, Instant::now(), true);
        assert!(
            host.writer.is_none(),
            "a disconnected render-side sender is detached, never joined"
        );
        assert_eq!(host.delta.sightings, 1, "the rejected batch stays owned");
        host.flush_exit();
        assert!(
            !ledger.exists(),
            "a failed pre-arm must not fall back to synchronous exit IO"
        );
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    #[test]
    fn exit_detaches_a_filesystem_stalled_worker_at_the_deadline() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer-stalled-exit");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), |_| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel(1);
            let handle = std::thread::spawn(move || {
                let owned_batch = rx.recv().unwrap_or_default();
                let _ = release_rx.recv();
                drop(exit_rx);
                owned_batch
            });
            Some(KittyWriter {
                tx,
                exit_tx,
                initial: None,
                handle,
            })
        });
        host.observe(7, [sighting(11)], lex, Instant::now(), true);

        host.flush_exit();

        let model = kitty_flush_worker_model();
        let project = |stalled, deadline, detached| {
            [
                ("accepted", 1),
                ("normal_lane", 0),
                ("host_tail", 0),
                ("exit_lane", 0),
                ("pending", 1),
                ("persisted", 0),
                ("exiting", 1),
                ("retries", 0),
                ("joined", 0),
                ("stalled", stalled),
                ("deadline", deadline),
                ("detached", detached),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
        };
        let before_stall = project(0, 0, 0);
        let stalled = project(1, 0, 0);
        let tick_one = project(1, 1, 0);
        let tick_two = project(1, 2, 0);
        let detached = project(1, 2, 1);
        for (action, before, after) in [
            ("StallIo", &before_stall, &stalled),
            ("TickDeadline", &stalled, &tick_one),
            ("TickDeadline", &tick_one, &tick_two),
            ("Detach", &tick_two, &detached),
        ] {
            let (ok, why) = verify::validate_transition_tiered(
                &model,
                &[],
                before,
                after,
                Some(action),
                "real Kitty Log stalled-IO exit deadline",
            );
            assert!(ok, "model rejected {action}: {why}");
        }
        let early_detach = project(1, 0, 1);
        assert!(
            !model.check_invariant("DetachedOnlyAtDeadline", &early_detach),
            "negative control: detaching before the deadline must be rejected"
        );
        let _ = release_tx.send(());
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    #[test]
    fn contended_batch_retries_after_lock_release_without_a_new_sighting() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer-lock-retry");
        let sidecar = collectibles_path(&ledger);
        let locks = lock_pair(&ledger, &sidecar).expect("hold both ledger locks");
        let mut host = KittyLogHost::load(Some(ledger.clone()));
        host.observe(7, [sighting(11)], lex, Instant::now(), true);
        drop(locks);
        let deadline = Instant::now() + FLUSH_RETRY_DELAY + aterm_spec::wait::MUST_HAPPEN;
        while !ledger.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ledger.exists(),
            "the pending worker batch retries without another sighting"
        );
        host.flush_exit();
        assert_eq!(KittyLog::read_with_sidecar(&ledger).sightings, 1);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The ledger once a detached exit writer has had its chance to land.
    fn read_once_persisted(ledger: &Path, sightings: u64) -> KittyLog {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while KittyLog::read(ledger).sightings < sightings && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        KittyLog::read_with_sidecar(ledger)
    }

    #[test]
    fn full_normal_queue_cannot_drop_the_dedicated_exit_tail() {
        let ledger = tmp("writer-full-normal-exit-tail");
        let lex = Lexicon::builtin();
        let (pending_tx, pending_rx) = std::sync::mpsc::channel();
        let mut host = KittyLogHost::load_with_writer_spawn(Some(ledger.clone()), move |path| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
            let (exit_tx, exit_rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
            let handle = std::thread::spawn(move || {
                let Ok(mut pending) = rx.recv() else {
                    return KittyLog::default();
                };
                let _ = pending_tx.send(());
                let Ok(tail) = exit_rx.recv() else {
                    return pending;
                };
                let Ok(normal) = rx.recv() else {
                    pending.merge_from(&tail);
                    return pending;
                };
                pending.merge_from(&normal);
                pending.merge_from(&tail);
                if KittyLog::flush_merge(&path, &pending) {
                    KittyLog::default()
                } else {
                    pending
                }
            });
            Some(KittyWriter {
                tx,
                exit_tx,
                initial: None,
                handle,
            })
        });
        let t0 = Instant::now();
        host.observe(7, [sighting(11)], lex, t0, true);
        assert!(host.delta.is_empty(), "the normal queue accepted batch one");
        pending_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the worker owns batch one before the queue is refilled");
        host.observe(8, [sighting(22)], lex, t0 + FLUSH_DEBOUNCE, true);
        assert!(host.delta.is_empty(), "the normal queue accepted batch two");
        host.observe(
            9,
            [sighting(33)],
            lex,
            t0 + FLUSH_DEBOUNCE + Duration::from_secs(1),
            true,
        );
        assert_eq!(
            host.delta.sightings, 1,
            "batch three remains host-owned while pending and normal are occupied"
        );

        let model = kitty_flush_worker_model();
        let project =
            |accepted, normal_lane, host_tail, exit_lane, pending, persisted, exiting, joined| {
                [
                    ("accepted", accepted),
                    ("normal_lane", normal_lane),
                    ("host_tail", host_tail),
                    ("exit_lane", exit_lane),
                    ("pending", pending),
                    ("persisted", persisted),
                    ("exiting", exiting),
                    ("retries", 0),
                    ("joined", joined),
                    ("stalled", 0),
                    ("deadline", 0),
                    ("detached", 0),
                ]
                .into_iter()
                .collect::<BTreeMap<_, _>>()
            };
        let validate = |action: &str,
                        before: &BTreeMap<&'static str, i64>,
                        after: &BTreeMap<&'static str, i64>| {
            let (ok, why) = verify::validate_transition_tiered(
                &model,
                &[],
                before,
                after,
                Some(action),
                "Kitty Log dedicated exit-tail delivery",
            );
            assert!(ok, "model rejected {action}: {why}");
        };
        let initial = model.init_state();
        let queued_one = project(1, 1, 0, 0, 0, 0, 0, 0);
        validate("QueueNormal", &initial, &queued_one);
        let pending_one = project(1, 0, 0, 0, 1, 0, 0, 0);
        validate("DrainNormal", &queued_one, &pending_one);
        let queued_two = project(2, 1, 0, 0, 1, 0, 0, 0);
        validate("QueueNormal", &pending_one, &queued_two);
        let retained = project(3, 1, 1, 0, 1, 0, 0, 0);
        validate("RetainTailOnFull", &queued_two, &retained);
        let exiting = project(3, 1, 1, 0, 1, 0, 1, 0);
        validate("BeginExit", &retained, &exiting);
        let offered = project(3, 1, 0, 1, 1, 0, 1, 0);
        validate("OfferTail", &exiting, &offered);
        let normal_pending = project(3, 0, 0, 1, 2, 0, 1, 0);
        validate("DrainNormal", &offered, &normal_pending);
        let both_pending = project(3, 0, 0, 0, 3, 0, 1, 0);
        validate("AbsorbTail", &normal_pending, &both_pending);

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut dropped_tail = buggy.init_state();
        for action in [
            "QueueNormal",
            "DrainNormal",
            "QueueNormal",
            "RetainTailOnFull",
            "BeginExit",
            "OfferTail",
        ] {
            assert!(buggy.fire(action, &mut dropped_tail), "buggy {action}");
        }
        assert_eq!(dropped_tail["accepted"], 3);
        assert_eq!(dropped_tail["pending"], 1);
        assert_eq!(dropped_tail["normal_lane"], 1);
        assert!(
            !buggy.check_invariant("AcceptedConserved", &dropped_tail),
            "negative control: the retired saturated one-lane exit send must lose ownership"
        );

        host.flush_exit();
        assert!(
            host.delta.is_empty(),
            "successful shutdown transfers and persists every owned batch"
        );
        let saved = read_once_persisted(&ledger, 3);
        assert_eq!(saved.sightings, 3);
        assert_eq!(saved.collectibles.len(), 1);
        assert_eq!(saved.collectibles[0].count, 3);
        let persisted = project(3, 0, 0, 0, 0, 3, 1, 0);
        validate("Flush", &both_pending, &persisted);
        let joined = project(3, 0, 0, 0, 0, 3, 1, 1);
        validate("Join", &persisted, &joined);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The long-lived writer end to end: a real-path host pre-arms one worker,
    /// `observe` hands it the delta without spawning, a debounced second sighting
    /// accumulates, and `flush_exit` sends the tail over its shutdown lane and joins
    /// the healthy worker within its deadline, so every count is durable.
    #[test]
    fn background_writer_persists_and_drains_on_exit() {
        let lex = Lexicon::builtin();
        let ledger = tmp("writer");
        let mut host = KittyLogHost::load(Some(ledger.clone()));
        let writer_id = host
            .writer
            .as_ref()
            .expect("persistent host pre-arms its writer")
            .handle
            .thread()
            .id();
        let t0 = Instant::now();
        host.observe(7, [sighting(11)], lex, t0, true);
        assert_eq!(
            host.writer
                .as_ref()
                .map(|writer| writer.handle.thread().id()),
            Some(writer_id),
            "observe reuses the pre-armed worker"
        );
        host.observe(8, [sighting(22)], lex, t0 + Duration::from_secs(1), true);
        host.flush_exit();
        let saved = read_once_persisted(&ledger, 2);
        assert_eq!(
            saved.sightings, 2,
            "both sightings persisted across the background writer + exit drain"
        );
        assert_eq!(saved.collectibles.len(), 1);
        assert_eq!(saved.collectibles[0].count, 2);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The (session, ident) ring: a shared-session second window and a vim
    /// round-trip recount collapse to one count; a different session or an expired
    /// entry counts again; `enabled = false` drains and drops.
    #[test]
    fn dedupe_ring_absorbs_recounts() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let t0 = Instant::now();
        host.observe(7, [sighting(42)], lex, t0, true);
        assert_eq!(host.log().sightings, 1);
        let rev = host.revision();
        host.observe(7, [sighting(42)], lex, t0, true);
        host.observe(7, [sighting(42)], lex, t0 + Duration::from_secs(60), true);
        assert_eq!(host.log().sightings, 1, "ring absorbs the recounts");
        assert_eq!(host.revision(), rev, "no revision bump when suppressed");
        host.observe(8, [sighting(42)], lex, t0, true);
        assert_eq!(host.log().sightings, 2);
        host.observe(
            7,
            [sighting(42)],
            lex,
            t0 + Duration::from_secs(60) + RING_TTL + Duration::from_secs(1),
            true,
        );
        assert_eq!(host.log().sightings, 3, "an expired episode recounts");
        let rev = host.revision();
        host.observe(9, [sighting(1)], lex, t0, false);
        assert_eq!(host.log().sightings, 3, "log=false drains-and-drops");
        assert_eq!(host.revision(), rev);
    }

    /// Tier-1 conformance for the semantic-key set: every generated key drives one
    /// `Unlock`, the first repeat `Repeat`, and an invalid key no transition.
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
}
