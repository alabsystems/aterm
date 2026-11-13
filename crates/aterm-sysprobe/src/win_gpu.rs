// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows arm's GPU and thermal decoders: pure functions over what the Performance Data
//! Helper returns (`PdhGetRawCounterArrayW`'s items for `\GPU Engine(*)\Running Time`, the
//! `\GPU Adapter Memory(*)` levels and `\Thermal Zone Information(*)\% Passive Limit`) and over
//! the adapters DirectX lists in the registry, so every host compiles and tests them (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §8.2, §16). The calls are in `win_pdh.rs`.
//!
//! `Running Time` is the time one process's contexts ran on one engine of one adapter, in
//! 100 ns units, cumulative while the process holds the engine: the counter Task Manager's
//! `Utilization Percentage` is the rate of. Reading it raw gives the device's busy share between
//! two reads (each engine's processes summed, the busiest engine of an adapter taken, as Task
//! Manager draws it) and each process's cumulative GPU time from one counter.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::{GpuClient, GpuClients, GpuReading, MAX_GPU_CLIENTS, Throttle, ThrottleLevel, Why};

/// How long a collection answers every caller: a read within it of the last returns what that
/// one read ([`held`]), so two callers on one tick cost one collection, and no window is too short
/// to hold a share. Shorter than the 100 ms between the capacity reader's looks.
pub const MIN_WINDOW: Duration = Duration::from_millis(50);
/// How long after a read of DirectX's adapter rows that matched no live hardware adapter they are
/// read again ([`Listing`]); each further such read doubles it, up to [`RELIST_MOST`].
pub const RELIST_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two reads of rows that match no live hardware adapter: a machine
/// whose only adapter is the software rasteriser reads its handful of registry values this often.
pub const RELIST_MOST: Duration = Duration::from_secs(60);

/// `sizeof(PDH_RAW_COUNTER_ITEM_W)` (`pdh.h`), 64-bit: the name's pointer, then a
/// `PDH_RAW_COUNTER`.
pub const RAW_ITEM_SIZE: usize = 48;
/// `PDH_RAW_COUNTER_ITEM_W.szName`: a pointer into the same buffer, past the items.
pub const RAW_ITEM_NAME: usize = 0;
/// `PDH_RAW_COUNTER_ITEM_W.RawValue.CStatus`.
pub const RAW_ITEM_STATUS: usize = 8;
/// `PDH_RAW_COUNTER_ITEM_W.RawValue.FirstValue`, after the `FILETIME` and its padding.
pub const RAW_ITEM_VALUE: usize = 24;
/// `PDH_CSTATUS_VALID_DATA`.
pub const CSTATUS_VALID_DATA: u32 = 0;
/// `PDH_CSTATUS_NEW_DATA`.
pub const CSTATUS_NEW_DATA: u32 = 1;
/// The most items of one counter that are decoded: a desktop lists a few hundred engine
/// instances (about ten per process that holds a device).
pub const MAX_RAW_ITEMS: usize = 65_536;
/// The most UTF-16 units of an instance name that are read.
const NAME_UNITS: usize = 256;

/// PCI vendor `0x1414`, Microsoft: the Basic Render Driver (WARP), a software rasteriser that
/// DirectX lists beside the hardware adapters.
pub const VENDOR_MICROSOFT: u32 = 0x1414;

/// One instance of a wildcard counter: its name and raw value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawItem {
    /// The instance's name (`pid_1160_luid_0x00000000_0x0001137F_phys_0_eng_0_engtype_3D`).
    pub name: String,
    /// `FirstValue`: the counter's raw value.
    pub value: u64,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The NUL-terminated UTF-16LE text at `at`; `None` where it runs off the buffer or past
/// [`NAME_UNITS`] without its terminator.
fn wide_at(buf: &[u8], at: usize) -> Option<String> {
    let (units, _) = buf.get(at..)?.as_chunks::<2>();
    let wide: Vec<u16> = units
        .iter()
        .take(NAME_UNITS + 1)
        .map(|u| u16::from_le_bytes(*u))
        .take_while(|u| *u != 0)
        .collect();
    let ended = wide.len() < units.len().min(NAME_UNITS + 1);
    ended.then(|| String::from_utf16_lossy(&wide))
}

