// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The block-storage reader against recorded IORegistry walks and kernel-format
//! `/proc/diskstats` text, sysfs trees built in a scratch root, and (macOS) the live registry.

use super::*;

/// The walk from the home volume (`disk3s5`) to its NVMe store, `ioreg -t` on a MacBook Air
/// (macOS 27.0.1), every node but the trace and every property but the walk's dropped; the
/// serial and every UUID replaced.
const HOME_WALK: &str = include_str!("../fixtures/ioreg/air-27.0.1-home-walk.txt");
/// The walk from a mounted APFS disk image's volume (`disk5s1`) to the image's driver, the same
/// Mac, redacted the same way.
const IMAGE_WALK: &str = include_str!("../fixtures/ioreg/air-27.0.1-image-walk.txt");
/// R1's record of the same Mac's NVMe device, driver and whole-disk media (A1 §4.4's cited
/// fixture, kept beside the hardware reader that recorded it).
const AIR_BLOCK_STORAGE: &str =
    include_str!("../../aterm-machines/fixtures/ioreg/air-27.0.1-block-storage.txt");
/// A macOS 27.0 virtual machine's VirtIO disk and disk image, the same reader's record.
const VM_BLOCK_STORAGE: &str =
    include_str!("../../aterm-machines/fixtures/ioreg/vm-27.0-block-storage.txt");

/// One IORegistry entry as `ioreg` printed it.
struct Node {
    id: u64,
    class: String,
    props: Vec<(String, String)>,
    /// Its providers, as indexes.
    parents: Vec<usize>,
}

/// A registry from `ioreg` text (each node's provider is the node it is printed under), or
/// built by hand.
struct Recorded {
    nodes: Vec<Node>,
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_owned()
}

/// The pairs of an `ioreg` dictionary value (`{"k"=v,"k"="v",…}`), split at its top-level
/// commas.
fn dict(raw: &str) -> Vec<(String, String)> {
    let inner = raw
        .trim()
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .expect("a dictionary value");
    let mut out = Vec::new();
    let (mut quoted, mut angle, mut start) = (false, 0usize, 0usize);
    let mut push = |s: &str| {
        if let Some((k, v)) = s.split_once('=') {
            out.push((unquote(k), unquote(v)));
        }
    };
    for (i, b) in inner.bytes().enumerate() {
        match b {
            b'"' => quoted = !quoted,
            b'<' if !quoted => angle += 1,
            b'>' if !quoted => angle = angle.saturating_sub(1),
            b',' if !quoted && angle == 0 => {
                push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    push(&inner[start..]);
    out
}

fn node(id: u64, class: &str, props: &[(&str, &str)], parents: &[usize]) -> Node {
    Node {
        id,
        class: class.to_owned(),
        props: props
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        parents: parents.to_vec(),
    }
}

impl Recorded {
    fn parse(text: &str) -> Self {
        let mut nodes: Vec<Node> = Vec::new();
        let mut open: Vec<(usize, usize)> = Vec::new();
        for line in text.lines() {
            if let Some(col) = line.find("+-o ") {
                let (_, meta) = line[col + 4..]
                    .split_once("  <class ")
                    .expect("a node line names its class");
                let class = meta.split(',').next().expect("a class").trim().to_owned();
                let hex = meta
                    .split(", id 0x")
                    .nth(1)
                    .and_then(|s| s.split(',').next())
                    .expect("a node line names its id");
                let id = u64::from_str_radix(hex, 16).expect("a hex id");
                while open.last().is_some_and(|&(c, _)| c >= col) {
                    open.pop();
                }
                let parents = open.last().map(|&(_, i)| vec![i]).unwrap_or_default();
                nodes.push(Node {
                    id,
                    class,
                    props: Vec::new(),
                    parents,
                });
                open.push((col, nodes.len() - 1));
            } else if let Some(q) = line.find('"')
                && let Some((k, v)) = line[q..].split_once(" = ")
                && let Some(last) = nodes.last_mut()
            {
                last.props.push((unquote(k), v.to_owned()));
            }
        }
        Self { nodes }
    }

    fn prop(&self, e: usize, key: &str) -> Option<&str> {
        self.nodes[e]
            .props
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The entry whose `BSD Name` is `bsd`.
    fn bsd(&self, bsd: &str) -> usize {
        (0..self.nodes.len())
            .find(|&i| self.prop(i, "BSD Name").map(unquote).as_deref() == Some(bsd))
            .expect("the fixture holds the BSD device")
    }

    /// The entries of class `class`.
    fn of_class(&self, class: &str) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].class == class)
            .collect()
    }

    /// Entry `e`'s `Statistics`, read as the live reader reads a driver's.
    fn statistics(&self, e: usize) -> Option<BlockCounters> {
        let stats = dict(self.prop(e, "Statistics")?);
        counters_of(STATISTICS_KEYS.map(|key| {
            stats
                .iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.parse().ok())
        }))
    }
}

