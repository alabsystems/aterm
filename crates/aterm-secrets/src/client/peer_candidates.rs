// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One bounded peer plan across configured URLs and unresolved LAN names.

use super::*;
use crate::discover::{DiscoveredVault, MAX_CANDIDATES, Watch};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// Consumers advance independently: a read cannot skip a record peer, a join destination or
/// an approver queue.
#[derive(Clone, Copy)]
pub(super) enum PeerUse {
    Records,
    Envelope,
    Reader,
    Join,
    Queue,
}

impl PeerUse {
    /// Whether this purpose's peers are read for the vault itself (its envelope or its records),
    /// which a Mac's mirrors serve ([`Config::mirrors`]), rather than sent a join or read for a
    /// join queue, which only the vault's servers hold.
    pub(super) const fn vault_read(self) -> bool {
        matches!(self, Self::Records | Self::Envelope | Self::Reader)
    }

    const fn index(self) -> usize {
        match self {
            Self::Records => 0,
            Self::Envelope => 1,
            Self::Reader => 2,
            Self::Join => 3,
            Self::Queue => 4,
        }
    }
}

enum DiscoverySource<'a> {
    Empty,
    #[cfg(test)]
    Supplied(&'a (dyn Fn(&Config) -> Vec<DiscoveredVault> + Sync)),
    Watching {
        bell: &'a Arc<crate::wake::Bell>,
        standing: &'a StandingBrowse,
    },
    Direct(Option<Instant>),
    Selected(&'a [String]),
}

/// The subscriber owns its selection cursors with its discovery source.
pub(super) struct PeerDiscovery<'a> {
    source: DiscoverySource<'a>,
    from: [AtomicUsize; 5],
}

impl<'a> PeerDiscovery<'a> {
    const fn new(source: DiscoverySource<'a>) -> Self {
        Self {
            source,
            from: [const { AtomicUsize::new(0) }; 5],
        }
    }

    pub(super) const fn empty() -> Self {
        Self::new(DiscoverySource::Empty)
    }

    #[cfg(test)]
    pub(super) fn supplied(found: &'a (dyn Fn(&Config) -> Vec<DiscoveredVault> + Sync)) -> Self {
        Self::new(DiscoverySource::Supplied(found))
    }

    pub(super) fn watching(bell: &'a Arc<crate::wake::Bell>, standing: &'a StandingBrowse) -> Self {
        Self::new(DiscoverySource::Watching { bell, standing })
    }

    pub(super) fn direct(deadline: Option<Instant>) -> Self {
        Self::new(DiscoverySource::Direct(deadline))
    }

    pub(super) fn selected(peers: &'a [String]) -> Self {
        Self::new(DiscoverySource::Selected(peers))
    }

    fn cursor(&self, purpose: PeerUse) -> &AtomicUsize {
        static DIRECT_FROM: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
        if matches!(self.source, DiscoverySource::Direct(_)) {
            &DIRECT_FROM[purpose.index()]
        } else {
            &self.from[purpose.index()]
        }
    }

    fn budget(&self) -> Duration {
        match self.source {
            DiscoverySource::Direct(Some(deadline)) => deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_default()
                .min(PROBE_DEADLINE),
            _ => PROBE_DEADLINE,
        }
    }

    /// The publisher keeps its existing raw-discovery path; its trusted URL list is uncapped.
    pub(super) fn read(&self, config: &Config) -> Vec<DiscoveredVault> {
        match &self.source {
            DiscoverySource::Empty => Vec::new(),
            #[cfg(test)]
            DiscoverySource::Supplied(found) => found(config),
            DiscoverySource::Watching { bell, standing } => super::browsed(config, bell, standing),
            DiscoverySource::Direct(_) if !cfg!(test) && alab_vault(config) => {
                crate::discover::browse_alab(self.budget()).unwrap_or_default()
            }
            DiscoverySource::Direct(_) => Vec::new(),
            DiscoverySource::Selected(peers) => peers
                .iter()
                .map(|url| DiscoveredVault {
                    url: url.clone(),
                    txt: None,
                })
                .collect(),
        }
    }

    /// A bounded reader's already selected list is reused without reintroducing policy peers.
    pub(super) fn preselected(&self, config: &Config, primary: &str) -> Option<Vec<String>> {
        let DiscoverySource::Selected(peers) = self.source else {
            return None;
        };
        let clean: Vec<_> = peers
            .iter()
            .filter_map(|url| normalized(url, &config.name, primary))
            .collect();
        let refs: Vec<&str> = clean.iter().map(String::as_str).collect();
        Some(crate::discover::candidate_urls(&refs, &[], &config.name))
    }