/// The items of one `PdhGetRawCounterArrayW` buffer: `count` `PDH_RAW_COUNTER_ITEM_W`s at the
/// start of `buf`, which the call filled at address `base`, each name a pointer into `buf`. An
/// item whose status is not valid data, whose value is negative, or whose name lies outside the
/// buffer is left out; at most [`MAX_RAW_ITEMS`] are decoded.
#[must_use]
pub fn raw_items(buf: &[u8], base: u64, count: usize) -> Vec<RawItem> {
    let mut out = Vec::new();
    for i in 0..count.min(MAX_RAW_ITEMS) {
        let Some(item) = i
            .checked_mul(RAW_ITEM_SIZE)
            .and_then(|at| buf.get(at..at.checked_add(RAW_ITEM_SIZE)?))
        else {
            break;
        };
        let valid = matches!(
            u32_at(item, RAW_ITEM_STATUS),
            Some(CSTATUS_VALID_DATA | CSTATUS_NEW_DATA)
        );
        let name = u64_at(item, RAW_ITEM_NAME)
            .and_then(|ptr| ptr.checked_sub(base))
            .and_then(|at| usize::try_from(at).ok())
            .and_then(|at| wide_at(buf, at));
        let value = u64_at(item, RAW_ITEM_VALUE).filter(|v| i64::try_from(*v).is_ok());
        if let (true, Some(name), Some(value)) = (valid, name, value) {
            out.push(RawItem { name, value });
        }
    }
    out
}

/// A run of ASCII digits at the start of `text` as a `u32`, and what follows it.
fn decimal(text: &str) -> Option<(u32, &str)> {
    let end = text
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, rest) = text.split_at(end);
    Some((crate::parse_uint(digits)?, rest))
}

/// `0x` and exactly eight hex digits at the start of `text`, and what follows them.
fn hex32(text: &str) -> Option<(u32, &str)> {
    let text = text.strip_prefix("0x")?;
    let (digits, rest) = text.split_at_checked(8)?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((u32::from_str_radix(digits, 16).ok()?, rest))
}

/// `luid_0x<high>_0x<low>_phys_<n>` at the start of `text`: the adapter's locally unique id,
/// the physical adapter's index under it (a linked-adapter group has several), and what follows.
fn adapter_at(text: &str) -> Option<(u64, u32, &str)> {
    let (high, rest) = hex32(text.strip_prefix("luid_")?)?;
    let (low, rest) = hex32(rest.strip_prefix('_')?)?;
    let (phys, rest) = decimal(rest.strip_prefix("_phys_")?)?;
    Some(((u64::from(high) << 32) | u64::from(low), phys, rest))
}

/// One `GPU Engine` instance, as its name states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Engine<'a> {
    /// The process whose contexts ran on the engine.
    pub pid: u32,
    /// The adapter's locally unique id, assigned when its driver started.
    pub luid: u64,
    /// The physical adapter's index under that id.
    pub phys: u32,
    /// The engine's index on that adapter (a node of its scheduler).
    pub eng: u32,
    /// The engine's type as the driver names it (`3D`, `VideoDecode`, `Copy`); empty where the
    /// driver names none.
    pub kind: &'a str,
}

/// A `GPU Engine` instance name,
/// `pid_<pid>_luid_0x<high>_0x<low>_phys_<n>_eng_<n>_engtype_<type>`, or `None` for any other
/// shape.
#[must_use]
pub fn engine_of(name: &str) -> Option<Engine<'_>> {
    let (pid, rest) = decimal(name.strip_prefix("pid_")?)?;
    let (luid, phys, rest) = adapter_at(rest.strip_prefix('_')?)?;
    let (eng, rest) = decimal(rest.strip_prefix("_eng_")?)?;
    let kind = rest.strip_prefix("_engtype_")?;
    Some(Engine {
        pid,
        luid,
        phys,
        eng,
        kind,
    })
}

