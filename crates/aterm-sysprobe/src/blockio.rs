// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The physical devices volumes are stored on, and each one's I/O since boot (stats deck
//! amendment A1 §4.4, step R4): one [`BlockReading`] per device, in the order the mounts were
//! asked, home's first.
//!
//! - macOS: each mount's BSD device (`/dev/disk3s5`) walks up the IORegistry's service plane
//!   through every provider (the APFS volume, its container, the container's physical store, the
//!   partition scheme, the whole disk) to the `IOBlockStorageDriver`s above it, in process and
//!   unprivileged ([`drivers_above`]); a reading copies each driver's `Statistics` (bytes,
//!   operations and total time, per direction). A walk costs about a millisecond and a
//!   `Statistics` copy about 60 µs (R1's M-IO), so a reader keeps each volume's drivers
//!   ([`KEPT_VOLUMES`]) and walks again only for a volume it does not keep or whose driver
//!   stopped answering.
//! - Linux: the whole disks under each mount's block device ([`whole_disks`]: a partition's
//!   disk, a device-mapper or md device's slaves' disks, through sysfs), their
//!   `/proc/diskstats` counters ([`diskstats`]) summed, sectors × 512 bytes. The reader keeps
//!   each volume's disks too, and walks again for one whose disk changed or left diskstats.
//! - Elsewhere nothing: a Windows Disk row draws no units, its % and strip unchanged (A1 §4.4).
//!
//! A volume over several physical devices (a Fusion container, an md mirror) reads as one
//! device, its devices' counters summed: its I/O is what the volume's storage did.

use std::path::{Path, PathBuf};

use crate::volumes::Mount;

/// The most devices one reading reads (A1 §4.4): home's, and the front tab's when it is another.
pub const MAX_BLOCK_DEVICES: usize = 2;

/// The volumes whose walk a reader keeps, most recently asked first: home, the front tab's, and
/// the two it was on before, so a tab switched back and forth walks once.
pub const KEPT_VOLUMES: usize = 4;

/// The deepest a walk climbs, and the most drivers it keeps (R1's probe's bounds).
#[cfg(any(target_os = "macos", test))]
const MAX_DEPTH: usize = 40;
#[cfg(any(target_os = "macos", test))]
const MAX_DRIVERS: usize = 8;

/// How deep a Linux block device's slaves are followed (a dm-crypt over LVM over a partition is
/// three).
const MAX_SLAVE_DEPTH: usize = 4;

/// The most of `/proc/diskstats` a reading reads: hundreds of devices' lines.
#[cfg(any(target_os = "linux", all(test, unix)))]
const DISKSTATS_CAP: usize = 256 * 1024;

/// One device's cumulative counters since boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BlockCounters {
    /// Bytes read.
    pub read_bytes: u64,
    /// Bytes written.
    pub write_bytes: u64,
    /// Read operations completed.
    pub reads: u64,
    /// Write operations completed.
    pub writes: u64,
    /// Time the device spent serving reads, ns (macOS `Total Time (Read)`; Linux's ms × 10⁶).
    pub read_ns: u64,
    /// Time the device spent serving writes, ns.
    pub write_ns: u64,
}

impl BlockCounters {
    /// Both counters added, each saturating.
    #[must_use]
    pub fn plus(self, o: Self) -> Self {
        Self {
            read_bytes: self.read_bytes.saturating_add(o.read_bytes),
            write_bytes: self.write_bytes.saturating_add(o.write_bytes),
            reads: self.reads.saturating_add(o.reads),
            writes: self.writes.saturating_add(o.writes),
            read_ns: self.read_ns.saturating_add(o.read_ns),
            write_ns: self.write_ns.saturating_add(o.write_ns),
        }
    }
}

/// Where a device is attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockLocation {
    /// Inside the machine (macOS `Physical Interconnect Location` `Internal`).
    Internal,
    /// Plugged in (`External`; Linux `removable` 1).
    External,
    /// A disk image (`File`; a Linux loop device).
    Image,
    /// Not said.
    Unknown,
}

/// One device's reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockReading {
    /// Its identity across boots and readings: 16 hex digits of FNV-1a over what names it (its
    /// serial, else the UUID nearest it on the walk, else its product and BSD name; Linux its
    /// `wwid`, else its name), so a peak learned for it is its own and the id says nothing of a
    /// serial.
    pub id: String,
    /// What it calls itself (`APPLE SSD AP4096Z`, `Disk Image`, `nvme0n1`): data, never an
    /// instruction.
    pub name: String,
    /// Where it is attached.
    pub location: BlockLocation,
    /// Its counters since boot.
    pub counters: BlockCounters,
}

