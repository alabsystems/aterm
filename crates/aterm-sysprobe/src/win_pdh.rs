// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows GPU and thermal readers' calls: the Performance Data Helper's raw counters
//! (`\GPU Engine(*)\Running Time`, `\GPU Adapter Memory(*)`, `\Thermal Zone Information(*)\%
//! Passive Limit`), which a process without privilege reads, and the adapters DirectX lists in
//! the registry. What they return is decoded by `win_gpu.rs` (design
//! `docs/DESIGN-system-awareness-2026-10-16.md` §8.2, §16).
//!
//! `pdh.dll` is loaded at the first read, never imported: a process that reads no GPU (every
//! `aterm` verb but the few that ask) maps none of it. Each reader keeps one query open for the
//! process's life, so a read is one `PdhCollectQueryData` and never a reopen, and no reader here
//! holds a timer or a thread: a read happens when a caller asks.

#![allow(
    non_snake_case,
    clippy::upper_case_acronyms,
    reason = "Win32 names are kept verbatim (`HANDLE`, `LoadLibraryExW`) to check against the SDK"
)]

use std::collections::{BTreeSet, HashMap};
use std::ffi::{CStr, c_void};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::win::{as_bytes, reg_dword, reg_qword, reg_string, wide_z};
use crate::win_gpu::{
    Adapter, EngineRead, Listing, RawItem, Step, adapter_memory, choose, clients, driver_version,
    engine_read, held, raw_items, reading, step, throttle_of,
};
use crate::{GpuClients, GpuDriver, GpuReading, Throttle, Why};

type HANDLE = isize;
/// Any export, before it is given its signature.
type Export = unsafe extern "system" fn() -> isize;

/// `LOAD_LIBRARY_SEARCH_SYSTEM32`: the system directory only, never the application's or the
/// working directory.
const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;
/// `PDH_MORE_DATA`: the buffer is smaller than the items.
const PDH_MORE_DATA: u32 = 0x8000_07D2;
/// The most bytes of one counter's items that are read.
const ITEMS_CAP: usize = 64 << 20;
/// `KEY_READ`: query a key's values and enumerate its subkeys.
const KEY_READ: u32 = 0x0002_0019;
/// Where DirectX keeps a row per adapter it has seen, each under a GUID.
const DIRECTX_KEY: &str = r"SOFTWARE\Microsoft\DirectX";
/// The most adapter rows read: a machine has a handful, and rows of earlier boots stay.
const ADAPTER_ROWS: u32 = 64;

/// Between the two collections of a process's first read: a rate needs two, and a caller that
/// reads once (a CLI verb) is owed a figure, so the first read alone waits this long.
const PRIME: Duration = Duration::from_millis(50);

#[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn LoadLibraryExW(lpLibFileName: *const u16, hFile: HANDLE, dwFlags: u32) -> HANDLE;
    fn GetProcAddress(hModule: HANDLE, lpProcName: *const u8) -> Option<Export>;
}

#[link(name = "advapi32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        hKey: HANDLE,
        lpSubKey: *const u16,
        ulOptions: u32,
        samDesired: u32,
        phkResult: *mut HANDLE,
    ) -> i32;
    fn RegEnumKeyExW(
        hKey: HANDLE,
        dwIndex: u32,
        lpName: *mut u16,
        lpcchName: *mut u32,
        lpReserved: *mut u32,
        lpClass: *mut u16,
        lpcchClass: *mut u32,
        lpftLastWriteTime: *mut c_void,
    ) -> i32;
    fn RegCloseKey(hKey: HANDLE) -> i32;
}

type OpenQuery = unsafe extern "system" fn(*const u16, usize, *mut HANDLE) -> u32;
type AddCounter = unsafe extern "system" fn(HANDLE, *const u16, usize, *mut HANDLE) -> u32;
type CollectQueryData = unsafe extern "system" fn(HANDLE) -> u32;
type GetRawCounterArray = unsafe extern "system" fn(HANDLE, *mut u32, *mut u32, *mut c_void) -> u32;
type CloseQuery = unsafe extern "system" fn(HANDLE) -> u32;