/// A `GPU Adapter Memory` instance name, `luid_0x<high>_0x<low>_phys_<n>`, as the adapter's id
/// and the physical adapter's index; `None` for any other shape.
#[must_use]
pub fn adapter_of(name: &str) -> Option<(u64, u32)> {
    match adapter_at(name)? {
        (luid, phys, "") => Some((luid, phys)),
        _ => None,
    }
}

/// One engine instance's cumulative running time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineRow {
    /// The instance's id: [`entry_of`] its process, adapter and engine.
    pub entry: u64,
    /// The process.
    pub pid: u32,
    /// The adapter.
    pub luid: u64,
    /// The physical adapter's index.
    pub phys: u32,
    /// The engine's index.
    pub eng: u32,
    /// Time the process's contexts ran on the engine, 100 ns units, cumulative.
    pub running: u64,
}

/// Every engine instance of one collection, and when it was taken.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineRead {
    /// When the collection returned, 100 ns units on any clock that never steps back.
    pub at: u64,
    /// The instances whose names parsed.
    pub rows: Vec<EngineRow>,
}

/// An engine instance's id, the same at every read while the instance lives: FNV-1a over its
/// process, adapter, physical index and engine index.
#[must_use]
pub fn entry_of(pid: u32, luid: u64, phys: u32, eng: u32) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let bytes = pid
        .to_le_bytes()
        .into_iter()
        .chain(luid.to_le_bytes())
        .chain(phys.to_le_bytes())
        .chain(eng.to_le_bytes());
    for b in bytes {
        hash = (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The engine instances among `items` (`\GPU Engine(*)\Running Time`), read at `at`. An item
/// whose name is no engine's is left out.
#[must_use]
pub fn engine_read(items: &[RawItem], at: u64) -> EngineRead {
    let rows = items
        .iter()
        .filter_map(|item| {
            let e = engine_of(&item.name)?;
            Some(EngineRow {
                entry: entry_of(e.pid, e.luid, e.phys, e.eng),
                pid: e.pid,
                luid: e.luid,
                phys: e.phys,
                eng: e.eng,
                running: item.value,
            })
        })
        .collect();
    EngineRead { at, rows }
}

/// Each adapter's busy percent between two reads, by adapter id: on each engine the time every
/// process ran there is summed (an engine runs one context at a time, so the sum is at most the
/// window), and the adapter's figure is its busiest engine's share of the window, rounded, at
/// most 100. An instance absent from `then` was created inside the window and counts all its
/// time, at most the window; one that is gone counts nothing; a counter that fell (a pid reused)
/// counts nothing. Every adapter with an instance in `now` is listed, an idle one at 0. `None`
/// when no time passed between the reads.
#[must_use]
pub fn busy(then: &EngineRead, now: &EngineRead) -> Option<BTreeMap<u64, u8>> {
    let window = now.at.checked_sub(then.at).filter(|w| *w > 0)?;
    let before: HashMap<u64, u64> = then.rows.iter().map(|r| (r.entry, r.running)).collect();
    let mut engines: BTreeMap<(u64, u32, u32), u64> = BTreeMap::new();
    for r in &now.rows {
        let ran = match before.get(&r.entry) {
            Some(b) => r.running.saturating_sub(*b),
            None => r.running.min(window),
        };
        let sum = engines.entry((r.luid, r.phys, r.eng)).or_default();
        *sum = sum.saturating_add(ran);
    }
    let mut adapters: BTreeMap<u64, u8> = BTreeMap::new();
    for ((luid, _, _), ran) in engines {
        let ran = u128::from(ran.min(window));
        let pct = (ran * 100 + u128::from(window) / 2) / u128::from(window);
        let pct = u8::try_from(pct.min(100)).unwrap_or(100);
        let most = adapters.entry(luid).or_default();
        *most = (*most).max(pct);
    }
    Some(adapters)
}

/// Whether a read at `now` is answered by the last collection, taken at `last`: one younger than
/// [`MIN_WINDOW`] is.
#[must_use]
pub fn held(last: Option<Instant>, now: Instant) -> bool {
    last.is_some_and(|at| now.saturating_duration_since(at) < MIN_WINDOW)
}

/// What one collection makes of the baseline its window starts at ([`step`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The collection failed, or read no engine where the baseline read some (a collection that
    /// a driver restart or a session switch cut short): nothing is read this time, and the
    /// baseline, kept here, stands for the next collection, whose window then starts where this
    /// one's did.
    Missed(Option<EngineRead>),
    /// The collection read: each adapter's busy percent since the baseline ([`busy`]), and the
    /// read that is the next baseline.
    Read {
        /// `None` where no baseline can stand for the window's start: none was read, or it read
        /// no engine while this collection reads some. An instance absent from the baseline
        /// counts its whole running time, so a baseline emptied by a failed read would charge
        /// every long-lived process (the compositor among them) the whole window: the device
        /// would read 100 % busy.
        shares: Option<BTreeMap<u64, u8>>,
        /// This collection's engines.
        next: EngineRead,
    },
}

/// The collection `now` (`collected` false where it failed) against the baseline `then`. A
/// failed or empty collection never becomes a baseline where one with engines is held, and an
/// empty baseline never rates a collection with engines; two empty ones rate as an idle device.
#[must_use]
pub fn step(then: Option<EngineRead>, collected: bool, now: EngineRead) -> Step {
    let had = then.as_ref().is_some_and(|t| !t.rows.is_empty());
    if !collected || (had && now.rows.is_empty()) {
        return Step::Missed(then);
    }
    let shares = then
        .filter(|t| !t.rows.is_empty() || now.rows.is_empty())
        .and_then(|t| busy(&t, &now));
    Step::Read { shares, next: now }
}

/// DirectX's adapter rows as the GPU reader holds them, and when it read them, for which live
/// ids. The rows are a cache, not the driver's: the scheduled task
/// `\Microsoft\Windows\DirectX\DXGIAdapterCache` rewrites them minutes after boot and after a
/// driver restart or an adapter's re-enable, so until it runs they carry the ids of the adapters'
/// previous start while the counters already carry the new ones. The rows are read again when the
/// live ids change, and, while they match no live hardware adapter, [`RELIST_FIRST`] after the
/// last read, the wait doubling at each read that still matches none, up to [`RELIST_MOST`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The rows last read.
    pub rows: Vec<Adapter>,
    /// The live ids they were read for; `None` before the first read.
    pub ids: Option<BTreeSet<u64>>,
    /// When they were read.
    pub at: Option<Instant>,
    /// The reads in a row, for the same live ids, whose rows matched no live hardware adapter.
    pub misses: u32,
}