impl Registry for Recorded {
    type Entry = usize;

    fn id(&self, e: &usize) -> u64 {
        self.nodes[*e].id
    }

    fn parents(&self, e: &usize) -> Vec<usize> {
        self.nodes[*e].parents.clone()
    }

    fn is_driver(&self, e: &usize) -> bool {
        self.nodes[*e].class == "IOBlockStorageDriver"
    }

    fn text(&self, e: &usize, p: Prop) -> Option<String> {
        let key = match p {
            Prop::Uuid => "UUID",
            Prop::Bsd => "BSD Name",
        };
        self.prop(*e, key).map(unquote)
    }

    fn device(&self, e: &usize) -> DeviceWords {
        let Some(&dev) = self.nodes[*e].parents.first() else {
            return DeviceWords::default();
        };
        let word = |dict_key: &str, key: &str| {
            self.prop(dev, dict_key)
                .and_then(|d| dict(d).into_iter().find(|(k, _)| k == key))
                .map(|(_, v)| v)
        };
        DeviceWords {
            product: word("Device Characteristics", "Product Name"),
            serial: word("Device Characteristics", "Serial Number"),
            location: word("Protocol Characteristics", "Physical Interconnect Location"),
        }
    }
}

#[test]
fn block_storage_statistics_parse_from_the_recorded_fixture() {
    // A1 §4.4's fixture (`air-27.0.1-block-storage.txt:41`): its one driver's six counters.
    let air = Recorded::parse(AIR_BLOCK_STORAGE);
    let drivers = air.of_class("IOBlockStorageDriver");
    assert_eq!(drivers.len(), 1);
    assert_eq!(
        air.statistics(drivers[0]),
        Some(BlockCounters {
            read_bytes: 3_294_332_960_768,
            write_bytes: 1_803_910_754_304,
            reads: 156_216_805,
            writes: 50_639_554,
            read_ns: 19_075_753_303_870,
            write_ns: 1_737_421_768_236,
        })
    );
    // The home walk's driver, recorded later the same week: the same device, further on.
    let home = Recorded::parse(HOME_WALK);
    let driver = home.of_class("IOBlockStorageDriver");
    assert_eq!(
        home.statistics(driver[0]),
        Some(BlockCounters {
            read_bytes: 3_115_957_231_616,
            write_bytes: 3_591_372_808_192,
            reads: 153_387_397,
            writes: 143_212_648,
            read_ns: 16_610_253_851_647,
            write_ns: 4_362_165_277_010,
        })
    );
    // A virtual machine's VirtIO disk and its disk image publish the same keys.
    let vm = Recorded::parse(VM_BLOCK_STORAGE);
    let read: Vec<BlockCounters> = vm
        .of_class("IOBlockStorageDriver")
        .into_iter()
        .filter_map(|d| vm.statistics(d))
        .collect();
    assert_eq!(read.len(), 2, "both of the VM's drivers read");
    assert_eq!(read[0].read_bytes, 2_431_920_128);
    assert_eq!(read[1].reads, 80);

    // Negative control: the APFS container scheme above the physical store publishes a
    // `Statistics` too, without times; it is no driver's and reads nothing, and a driver whose
    // `Statistics` lost one counter reads nothing rather than a zero.
    let scheme = home.of_class("AppleAPFSContainerScheme");
    assert_eq!(scheme.len(), 1);
    assert!(home.prop(scheme[0], "Statistics").is_some());
    assert_eq!(home.statistics(scheme[0]), None);
    let mut values = STATISTICS_KEYS.map(|_| Some(1u64));
    assert!(counters_of(values).is_some());
    values[4] = None;
    assert_eq!(counters_of(values), None);
}

