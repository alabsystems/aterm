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
//! second aterm's sightings away. A contended lock is never waited on: the
//! worker retains and coalesces that delta, then makes a finite best-effort
//! retry at exit so another process cannot hang quit. The crash-loss window is
//! the deltas since the last flush (≤ the 30 s debounce, plus a permanently
//! contended exit or a hard kill) — documented, accepted for observability.
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
//! Ledger admission is also fail-open but finite: both TOML files must be
//! same-handle regular, non-link UTF-8 files no larger than
//! [`MAX_KITTY_LEDGER_BYTES`]. A FIFO, link/reparse point, device, oversized
//! file, or malformed TOML is treated exactly like an absent ledger. Flushes
//! likewise refuse to replace a non-regular or linked destination.
//!
//! [`KittyLogHost`] is the App-side state: the in-memory totals the settings
//! page renders (settings-open does NO synchronous IO), the unflushed delta,
//! the `(session, ident)` dedupe ring (multi-window shared sessions count a
//! cat once; a vim round-trip's grace-expiry recount is absorbed for
//! [`RING_TTL`]), and the drain-time debounce that hands the delta to a
//! long-lived background writer (the `config_watcher` thread precedent — no
//! filesystem work on the render thread). Startup admission is performed by
//! that worker and imported through a nonblocking one-shot poll.

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

/// The ledger's filename, a sibling of `aterm.toml` (see the module doc for
/// why it must never live INSIDE the user's config file).
const KITTY_LOG_FILE: &str = "kitty-log.toml";

/// Authoritative collectible identities and the embedded-replica baseline are
/// isolated from the legacy ledger so a destructive rollback cannot drop them.
const KITTY_COLLECTIBLES_FILE: &str = "kitty-collectibles.toml";

/// Maximum accepted and emitted size of either Kitty Log TOML file.
///
/// The semantic schema has at most every shown type × every magic bucket ×
/// every `LangSet` slot, plus two generated-roster copies in the sidecar
/// (`collectibles` and `legacy_mirror`). Budgeting a generous 2 KiB for every
/// possible row ties the ceiling to those source-of-truth registries without
/// making ordinary roster growth an accidental compatibility break. It also
/// makes startup parsing and the background read→merge→write path finite
/// under hostile filesystem input. Files outside the admission envelope are
/// observability failures and therefore read as the documented empty/default
/// state.
const MAX_KITTY_LEDGER_ROWS: usize =
    KittyType::ALL.len() * KittyMagic::ALL.len() * LangSet::CAPACITY + (2 * GLYPH_IDS.len());
const MAX_KITTY_LEDGER_BYTES: usize = MAX_KITTY_LEDGER_ROWS * 2 * 1024;

fn collectibles_path(legacy_path: &Path) -> PathBuf {
    legacy_path.with_file_name(KITTY_COLLECTIBLES_FILE)
}

/// Purpose-specific admission boundary shared by the legacy ledger and its
/// sidecar. The effects helper opens once, uses non-blocking/no-follow flags on
/// Unix (and refuses Windows reparse points), verifies that same handle is a
/// regular file, then reads at most `MAX + 1` bytes before requiring UTF-8.
fn read_kitty_ledger_text(path: &Path) -> Option<String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(path, MAX_KITTY_LEDGER_BYTES).ok()
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
/// A retained batch retries after transient lock contention even when no new
/// sighting arrives. Empty workers still block indefinitely on `recv`, so this
/// timer has exactly zero idle cost.
const FLUSH_RETRY_DELAY: Duration = Duration::from_secs(1);

/// A contended best-effort ledger must never turn application quit into an
/// unbounded join. The worker keeps the coalesced delta for later deliveries,
/// then makes this small, finite retry budget after the sender closes.
const EXIT_LOCK_RETRIES: usize = 4;
const EXIT_LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);
/// Main-thread quit waits only this long for the best-effort ledger worker.
/// Regular-file operations can stall indefinitely on a dead network mount, so
/// the worker is detached after this deadline and process teardown wins over
/// observability durability.
const EXIT_JOIN_BUDGET: Duration = Duration::from_millis(100);
const EXIT_JOIN_POLL: Duration = Duration::from_millis(1);

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
    /// RFC3339 UTC of the moment the user FAVOURITED this cat — empty for an
    /// ordinary automatic discovery (owner: "if somebody really likes that
    /// kitty it goes into the kitty registry"). The greatest stamp in the
    /// roster elects the companion, so an explicit pick survives restart and
    /// later discoveries; the stamp also transfers composition ownership (see
    /// [`collectible_owns_look`]), because the look the user pinned is the one
    /// they liked, not the one first stumbled upon. Election by MAX is
    /// commutative, so it merges across processes exactly like `last_seen`.
    /// A pre-favourite rollback rewriting the sidecar drops the pin (never the
    /// unlock) and the companion falls back to the latest-discovery rule.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) favourite: String,
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