/// Whether the registry rows `rows` name an adapter to report among the live ids `live`
/// ([`choose`]), or no id is live, so that reading them again cannot name one.
#[must_use]
pub fn matches(rows: &[Adapter], live: &BTreeSet<u64>) -> bool {
    live.is_empty() || choose(rows, live, None).is_some()
}

impl Listing {
    /// How long after the last read rows that match no live hardware adapter are read again.
    #[must_use]
    pub fn wait(&self) -> Duration {
        RELIST_FIRST
            .saturating_mul(1u32 << self.misses.min(16))
            .min(RELIST_MOST)
    }

    /// Whether the rows are read again for the live ids `live` at `now`.
    #[must_use]
    pub fn due(&self, live: &BTreeSet<u64>, now: Instant) -> bool {
        if self.ids.as_ref() != Some(live) {
            return true;
        }
        !matches(&self.rows, live)
            && self
                .at
                .is_none_or(|at| now.saturating_duration_since(at) >= self.wait())
    }

    /// The rows for the live ids `live` at `now`: those held, or `read`'s (the registry's) where
    /// they are due again ([`Self::due`]).
    pub fn rows(
        &mut self,
        live: &BTreeSet<u64>,
        now: Instant,
        read: impl FnOnce() -> Vec<Adapter>,
    ) -> &[Adapter] {
        if self.due(live, now) {
            let rows = read();
            let again = self.ids.as_ref() == Some(live);
            self.misses = if again && !matches(&rows, live) {
                self.misses.saturating_add(1)
            } else {
                0
            };
            self.rows = rows;
            self.ids = Some(live.clone());
            self.at = Some(now);
        }
        &self.rows
    }
}

