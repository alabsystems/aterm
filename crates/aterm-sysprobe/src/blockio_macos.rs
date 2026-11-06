// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`crate::blockio`] on macOS: the live IORegistry a walk climbs, and the reader that keeps
//! each volume's drivers and copies their `Statistics` at each reading.

use std::ffi::CString;

use crate::blockio::{
    BlockCounters, BlockReading, Device, DeviceWords, KEPT_VOLUMES, Prop, Registry,
    STATISTICS_KEYS, counters_of, device_of_walk, drivers_above,
};
use crate::iokit::{self, MAIN_PORT_DEFAULT, io_object_t};
use crate::macos::{Cf, Io, cf_text, cf_u64};
use crate::volumes::Mount;

/// The most parents one entry's iterator gives a walk.
const MAX_PARENTS: usize = 16;

/// The registry keys a walk and a reading read, made once per reader.
struct Keys {
    statistics: Cf,
    stats: [Cf; 6],
    uuid: Cf,
    bsd: Cf,
    device: Cf,
    protocol: Cf,
    product: Cf,
    serial: Cf,
    location: Cf,
}

impl Keys {
    fn new() -> Option<Self> {
        let stat = |i: usize| {
            CString::new(STATISTICS_KEYS[i])
                .ok()
                .and_then(|k| Cf::string(&k))
        };
        Some(Self {
            statistics: Cf::string(c"Statistics")?,
            stats: [stat(0)?, stat(1)?, stat(2)?, stat(3)?, stat(4)?, stat(5)?],
            uuid: Cf::string(c"UUID")?,
            bsd: Cf::string(c"BSD Name")?,
            device: Cf::string(c"Device Characteristics")?,
            protocol: Cf::string(c"Protocol Characteristics")?,
            product: Cf::string(c"Product Name")?,
            serial: Cf::string(c"Serial Number")?,
            location: Cf::string(c"Physical Interconnect Location")?,
        })
    }

    /// Text value `key` of dictionary property `dict` of `entry`.
    fn dict_text(entry: io_object_t, dict: &Cf, key: &Cf) -> Option<String> {
        let d = Cf::property(entry, dict.0)?;
        // SAFETY: `d` is live; its type is checked before it is read as a dictionary, and the
        // value is borrowed (the Get rule) while `d` lives, which `cf_text` copies out of.
        unsafe {
            if iokit::CFGetTypeID(d.0) != iokit::CFDictionaryGetTypeID() {
                return None;
            }
            cf_text(iokit::CFDictionaryGetValue(d.0.cast(), key.0))
        }
    }
}

/// The live service plane.
struct Live<'k>(&'k Keys);

impl Registry for Live<'_> {
    type Entry = Io;

    fn id(&self, e: &Io) -> u64 {
        let mut id = 0u64;
        // SAFETY: `e` is a live entry and `id` writable.
        let kr = unsafe { iokit::IORegistryEntryGetRegistryEntryID(e.0, &raw mut id) };
        if kr == 0 { id } else { u64::from(e.0) }
    }

    fn parents(&self, e: &Io) -> Vec<Io> {
        let mut iter: io_object_t = 0;
        // SAFETY: `e` is a live entry, the plane name NUL-terminated, `iter` writable; on success
        // the iterator is +1, released by `Io`.
        let kr = unsafe {
            iokit::IORegistryEntryGetParentIterator(e.0, c"IOService".as_ptr(), &raw mut iter)
        };
        if kr != 0 || iter == 0 {
            return Vec::new();
        }
        let iter = Io(iter);
        let mut out = Vec::new();
        while out.len() < MAX_PARENTS {
            // SAFETY: `iter` is a live iterator; each parent is +1, or 0 at the end.
            let p = unsafe { iokit::IOIteratorNext(iter.0) };
            if p == 0 {
                break;
            }
            out.push(Io(p));
        }
        out
    }

    fn is_driver(&self, e: &Io) -> bool {
        // SAFETY: `e` is a live object and the class name NUL-terminated.
        unsafe { iokit::IOObjectConformsTo(e.0, c"IOBlockStorageDriver".as_ptr()) != 0 }
    }

    fn text(&self, e: &Io, p: Prop) -> Option<String> {
        let key = match p {
            Prop::Uuid => &self.0.uuid,
            Prop::Bsd => &self.0.bsd,
        };
        Cf::property(e.0, key.0).and_then(|v| cf_text(v.0))
    }

    fn device(&self, e: &Io) -> DeviceWords {
        let Some(dev) = self.parents(e).into_iter().next() else {
            return DeviceWords::default();
        };
        let k = self.0;
        DeviceWords {
            product: Keys::dict_text(dev.0, &k.device, &k.product),
            serial: Keys::dict_text(dev.0, &k.device, &k.serial),
            location: Keys::dict_text(dev.0, &k.protocol, &k.location),
        }
    }
}