/// Whether `candidate`'s composition wins over `current`'s for the same
/// semantic key. An explicit favourite wins outright; otherwise the earlier
/// first-discovery composition owns the row. Equal or missing timestamps use
/// the serialized look tuple as a deterministic tie break, making duplicate
/// normalization independent of flush order.
fn collectible_owns_look(candidate: &KittyCollectible, current: &KittyCollectible) -> bool {
    // An explicit favourite outranks the automatic first-discovery record, and
    // between two favourites the LATER one is the user's current pick. Only
    // unfavourited rows fall through to "earliest discovery owns the look".
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
    /// Read the ledger; absent/corrupt ⇒ empty default (fail-open, exactly
    /// like `Health::read` — under Containment the read is denied and this
    /// yields the same empty default).
    pub(crate) fn read(path: &Path) -> Self {
        let mut log: Self = read_kitty_ledger_text(path)
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
        // Lock contention is an ordinary multi-process observation, not
        // permission to race an unlocked migration. Reconcile the two bounded
        // snapshots for this process, but defer every repair write until a
        // later flush can own both locks.
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
            read_kitty_ledger_text(path).and_then(|text| toml::from_str(&text).ok())?;
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
            let candidate_owns_look = collectible_owns_look(candidate, item);
            item.count = if additive {
                item.count.saturating_add(candidate.count)
            } else {
                item.count.max(candidate.count)
            };
            item.first_seen = min_ts(&item.first_seen, &candidate.first_seen);
            item.last_seen = max_ts(&item.last_seen, &candidate.last_seen);
            // MAX, like `last_seen`: the pin is the user's LATEST pick, and a
            // commutative fold is what makes two processes converge regardless
            // of which one's flush lands first.
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

    /// Stamp an EXISTING roster row as the user's favourite and give it the
    /// pinned composition. Row CREATION stays with [`Self::record_collectible`]
    /// (which owns the roster cap): the caller records the sighting first, so a
    /// brand-new head is already present here. A missing row (invalid key, or
    /// the roster is full) fails closed — the ledger is observability, never a
    /// place to force an entry in.
    fn favourite_collectible(&mut self, look: KittyLook, now: &str) {
        let look = look.normalized();
        let key = glyph_key(look.variant);
        let Some(item) = self.collectibles.iter_mut().find(|item| item.key == key) else {
            return;
        };
        // The pinned composition replaces the first-discovery one: the cat the
        // user pointed at is the cat they liked.
        item.variant = key.to_string();
        item.accessory = look.accessory.map(glyph_key).unwrap_or("").to_string();
        item.coat = look.coat;
        item.iris = look.iris;
        item.age = age_key(look.age).to_string();
        item.favourite = max_ts(&item.favourite, now);
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
    /// delta. Best-effort everywhere: a denied or contended lock returns
    /// immediately and the background worker retains the coalesced delta for a
    /// later delivery; a denied write remains an observability failure.
    ///
    /// `true` means this delta reached the legacy ledger (or was empty).
    /// `false` means the caller may retry without double-counting it.
    pub(crate) fn flush_merge(path: &Path, delta: &KittyLog) -> bool {
        if delta.is_empty() {
            return true; // no-op skip: never rewrite (or create) the file for nothing
        }
        // The lock files are siblings of the ledgers, so first-run profiles
        // must create the config directory before attempting `open_lock`.
        // This runs only on the background writer (or in focused tests), never
        // on the render/present path. A hostile/non-directory final component
        // fails open and leaves the delta in memory.
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
            return false;
        }
        merged.merge_from(delta);
        merged.collectibles = collection.collectibles;
        merged.write(path)
    }

    /// Best-effort atomic write: create-parent, pid+seq-unique sibling temp,
    /// rename (mirrors `Health::write` + `save_prefs_edits`). Never panics.
    fn write(&self, path: &Path) -> bool {
        atomic_write_toml(path, self)
    }
}

/// Create and validate the ledger's immediate parent before lock acquisition.
/// `create_dir_all` is idempotent for an ordinary first-run config directory;
/// the post-create `symlink_metadata` check refuses a final symlink/reparse
/// point instead of treating it as Kitty Log authority.
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

    // A Kitty Log path is machine-owned, but its directory remains writable by
    // the user. Never turn a planted FIFO/link/reparse point into a regular
    // ledger as a side effect of best-effort observability.
    if !ledger_destination_is_safe(path) {
        return false;
    }
    let Ok(text) = toml::to_string(value) else {
        return false;
    };
    if text.len() > MAX_KITTY_LEDGER_BYTES {
        return false;
    }
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
    // `create_new` is atomic and refuses a pre-planted temp symlink instead of
    // following it and truncating an unrelated target.
    let write_result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    // Narrow the replacement race. If the destination changed to a hostile
    // type while TOML was serialized/staged, preserve it and discard our temp.
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
/// Missing is expected on first sighting; an existing target must itself be a
/// regular, non-link file. `symlink_metadata` intentionally inspects the final
/// directory entry rather than following it.
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
/// component. The same handle which is subsequently locked is proved regular.
#[cfg(unix)]
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // A lock file is a rendezvous, not data — never clobber its contents.
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
/// read→merge→write. Every failure, including another process owning the lock,
/// returns immediately. Held for the guard's lifetime; the kernel releases the
/// advisory lock on drop/exit.
fn try_lock(path: &Path) -> Option<std::fs::File> {
    let lock_path = path.with_extension("toml.lock");
    let file = open_lock(&lock_path).ok()?;
    file.try_lock().ok()?;
    Some(file)
}

fn lock_pair(legacy_path: &Path, sidecar_path: &Path) -> Option<(std::fs::File, std::fs::File)> {
    let legacy = try_lock(legacy_path)?;
    let sidecar = try_lock(sidecar_path)?;
    Some((legacy, sidecar))
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

// The RFC3339 UTC stamp is `aterm_types::rfc3339::format_rfc3339` — the ONE
// workspace home for the Howard-Hinnant civil-calendar math. This file used to
// carry a byte-identical copy and said so in its own doc comment.
use aterm_types::rfc3339::format_rfc3339;

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
    /// The in-memory totals (admitted startup ledger + this session's sightings).
    pub(crate) log: KittyLog,
}

impl Default for KittyLogView {
    /// The NEVER-SYNCED sentinel: `revision = u64::MAX` can never equal the
    /// host's counter (which starts at 0 and bumps once per sighting), so a
    /// freshly opened overlay always takes its first snapshot — even when the
    /// host holds only the admitted startup ledger at revision 0.
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
    /// Display totals: the asynchronously admitted startup ledger plus every
    /// sighting recorded this session (flushed or not) — what the settings
    /// page renders, memory-only, no IO on the interaction path.
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
    /// offers the final delta and waits only [`EXIT_JOIN_BUDGET`]. The worker's
    /// filesystem can stall indefinitely, so an unfinished worker is detached
    /// at that deadline and cannot wedge quit. `None` only for an in-memory
    /// host or a failed pre-arm; observability then remains memory-only.
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
    /// Dedicated one-shot exit lane. This queue is empty for the worker's
    /// entire runtime and receives at most the final debounced tail, so a full
    /// ordinary queue can never make shutdown silently discard that tail.
    exit_tx: std::sync::mpsc::SyncSender<KittyLog>,
    /// One-shot startup admission produced by this same background worker.
    /// Polling is nonblocking and the receiver is discarded after one result.
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
    // A contended batch stays owned by this worker and coalesces with later
    // deliveries. The ordinary loop ends when its sender drops at shutdown.
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
    // The exit lane is independent of the capacity-one ordinary queue and its
    // sender is dropped alongside `tx`, so this receive is finite. Merge the
    // last debounced tail before applying the existing bounded exit retry.
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
    /// Spawn the worker bound to `path`. `None` if the thread can't be spawned
    /// (best-effort, matching the old `.ok()` on spawn). The host then remains
    /// memory-only and retains later deltas rather than attempting filesystem
    /// work from the render or exit path.
    fn spawn(path: PathBuf) -> Option<Self> {
        // Depth 1: the worker can hold one in-flight batch while another queues;
        // a third arriving before the first drains coalesces back into `delta`.
        let (tx, rx) = std::sync::mpsc::sync_channel::<KittyLog>(1);
        // This second capacity-one lane is reserved exclusively for
        // `flush_exit`; it cannot be full from ordinary render-side traffic.
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
    /// Host state persisting beside the given CONFIG path (the ledger is the
    /// `kitty-log.toml` sibling). `None` — or a parentless path — degrades to
    /// in-memory-only. The worker's one startup read is fail-open (absent /
    /// corrupt / Containment-denied ⇒ empty) and never delays construction.
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
        // Pre-arm outside `observe`/present. A parked receiver consumes no CPU;
        // the worker performs both startup admission and later persistence.
        let writer = path.clone().and_then(spawn);
        Self {
            path,
            mem: KittyLog::default(),
            delta: KittyLog::default(),
            revision: 0,
            companion: None,
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

    /// The in-memory totals exposed to model-level regression tests.
    #[cfg(test)]
    pub(crate) fn log(&self) -> &KittyLog {
        &self.mem
    }

    #[cfg(test)]
    fn await_initial_load(&mut self) {
        // BLOCKS, deliberately, with no clock. This used to spin on `yield_now()`
        // to a 1s deadline and then SILENTLY FALL THROUGH — which is the dangerous
        // direction. The startup ledger is delivered by a freshly spawned thread
        // whose first act is an flock plus two file opens and two TOML parses; on a
        // loaded box that can miss 1s, and the waiter's own spin starved the very
        // worker it waited for. Callers then saw an EMPTY log and asserted against
        // it: the symlink/FIFO guards assert `(0, 0)` to prove startup did not
        // follow a planted ledger, so a regression that DID follow it still reported
        // `(0, 0)` and passed green.
        //
        // A blocking receive is finite by construction: `initial` is a
        // `sync_channel(1)` written exactly once, and the sender is dropped when the
        // worker moves on — so this resolves with either the ledger or
        // `Err(Disconnected)`. There is no window left to cross.
        let received = self
            .writer
            .as_mut()
            .and_then(|writer| writer.initial.take())
            .map(|receiver| receiver.recv());
        // A worker that went away without sending — `None`, or `Err(Disconnected)`
        // — leaves nothing to import, which is the same no-op as before.
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
        // THE RESTART ELECTION. An explicit pin is the strongest statement the
        // user can make about which cat they want, so it outranks the
        // chronologically-latest discovery it used to lose to.
        self.companion = self.mem.favourite_look().or_else(|| {
            self.mem
                .collectibles
                .iter()
                .rev()
                .find_map(KittyCollectible::look)
        });
        self.revision = self.revision.wrapping_add(1);
    }

    /// The current change stamp (bumps once per recorded sighting).
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// The collected identity used by the cursor companion (O(1), no I/O).
    pub(crate) fn companion_look(&mut self) -> Option<KittyLook> {
        self.poll_initial_load();
        self.companion
    }

    /// Snapshot for the settings overlay (memory only — no IO).
    #[cfg(test)]
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
        self.poll_initial_load();
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
                // The hello still plays for a genuine discovery, but an
                // explicit favourite is a stronger reason than stumbling on a
                // new glyph: the pinned cat comes back once the newcomer's
                // hello ends, rather than being silently displaced.
                if self.mem.favourite_look().is_none() {
                    self.companion = Some(look);
                }
                discovery = Some(look);
            }
            let _ = self.delta.record(&s, lexicon, &stamp);
            self.revision = self.revision.wrapping_add(1);
        }
        self.maybe_flush(now);
        discovery
    }

    /// Promote one look into the durable registry and PIN it as the companion
    /// (owner: "if somebody really likes that kitty it goes into the kitty
    /// registry").
    ///
    /// Deliberately NOT [`Self::observe`]:
    /// * `KittyLog::record` reports "new" only for an unseen glyph KEY, so a
    ///   favourite routed through it would silently no-op on any
    ///   already-collected head — the common case on a used ledger.
    /// * The `(session, ident)` dedupe ring exists to absorb re-observations of
    ///   ONE on-screen episode and must never be able to swallow a button
    ///   press, so it is bypassed by construction.
    ///
    /// `enabled = false` (`[sparkle_words.feline] log = false`) pins in memory
    /// only — the same degradation Containment already documents: the session
    /// keeps the cat it asked for, nothing is written.
    pub(crate) fn favourite(
        &mut self,
        s: &KittySighting,
        lexicon: &Lexicon,
        now: Instant,
        enabled: bool,
    ) {
        self.poll_initial_load();
        let look = s.look.normalized();
        // Unconditional: the press IS the reason to change the identity, and
        // `record`'s "new key" answer cannot express that.
        self.companion = Some(look);
        if !enabled {
            return;
        }
        // NO ring check. The `(session, ident)` ring absorbs re-observations of one
        // on-screen episode; a button press is not an observation, and a user who
        // clicks the same cat twice means it twice. Note also that `observe` keys the
        // ring by the REAL session (:1500) while this path would key it by 0, so an
        // entry recorded here could only ever match ANOTHER favourite — i.e. its sole
        // possible effect was swallowing the press it must never swallow, at the cost
        // of a ring slot that can never usefully match. Pinned by
        // `a_favourite_is_never_absorbed_by_the_recount_ring`.
        let stamp = now_rfc3339();
        // Record first so a never-before-seen head exists as a roster row (that
        // call, and only that call, owns the roster cap); then stamp the pin.
        let _ = self.mem.record(s, lexicon, &stamp);
        self.mem.favourite_collectible(look, &stamp);
        let _ = self.delta.record(s, lexicon, &stamp);
        self.delta.favourite_collectible(look, &stamp);
        self.revision = self.revision.wrapping_add(1);
        // A user's explicit pick is not observability: skip the debounce so
        // quitting right after the click cannot lose it.
        self.last_flush = None;
        self.maybe_flush(now);
    }

    /// Whether `look` is the currently pinned favourite (the palette
    /// checkmark). `&self` on purpose: `App::palette_live` is `&self` and
    /// cannot poll the startup import — staleness is a non-issue because the
    /// render loop polls every tick.
    pub(crate) fn is_favourite(&self, look: KittyLook) -> bool {
        self.mem.favourite_look() == Some(look.normalized())
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
    /// batch (bounded channel full), the delta remains COALESCED in `self.delta`;
    /// `flush_exit` offers that host-owned tail through the independent shutdown
    /// lane. The worker's bounded backoff applies to batches it already owns. If
    /// construction-time pre-arm failed, the delta remains resident; render and
    /// quit never retry filesystem work synchronously.
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
            // Pre-arm failed. Keep the accumulated in-memory delta; never retry
            // thread creation from a render frame or perform synchronous exit IO.
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
                // handle without joining from render. Observability remains
                // memory-only rather than moving filesystem work onto the UI.
                self.delta = delta;
                self.last_flush = None;
                let _ = self.writer.take();
            }
        }
    }

    /// Exit-path flush: offer the writer any remaining delta without waiting,
    /// close its channel, and join only if the worker finishes within
    /// [`EXIT_JOIN_BUDGET`]. Advisory lock acquisition is nonblocking, but an
    /// ordinary file operation can still stall forever on a dead mount; that
    /// worker is detached at the deadline so best-effort observability can
    /// never wedge process quit.
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
                // A disconnected worker cannot take ownership. Retain the
                // batch in memory instead of reporting or modeling it as
                // accepted; quit remains best-effort and nonblocking.
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
                // Retry exhaustion is not an ownership sink. The process is
                // still quitting, but retaining the batch here keeps the
                // lifecycle truthful and lets callers/tests observe that it
                // was not durably accepted.
                self.delta.merge_from(&unpersisted);
            }
        }
        // `None` is the in-memory-only or pre-arm-failure path: never perform
        // exit I/O there.
    }
}