#[test]
fn the_home_volume_walks_to_its_physical_store() {
    let home = Recorded::parse(HOME_WALK);
    let start = home.bsd("disk3s5");
    let stores = drivers_above(&home, start);
    assert_eq!(stores.len(), 1, "one physical store under the home volume");
    let store = &stores[0];
    assert_eq!(
        store.id, 0x1_0000_0879,
        "the NVMe device's IOBlockStorageDriver"
    );
    // The nearest UUID below the driver is the partition scheme's (the disk's GUID), not the
    // volume's, the container's or the physical store's, each of which a walk passes first.
    assert_eq!(
        store.uuid.as_deref(),
        Some("00000000-0000-0000-0000-00000000D150")
    );
    assert_eq!(store.bsd.as_deref(), Some("disk0"));
    let device = device_of_walk(&home, &stores).expect("a device");
    assert_eq!(
        device,
        Device {
            id: device_id("serial:APPLE SSD AP4096Z:0000000000000001"),
            name: "APPLE SSD AP4096Z".into(),
            location: BlockLocation::Internal,
        }
    );
    // Every volume of the container walks to the same device: the container's own media.
    assert_eq!(
        device_of_walk(&home, &drivers_above(&home, home.bsd("disk3"))),
        Some(device.clone())
    );

    // Negative control: the same registry with no `IOBlockStorageDriver` above the volume
    // reaches no store and names no device (the tile is absent, never wrong); the container
    // scheme's `Statistics` does not make it one.
    let mut no_driver = Recorded::parse(HOME_WALK);
    for n in &mut no_driver.nodes {
        if n.class == "IOBlockStorageDriver" {
            n.class = "IOService".into();
        }
    }
    let none = drivers_above(&no_driver, no_driver.bsd("disk3s5"));
    assert!(none.is_empty());
    assert_eq!(device_of_walk(&no_driver, &none), None);
}

#[test]
fn a_disk_image_walks_to_its_own_device_named_by_its_disks_guid() {
    let image = Recorded::parse(IMAGE_WALK);
    let stores = drivers_above(&image, image.bsd("disk5s1"));
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].id, 0x1_0001_540c);
    let device = device_of_walk(&image, &stores).expect("a device");
    // No serial: the image's partition scheme's UUID names it, so a re-attached image keeps
    // its peaks and another image does not inherit them.
    assert_eq!(
        device,
        Device {
            id: device_id("uuid:00000000-0000-0000-0000-00000000D140"),
            name: "Disk Image".into(),
            location: BlockLocation::Image,
        }
    );
    let home = Recorded::parse(HOME_WALK);
    let internal = device_of_walk(&home, &drivers_above(&home, home.bsd("disk3s5")));
    assert_ne!(Some(device.id), internal.map(|d| d.id));
}

/// A volume on a container over two physical stores (a Fusion drive): each store's media under
/// its own driver and device, both providers of the container scheme.
fn fusion(scheme_parents: &[usize]) -> Recorded {
    let dev = |serial: &str, at: &str| {
        (
            format!("{{\"Product Name\"=\"P\",\"Serial Number\"=\"{serial}\"}}"),
            format!("{{\"Physical Interconnect Location\"=\"{at}\"}}"),
        )
    };
    let (ssd_dc, ssd_pc) = dev("A", "Internal");
    let (hdd_dc, hdd_pc) = dev("B", "External");
    Recorded {
        nodes: vec![
            node(
                10,
                "IOBlockStorageDevice",
                &[
                    ("Device Characteristics", ssd_dc.as_str()),
                    ("Protocol Characteristics", ssd_pc.as_str()),
                ],
                &[],
            ),
            node(11, "IOBlockStorageDriver", &[], &[0]),
            node(12, "IOMedia", &[("BSD Name", "\"disk0\"")], &[1]),
            node(
                20,
                "IOBlockStorageDevice",
                &[
                    ("Device Characteristics", hdd_dc.as_str()),
                    ("Protocol Characteristics", hdd_pc.as_str()),
                ],
                &[],
            ),
            node(21, "IOBlockStorageDriver", &[], &[3]),
            node(22, "IOMedia", &[("BSD Name", "\"disk1\"")], &[4]),
            node(30, "AppleAPFSContainerScheme", &[], scheme_parents),
            node(31, "AppleAPFSVolume", &[("BSD Name", "\"disk2s1\"")], &[6]),
        ],
    }
}