/// One kept volume: its mount source, its drivers (+1, with their ids) and the device they make.
struct Kept {
    from: String,
    drivers: Vec<(Io, u64)>,
    device: Device,
}

/// The macOS arm of [`crate::blockio::BlockReader`].
pub(crate) struct Reader {
    keys: Option<Keys>,
    /// Most recently asked first, at most [`KEPT_VOLUMES`].
    kept: Vec<Kept>,
}

// SAFETY: The keys are immutable CFStrings and the drivers Mach port names, all owned by this
// reader and usable from any thread; Rust dereferences none of them, and the reader is used by
// one thread at a time (`&mut self`).
unsafe impl Send for Reader {}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field(
                "kept",
                &self
                    .kept
                    .iter()
                    .map(|k| (&k.from, &k.device))
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Reader {
    pub(crate) fn new() -> Self {
        Self {
            keys: Keys::new(),
            kept: Vec::new(),
        }
    }

    /// One reading per device of the mounts that was found and read, in their order: a mount
    /// whose device an earlier one read gives none.
    pub(crate) fn read(&mut self, mounts: &[&Mount]) -> Vec<BlockReading> {
        let Some(keys) = self.keys.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for m in mounts {
            let Some(bsd) = m.from.strip_prefix("/dev/").filter(|b| !b.is_empty()) else {
                continue;
            };
            let kept = match self.kept.iter().position(|k| k.from == m.from) {
                Some(i) => self.kept.remove(i),
                None => match walk(keys, &m.from, bsd) {
                    Some(k) => k,
                    None => continue,
                },
            };
            // Another volume of a device this reading already copied (one of its container's,
            // or the same mount asked twice) copies its drivers' `Statistics` no second time.
            if out.iter().any(|r: &BlockReading| r.id == kept.device.id) {
                self.kept.insert(0, kept);
                self.kept.truncate(KEPT_VOLUMES);
                continue;
            }
            let counters = kept
                .drivers
                .iter()
                .try_fold(BlockCounters::default(), |sum, d| {
                    Some(sum.plus(statistics(keys, d.0.0)?))
                });
            // A driver that stopped answering (its disk went) is walked again next time.
            let Some(counters) = counters else {
                continue;
            };
            out.push(kept.device.reading(counters));
            self.kept.insert(0, kept);
            self.kept.truncate(KEPT_VOLUMES);
        }
        out
    }
}

/// Walks from BSD device `bsd` (mount source `from`) to its drivers.
fn walk(keys: &Keys, from: &str, bsd: &str) -> Option<Kept> {
    let name = CString::new(bsd).ok()?;
    // SAFETY: `name` is NUL-terminated; the dictionary is +1 or NULL.
    let matching = unsafe { iokit::IOBSDNameMatching(MAIN_PORT_DEFAULT, 0, name.as_ptr()) };
    if matching.is_null() {
        return None;
    }
    let mut iter: io_object_t = 0;
    // SAFETY: `matching` is +1 and consumed by the call whatever it returns; `iter` is writable
    // and +1 on success.
    let kr = unsafe {
        iokit::IOServiceGetMatchingServices(MAIN_PORT_DEFAULT, matching.cast_const(), &raw mut iter)
    };
    if kr != 0 || iter == 0 {
        return None;
    }
    let iter = Io(iter);
    // SAFETY: `iter` is live; the media is +1 or 0.
    let media = unsafe { iokit::IOIteratorNext(iter.0) };
    if media == 0 {
        return None;
    }
    let live = Live(keys);
    let stores = drivers_above(&live, Io(media));
    let device = device_of_walk(&live, &stores)?;
    Some(Kept {
        from: from.to_owned(),
        drivers: stores.into_iter().map(|s| (s.entry, s.id)).collect(),
        device,
    })
}

/// Driver `driver`'s `Statistics`, copied once.
fn statistics(keys: &Keys, driver: io_object_t) -> Option<BlockCounters> {
    let stats = Cf::property(driver, keys.statistics.0)?;
    // SAFETY: `stats` is live; its type is checked before it is read as a dictionary, and each
    // value is borrowed (the Get rule) while it lives, read out by `cf_u64`.
    unsafe {
        if iokit::CFGetTypeID(stats.0) != iokit::CFDictionaryGetTypeID() {
            return None;
        }
        let value = |key: &Cf| cf_u64(iokit::CFDictionaryGetValue(stats.0.cast(), key.0));
        let k = &keys.stats;
        counters_of([
            value(&k[0]),
            value(&k[1]),
            value(&k[2]),
            value(&k[3]),
            value(&k[4]),
            value(&k[5]),
        ])
    }
}