    fn pool(&self, config: &Config, budget: Duration) -> Pool {
        match &self.source {
            #[cfg(test)]
            DiscoverySource::Supplied(found) => Pool::Ready(found(config)),
            DiscoverySource::Watching { bell, standing } if alab_vault(config) => {
                let watch = standing.watch_or_start(|| Watch::start_look(bell, &[]));
                let heard = watch.heard();
                Pool::Names(if heard.is_empty() {
                    crate::discover::browse_names(budget).unwrap_or_default()
                } else {
                    heard
                })
            }
            DiscoverySource::Direct(_) if !cfg!(test) && alab_vault(config) => {
                Pool::Names(crate::discover::browse_names(budget).unwrap_or_default())
            }
            _ => Pool::Ready(Vec::new()),
        }
    }

    pub(super) fn select(
        &self,
        paths: &Paths,
        config: &Config,
        primary: &str,
        policy: &[String],
        explicit: &[String],
        purpose: PeerUse,
    ) -> Vec<String> {
        if let Some(selected) = self.preselected(config, primary) {
            return selected;
        }
        let started = Instant::now();
        let budget = self.budget();
        let pool = if explicit.is_empty() {
            self.pool(config, budget)
        } else {
            Pool::Ready(Vec::new())
        };
        let defaults = if explicit.is_empty() && !cfg!(test) && alab_vault(config) {
            crate::ALAB_DEFAULT_URLS
        } else {
            &[]
        };
        Plan::new(
            &config.name,
            primary,
            defaults,
            policy,
            explicit,
            pool,
            Cursor {
                memory: self.cursor(purpose),
                saved: matches!(self.source, DiscoverySource::Direct(_))
                    .then_some((paths, purpose)),
            },
        )
        .finish(
            &config.name,
            primary,
            budget.saturating_sub(started.elapsed()),
            crate::discover::resolve_all,
        )
    }

    /// Every queue becomes the first candidate in turn, including across fresh readers.
    /// Keep the configured source and a still-authorized cached listing available outside
    /// the rotating batch for fallback and idle long-polling.
    pub(super) fn select_queues(
        &self,
        paths: &Paths,
        config: &Config,
        primary: &str,
        policy: &[String],
        current: &str,
    ) -> Vec<String> {
        let mut urls = policy_queues(config, primary, policy);
        if urls.is_empty() {
            return urls;
        }
        let cursor = Cursor {
            memory: self.cursor(PeerUse::Queue),
            saved: matches!(self.source, DiscoverySource::Direct(_))
                .then_some((paths, PeerUse::Queue)),
        };
        let first = cursor
            .select((0..urls.len()).collect::<Vec<_>>(), 1)
            .into_iter()
            .next()
            .unwrap_or(0);
        let retained: Vec<_> = [primary, current]
            .into_iter()
            .filter_map(|required| {
                urls.iter()
                    .find(|url| {
                        transport::describe(url)
                            .eq_ignore_ascii_case(&transport::describe(required))
                    })
                    .cloned()
            })
            .collect();
        urls.rotate_left(first);
        urls.truncate(MAX_CANDIDATES);
        for retained in retained {
            if !urls.contains(&retained) {
                urls.push(retained);
            }
        }
        urls
    }
}

/// Every explicitly configured queue, without discovery hints or cursor selection.
pub(super) fn policy_queues(config: &Config, primary: &str, policy: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    std::iter::once(primary)
        .chain(policy.iter().map(String::as_str))
        .filter_map(|raw| normalized_url(raw, &config.name))
        .filter(|url| seen.insert(transport::describe(url).to_ascii_lowercase()))
        .collect()
}

enum Pool {
    Ready(Vec<DiscoveredVault>),
    Names(Vec<String>),
}

enum Target {
    Known(String),
    Ready(DiscoveredVault),
    Name(String),
}

struct Plan {
    preferred: Vec<String>,
    ready: Vec<DiscoveredVault>,
    names: Vec<String>,
}

struct Cursor<'a> {
    memory: &'a AtomicUsize,
    saved: Option<(&'a Paths, PeerUse)>,
}

impl Cursor<'_> {
    fn select<T>(&self, values: Vec<T>, limit: usize) -> Vec<T> {
        // Small lists need neither a cursor nor a disk write. Only one-shot
        // readers use the saved position; a standing subscriber owns its cursor.
        let saved = if !values.is_empty() && values.len() > limit && limit != 0 {
            self.saved
                .and_then(|(paths, purpose)| saved_start(paths, purpose, values.len(), limit))
                .map(AtomicUsize::new)
        } else {
            None
        };
        crate::discover::rotated(values, saved.as_ref().unwrap_or(self.memory), limit)
    }
}

const CURSOR_FILE: &str = "peer-cursors";
const CURSOR_LOCK: &str = "peer-cursors.lock";