#[test]
fn a_volume_over_two_stores_is_one_device_named_by_both() {
    // The scheme lists the volume itself among its providers too: the walk visits each entry
    // once and still ends.
    let one = fusion(&[2, 5, 7]);
    let stores = drivers_above(&one, 7);
    let ids: Vec<u64> = stores.iter().map(|s| s.id).collect();
    assert_eq!(ids, [11, 21]);
    let device = device_of_walk(&one, &stores).expect("a device");
    assert_eq!(device.id, device_id("serial:P:A+serial:P:B"));
    assert_eq!(device.name, "P + P");
    assert_eq!(
        device.location,
        BlockLocation::Unknown,
        "an internal and an external store are attached nowhere in particular"
    );
    // The providers' order is the registry's; the device's identity is not.
    let other = fusion(&[5, 2]);
    let reversed = device_of_walk(&other, &drivers_above(&other, 7)).expect("a device");
    assert_eq!(reversed.id, device.id);
}

#[test]
fn a_device_id_is_fnv_1a_of_its_identity() {
    // Peaks are keyed by the id across releases: the hash is pinned to FNV-1a 64's published
    // vectors, so a peak learned under one build is found by the next.
    assert_eq!(device_id(""), "cbf29ce484222325");
    assert_eq!(device_id("a"), "af63dc4c8601ec8c");
    assert_eq!(device_id("foobar"), "85944171f73967e8");
    // Each kind of identity is spelled apart: a serial equal to a UUID names another device.
    let words = DeviceWords {
        product: Some("P".into()),
        serial: Some("X".into()),
        location: None,
    };
    assert_eq!(identity(&words, Some("X"), None), "serial:P:X");
    let unsaid = DeviceWords {
        serial: Some(String::new()),
        ..words.clone()
    };
    assert_eq!(identity(&unsaid, Some("X"), None), "uuid:X");
    assert_eq!(identity(&unsaid, None, Some("disk0")), "bsd:P:disk0");
}

/// `/proc/diskstats` lines in the kernel's three documented layouts
/// (`Documentation/admin-guide/iostats.rst`): 14 fields (2.6.25+; the document's own `hda`
/// example), 18 (4.18's discard fields) and 20 (5.5's flush fields), a short line and a last
/// line cut mid-number.
const DISKSTATS: &str = "\
   3    0 hda 446216 784926 9550688 4382310 424847 312726 5922052 19310380 0 3376340 23705160
 259    0 nvme0n1 1200 10 409600 350 800 20 204800 1700 0 2000 2050 7 0 56 3
 259    2 nvme0n1p2 1100 10 400000 340 790 20 200000 1690 0 1990 2030 7 0 56 3 40 12
   8    0 sda 12
 253    0 dm-0 1000 0 390000 330 780 0 190000 1";

#[test]
fn proc_diskstats_counts_sectors_as_bytes() {
    let stats = diskstats(DISKSTATS);
    let names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
    // `sda` has one counter of eight; `dm-0` is the last line, cut mid-number (its write ms).
    assert_eq!(names, ["hda", "nvme0n1", "nvme0n1p2"]);
    assert_eq!(
        stats[0].counters,
        BlockCounters {
            read_bytes: 9_550_688 * 512,
            write_bytes: 5_922_052 * 512,
            reads: 446_216,
            writes: 424_847,
            read_ns: 4_382_310 * 1_000_000,
            write_ns: 19_310_380 * 1_000_000,
        }
    );
    assert_eq!(stats[1].counters.read_bytes, 409_600 * 512);
    assert_eq!(stats[2].counters.write_ns, 1_690 * 1_000_000);

    // Negative control: a field that is not a whole number (a sign, a decimal) is no line, and
    // the text with its last newline restored reads the cut line whole.
    assert!(diskstats("   8 0 sda 1 0 -8 1 1 0 8 1\n").is_empty());
    assert!(diskstats("   8 0 sda 1 0 8.0 1 1 0 8 1\n").is_empty());
    let whole = diskstats(&format!("{DISKSTATS}\n"));
    assert_eq!(whole.len(), 4);
    assert_eq!(whole[3].counters.write_bytes, 190_000 * 512);
    assert_eq!(whole[3].counters.write_ns, 1_000_000);
}