/// What a reader knows of a device once it walked to it.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Device {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) location: BlockLocation,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl Device {
    /// One device from its parts' identities (in any order), names and locations.
    pub(crate) fn of(parts: &[(String, String, BlockLocation)]) -> Option<Self> {
        let first = parts.first()?;
        let mut identities: Vec<&str> = parts.iter().map(|(i, _, _)| i.as_str()).collect();
        identities.sort_unstable();
        let location = if parts.iter().all(|(_, _, l)| *l == first.2) {
            first.2
        } else {
            BlockLocation::Unknown
        };
        Some(Self {
            id: device_id(&identities.join("+")),
            name: parts
                .iter()
                .map(|(_, n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" + "),
            location,
        })
    }

    #[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
    pub(crate) fn reading(&self, counters: BlockCounters) -> BlockReading {
        BlockReading {
            id: self.id.clone(),
            name: self.name.clone(),
            location: self.location,
            counters,
        }
    }
}

/// 16 hex digits of FNV-1a (64 bit) over `identity`.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn device_id(identity: &str) -> String {
    let hash = identity.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// Keeps `readings` to distinct devices, at most [`MAX_BLOCK_DEVICES`], first asked first.
#[cfg(any(target_os = "macos", target_os = "linux", all(test, unix)))]
fn distinct(readings: impl IntoIterator<Item = BlockReading>) -> Vec<BlockReading> {
    let mut out: Vec<BlockReading> = Vec::new();
    for r in readings {
        if out.len() == MAX_BLOCK_DEVICES {
            break;
        }
        if !out.iter().any(|o| o.id == r.id) {
            out.push(r);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// macOS: the walk, over any registry (the live one in `blockio_macos`, a recorded one in tests)

/// The `Statistics` keys an `IOBlockStorageDriver` publishes, in [`counters_of`]'s order.
#[cfg(any(target_os = "macos", test))]
pub(crate) const STATISTICS_KEYS: [&str; 6] = [
    "Bytes (Read)",
    "Bytes (Write)",
    "Operations (Read)",
    "Operations (Write)",
    "Total Time (Read)",
    "Total Time (Write)",
];

/// A driver's counters from its `Statistics` values in [`STATISTICS_KEYS`] order; `None` unless
/// every one was read (an `AppleAPFSContainerScheme` publishes a `Statistics` without times,
/// which is no driver's).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn counters_of(values: [Option<u64>; 6]) -> Option<BlockCounters> {
    let [read_bytes, write_bytes, reads, writes, read_ns, write_ns] = values;
    Some(BlockCounters {
        read_bytes: read_bytes?,
        write_bytes: write_bytes?,
        reads: reads?,
        writes: writes?,
        read_ns: read_ns?,
        write_ns: write_ns?,
    })
}

/// A registry property a walk reads.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prop {
    /// `UUID`: a volume's, a container's, a partition's, a partition scheme's (the disk's GUID).
    Uuid,
    /// `BSD Name`: `disk3s5`, `disk0`.
    Bsd,
}

/// What the device above a driver says of itself (`Device Characteristics`, `Protocol
/// Characteristics`).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceWords {
    pub(crate) product: Option<String>,
    pub(crate) serial: Option<String>,
    /// `Physical Interconnect Location`.
    pub(crate) location: Option<String>,
}

/// What [`drivers_above`] reads of the IORegistry's service plane.
#[cfg(any(target_os = "macos", test))]
pub(crate) trait Registry {
    /// One entry, released when dropped.
    type Entry;
    /// Its registry-wide id.
    fn id(&self, e: &Self::Entry) -> u64;
    /// Its providers (parents in the service plane).
    fn parents(&self, e: &Self::Entry) -> Vec<Self::Entry>;
    /// Whether it is an `IOBlockStorageDriver`.
    fn is_driver(&self, e: &Self::Entry) -> bool;
    /// A text property.
    fn text(&self, e: &Self::Entry, p: Prop) -> Option<String>;
    /// What the device above driver `e` says of itself.
    fn device(&self, e: &Self::Entry) -> DeviceWords;
}

/// One driver a walk reached, with the UUID and BSD name nearest below it on the way.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct Store<E> {
    pub(crate) entry: E,
    pub(crate) id: u64,
    pub(crate) uuid: Option<String>,
    pub(crate) bsd: Option<String>,
}

