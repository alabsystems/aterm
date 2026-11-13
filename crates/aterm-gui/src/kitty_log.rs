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

const KITTY_LOG_FILE: &str = "kitty-log.toml";
const KITTY_COLLECTIBLES_FILE: &str = "kitty-collectibles.toml";
const MAX_KITTY_LEDGER_ROWS: usize =
    KittyType::ALL.len() * KittyMagic::ALL.len() * LangSet::CAPACITY + (2 * GLYPH_IDS.len());
const MAX_KITTY_LEDGER_BYTES: usize = MAX_KITTY_LEDGER_ROWS * 2 * 1024;

fn collectibles_path(legacy_path: &Path) -> PathBuf {
    legacy_path.with_file_name(KITTY_COLLECTIBLES_FILE)
}

fn read_kitty_ledger_text(path: &Path) -> Option<String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(path, MAX_KITTY_LEDGER_BYTES).ok()
}

const RING_SLOTS: usize = 256;
const RING_TTL: Duration = Duration::from_secs(600);
const FLUSH_DEBOUNCE: Duration = Duration::from_secs(30);
const FLUSH_RETRY_DELAY: Duration = Duration::from_secs(1);
const EXIT_LOCK_RETRIES: usize = 4;
const EXIT_LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);
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

fn min_ts(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => if a <= b { a } else { b }.to_string(),
    }
}

fn max_ts(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => if a >= b { a } else { b }.to_string(),
    }
}

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

    fn normalize_collectibles(&mut self) {
        let persisted = std::mem::take(&mut self.collectibles);
        self.collectibles
            .reserve(persisted.len().min(GLYPH_IDS.len()));
        for candidate in persisted {
            self.merge_collectible(candidate);
        }
        self.collectibles.sort_by(collectible_order);
    }

    fn merge_collectible(&mut self, mut candidate: KittyCollectible) {
        self.merge_collectible_with(&mut candidate, true);
    }

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

    fn wear(&mut self, key: &str, now: &str) -> Option<KittyCollectible> {
        let item = self.collectibles.iter_mut().find(|item| item.key == key)?;
        item.favourite = max_ts(&item.favourite, now);
        item.look()?;
        Some(item.clone())
    }

    fn adopt_wear(&mut self, row: &KittyCollectible) {
        let mut carried = row.clone();
        carried.count = 0;
        self.merge_collectible(carried);
    }

    /// The roster, in collection-book order: the menu of [`Self::wear`]'s `key`.
    pub(crate) fn roster(&self) -> &[KittyCollectible] {
        &self.collectibles
    }

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

    /// Flush `delta` into the ledger at `path`: read-merge-write under the sibling `.lock`.
    pub(crate) fn flush_merge(path: &Path, delta: &KittyLog) -> bool {
        if delta.is_empty() {
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

    fn write(&self, path: &Path) -> bool {
        atomic_write_toml(path, self)
    }
}

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
    let regular_directory = metadata.file_type().is_dir() && !metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        regular_directory && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        regular_directory
    }
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
        return false;
    }
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let write_result = opts
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

fn ledger_destination_is_safe(path: &Path) -> bool {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let regular_file = metadata.file_type().is_file() && !metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        regular_file && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        regular_file
    }
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

fn try_lock(path: &Path) -> Option<HeldAdvisoryLock> {
    let lock_path = path.with_extension("toml.lock");
    let held = HeldAdvisoryLock::try_lock(open_lock(&lock_path).ok()?).ok()?;
    #[cfg(all(test, unix))]
    crate::parked_fork::fork_if_armed();
    Some(held)
}

fn lock_pair(
    legacy_path: &Path,
    sidecar_path: &Path,
) -> Option<(HeldAdvisoryLock, HeldAdvisoryLock)> {
    let legacy = try_lock(legacy_path)?;
    let sidecar = try_lock(sidecar_path)?;
    Some((legacy, sidecar))
}

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
                continue;
            }
            if self.ring_note(session, s.ident, now) {
                continue;
            }
            let stamp = now_rfc3339();
            if self.mem.record(&s, lexicon, &stamp) && source == SightingSource::Typed {
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
            return;
        }
        let Some(writer) = self.writer.as_ref() else {
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
                self.delta.merge_from(&unpersisted);
            }
        }
    }
}

#[cfg(test)]
#[path = "kitty_log_tests.rs"]
mod tests;