/// The five `pdh.dll` entry points the readers call.
#[derive(Debug)]
struct Pdh {
    /// `PdhOpenQueryW`.
    open: OpenQuery,
    /// `PdhAddEnglishCounterW`: the path in English whatever the display language.
    add: AddCounter,
    /// `PdhCollectQueryData`.
    collect: CollectQueryData,
    /// `PdhGetRawCounterArrayW`.
    raw_array: GetRawCounterArray,
    /// `PdhCloseQuery`.
    close: CloseQuery,
}

fn load() -> Option<Pdh> {
    let name = wide_z("pdh.dll");
    // SAFETY: `name` is a NUL-terminated wide string; the file handle is reserved (0).
    let module = unsafe { LoadLibraryExW(name.as_ptr(), 0, LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module == 0 {
        return None;
    }
    let export = |name: &CStr| {
        // SAFETY: a live module handle (never freed) and a NUL-terminated export name.
        unsafe { GetProcAddress(module, name.as_ptr().cast()) }
    };
    // SAFETY (each transmute): the export has the signature `pdh.h` declares for its name, and
    // function pointers of one ABI have one representation.
    Some(Pdh {
        open: unsafe { std::mem::transmute::<Export, OpenQuery>(export(c"PdhOpenQueryW")?) },
        add: unsafe {
            std::mem::transmute::<Export, AddCounter>(export(c"PdhAddEnglishCounterW")?)
        },
        collect: unsafe {
            std::mem::transmute::<Export, CollectQueryData>(export(c"PdhCollectQueryData")?)
        },
        raw_array: unsafe {
            std::mem::transmute::<Export, GetRawCounterArray>(export(c"PdhGetRawCounterArrayW")?)
        },
        close: unsafe { std::mem::transmute::<Export, CloseQuery>(export(c"PdhCloseQuery")?) },
    })
}

fn pdh() -> Option<&'static Pdh> {
    static PDH: OnceLock<Option<Pdh>> = OnceLock::new();
    PDH.get_or_init(load).as_ref()
}

/// One open query on the live counters, closed on drop, with the buffer its items are read into.
#[derive(Debug)]
struct Query {
    pdh: &'static Pdh,
    handle: HANDLE,
    buf: Vec<u64>,
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: the handle `PdhOpenQueryW` returned, closed once; its counters go with it.
        unsafe { (self.pdh.close)(self.handle) };
    }
}

impl Query {
    fn open() -> Option<Self> {
        let pdh = pdh()?;
        let mut handle: HANDLE = 0;
        // SAFETY: a null data source is the live counters; `handle` is a writable handle.
        if unsafe { (pdh.open)(std::ptr::null(), 0, &raw mut handle) } != 0 {
            return None;
        }
        Some(Self {
            pdh,
            handle,
            buf: Vec::new(),
        })
    }

    /// Add the counter at the English `path` (a wildcard instance stands for every instance
    /// at each collection); `None` where this Windows has no such counter.
    fn add(&self, path: &str) -> Option<HANDLE> {
        let path = wide_z(path);
        let mut counter: HANDLE = 0;
        // SAFETY: a live query, a NUL-terminated wide path and a writable handle.
        let status = unsafe { (self.pdh.add)(self.handle, path.as_ptr(), 0, &raw mut counter) };
        (status == 0).then_some(counter)
    }

    /// Read every counter of the query once; false where none had data.
    fn collect(&self) -> bool {
        // SAFETY: a live query.
        unsafe { (self.pdh.collect)(self.handle) == 0 }
    }