/// Every `IOBlockStorageDriver` above `start`, depth first through every provider (an APFS
/// container on two physical stores reaches both), each once, at most [`MAX_DRIVERS`] and no
/// deeper than [`MAX_DEPTH`].
#[cfg(any(target_os = "macos", test))]
pub(crate) fn drivers_above<R: Registry>(reg: &R, start: R::Entry) -> Vec<Store<R::Entry>> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    up(reg, start, (None, None), 0, &mut seen, &mut out);
    out
}

#[cfg(any(target_os = "macos", test))]
fn up<R: Registry>(
    reg: &R,
    e: R::Entry,
    (uuid, bsd): (Option<String>, Option<String>),
    depth: usize,
    seen: &mut Vec<u64>,
    out: &mut Vec<Store<R::Entry>>,
) {
    let id = reg.id(&e);
    if seen.contains(&id) || out.len() == MAX_DRIVERS {
        return;
    }
    seen.push(id);
    if reg.is_driver(&e) {
        out.push(Store {
            entry: e,
            id,
            uuid,
            bsd,
        });
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let near = (
        reg.text(&e, Prop::Uuid).or(uuid),
        reg.text(&e, Prop::Bsd).or(bsd),
    );
    for p in reg.parents(&e) {
        up(reg, p, near.clone(), depth + 1, seen, out);
    }
}

/// What names one driver's device: its serial where it has one, else the UUID nearest below
/// the driver (its partition scheme's, the disk's GUID), else its product and the whole disk's
/// BSD name. Each kind is spelled apart so two never collide.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn identity(words: &DeviceWords, uuid: Option<&str>, bsd: Option<&str>) -> String {
    let product = words.product.as_deref().unwrap_or("");
    match (words.serial.as_deref().filter(|s| !s.is_empty()), uuid) {
        (Some(serial), _) => format!("serial:{product}:{serial}"),
        (None, Some(uuid)) => format!("uuid:{uuid}"),
        (None, None) => format!("bsd:{product}:{}", bsd.unwrap_or("")),
    }
}

/// `Physical Interconnect Location` as a [`BlockLocation`].
#[cfg(any(target_os = "macos", test))]
pub(crate) fn location_of(word: Option<&str>) -> BlockLocation {
    match word {
        Some("Internal") => BlockLocation::Internal,
        Some("External") => BlockLocation::External,
        Some("File") => BlockLocation::Image,
        _ => BlockLocation::Unknown,
    }
}

/// The device the drivers of one walk make, and their ids.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn device_of_walk<R: Registry>(reg: &R, stores: &[Store<R::Entry>]) -> Option<Device> {
    let parts: Vec<(String, String, BlockLocation)> = stores
        .iter()
        .map(|s| {
            let words = reg.device(&s.entry);
            (
                identity(&words, s.uuid.as_deref(), s.bsd.as_deref()),
                words
                    .product
                    .clone()
                    .or_else(|| s.bsd.clone())
                    .unwrap_or_else(|| "disk".into()),
                location_of(words.location.as_deref()),
            )
        })
        .collect();
    Device::of(&parts)
}

// ---------------------------------------------------------------------------------------------
// Linux: sysfs and /proc/diskstats under a root (`/`, or a fixture tree in tests)

/// One `/proc/diskstats` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStat {
    /// The kernel's name for it (`nvme0n1`, `sda2`, `dm-0`).
    pub name: String,
    /// Its counters: sectors are the kernel's 512-byte units whatever the device's block size
    /// (`Documentation/block/stat.rst`), times its ms.
    pub counters: BlockCounters,
}

/// Every whole line of `/proc/diskstats` text that holds the read and write fields (fields 1 to
/// 8 after major, minor and name: every format since 2.6.25, with or without 4.18's discard and
/// 5.5's flush fields). A last line cut short is no line.
#[must_use]
pub fn diskstats(text: &str) -> Vec<DiskStat> {
    let whole = match text.rfind('\n') {
        Some(i) => &text[..=i],
        None => "",
    };
    whole
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (_major, _minor, name) = (f.next()?, f.next()?, f.next()?);
            let mut n = [0u64; 8];
            for v in &mut n {
                *v = crate::parse_uint(f.next()?)?;
            }
            let [reads, _rmerged, rsect, rms, writes, _wmerged, wsect, wms] = n;
            Some(DiskStat {
                name: name.to_owned(),
                counters: BlockCounters {
                    read_bytes: rsect.saturating_mul(512),
                    write_bytes: wsect.saturating_mul(512),
                    reads,
                    writes,
                    read_ns: rms.saturating_mul(1_000_000),
                    write_ns: wms.saturating_mul(1_000_000),
                },
            })
        })
        .collect()
}

