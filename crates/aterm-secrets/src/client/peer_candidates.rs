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
                super::note_lan_use();
                let watch = standing.watch_or_start(|| Watch::start(bell, &[]));
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
    let _lock = StateLock(file);
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
mod tests {
    use super::*;

    #[test]
    fn reordered_browse_events_do_not_hide_later_peers() {
        const DEFAULT: &str = "http://default.local:7484/v1/alab";
        const PRIMARY: &str = "http://primary.local:7484/v1/alab";
        let names: Vec<_> = (0..35).map(|n| format!("node-{n:02}")).collect();
        let expected_names: BTreeSet<_> = names.iter().cloned().collect();
        let expected_urls: BTreeSet<_> = std::iter::once(DEFAULT.to_owned())
            .chain(
                names
                    .iter()
                    .map(|name| format!("http://{name}.local:7484/v1/alab")),
            )
            .collect();
        let limit = MAX_CANDIDATES - 1;
        for saved in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(root.path());
            fsx::ensure_private_dir(&paths.dir).unwrap();
            let memory = AtomicUsize::new(0);
            let mut queried = BTreeSet::new();
            let mut reached = BTreeSet::new();
            for pass in 0..names.len().div_ceil(limit) {
                // A new browse can hear exactly the same services in another
                // order. This order would cancel a position-only rotation.
                let mut arrivals = names.clone();
                arrivals.rotate_right((pass * limit) % names.len());
                let transcript: String = arrivals
                    .iter()
                    .map(|name| format!("08:30:01.200 Add 3 4 local. _alab-vault._tcp. {name}\n"))
                    .collect();
                let heard = crate::discover::parse_browse(&transcript);
                assert_eq!(heard, arrivals);
                let fresh = AtomicUsize::new(0);
                let plan = Plan::new(
                    "alab",
                    PRIMARY,
                    &[DEFAULT],
                    &[],
                    &[],
                    Pool::Names(heard),
                    Cursor {
                        memory: if saved { &fresh } else { &memory },
                        saved: saved.then_some((&paths, PeerUse::Reader)),
                    },
                );
                let urls = plan.finish("alab", PRIMARY, Duration::from_secs(1), |selected, _| {
                    assert!(selected.len() <= limit);
                    queried.extend(selected.iter().cloned());
                    selected
                        .iter()
                        .map(|name| DiscoveredVault {
                            url: format!("http://{name}.local:7484/v1/alab"),
                            txt: None,
                        })
                        .collect()
                });
                assert!(urls.len() <= MAX_CANDIDATES);
                assert_eq!(urls.first().map(String::as_str), Some(DEFAULT));
                reached.extend(urls);
            }
            assert_eq!(queried, expected_names, "saved={saved}");
            assert_eq!(reached, expected_urls, "saved={saved}");
        }
    }

    #[test]
    fn one_shot_cursors_are_per_home_and_skip_unneeded_storage() {
        let root = aterm_tempfile::tempdir().unwrap();
        let left = Paths::for_home(&root.path().join("left"));
        let right = Paths::for_home(&root.path().join("right"));
        for paths in [&left, &right] {
            fsx::ensure_private_dir(&paths.dir).unwrap();
        }
        let select = |paths: &Paths, purpose, count| {
            let memory = AtomicUsize::new(0);
            Cursor {
                memory: &memory,
                saved: Some((paths, purpose)),
            }
            .select((0..count).collect::<Vec<_>>(), MAX_CANDIDATES)
        };
        assert_eq!(select(&left, PeerUse::Records, 2), vec![0, 1]);
        assert!(!left.dir.join(CURSOR_FILE).exists());
        assert!(!left.dir.join(CURSOR_LOCK).exists());
        for purpose in [PeerUse::Records, PeerUse::Envelope, PeerUse::Reader] {
            assert_eq!(select(&left, purpose, 35), (0..16).collect::<Vec<_>>());
        }
        assert_eq!(
            select(&right, PeerUse::Records, 35),
            (0..16).collect::<Vec<_>>()
        );
        assert_eq!(
            select(&left, PeerUse::Records, 35),
            (16..32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn join_rotation_preserves_the_cursor_file_an_older_reader_uses() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        let original: Vec<u8> = b"APC1"
            .iter()
            .copied()
            .chain([5_u64, 9, 13].into_iter().flat_map(u64::to_le_bytes))
            .collect();
        let path = paths.dir.join(CURSOR_FILE);
        fsx::write_atomic(&path, &original, 0o600).unwrap();
        assert_eq!(saved_start(&paths, PeerUse::Join, 35, 16), Some(0));
        assert_eq!(saved_start(&paths, PeerUse::Queue, 35, 16), Some(0));
        assert_eq!(
            fsx::read_private(&path, 28).unwrap().unwrap().as_slice(),
            original
        );
        assert_eq!(saved_start(&paths, PeerUse::Reader, 35, 16), Some(13));
        assert_eq!(saved_start(&paths, PeerUse::Join, 35, 16), Some(16));
        assert_eq!(saved_start(&paths, PeerUse::Queue, 35, 16), Some(16));
        let updated = fsx::read_private(&path, 28).unwrap().unwrap();
        assert_eq!(&updated.as_slice()[..20], &original[..20]);
        assert_eq!(&updated.as_slice()[20..], &29_u64.to_le_bytes());
    }

    #[test]
    fn one_shot_cursor_falls_back_when_hint_is_busy_or_damaged() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let held = StateLock::take_at(&paths, &paths.dir.join(CURSOR_LOCK)).unwrap();
        let other = paths.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let memory = AtomicUsize::new(0);
            let cursor = Cursor {
                memory: &memory,
                saved: Some((&other, PeerUse::Records)),
            };
            send.send([
                cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
                cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
            ])
            .unwrap();
        });
        let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        drop(held);
        worker.join().unwrap();
        let expected = [(0..16).collect::<Vec<_>>(), (16..32).collect::<Vec<_>>()];
        assert_eq!(observed.unwrap(), expected);

        let file = paths.dir.join(CURSOR_FILE);
        fsx::write_atomic(&file, b"damaged", 0o600).unwrap();
        let memory = AtomicUsize::new(0);
        let cursor = Cursor {
            memory: &memory,
            saved: Some((&paths, PeerUse::Records)),
        };
        for expected in expected {
            assert_eq!(
                cursor.select((0..35).collect::<Vec<_>>(), MAX_CANDIDATES),
                expected
            );
        }
        assert_eq!(
            fsx::read_private(&file, 28).unwrap().unwrap().as_slice(),
            b"damaged"
        );
    }

    #[test]
    fn named_and_known_candidates_share_the_actual_final_url_budget() {
        const DEFAULT: &str = "http://default.local:7484/v1/alab";
        const PRIMARY: &str = "http://primary.local:7484/v1/alab";
        for (known_count, name_count) in [(0usize, 16usize), (2, 15), (16, 16), (35, 35)] {
            let known: Vec<_> = (0..known_count)
                .map(|n| format!("http://p{n}.local:7484/v1/alab"))
                .collect();
            let mut policy = known.clone();
            policy.extend([
                PRIMARY.to_owned(),
                DEFAULT.to_owned(),
                "not a URL".to_owned(),
                "http://foreign.local:7484/v1/elsewhere".to_owned(),
            ]);
            if known_count != 0 {
                policy.push("http://P0.LOCAL:7484/v1/alab#ignored-fragment".to_owned());
            }
            let names: Vec<_> = (0..name_count).map(|n| format!("n{n}")).collect();
            let expected_names: BTreeSet<_> = names.iter().cloned().collect();
            let expected: BTreeSet<_> = std::iter::once(DEFAULT.to_owned())
                .chain(known)
                .chain(
                    names
                        .iter()
                        .map(|name| format!("http://{name}.local:7484/v1/alab")),
                )
                .collect();
            let defaults = [
                DEFAULT,
                PRIMARY,
                "http://DEFAULT.LOCAL:7484/v1/alab",
                "not a URL",
            ];
            let from = AtomicUsize::new(0);
            let mut queried = BTreeSet::new();
            let mut reached = BTreeSet::new();
            for _ in 0..(known_count + name_count).div_ceil(MAX_CANDIDATES - 1) {
                let plan = Plan::new(
                    "alab",
                    PRIMARY,
                    &defaults,
                    &policy,
                    &[],
                    Pool::Names(names.clone()),
                    Cursor {
                        memory: &from,
                        saved: None,
                    },
                );
                let urls = plan.finish("alab", PRIMARY, Duration::from_secs(1), |selected, _| {
                    assert!(selected.len() < MAX_CANDIDATES);
                    queried.extend(selected.iter().cloned());
                    selected
                        .iter()
                        .map(|name| DiscoveredVault {
                            url: format!("http://{name}.local:7484/v1/alab"),
                            txt: None,
                        })
                        .collect()
                });
                assert!(urls.len() <= MAX_CANDIDATES);
                assert_eq!(urls.first().map(String::as_str), Some(DEFAULT));
                assert_eq!(urls.iter().collect::<BTreeSet<_>>().len(), urls.len());
                reached.extend(urls);
            }
            assert_eq!(
                queried, expected_names,
                "known={known_count}, names={name_count}"
            );
            assert_eq!(reached, expected, "known={known_count}, names={name_count}");
        }
    }
}