    /// The items `counter` held at the last collection; empty where it had none.
    fn items(&mut self, counter: HANDLE) -> Vec<RawItem> {
        for _ in 0..4 {
            let mut count = 0u32;
            if self.buf.is_empty() {
                let mut size = 0u32;
                // SAFETY: a counter of this query; a null buffer of size 0 asks only for the
                // size, written to `size` with the item count in `count`.
                let status = unsafe {
                    (self.pdh.raw_array)(
                        counter,
                        &raw mut size,
                        &raw mut count,
                        std::ptr::null_mut(),
                    )
                };
                let bytes = size as usize;
                if status != PDH_MORE_DATA || bytes == 0 || bytes > ITEMS_CAP {
                    return Vec::new();
                }
                let words = bytes.div_ceil(8);
                self.buf.resize(words + words / 4 + 64, 0);
                continue;
            }
            let Ok(mut size) = u32::try_from(self.buf.len() * 8) else {
                return Vec::new();
            };
            // SAFETY: a counter of this query; `buf` is `size` writable, 8-byte-aligned bytes,
            // and the call sets `size` to the bytes it wrote and `count` to the items.
            let status = unsafe {
                (self.pdh.raw_array)(
                    counter,
                    &raw mut size,
                    &raw mut count,
                    self.buf.as_mut_ptr().cast(),
                )
            };
            if status == PDH_MORE_DATA {
                self.buf.clear();
                continue;
            }
            if status != 0 {
                return Vec::new();
            }
            let n = (size as usize).min(self.buf.len() * 8);
            let Ok(base) = u64::try_from(self.buf.as_ptr().addr()) else {
                return Vec::new();
            };
            return raw_items(&as_bytes(&self.buf)[..n], base, count as usize);
        }
        Vec::new()
    }
}

/// A registry key this process opened, closed on drop.
struct RegKey(HANDLE);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: the handle `RegOpenKeyExW` returned, closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The names of the `HKEY_LOCAL_MACHINE` key `key`'s subkeys, the first `most` of them; empty
/// where the key is missing or unreadable.
fn reg_subkeys(key: &str, most: u32) -> Vec<String> {
    let key = wide_z(key);
    let mut opened: HANDLE = 0;
    // SAFETY: `key` is a NUL-terminated wide string; `opened` is a writable handle.
    let status = unsafe {
        RegOpenKeyExW(
            crate::win::HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            0,
            KEY_READ,
            &raw mut opened,
        )
    };
    if status != 0 {
        return Vec::new();
    }
    let opened = RegKey(opened);
    let mut names = Vec::new();
    for index in 0..most {
        // A key's name is at most 255 units.
        let mut buf = [0u16; 256];
        let mut len = 256u32;
        // SAFETY: `opened` is a live key; `buf` is writable for `len` units, which the call
        // sets to the name's length without its terminator; every other output is null (not
        // asked for).
        let status = unsafe {
            RegEnumKeyExW(
                opened.0,
                index,
                buf.as_mut_ptr(),
                &raw mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(
            &buf[..(len as usize).min(buf.len())],
        ));
    }
    names
}

/// Every adapter DirectX lists in the registry: registry reads only, so no device is opened and
/// none is woken.
pub(crate) fn adapters() -> Vec<Adapter> {
    reg_subkeys(DIRECTX_KEY, ADAPTER_ROWS)
        .iter()
        .filter_map(|sub| {
            let key = format!("{DIRECTX_KEY}\\{sub}");
            Some(Adapter {
                luid: reg_qword(&key, "AdapterLuid")?,
                name: reg_string(&key, "Description").unwrap_or_default(),
                vendor: reg_dword(&key, "VendorId").unwrap_or(0),
                dedicated: reg_qword(&key, "DedicatedVideoMemory").unwrap_or(0),
                driver: reg_qword(&key, "DriverVersion"),
                seen: reg_qword(&key, "LastSeen").unwrap_or(0),
            })
        })
        .collect()
}

/// 100 ns units since this process first read a counter: the clock the collections are stamped
/// on (PDH's own stamp is local time, which steps at a clock change).
fn now_100ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let since = EPOCH.get_or_init(Instant::now).elapsed();
    u64::try_from(since.as_nanos() / 100).unwrap_or(u64::MAX)
}