/// The whole disks a mount's block device is stored on, sorted: `dev` (its `st_dev`, major and
/// minor) through `sys/dev/block`, or, for a device with no block number of its own (major 0: a
/// btrfs subvolume), its `source` (`/dev/nvme0n1p2`, `/dev/mapper/vg-root`) through `dev` and
/// `sys/class/block`. A partition's disk is its parent in sysfs; a device-mapper or md device's
/// are its slaves' disks. Empty where neither leads to a block device (`tmpfs`, `overlay`, a
/// network share).
#[must_use]
pub fn whole_disks(root: &Path, source: &str, dev: Option<(u32, u32)>) -> Vec<String> {
    let dir = match dev.filter(|(major, _)| *major != 0) {
        Some((major, minor)) => {
            std::fs::canonicalize(root.join(format!("sys/dev/block/{major}:{minor}"))).ok()
        }
        None => source
            .strip_prefix("/dev/")
            .filter(|rel| !rel.is_empty())
            .and_then(|rel| std::fs::canonicalize(root.join("dev").join(rel)).ok())
            .and_then(|node| {
                let name = node.file_name()?.to_owned();
                std::fs::canonicalize(root.join("sys/class/block").join(name)).ok()
            }),
    };
    let mut out = Vec::new();
    if let Some(dir) = dir {
        disks_of(&dir, 0, &mut out);
    }
    out.sort_unstable();
    out
}