/// The scratch tree's `/proc/diskstats`: every disk, partition and mapper it holds.
#[cfg(unix)]
const SYSFS_DISKSTATS: &str = " 259 0 nvme0n1 100 0 2000 10 50 0 1000 20 0 30 30
 259 2 nvme0n1p2 90 0 1900 9 45 0 900 18 0 27 27
 8 0 sda 7 0 70 1 3 0 30 2 0 3 3
 8 1 sda1 6 0 60 1 2 0 20 1 0 2 2
 8 16 sdb 5 0 50 1 4 0 40 2 0 3 3
 7 0 loop0 1 0 8 0 0 0 0 0 0 0 0
 253 0 dm-0 80 0 1800 9 40 0 800 17 0 26 26
 253 1 dm-1 80 0 1800 9 40 0 800 17 0 26 26
 9 0 md0 12 0 120 2 7 0 70 4 0 6 6
";

/// Where the scratch tree's NVMe disk sits under `sys/devices`.
#[cfg(unix)]
const NVME: &str = "sys/devices/pci0000:00/0000:00:01.0/nvme/nvme0/nvme0n1";
/// Where its SATA disks sit.
#[cfg(unix)]
const SATA: &str = "sys/devices/pci0000:00/0000:00:02.0/ata1";

/// A sysfs and `/dev` tree in a scratch root:
/// - `nvme0n1` (partitions `p1`, `p2`; `diskseq`, `wwid`, model), `sda`/`sdb` (`sdb` removable,
///   neither with a `diskseq` or `wwid`: an older kernel's), `loop0`;
/// - `dm-0` over `nvme0n1p2` (dm-crypt), `dm-1` over `dm-0` (LVM), `/dev/mapper/vg-root` → `dm-1`;
/// - `md0` over `sda1` and `sdb1`.
///
/// Its links are symbolic links, so the trees are built on Unix hosts only.
#[cfg(unix)]
fn sysfs(root: &std::path::Path) {
    use std::os::unix::fs::symlink;
    let mk = |rel: &str| std::fs::create_dir_all(root.join(rel)).expect("mkdir");
    let put = |rel: &str, text: &str| std::fs::write(root.join(rel), text).expect("write");
    let link = |target: &str, rel: &str| {
        if let Some(parent) = root.join(rel).parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        symlink(target, root.join(rel)).expect("symlink");
    };
    let (nvme, sata) = (NVME, SATA);
    for d in [
        &format!("{nvme}/nvme0n1p1"),
        &format!("{nvme}/nvme0n1p2"),
        &format!("{nvme}/device"),
        &format!("{sata}/sda/sda1"),
        &format!("{sata}/sda/device"),
        &format!("{sata}/sdb/sdb1"),
        &format!("{sata}/sdb/device"),
        "sys/devices/virtual/block/dm-0/slaves",
        "sys/devices/virtual/block/dm-1/slaves",
        "sys/devices/virtual/block/md0/slaves",
        "sys/devices/virtual/block/loop0",
        "sys/block",
        "sys/class/block",
        "sys/dev/block",
        "dev/mapper",
        "proc",
    ] {
        mk(d);
    }
    put(&format!("{nvme}/nvme0n1p1/partition"), "1\n");
    put(&format!("{nvme}/nvme0n1p2/partition"), "2\n");
    put(&format!("{nvme}/diskseq"), "1\n");
    put(&format!("{nvme}/wwid"), "eui.0025385b71b0a1c2\n");
    put(&format!("{nvme}/device/model"), "SAMSUNG MZVL21T0HCLR\n");
    put(&format!("{nvme}/removable"), "0\n");
    put(&format!("{sata}/sda/sda1/partition"), "1\n");
    put(&format!("{sata}/sdb/sdb1/partition"), "1\n");
    put(&format!("{sata}/sda/device/model"), "WDC WD40EFRX\n");
    put(&format!("{sata}/sdb/device/model"), "ST4000VN008\n");
    put(&format!("{sata}/sda/removable"), "0\n");
    put(&format!("{sata}/sdb/removable"), "1\n");
    // A `slaves` entry sits six levels under the root.
    let up = "../../../../../..";
    link(
        &format!("{up}/{nvme}/nvme0n1p2"),
        "sys/devices/virtual/block/dm-0/slaves/nvme0n1p2",
    );
    link("../../dm-0", "sys/devices/virtual/block/dm-1/slaves/dm-0");
    link(
        &format!("{up}/{sata}/sda/sda1"),
        "sys/devices/virtual/block/md0/slaves/sda1",
    );
    link(
        &format!("{up}/{sata}/sdb/sdb1"),
        "sys/devices/virtual/block/md0/slaves/sdb1",
    );
    for (name, dir) in [
        ("nvme0n1", nvme.to_owned()),
        ("sda", format!("{sata}/sda")),
        ("sdb", format!("{sata}/sdb")),
        ("loop0", "sys/devices/virtual/block/loop0".into()),
    ] {
        link(&format!("../../{dir}"), &format!("sys/block/{name}"));
    }
    for (dev, dir) in [
        ("259:0", nvme.to_owned()),
        ("259:2", format!("{nvme}/nvme0n1p2")),
        ("8:1", format!("{sata}/sda/sda1")),
        ("253:0", "sys/devices/virtual/block/dm-0".into()),
        ("253:1", "sys/devices/virtual/block/dm-1".into()),
        ("9:0", "sys/devices/virtual/block/md0".into()),
        ("7:0", "sys/devices/virtual/block/loop0".into()),
    ] {
        link(&format!("../../../{dir}"), &format!("sys/dev/block/{dev}"));
    }
    link("../../devices/virtual/block/dm-1", "sys/class/block/dm-1");
    put("dev/dm-1", "");
    link("../dm-1", "dev/mapper/vg-root");
    put("proc/diskstats", SYSFS_DISKSTATS);
}