/// The GPU reader's open query and what it keeps between reads.
struct Gpu {
    query: Query,
    /// `\GPU Engine(*)\Running Time`.
    running: HANDLE,
    /// `\GPU Adapter Memory(*)\Dedicated Usage`, `Shared Usage` and `Total Committed`, each
    /// where this Windows has it.
    memory: [Option<HANDLE>; 3],
    /// The registry's adapter rows: read again when the live ids change (a driver restarted, an
    /// adapter came or went), and while they name no live hardware adapter ([`Listing`]).
    listing: Listing,
    /// When the last collection was taken, and the baseline the next one's window starts at
    /// (the last collection that read, [`step`]); `None` before the first collection.
    last: Option<(Instant, Option<EngineRead>)>,
    /// What the last collection read: the device-wide reading and its adapter's driver version.
    reading: Option<GpuReading>,
    driver: Option<u64>,
}

impl Gpu {
    fn open() -> Option<Self> {
        let query = Query::open()?;
        let running = query.add(r"\GPU Engine(*)\Running Time")?;
        let memory = [
            query.add(r"\GPU Adapter Memory(*)\Dedicated Usage"),
            query.add(r"\GPU Adapter Memory(*)\Shared Usage"),
            query.add(r"\GPU Adapter Memory(*)\Total Committed"),
        ];
        Some(Self {
            query,
            running,
            memory,
            listing: Listing::default(),
            last: None,
            reading: None,
            driver: None,
        })
    }

    /// One collection's engines, and whether the collection read any counter at all (it reads
    /// none on a machine with no adapter, and then the counters' last items are stale).
    fn engines(&mut self) -> (bool, EngineRead) {
        let collected = self.query.collect();
        let items = if collected {
            self.query.items(self.running)
        } else {
            Vec::new()
        };
        (collected, engine_read(&items, now_100ns()))
    }

    /// Collect, unless the last collection is younger than [`crate::win_gpu::MIN_WINDOW`]
    /// ([`held`]). The process's first read collects twice, [`PRIME`] apart, so it too has a
    /// window. A collection that fails, or reads no engine where the baseline read some, reads
    /// nothing and keeps the baseline ([`step`]).
    fn refresh(&mut self) {
        if held(self.last.as_ref().map(|(at, _)| *at), Instant::now()) {
            return;
        }
        let then = match self.last.take() {
            Some((_, then)) => then,
            None => {
                let (collected, first) = self.engines();
                std::thread::sleep(PRIME);
                collected.then_some(first)
            }
        };
        let (collected, now) = self.engines();
        let (shares, now) = match step(then, collected, now) {
            Step::Missed(kept) => {
                self.reading = None;
                self.driver = None;
                self.last = Some((Instant::now(), kept));
                return;
            }
            Step::Read { shares, next } => (shares, next),
        };
        let [dedicated, shared, committed] = self
            .memory
            .map(|counter| counter.map_or_else(Vec::new, |c| self.query.items(c)));
        let memory = adapter_memory(&dedicated, &shared, &committed);
        let live: BTreeSet<u64> = now
            .rows
            .iter()
            .map(|r| r.luid)
            .chain(memory.keys().copied())
            .collect();
        let rows = self.listing.rows(&live, Instant::now(), adapters);
        let chosen = choose(rows, &live, shares.as_ref());
        self.reading = chosen.map(|c| reading(c, shares.as_ref(), &memory));
        self.driver = chosen.and_then(|c| c.adapter).and_then(|a| a.driver);
        self.last = Some((Instant::now(), Some(now)));
    }
}

/// The process's one GPU reader: `None` until the first read opens it, then `Some(None)` where
/// it could not be opened.
static GPU: Mutex<Option<Option<Gpu>>> = Mutex::new(None);