fn disks_of(dir: &Path, depth: usize, out: &mut Vec<String>) {
    let slaves: Vec<PathBuf> = std::fs::read_dir(dir.join("slaves"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    if !slaves.is_empty() {
        if depth < MAX_SLAVE_DEPTH {
            for s in slaves {
                if let Ok(s) = std::fs::canonicalize(s) {
                    disks_of(&s, depth + 1, out);
                }
            }
        }
        return;
    }
    let disk = if dir.join("partition").exists() {
        dir.parent()
    } else {
        Some(dir)
    };
    if let Some(name) = disk
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        && !out.iter().any(|o| o == name)
    {
        out.push(name.to_owned());
    }
}

/// A sysfs attribute's text under `root`, trimmed; `None` where it is absent or empty.
#[cfg(any(target_os = "linux", all(test, unix)))]
fn sys_text(root: &Path, rel: &str) -> Option<String> {
    crate::read_bounded(&root.join(rel))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

/// What tells disk `disk` from another disk given its name later (one unplugged, another
/// plugged in): its `diskseq` (Linux 5.15+, which the kernel raises for every disk it adds),
/// else its `wwid`. `None` where sysfs gives neither: such a disk is told apart only by leaving
/// `/proc/diskstats` ([`LinuxReader::read`]).
#[cfg(any(target_os = "linux", all(test, unix)))]
fn witness(root: &Path, disk: &str) -> Option<String> {
    sys_text(root, &format!("sys/block/{disk}/diskseq"))
        .or_else(|| sys_text(root, &format!("sys/block/{disk}/wwid")))
        .or_else(|| sys_text(root, &format!("sys/block/{disk}/device/wwid")))
}

#[cfg(any(target_os = "linux", all(test, unix)))]
fn witnesses(root: &Path, disks: &[String]) -> Vec<Option<String>> {
    disks.iter().map(|d| witness(root, d)).collect()
}

/// The device `disks` (sorted, as [`whole_disks`] gives them) make: each named by its `wwid`
/// where sysfs gives one, else by its name; called by its model, else its name.
#[cfg(any(target_os = "linux", all(test, unix)))]
pub(crate) fn linux_device(root: &Path, disks: &[String]) -> Option<Device> {
    let text = |rel: String| sys_text(root, &rel);
    let parts: Vec<(String, String, BlockLocation)> = disks
        .iter()
        .map(|d| {
            let identity = text(format!("sys/block/{d}/wwid"))
                .or_else(|| text(format!("sys/block/{d}/device/wwid")))
                .map_or_else(|| format!("name:{d}"), |w| format!("wwid:{w}"));
            let name = text(format!("sys/block/{d}/device/model")).unwrap_or_else(|| d.clone());
            let location = if d.starts_with("loop") {
                BlockLocation::Image
            } else if text(format!("sys/block/{d}/removable")).as_deref() == Some("1") {
                BlockLocation::External
            } else {
                BlockLocation::Unknown
            };
            (identity, name, location)
        })
        .collect();
    Device::of(&parts)
}

/// `disks`' counters summed from `stats`; `None` unless each is there.
#[cfg(any(target_os = "linux", all(test, unix)))]
pub(crate) fn summed(stats: &[DiskStat], disks: &[String]) -> Option<BlockCounters> {
    disks.iter().try_fold(BlockCounters::default(), |sum, d| {
        Some(sum.plus(stats.iter().find(|s| &s.name == d)?.counters))
    })
}

/// One volume a Linux reader keeps: its mount's source and `st_dev`, and the disks and device
/// they lead to, with each disk's [`witness`] when it was walked.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[derive(Debug)]
struct KeptVolume {
    source: String,
    dev: Option<(u32, u32)>,
    disks: Vec<String>,
    witnesses: Vec<Option<String>>,
    device: Device,
}

/// The Linux arm's state: each kept volume's disks and device.
#[cfg(any(target_os = "linux", all(test, unix)))]
#[derive(Debug)]
pub(crate) struct LinuxReader {
    root: PathBuf,
    /// Most recently asked first, at most [`KEPT_VOLUMES`].
    kept: Vec<KeptVolume>,
}

#[cfg(any(target_os = "linux", all(test, unix)))]
impl LinuxReader {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            kept: Vec::new(),
        }
    }

    /// One reading per distinct device under `volumes` (each a mount's source and `st_dev`).
    ///
    /// A kept volume reads without a walk while each of its disks has the [`witness`] it was
    /// walked with. One whose witness changed is walked again now, and one with a disk
    /// `/proc/diskstats` no longer lists reads nothing and is walked again next time (as a macOS
    /// driver that stopped answering is), so a disk that took another's name is never read
    /// under the other's identity.
    pub(crate) fn read(&mut self, volumes: &[(&str, Option<(u32, u32)>)]) -> Vec<BlockReading> {
        let mut asked: Vec<KeptVolume> = Vec::new();
        for &(source, dev) in volumes {
            let same = |k: &KeptVolume| k.source == source && k.dev == dev;
            if asked.iter().any(same) {
                continue;
            }
            let kept = self
                .kept
                .iter()
                .position(same)
                .map(|i| self.kept.remove(i))
                .filter(|k| k.witnesses == witnesses(&self.root, &k.disks));
            let kept = match kept {
                Some(k) => k,
                None => {
                    let disks = whole_disks(&self.root, source, dev);
                    let Some(device) = linux_device(&self.root, &disks) else {
                        continue;
                    };
                    KeptVolume {
                        source: source.to_owned(),
                        dev,
                        witnesses: witnesses(&self.root, &disks),
                        disks,
                        device,
                    }
                }
            };
            asked.push(kept);
        }
        if asked.is_empty() {
            return Vec::new();
        }
        let stats = crate::volumes::read_capped(&self.root.join("proc/diskstats"), DISKSTATS_CAP)
            .map(|(text, _)| diskstats(&text));
        let mut out = Vec::new();
        for k in asked {
            if let Some(stats) = &stats {
                // A disk diskstats no longer lists went: its volume is walked again next time.
                let Some(counters) = summed(stats, &k.disks) else {
                    continue;
                };
                out.push(k.device.reading(counters));
            }
            self.kept.insert(0, k);
        }
        self.kept.truncate(KEPT_VOLUMES);
        distinct(out)
    }
}

// ---------------------------------------------------------------------------------------------
// The reader

/// Reads the devices under mounts, keeping what each walk found (module docs).
#[derive(Debug)]
pub struct BlockReader {
    #[cfg(target_os = "macos")]
    mac: crate::blockio_macos::Reader,
    #[cfg(target_os = "linux")]
    linux: LinuxReader,
}

impl Default for BlockReader {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockReader {
    /// A reader that has walked nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            mac: crate::blockio_macos::Reader::new(),
            #[cfg(target_os = "linux")]
            linux: LinuxReader::new("/"),
        }
    }

    /// One reading per distinct device the `mounts` are stored on, in their order, at most
    /// [`MAX_BLOCK_DEVICES`]; a mount whose device cannot be found or read gives none (the tile
    /// is absent, never wrong). Empty off macOS and Linux.
    pub fn read(&mut self, mounts: &[&Mount]) -> Vec<BlockReading> {
        #[cfg(target_os = "macos")]
        {
            distinct(self.mac.read(mounts))
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt as _;
            let volumes: Vec<(&str, Option<(u32, u32)>)> = mounts
                .iter()
                .map(|m| {
                    let dev = std::fs::metadata(&m.on)
                        .ok()
                        .map(|meta| (libc::major(meta.dev()), libc::minor(meta.dev())));
                    (m.from.as_str(), dev)
                })
                .collect();
            self.linux.read(&volumes)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = mounts;
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "blockio_tests.rs"]
mod tests;