#[cfg(unix)]
#[test]
fn whole_disks_follow_partitions_and_device_mapper_slaves_to_their_disks() {
    let dir = aterm_tempfile::tempdir().expect("scratch root");
    let root = dir.path();
    sysfs(root);
    let nvme = vec!["nvme0n1".to_owned()];
    assert_eq!(whole_disks(root, "/dev/nvme0n1p2", Some((259, 2))), nvme);
    assert_eq!(whole_disks(root, "/dev/nvme0n1", Some((259, 0))), nvme);
    // dm-crypt under LVM: two device-mapper layers down to the partition, then up to its disk.
    assert_eq!(
        whole_disks(root, "/dev/mapper/vg-root", Some((253, 1))),
        nvme
    );
    // A btrfs subvolume's `st_dev` has major 0: its source names the device instead.
    assert_eq!(
        whole_disks(root, "/dev/mapper/vg-root", Some((0, 45))),
        nvme
    );
    assert_eq!(whole_disks(root, "/dev/mapper/vg-root", None), nvme);
    // An md mirror is both of its disks.
    assert_eq!(
        whole_disks(root, "/dev/md0", Some((9, 0))),
        ["sda".to_owned(), "sdb".to_owned()]
    );

    // Negative control: what no block device holds names no disk.
    assert!(whole_disks(root, "tmpfs", Some((0, 45))).is_empty());
    assert!(whole_disks(root, "overlay", None).is_empty());
    assert!(whole_disks(root, "/dev/nvme9n9", Some((259, 99))).is_empty());
    assert!(whole_disks(root, "/dev/", None).is_empty());
}