/// `f` over the process's one GPU reader, opened at the first call; `None` where `pdh.dll` or
/// the `GPU Engine` counters are missing (a Windows before 10 1709, or no WDDM 2 driver).
fn with_gpu<T>(f: impl FnOnce(&mut Gpu) -> Option<T>) -> Option<T> {
    let mut state = GPU.lock().unwrap_or_else(PoisonError::into_inner);
    state.get_or_insert_with(Gpu::open).as_mut().and_then(f)
}

/// [`crate::gpu`] on Windows: the busiest hardware adapter's busy share since the last read,
/// its memory in use and committed, and its name.
pub(crate) fn gpu() -> Option<GpuReading> {
    with_gpu(|gpu| {
        gpu.refresh();
        gpu.reading.clone()
    })
}

/// [`crate::gpu_if_read`] on Windows: [`gpu`] where an earlier read opened the reader; `None`,
/// opening nothing, before any has.
pub(crate) fn gpu_if_open() -> Option<GpuReading> {
    let mut state = GPU.lock().unwrap_or_else(PoisonError::into_inner);
    let gpu = state.as_mut()?.as_mut()?;
    gpu.refresh();
    gpu.reading.clone()
}

/// [`crate::gpu_driver`] on Windows: the reported adapter's driver version.
pub(crate) fn gpu_driver() -> Option<GpuDriver> {
    with_gpu(|gpu| {
        gpu.refresh();
        gpu.driver.map(|version| GpuDriver {
            bundle: None,
            version: Some(driver_version(version)),
        })
    })
}

/// [`crate::gpu_clients`] on Windows: every engine instance of the last collection (at most
/// [`crate::win_gpu::MIN_WINDOW`] old), each with its process's name from one sweep; `None`
/// where [`gpu`] reads no device.
pub(crate) fn gpu_clients() -> Option<GpuClients> {
    let read = with_gpu(|gpu| {
        gpu.refresh();
        gpu.reading.as_ref()?;
        gpu.last.as_ref().and_then(|(_, read)| read.clone())
    })?;
    let names: HashMap<u32, String> = crate::win::processes()
        .map(|sweep| sweep.procs.into_iter().map(|p| (p.pid, p.name)).collect())
        .unwrap_or_default();
    Some(clients(&read, |pid| names.get(&pid).cloned()))
}

/// The thermal reader's open query and its `\Thermal Zone Information(*)\% Passive Limit`.
struct Thermal {
    query: Query,
    passive: HANDLE,
}

impl Thermal {
    fn open() -> Option<Self> {
        let query = Query::open()?;
        let passive = query.add(r"\Thermal Zone Information(*)\% Passive Limit")?;
        Some(Self { query, passive })
    }
}

/// [`crate::Probe::throttle`] on Windows: the most constrained thermal zone's passive limit as
/// a level ([`throttle_of`]). Windows states no CPU temperature and no thermal level to a
/// process without privilege; what it does state is how far each ACPI thermal zone is holding
/// back the devices it cools, which is 100 % on a machine whose firmware throttles by itself.
///
/// # Errors
/// [`Why::Unsupported`] where `pdh.dll` does not load; [`Why::Absent`] where the machine lists
/// no thermal zone.
pub(crate) fn throttle() -> Result<Throttle, Why> {
    static THERMAL: Mutex<Option<Option<Thermal>>> = Mutex::new(None);
    if pdh().is_none() {
        return Err(Why::Unsupported);
    }
    let mut state = THERMAL.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(thermal) = state.get_or_insert_with(Thermal::open).as_mut() else {
        return Err(Why::Absent);
    };
    let limits: Vec<u64> = if thermal.query.collect() {
        let passive = thermal.passive;
        thermal
            .query
            .items(passive)
            .iter()
            .map(|item| item.value)
            .collect()
    } else {
        Vec::new()
    };
    throttle_of(&limits)
}