/// Scheduling hints contain offsets, never endpoints or authority. Join senders and queue
/// readers have separate hints, so neither advances the other or the vault-read cursors.
/// A busy, damaged or unwritable hint leaves in-process rotation available.
/// The lock covers only this bounded file update, never DNS or an HTTP request.
fn saved_start(paths: &Paths, purpose: PeerUse, count: usize, limit: usize) -> Option<usize> {
    const MAX_SIZE: usize = 4 + 3 * 8;
    let (name, lock, magic, slots, index) = match purpose {
        PeerUse::Join => ("join-peer-cursor", "join-peer-cursor.lock", b"APJ1", 1, 0),
        PeerUse::Queue => (
            "join-queue-turn-cursor",
            "join-queue-turn-cursor.lock",
            b"AQT1",
            1,
            0,
        ),
        PeerUse::Records | PeerUse::Envelope | PeerUse::Reader => {
            (CURSOR_FILE, CURSOR_LOCK, b"APC1", 3, purpose.index())
        }
    };
    let size = 4 + slots * 8;
    let file = fsx::open_lock(&paths.dir.join(lock)).ok()?;
    #[expect(
        clippy::disallowed_methods,
        reason = "optional peer cursor never waits for a lock; StateLock releases it on drop"
    )]
    file.try_lock().ok()?;
    let _lock = StateLock::exclusive(file);
    let path = paths.dir.join(name);
    let mut offsets = [0u64; 3];
    if let Some(bytes) = fsx::read_private(&path, MAX_SIZE).ok()? {
        let bytes = bytes.as_slice();
        if bytes.len() != size || &bytes[..4] != magic {
            return None;
        }
        for (offset, bytes) in offsets.iter_mut().zip(bytes[4..].as_chunks::<8>().0) {
            *offset = u64::from_le_bytes(*bytes);
        }
    }
    let count = u64::try_from(count).ok()?;
    let start = offsets[index] % count;
    offsets[index] = start.checked_add(u64::try_from(limit).ok()?)? % count;
    let mut bytes = [0u8; MAX_SIZE];
    bytes[..4].copy_from_slice(magic);
    for (bytes, offset) in bytes[4..size]
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(offsets)
    {
        bytes.copy_from_slice(&offset.to_le_bytes());
    }
    fsx::write_atomic(&path, &bytes[..size], 0o600).ok()?;
    usize::try_from(start).ok()
}

fn normalized_url(raw: &str, vault: &str) -> Option<String> {
    let base = raw.split_once('#').map_or(raw, |(base, _)| base).trim();
    let (transport::Source::Url(clean), None) =
        transport::parse_source(base, Path::new("/")).ok()?
    else {
        return None;
    };
    (transport::vault_name(&transport::Source::Url(clean.clone()))
        .ok()
        .as_deref()
        == Some(vault))
    .then_some(clean)
}

fn normalized(raw: &str, vault: &str, primary: &str) -> Option<String> {
    let clean = normalized_url(raw, vault)?;
    (!transport::describe(&clean).eq_ignore_ascii_case(&transport::describe(primary)))
        .then_some(clean)
}

impl Plan {
    fn new(
        vault: &str,
        primary: &str,
        defaults: &[&str],
        policy: &[String],
        explicit: &[String],
        pool: Pool,
        from: Cursor<'_>,
    ) -> Self {
        let mut seen = BTreeSet::new();
        let mut take = |raw: &str| {
            let clean = normalized(raw, vault, primary)?;
            seen.insert(transport::describe(&clean).to_ascii_lowercase())
                .then_some(clean)
        };
        let mut preferred: Vec<_> = defaults.iter().filter_map(|raw| take(raw)).collect();
        preferred.truncate(MAX_CANDIDATES);
        let mut targets: Vec<_> = policy
            .iter()
            .chain(explicit)
            .filter_map(|raw| take(raw).map(Target::Known))
            .collect();
        match pool {
            Pool::Ready(ready) => {
                targets.extend(ready.into_iter().filter_map(|mut item| {
                    item.url = take(&item.url)?;
                    Some(Target::Ready(item))
                }));
            }
            Pool::Names(mut names) => {
                // Browse events can arrive in a different order on every pass.
                // Stabilize the list before either cursor advances through it.
                names.sort_unstable();
                targets.extend(names.into_iter().map(Target::Name));
            }
        }
        let selected = from.select(targets, MAX_CANDIDATES - preferred.len());
        let mut ready = Vec::new();
        let mut names = Vec::new();
        for target in selected {
            match target {
                Target::Known(url) => preferred.push(url),
                Target::Ready(item) => ready.push(item),
                Target::Name(name) => names.push(name),
            }
        }
        Self {
            preferred,
            ready,
            names,
        }
    }

    fn finish(
        mut self,
        vault: &str,
        primary: &str,
        budget: Duration,
        resolve: impl FnOnce(&[String], Duration) -> Vec<DiscoveredVault>,
    ) -> Vec<String> {
        if !self.names.is_empty() && budget >= Duration::from_millis(40) {
            self.ready.extend(resolve(&self.names, budget));
        }
        self.ready.retain_mut(|item| {
            let Some(clean) = normalized(&item.url, vault, primary) else {
                return false;
            };
            item.url = clean;
            true
        });
        let preferred: Vec<&str> = self.preferred.iter().map(String::as_str).collect();
        crate::discover::candidate_urls(&preferred, &self.ready, vault)
    }
}

#[cfg(test)]
#[path = "peer_candidates_own_tests.rs"]
mod tests;