#[cfg(unix)]
#[test]
fn a_linux_reader_reads_each_volumes_disks_from_diskstats() {
    let dir = aterm_tempfile::tempdir().expect("scratch root");
    let root = dir.path();
    sysfs(root);
    let mut reader = LinuxReader::new(root);
    // Home on LVM over dm-crypt, the front tab on the md mirror, a third volume on the first's
    // disk again: two devices, home's first.
    let readings = reader.read(&[
        ("/dev/mapper/vg-root", Some((253, 1))),
        ("/dev/md0", Some((9, 0))),
        ("/dev/nvme0n1p2", Some((259, 2))),
    ]);
    assert_eq!(readings.len(), 2);
    let (home, mirror) = (&readings[0], &readings[1]);
    assert_eq!(home.id, device_id("wwid:eui.0025385b71b0a1c2"));
    assert_eq!(home.name, "SAMSUNG MZVL21T0HCLR");
    assert_eq!(home.location, BlockLocation::Unknown);
    // The whole disk's counters, not the partition's or the mapper's.
    assert_eq!(
        home.counters,
        BlockCounters {
            read_bytes: 2000 * 512,
            write_bytes: 1000 * 512,
            reads: 100,
            writes: 50,
            read_ns: 10_000_000,
            write_ns: 20_000_000,
        }
    );
    assert_eq!(mirror.id, device_id("name:sda+name:sdb"));
    assert_eq!(mirror.name, "WDC WD40EFRX + ST4000VN008");
    assert_eq!(mirror.counters.read_bytes, (70 + 50) * 512);
    assert_eq!(mirror.counters.writes, 3 + 4);
    assert_eq!(
        mirror.location,
        BlockLocation::Unknown,
        "a fixed and a removable disk"
    );

    // A loop device is an image; the reader keeps what it found, so a kept volume reads without
    // a walk: with its sysfs link gone, it reads from the kept disks while diskstats lists them.
    let image = reader.read(&[("/dev/loop0", Some((7, 0)))]);
    assert_eq!(image.len(), 1);
    assert_eq!(image[0].location, BlockLocation::Image);
    std::fs::remove_file(root.join("sys/dev/block/9:0")).expect("unlink");
    assert_eq!(reader.read(&[("/dev/md0", Some((9, 0)))]).len(), 1);
}

#[cfg(unix)]
#[test]
fn a_disk_that_took_a_kept_disks_name_is_walked_not_read_as_the_kept_one() {
    let dir = aterm_tempfile::tempdir().expect("scratch root");
    let root = dir.path();
    sysfs(root);
    let mut reader = LinuxReader::new(root);
    let (mirror, nvme) = (
        ("/dev/md0", Some((9, 0))),
        ("/dev/nvme0n1p2", Some((259, 2))),
    );
    let first = reader.read(&[mirror, nvme]);
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].name, "WDC WD40EFRX + ST4000VN008");
    let kept_nvme = device_id("wwid:eui.0025385b71b0a1c2");
    assert_eq!(first[1].id, kept_nvme);
    let diskstats = root.join("proc/diskstats");

    // sdb is unplugged: diskstats drops it, and the mirror reads nothing (absent, never a zero).
    let without_sdb: String = SYSFS_DISKSTATS
        .lines()
        .filter(|l| !l.contains(" sdb "))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&diskstats, without_sdb).expect("write");
    let ids: Vec<String> = reader
        .read(&[mirror, nvme])
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, std::slice::from_ref(&kept_nvme));
    // Another disk (no diskseq or wwid to tell it by) is plugged in under sdb's name and numbers:
    // the mirror is walked again and reads as the new disk, not the kept one.
    std::fs::write(root.join(format!("{SATA}/sdb/device/model")), "NEW DISK\n").expect("write");
    std::fs::write(&diskstats, SYSFS_DISKSTATS).expect("write");
    let again = reader.read(&[mirror]);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].name, "WDC WD40EFRX + NEW DISK");

    // The NVMe disk is swapped between two readings for one with another diskseq and wwid,
    // under the same name and numbers, which diskstats lists throughout: its witness changed, so
    // it is walked again and named by the new disk's wwid.
    std::fs::write(root.join(format!("{NVME}/diskseq")), "2\n").expect("write");
    std::fs::write(root.join(format!("{NVME}/wwid")), "eui.00000000000000ff\n").expect("write");
    let swapped = reader.read(&[nvme]);
    assert_eq!(swapped.len(), 1);
    assert_eq!(swapped[0].id, device_id("wwid:eui.00000000000000ff"));
    // Negative control: a kept disk whose witness held is read without a walk (its sysfs link
    // gone, it still reads under the identity it was walked with).
    std::fs::remove_file(root.join("sys/dev/block/259:2")).expect("unlink");
    std::fs::write(root.join(format!("{NVME}/device/model")), "OTHER MODEL\n").expect("write");
    let held = reader.read(&[nvme]);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, swapped[0].id);
    assert_eq!(held[0].name, swapped[0].name);
}