// ---- The collection book (settings §F4.6) -------------------------------------------

/// One rendered collection-book row — shared by the legacy settings painter and
/// `SettingsState::controls_lines` model tests. Native Settings owns its own compiled
/// semantic projection.
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

/// Serialize the retired Settings-card book as `kittylog …` introspection lines for
/// legacy model tests. Production `controls settings` compiles the native route's
/// semantic tree and does not append an off-screen Kitty Log catalog.
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
    use aterm_spec::derive::{
        kitty_collectibles_model, kitty_flush_worker_model, kitty_sidecar_durability_model,
    };
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

    fn exact_limit_toml(prefix: &str) -> String {
        assert!(prefix.len() < MAX_KITTY_LEDGER_BYTES);
        let mut text = String::with_capacity(MAX_KITTY_LEDGER_BYTES);
        text.push_str(prefix);
        text.push('#');
        text.push_str(&"x".repeat(MAX_KITTY_LEDGER_BYTES - text.len()));
        assert_eq!(text.len(), MAX_KITTY_LEDGER_BYTES);
        text
    }

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
            .recv_timeout(Duration::from_secs(3))
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

    /// Absent and corrupt files both read as the empty default (fail-open —
    /// the log is observability, never a gate), and a partial/unknown-key
    /// file keeps whatever fields it does carry.
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
        // SAFETY: `path` is a live NUL-terminated pathname and `mkfifo` retains
        // no pointer after returning.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_fifo_cannot_block_startup_or_background_flush_exit() {
        use std::os::unix::fs::FileTypeExt as _;

        let legacy = tmp("fifo-legacy");
        make_fifo(&legacy);
        let config = legacy.with_file_name("aterm.toml");
        assert_eq!(
            host_startup_and_exit(config),
            (0, 0),
            "a writerless legacy FIFO is admitted as empty"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy FIFO remains")
                .file_type()
                .is_fifo(),
            "best-effort flush must not replace a hostile legacy target"
        );
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_fifo_cannot_block_startup_or_background_flush_exit() {
        use std::os::unix::fs::FileTypeExt as _;

        let legacy = tmp("fifo-sidecar");
        let sidecar = collectibles_path(&legacy);
        make_fifo(&sidecar);
        let config = legacy.with_file_name("aterm.toml");
        assert_eq!(
            host_startup_and_exit(config),
            (0, 0),
            "a writerless collectible FIFO is admitted as empty"
        );
        assert!(
            std::fs::symlink_metadata(&sidecar)
                .expect("sidecar FIFO remains")
                .file_type()
                .is_fifo(),
            "best-effort flush must not replace a hostile sidecar target"
        );
        assert!(
            !legacy.exists(),
            "an unsafe authoritative sidecar must gate the mirror write"
        );
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn hostile_lock_fifo_cannot_block_startup_or_flush_exit() {
        use std::os::unix::fs::FileTypeExt as _;

        let legacy = tmp("fifo-lock");
        let lock_path = legacy.with_extension("toml.lock");
        make_fifo(&lock_path);
        assert_eq!(
            host_startup_and_exit(legacy.with_file_name("aterm.toml")),
            (0, 0),
            "a writerless lock FIFO must fail immediately"
        );
        assert!(
            std::fs::symlink_metadata(&lock_path)
                .expect("lock FIFO remains")
                .file_type()
                .is_fifo(),
            "best-effort locking must not replace a hostile rendezvous"
        );
        assert!(!legacy.exists(), "an unlocked flush must be deferred");
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    #[test]
    fn held_sibling_lock_never_parks_flush_exit() {
        let legacy = tmp("held-lock");
        let lock_path = legacy.with_extension("toml.lock");
        let held = open_lock(&lock_path).expect("open first regular lock handle");
        held.try_lock().expect("hold sibling lock");

        assert_eq!(
            host_startup_and_exit(legacy.with_file_name("aterm.toml")),
            (0, 0),
            "startup plus exit stays bounded while another process owns the lock"
        );
        assert!(!legacy.exists(), "a contended flush must not race unlocked");
        drop(held);

        let mut delta = KittyLog::default();
        delta.record(&sighting(7), Lexicon::builtin(), "2026-07-21T00:00:00Z");
        // The sibling lock this test held is not the only contender. `flush_exit`
        // DETACHES a worker that outlives EXIT_JOIN_BUDGET — deliberate, so that
        // process teardown wins over ledger durability — which means the host's
        // own writer may still be working through its finite retry budget at this
        // instant. `flush_merge` takes the lock with a SINGLE try, so asserting it
        // succeeds the moment the sibling lock drops quietly required that
        // detached thread to have already exited: a scheduling accident, not a
        // property of the code. (Reproduced ~1 run in 4 of the full aterm-gui
        // suite; never alone, because alone nothing delays the worker.)
        //
        // Retry within a bounded window instead. The claim — "once contention
        // clears, the retained batch is writable" — is unchanged and still fails
        // for a path that never becomes writable; only the accidental demand that
        // contention already be over at one exact instant is dropped.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut wrote = KittyLog::flush_merge(&legacy, &delta);
        while !wrote && Instant::now() < deadline {
            std::thread::sleep(EXIT_LOCK_RETRY_DELAY);
            wrote = KittyLog::flush_merge(&legacy, &delta);
        }
        assert!(
            wrote,
            "the same retained batch is writable after contention clears"
        );
        assert_eq!(KittyLog::read(&legacy).sightings, 1);
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
        let lock_path = legacy.with_extension("toml.lock");
        let held = open_lock(&lock_path).expect("open first regular lock handle");
        held.try_lock().expect("hold sibling lock");
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

        // Bind the whole genuine worker/host terminal lifecycle, not only the
        // retry helper: with the lock held through shutdown, the worker returns
        // its exhausted batch through JoinHandle and flush_exit restores host
        // ownership exactly as the joined projection above requires.
        let mut host = KittyLogHost::load(Some(legacy.clone()));
        host.observe(9, [sighting(9)], Lexicon::builtin(), Instant::now(), true);
        host.flush_exit();
        assert!(host.writer.is_none());
        assert_eq!(
            host.delta.sightings, 1,
            "retry exhaustion must return the worker-owned batch to the host"
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

    #[cfg(unix)]
    #[test]
    fn final_lock_symlink_is_neither_followed_nor_replaced() {
        use std::os::unix::fs::symlink;

        let legacy = tmp("symlink-lock");
        let lock_path = legacy.with_extension("toml.lock");
        let victim = legacy.with_file_name("lock-victim");
        std::fs::write(&victim, "untouched").expect("write lock victim");
        symlink(&victim, &lock_path).expect("plant final lock symlink");

        assert_eq!(
            host_startup_and_exit(legacy.with_file_name("aterm.toml")),
            (0, 0),
            "a final lock symlink must fail immediately"
        );
        assert!(
            std::fs::symlink_metadata(&lock_path)
                .expect("lock link remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read untouched lock victim"),
            "untouched"
        );
        assert!(!legacy.exists(), "an unlocked flush must be deferred");
        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_ledgers_are_neither_followed_nor_replaced() {
        use std::os::unix::fs::symlink;

        let legacy = tmp("symlink-legacy");
        let victim = legacy.with_file_name("legacy-victim.toml");
        let victim_text = "sightings = 99\n";
        std::fs::write(&victim, victim_text).expect("write legacy victim");
        symlink(&victim, &legacy).expect("plant legacy final symlink");
        assert_eq!(
            host_startup_and_exit(legacy.with_file_name("aterm.toml")),
            (0, 0),
            "startup must not follow the legacy symlink"
        );
        assert!(
            std::fs::symlink_metadata(&legacy)
                .expect("legacy link remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read untouched legacy victim"),
            victim_text
        );

        let sidecar_legacy = tmp("symlink-sidecar");
        let sidecar = collectibles_path(&sidecar_legacy);
        let sidecar_victim = sidecar_legacy.with_file_name("sidecar-victim.toml");
        let lex = Lexicon::builtin();
        let mut seeded = KittyLog::default();
        seeded.record(&sighting(1), lex, "2026-07-01T00:00:00Z");
        let sidecar_victim_text =
            toml::to_string(&KittyCollectibleStore::mirrored(&seeded.collectibles))
                .expect("serialize sidecar victim");
        std::fs::write(&sidecar_victim, &sidecar_victim_text).expect("write sidecar victim");
        symlink(&sidecar_victim, &sidecar).expect("plant sidecar final symlink");
        assert_eq!(
            host_startup_and_exit(sidecar_legacy.with_file_name("aterm.toml")),
            (0, 0),
            "startup must not follow the sidecar symlink"
        );
        assert!(
            std::fs::symlink_metadata(&sidecar)
                .expect("sidecar link remains")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&sidecar_victim).expect("read untouched sidecar victim"),
            sidecar_victim_text
        );
        assert!(
            !sidecar_legacy.exists(),
            "an unsafe authoritative sidecar must gate the mirror write"
        );

        let _ = std::fs::remove_dir_all(legacy.parent().unwrap());
        let _ = std::fs::remove_dir_all(sidecar_legacy.parent().unwrap());
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
        let flush = |delta: &KittyLog| {
            let result = flush_pending_at_exit(&p, delta);
            assert!(
                result.persisted,
                "the semantic ordering check requires an accepted durable flush"
            );
        };
        flush(&late);
        flush(&early);

        // The same semantic key also arrives out of order. Its collectible
        // appearance must come from the earliest discovery, not the process
        // that happened to acquire the file lock first.
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
            host.companion_look().map(|look| look.variant),
            Some(CatGlyphId::SpecSleeping),
            "startup companion is the chronologically latest discovery"
        );
        assert_eq!(
            kitty_book(host.log()).rarest,
            Some("Cinnamon Roll"),
            "rarest selection consumes chronological ledger order"
        );
        host.flush_exit();
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A sighting carrying an EXPLICIT look — the shape the favourite path
    /// mints (no matched surface, so no displayed-trait bits either).
    fn look_sighting(ident: u64, look: KittyLook) -> KittySighting {
        KittySighting {
            look,
            traits: 0,
            ..sighting(ident)
        }
    }

    /// A head glyph with a chosen coat — enough to tell two compositions of the
    /// SAME collectible key apart.
    fn coated(variant: CatGlyphId, coat: u8) -> KittyLook {
        KittyLook {
            variant,
            coat,
            ..KittyLook::default()
        }
        .normalized()
    }

    /// THE HEADLINE TRAP. `KittyLog::record` reports "new" only for an unseen
    /// glyph KEY, so on any used ledger a favourite routed through `observe`
    /// would silently no-op. The explicit pin must move the composition of an
    /// ALREADY-collected head and become the companion.
    #[test]
    fn favourite_pins_an_already_collected_head() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let stumbled = coated(CatGlyphId::S100, 3);
        let pinned = coated(CatGlyphId::S100, 9);
        let now = Instant::now();

        host.observe(4, [look_sighting(11, stumbled)], lex, now, true);
        assert_eq!(host.companion_look(), Some(stumbled));

        host.favourite(&look_sighting(12, pinned), lex, now, true);

        assert_eq!(
            host.companion_look(),
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

    /// A later automatic discovery still earns its hello, but it must not steal
    /// a pin the user made on purpose.
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
            host.companion_look(),
            Some(pinned),
            "…but the companion returns to the pin once that hello ends"
        );
    }

    /// The merge law, proven in BOTH orders (the multi-process read-merge-write
    /// core is order-independent): a favourite owns the composition against an
    /// earlier unfavourited discovery, without rewriting chronology.
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

    /// THE RESTART ELECTION. A pin must outrank the chronologically-latest
    /// discovery the startup import used to elect unconditionally.
    #[test]
    fn a_favourite_survives_the_startup_import() {
        let lex = Lexicon::builtin();
        let p = tmp("favourite-restart");
        let pinned = coated(CatGlyphId::S100, 7);
        let later = coated(CatGlyphId::S101, 2);
        // Keyed so the unfavourited newcomer ALSO sorts last on disk: without
        // the favourite rule the startup election would hand it the companion.
        assert!(glyph_key(CatGlyphId::S100) < glyph_key(CatGlyphId::S101));

        // The injected pre-arm failure keeps the delta resident, so the durable
        // write below is synchronous — this test pins the ELECTION, not the
        // background writer's timing (which has its own proofs).
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
            restarted.companion_look(),
            Some(pinned),
            "the pin outranks the chronologically latest discovery on restart"
        );
        restarted.flush_exit();
        let _ = std::fs::remove_dir_all(p.parent().expect("scratch parent"));
    }

    /// The `(session, ident)` ring exists to absorb re-observations of ONE
    /// on-screen episode. It must never be able to swallow a BUTTON PRESS, so
    /// the favourite path bypasses it by construction — proven with a
    /// deliberately REUSED ident, which `observe` would drop for `RING_TTL`.
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

    /// Gate 4 (`[sparkle_words.feline] log = false`) closes the DURABLE tier
    /// only. The session still gets the cat it asked for — the same degradation
    /// Containment documents — but nothing is written.
    #[test]
    fn favourite_with_recording_off_is_memory_only() {
        let lex = Lexicon::builtin();
        let mut host = KittyLogHost::in_memory();
        let look = coated(CatGlyphId::S100, 6);

        host.favourite(&look_sighting(88, look), lex, Instant::now(), false);

        assert_eq!(
            host.companion_look(),
            Some(look),
            "the pick still holds for this session"
        );
        assert_eq!(host.log().sightings, 0, "the ledger tier stayed closed");
        assert!(
            host.log().collectibles.is_empty(),
            "and no roster row was minted"
        );
    }

    /// TYPING-5 negative control: even when the one construction-time writer
    /// pre-arm fails, observing the first sighting must not retry a thread spawn
    /// from the render path. The delta stays resident, while `flush_exit` avoids
    /// a synchronous filesystem fallback that could wedge quit.
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

        let started = Instant::now();
        host.flush_exit();
        // DETACHMENT is proven structurally, not by the clock: this worker blocks
        // on `release_rx` until the send at the very end of this test, so a
        // `flush_exit` that JOINED it could never return and execution could not
        // reach this line at all. The assertion below therefore only guards
        // PROMPTNESS — that quit honours its budget instead of lingering.
        //
        // So derive it from that budget with wide margin rather than pinning it at
        // budget + 100ms. `flush_exit` polls in EXIT_JOIN_POLL (1ms) sleeps, and a
        // 1ms sleep on a machine running 2,400 sibling tests can overshoot by far
        // more than the old 100ms of slack — that bound measured the scheduler.
        // A multiple still catches a real regression (quit waiting seconds for a
        // dead network mount) without failing on scheduling noise.
        assert!(
            started.elapsed() < EXIT_JOIN_BUDGET * 10,
            "quit must detach a worker whose regular-file operation never returns \
             promptly: took {:?}, budget is {EXIT_JOIN_BUDGET:?}",
            started.elapsed(),
        );

        // Tier-1 projection of that genuine stalled worker: delivery and exit
        // precede the unreturned IO, the UI-owned deadline advances without
        // mutating the accepted batch, and only its final edge admits detach.
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
        std::thread::sleep(Duration::from_millis(25));
        assert!(!ledger.exists(), "held lock keeps the batch pending");

        drop(locks);
        let deadline = Instant::now() + FLUSH_RETRY_DELAY + Duration::from_secs(2);
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

    #[test]
    fn full_normal_queue_cannot_drop_the_dedicated_exit_tail() {
        let ledger = tmp("writer-full-normal-exit-tail");
        let lex = Lexicon::builtin();
        // The injected worker takes batch one into its local accumulator, tells
        // the test that ownership moved, then waits on the exit lane before it
        // touches the ordinary receiver again. Batch two therefore fills the
        // capacity-one normal queue and debounced batch three remains host-owned:
        // all three real ownership containers are occupied simultaneously.
        // Only the REAL KittyLogHost::flush_exit call can unblock the worker.
        // Reverting that call site to `tx.try_send` would hit Full, close exit_rx
        // without a tail, and fail without depending on thread scheduling.
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
            // Failure bound: the worker that never takes batch one never sends at all.
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

        // Tier-1: expose each genuine ownership handoff. The ordinary queue is
        // full, so the second accepted batch remains host-owned until exit,
        // crosses the dedicated lane, and is absorbed only after the normal
        // batch drains. The Buggy branch below is the retired one-lane send: it
        // clears host ownership without establishing exit-lane ownership.
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
        let saved = KittyLog::read_with_sidecar(&ledger);
        assert_eq!(saved.sightings, 3);
        assert_eq!(saved.collectibles.len(), 1);
        assert_eq!(saved.collectibles[0].count, 3);
        let persisted = project(3, 0, 0, 0, 0, 3, 1, 0);
        validate("Flush", &both_pending, &persisted);
        let joined = project(3, 0, 0, 0, 0, 3, 1, 1);
        validate("Join", &persisted, &joined);
        let _ = std::fs::remove_dir_all(ledger.parent().unwrap());
    }

    /// The long-lived writer path end-to-end (TYPING-5): a real-path host
    /// pre-arms ONE background worker before any sighting; `observe` hands its
    /// delta over the bounded channel without replacing/spawning a worker. A
    /// second sighting is debounced and accumulates; `flush_exit` offers the
    /// tail over its shutdown-only lane and joins the healthy local worker
    /// within its finite deadline, so every count is durable before quit
    /// without trusting filesystem latency or ordinary-queue scheduling.
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