/// One adapter's memory levels, its physical adapters summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdapterMemory {
    /// `Dedicated Usage`: bytes of the adapter's own memory in use.
    pub dedicated: u64,
    /// `Shared Usage`: bytes of system memory the adapter is using.
    pub shared: u64,
    /// `Total Committed`: bytes the video memory manager has committed for the adapter.
    pub committed: u64,
}

/// Each adapter's memory levels from the three `GPU Adapter Memory` counters' items, by adapter
/// id. An item whose name is no adapter's is left out.
#[must_use]
pub fn adapter_memory(
    dedicated: &[RawItem],
    shared: &[RawItem],
    committed: &[RawItem],
) -> BTreeMap<u64, AdapterMemory> {
    let mut out: BTreeMap<u64, AdapterMemory> = BTreeMap::new();
    let mut add = |items: &[RawItem], field: fn(&mut AdapterMemory) -> &mut u64| {
        for item in items {
            if let Some((luid, _)) = adapter_of(&item.name) {
                let level = field(out.entry(luid).or_default());
                *level = level.saturating_add(item.value);
            }
        }
    };
    add(dedicated, |m| &mut m.dedicated);
    add(shared, |m| &mut m.shared);
    add(committed, |m| &mut m.committed);
    out
}

/// One adapter as DirectX lists it in the registry
/// (`HKLM\SOFTWARE\Microsoft\DirectX\{…}`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adapter {
    /// `AdapterLuid`: the id the counters' instances carry, new at every driver start.
    pub luid: u64,
    /// `Description`: the adapter's name (`AMD Radeon 780M Graphics`).
    pub name: String,
    /// `VendorId`: the PCI vendor.
    pub vendor: u32,
    /// `DedicatedVideoMemory`: bytes of memory of its own.
    pub dedicated: u64,
    /// `DriverVersion`: four 16-bit parts, where the row has one.
    pub driver: Option<u64>,
    /// `LastSeen`: when DirectX last wrote the row, a `FILETIME`; 0 where unread.
    pub seen: u64,
}

/// A driver version as Windows prints it: the `u64`'s four 16-bit parts, highest first
/// (`32.0.22029.15003`).
#[must_use]
pub fn driver_version(version: u64) -> String {
    format!(
        "{}.{}.{}.{}",
        version >> 48,
        (version >> 32) & 0xffff,
        (version >> 16) & 0xffff,
        version & 0xffff
    )
}

/// The adapter a single device-wide reading speaks for: its id, and its registry row where
/// DirectX lists one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chosen<'a> {
    /// The adapter's id.
    pub luid: u64,
    /// Its registry row.
    pub adapter: Option<&'a Adapter>,
}

/// The adapter to report among those with a counter instance now (`live`), given the registry's
/// rows and each adapter's busy percent. A registry row counts when its id is live (a row left by
/// an earlier boot names an id that is gone; of two rows with one id the later-seen stands), and
/// a row of [`VENDOR_MICROSOFT`] is the software rasteriser, never the machine's GPU. Of the
/// hardware adapters the busiest is chosen, then the one with the most memory of its own, then
/// the lowest id, so a machine with one GPU always names it and a hybrid one names whichever is
/// working. `None` when the listed adapters are software only. Where the registry lists no
/// adapter at all (a Windows before DirectX kept the rows), the live ids stand unnamed.
#[must_use]
pub fn choose<'a>(
    adapters: &'a [Adapter],
    live: &BTreeSet<u64>,
    busy: Option<&BTreeMap<u64, u8>>,
) -> Option<Chosen<'a>> {
    let pct = |luid: u64| busy.and_then(|b| b.get(&luid)).copied().unwrap_or(0);
    if adapters.is_empty() {
        return live
            .iter()
            .copied()
            .max_by_key(|luid| (pct(*luid), std::cmp::Reverse(*luid)))
            .map(|luid| Chosen {
                luid,
                adapter: None,
            });
    }
    let mut known: BTreeMap<u64, &Adapter> = BTreeMap::new();
    for a in adapters.iter().filter(|a| live.contains(&a.luid)) {
        let row = known.entry(a.luid).or_insert(a);
        if a.seen > row.seen {
            *row = a;
        }
    }
    known
        .into_values()
        .filter(|a| a.vendor != VENDOR_MICROSOFT)
        .max_by_key(|a| (pct(a.luid), a.dedicated, std::cmp::Reverse(a.luid)))
        .map(|a| Chosen {
            luid: a.luid,
            adapter: Some(a),
        })
}