#[cfg(target_os = "macos")]
#[test]
fn the_live_reader_reads_the_device_under_users() {
    let table = crate::volumes::table();
    let users = crate::volumes::mount_of(std::path::Path::new("/Users"), &table)
        .expect("/Users is mounted");
    let mut reader = BlockReader::new();
    let first = reader.read(&[users, users]);
    assert_eq!(
        first.len(),
        1,
        "one device however often its volume is asked"
    );
    let device = &first[0];
    assert_eq!(device.id.len(), 16);
    assert!(device.id.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(
        device.counters.reads > 0 && device.counters.read_bytes > 0,
        "{device:?}"
    );
    // The kept drivers read again: the counters only grow.
    let again = reader.read(&[users]);
    assert_eq!(again.len(), 1);
    let (a, b) = (device.counters, again[0].counters);
    assert_eq!(again[0].id, device.id);
    assert!(
        b.read_bytes >= a.read_bytes
            && b.write_bytes >= a.write_bytes
            && b.reads >= a.reads
            && b.writes >= a.writes
            && b.read_ns >= a.read_ns
            && b.write_ns >= a.write_ns,
        "{a:?} then {b:?}"
    );
    // Another volume of the same device (the sealed system volume at `/`, in home's container)
    // reads as that device, once.
    let system =
        crate::volumes::mount_of(std::path::Path::new("/usr/bin"), &table).expect("/ is mounted");
    assert_ne!(system.on, users.on);
    let both = reader.read(&[users, system]);
    assert_eq!(both.len(), 1, "{both:?}");
    assert_eq!(both[0].id, device.id);
    // Negative control: a mount with no BSD device (devfs) reads nothing.
    let dev = table
        .mounts
        .iter()
        .find(|m| m.fstype == "devfs")
        .expect("devfs is mounted");
    assert!(reader.read(&[dev]).is_empty());
}

/// The shipped Linux arm on this host: `stat` of the mount point, its major and minor, sysfs and
/// `/proc/diskstats` at `/`.
#[cfg(target_os = "linux")]
#[test]
fn the_live_linux_reader_reads_the_device_under_root() {
    use std::os::unix::fs::MetadataExt as _;
    let table = crate::volumes::table();
    let root = crate::volumes::mount_of(std::path::Path::new("/"), &table).expect("/ is mounted");
    let dev = std::fs::metadata("/").expect("stat /").dev();
    // A root on a block device (ext4 on a disk, btrfs through its source) is one device; an
    // overlay root (a container's) is none.
    let on_a_disk = libc::major(dev) != 0 || root.from.starts_with("/dev/");
    let mut reader = BlockReader::new();
    let first = reader.read(&[root, root]);
    assert_eq!(first.len(), usize::from(on_a_disk), "{root:?}: {first:?}");
    if let Some(device) = first.first() {
        assert_eq!(device.id.len(), 16);
        assert!(device.id.bytes().all(|b| b.is_ascii_hexdigit()));
        // The kept disks read again: the counters only grow.
        let again = reader.read(&[root]);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, device.id);
        let (a, b) = (device.counters, again[0].counters);
        assert!(
            b.read_bytes >= a.read_bytes
                && b.write_bytes >= a.write_bytes
                && b.reads >= a.reads
                && b.writes >= a.writes,
            "{a:?} then {b:?}"
        );
    }
    // Negative control: a mount no block device holds (tmpfs, overlay) reads nothing.
    for m in table
        .mounts
        .iter()
        .filter(|m| m.fstype == "tmpfs" || m.fstype == "overlay")
        .take(4)
    {
        assert!(reader.read(&[m]).is_empty(), "{m:?}");
    }
}