/// The device-wide reading of `chosen`: its busy percent from `busy` (an adapter with no engine
/// instance ran nothing: 0), unread as [`Why::NotRead`] while only one collection has been taken
/// (`busy` is `None`); its memory in use (`Dedicated Usage` plus `Shared Usage`, what Task
/// Manager calls GPU memory) and committed (`Total Committed`); its name; and whether it has no
/// memory of its own, where the registry says.
#[must_use]
pub fn reading(
    chosen: Chosen<'_>,
    busy: Option<&BTreeMap<u64, u8>>,
    memory: &BTreeMap<u64, AdapterMemory>,
) -> GpuReading {
    let device_pct = busy.map(|b| b.get(&chosen.luid).copied().unwrap_or(0));
    let mem = memory.get(&chosen.luid);
    GpuReading {
        device_pct,
        in_use_bytes: mem.map(|m| m.dedicated.saturating_add(m.shared)),
        alloc_bytes: mem.map(|m| m.committed),
        name: chosen
            .adapter
            .map(|a| a.name.trim().to_owned())
            .filter(|name| !name.is_empty()),
        unread: device_pct.is_none().then_some(Why::NotRead),
        unified: chosen.adapter.is_some_and(|a| a.dedicated == 0),
        ..GpuReading::default()
    }
}

/// Every engine instance as a GPU client: its id, its process, that process's name from `name`
/// (empty where the process is gone or unlisted) and its cumulative GPU time in ns. An instance
/// of pid 0 is listed in [`GpuClients::entries`] only. At most [`MAX_GPU_CLIENTS`] instances are
/// walked; past it the read says `truncated` and is not `complete`.
#[must_use]
pub fn clients(read: &EngineRead, name: impl Fn(u32) -> Option<String>) -> GpuClients {
    let truncated = read.rows.len() > MAX_GPU_CLIENTS;
    let mut out = GpuClients {
        children: u32::try_from(read.rows.len().min(MAX_GPU_CLIENTS)).unwrap_or(u32::MAX),
        truncated,
        complete: !truncated,
        ..GpuClients::default()
    };
    let mut names: HashMap<u32, String> = HashMap::new();
    for r in read.rows.iter().take(MAX_GPU_CLIENTS) {
        out.entries.push(r.entry);
        if r.pid == 0 {
            continue;
        }
        let name = names
            .entry(r.pid)
            .or_insert_with(|| name(r.pid).unwrap_or_default())
            .clone();
        out.rows.push(GpuClient {
            entry: r.entry,
            pid: r.pid,
            name,
            gpu_ns: r.running.saturating_mul(100),
        });
    }
    out
}

/// The throttle level of the thermal zones' `% Passive Limit` values: the share of full
/// performance the most constrained zone allows the devices it cools (100 is unconstrained).
/// The zone's limit is the kernel's own; the level is this reader's banding of it (below 100
/// light, below 80 heavy, below 50 severe), so the reading is never `exact`.
///
/// # Errors
/// [`Why::Absent`] when the machine lists no thermal zone.
pub fn throttle_of(passive_limits: &[u64]) -> Result<Throttle, Why> {
    let limit = passive_limits.iter().copied().min().ok_or(Why::Absent)?;
    let level = match limit {
        100.. => ThrottleLevel::Nominal,
        80..=99 => ThrottleLevel::Light,
        50..=79 => ThrottleLevel::Heavy,
        _ => ThrottleLevel::Severe,
    };
    Ok(Throttle {
        level,
        exact: false,
    })
}

#[cfg(test)]
#[path = "win_gpu_tests.rs"]
mod tests;
