// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mac hardware discovery ([`internal_disk`], [`HostFacts`], [`describe`]).

use crate::model::HostFacts;
use crate::port::Cmd;

/// The placeholder the agent fills with [`describe`] (in a Local hostname, label form).
pub const PLACEHOLDER: &str = "{hardware}";

/// `sysctl -n hw.memsize`: the memory, in bytes.
#[must_use]
pub fn memsize_cmd() -> Cmd {
    Cmd::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"])
}

/// `sysctl -n machdep.cpu.brand_string`: `Apple M4 Max`, or Intel's processor name.
#[must_use]
pub fn brand_cmd() -> Cmd {
    Cmd::new("/usr/sbin/sysctl").args(["-n", "machdep.cpu.brand_string"])
}

/// The device tree's `product` node, whose `product-name` is the marketing name on Apple silicon.
#[must_use]
pub fn product_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args(["-p", "IODeviceTree", "-r", "-n", "product", "-d", "1"])
}

/// `ioreg -r -c IOBlockStorageDevice -d 3 -l -w 0`.
#[must_use]
pub fn disk_cmd() -> Cmd {
    Cmd::new("/usr/sbin/ioreg").args([
        "-r",
        "-c",
        "IOBlockStorageDevice",
        "-d",
        "3",
        "-l",
        "-w",
        "0",
    ])
}

/// The bytes `sysctl -n hw.memsize` printed.
#[must_use]
pub fn parse_memsize(text: &str) -> Option<u64> {
    text.trim().parse().ok().filter(|b| *b > 0)
}

/// The processor `sysctl -n machdep.cpu.brand_string` names, and an Intel processor's speed.
#[must_use]
pub fn parse_brand(text: &str) -> Option<(String, String)> {
    let brand = text.lines().next()?.trim();
    if brand.is_empty() {
        return None;
    }
    if !brand.starts_with("Intel") {
        return Some((brand.to_owned(), String::new()));
    }
    let (name, speed) = brand
        .split_once('@')
        .map_or((brand, ""), |(n, s)| (n, s.trim()));
    let words: Vec<String> = name
        .split_whitespace()
        .take_while(|w| *w != "CPU")
        .map(|w| w.replace("(R)", "").replace("(TM)", ""))
        .filter_map(|w| w.split('-').next().map(str::to_owned))
        .filter(|w| !w.is_empty())
        .collect();
    let speed = speed
        .strip_suffix("GHz")
        .and_then(|digits| decimal(digits.trim()))
        .map(|ghz| format!("{ghz} GHz"))
        .unwrap_or_default();
    Some((words.join(" "), speed))
}

/// `3.10` as `3.1`, `2.00` as `2.0`: trailing zeros dropped, one decimal kept.
fn decimal(digits: &str) -> Option<String> {
    let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let plain = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !plain(whole) || !plain(frac) {
        return None;
    }
    let frac = frac.trim_end_matches('0');
    Some(format!(
        "{whole}.{}",
        if frac.is_empty() { "0" } else { frac }
    ))
}

/// Extract `product-name` from `ioreg -p IODeviceTree -r -n product -d 1`.
#[must_use]
pub fn parse_product_name(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (_, rest) = l.split_once("\"product-name\" = <\"")?;
        let value = rest.trim_end().strip_suffix("\">")?.trim_end_matches('\0');
        (!value.is_empty() && !value.contains('"')).then(|| value.to_owned())
    })
}

/// The model's marketing name.
#[must_use]
pub fn model_name(hw_model: &str, product_name: Option<&str>) -> Option<String> {
    if hw_model.starts_with("VirtualMac") {
        return Some("Virtual Machine".to_owned());
    }
    if let Some(base) = product_name
        .and_then(|p| p.split(" (").next())
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        return Some(base.to_owned());
    }
    let family: String = hw_model
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let name = match family.as_str() {
        "MacBookPro" => "MacBook Pro",
        "MacBookAir" => "MacBook Air",
        "MacBook" => "MacBook",
        "Macmini" => "Mac mini",
        "iMacPro" => "iMac Pro",
        "iMac" => "iMac",
        "MacPro" => "Mac Pro",
        _ => return None,
    };
    Some(name.to_owned())
}

/// One disk inside the Mac, as [`disk_cmd`] lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Disk {
    /// The whole medium's `Size`, in bytes.
    pub bytes: u64,
    /// Its device's `Medium Type` is `Rotational` (a hard disk), not `Solid State`.
    pub rotational: bool,
}

/// The Mac's internal disk in [`disk_cmd`]'s listing.
#[must_use]
pub fn internal_disk(text: &str) -> Option<Disk> {
    #[derive(Default)]
    struct Medium {
        bytes: u64,
        whole: bool,
        fixed: bool,
    }
    #[derive(Default)]
    struct Device {
        internal: bool,
        rotational: bool,
        media: Vec<Medium>,
    }
    impl Device {
        fn disks(&self) -> impl Iterator<Item = Disk> + '_ {
            let internal = self.internal;
            self.media
                .iter()
                .filter(move |m| internal && m.whole && m.fixed && m.bytes > 0)
                .map(|m| Disk {
                    bytes: m.bytes,
                    rotational: self.rotational,
                })
        }
    }
    enum Reading {
        Device,
        Medium,
        Other,
    }
    let mut devices: Vec<Device> = Vec::new();
    let mut reading = Reading::Other;
    for line in text.lines() {
        // under it (`  +-o`, `    +-o`), their properties behind `|` and spaces.
        if line.starts_with("+-o ") {
            devices.push(Device::default());
            reading = Reading::Device;
            continue;
        }
        let body = line.trim_start_matches([' ', '|']);
        if let Some(header) = body.strip_prefix("+-o ") {
            reading = if header.contains("<class IOMedia,") {
                if let Some(d) = devices.last_mut() {
                    d.media.push(Medium::default());
                }
                Reading::Medium
            } else {
                Reading::Other
            };
            continue;
        }
        let Some((key, value)) = body.split_once(" = ") else {
            continue;
        };
        let Some(device) = devices.last_mut() else {
            continue;
        };
        match (&reading, key) {
            (Reading::Device, "\"Protocol Characteristics\"") => {
                device.internal = value.contains("\"Physical Interconnect Location\"=\"Internal\"");
            }
            (Reading::Device, "\"Device Characteristics\"") => {
                device.rotational = value.contains("\"Medium Type\"=\"Rotational\"");
            }
            (Reading::Medium, _) => {
                let Some(m) = device.media.last_mut() else {
                    continue;
                };
                match key {
                    "\"Whole\"" => m.whole = value == "Yes",
                    "\"Removable\"" => m.fixed = value == "No",
                    "\"Size\"" => m.bytes = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    devices
        .iter()
        .flat_map(Device::disks)
        .max_by_key(|d| d.bytes)
}

/// Apple internal SSD capacities and minimum reported size ratios ([`disk_size`]).
const SOLD: [(u64, &str, u128); 8] = [
    (128_000_000_000, "128GB", 940),
    (256_000_000_000, "256GB", 940),
    (512_000_000_000, "512GB", 940),
    (1_000_000_000_000, "1TB", 995),
    (2_000_000_000_000, "2TB", 995),
    (4_000_000_000_000, "4TB", 995),
    (8_000_000_000_000, "8TB", 995),
    (16_000_000_000_000, "16TB", 995),
];

/// A disk's size as Apple sells it, from the bytes `ioreg` reports for the whole disk.
#[must_use]
pub fn disk_size(bytes: u64, rotational: bool) -> String {
    let sold = SOLD.iter().find(|(size, _, least)| {
        let (b, s) = (u128::from(bytes), u128::from(*size));
        b * 1000 >= s * least && b * 1000 <= s * 1001
    });
    if let (false, Some((_, sold, _))) = (rotational, sold) {
        return (*sold).to_owned();
    }
    if bytes < 999_500_000_000 {
        return format!("{}GB", (bytes + 500_000_000) / 1_000_000_000);
    }
    let tenths = (bytes + 50_000_000_000) / 100_000_000_000;
    if tenths.is_multiple_of(10) {
        format!("{}TB", tenths / 10)
    } else {
        format!("{}.{}TB", tenths / 10, tenths % 10)
    }
}

/// Memory as Apple sells it: whole gibibytes, written `GB` (`hw.memsize` 34359738368 is `32GB`).
#[must_use]
pub fn memory_size(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    format!("{}GB", (bytes + GIB / 2) / GIB)
}

/// What a Mac is, in the order its name says it: model, chip, memory, disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Description {
    parts: Vec<String>,
}

impl Description {
    /// The words, spaced: `MacBook Pro M4 Max 36GB 1TB`.
    #[must_use]
    pub fn spaced(&self) -> String {
        self.parts.join(" ")
    }

    /// Each part in a Local hostname's form (`MacBook-Pro`).
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        self.parts.iter().map(|p| label_form(p)).collect()
    }

    /// The parts, as a `hello` carries them to the controller (`MacBook Air`, `M5`, `32GB`, `4TB`).
    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    /// The description a `hello` carried ([`Description::parts`]); `None` when it carried none.
    #[must_use]
    pub fn from_parts(parts: &[String]) -> Option<Self> {
        (!parts.is_empty() && parts.iter().all(|p| !p.trim().is_empty())).then(|| Self {
            parts: parts.to_vec(),
        })
    }
}

/// The description of the Mac `facts` were read on.
///
/// # Errors
/// One line naming each fact the agent could not read; the Mac is never described without one.
pub fn describe(facts: &HostFacts) -> Result<Description, String> {
    let mut missing: Vec<&str> = Vec::new();
    if facts.model_name.is_empty() {
        missing.push("its model (ioreg's product-name, sysctl hw.model)");
    }
    if facts.chip.is_empty() {
        missing.push("its chip (sysctl machdep.cpu.brand_string)");
    }
    if facts.memory_bytes == 0 {
        missing.push("its memory (sysctl hw.memsize)");
    }
    if facts.disk_bytes == 0 {
        missing.push("its internal disk (ioreg -c IOBlockStorageDevice)");
    }
    if !missing.is_empty() {
        return Err(format!(
            "the Mac's hardware is not known: {} did not answer",
            missing.join(", ")
        ));
    }
    let chip = facts.chip.strip_prefix("Apple ").unwrap_or(&facts.chip);
    let chip = chip.split(" (").next().unwrap_or(chip).trim();
    let chip = if facts.processor_speed.is_empty() {
        chip.to_owned()
    } else {
        format!("{chip} {}", facts.processor_speed.replace(' ', ""))
    };
    Ok(Description {
        parts: vec![
            facts.model_name.clone(),
            chip,
            memory_size(facts.memory_bytes),
            disk_size(facts.disk_bytes, facts.disk_rotational),
        ],
    })
}

/// `text` as macOS turns a Computer Name into a Local hostname.
#[must_use]
pub fn label_form(text: &str) -> String {
    label_form_keeping(text, "")
}

/// [`label_form`] with each `keep` in `text` (such as [`PLACEHOLDER`]) preserved.
#[must_use]
pub fn label_form_keeping(text: &str, keep: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if !keep.is_empty()
            && let Some(after) = rest.strip_prefix(keep)
        {
            out.push_str(keep);
            rest = after;
            continue;
        }
        rest = &rest[c.len_utf8()..];
        if matches!(c, '\'' | '\u{2019}') {
            continue;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `ioreg -p IODeviceTree -r -n product -d 1` on the MacBook Air (Mac17,4, macOS
    /// 27.0.1), the serial-number rows and the binary ones around them left out.
    pub(crate) const AIR_PRODUCT: &str = r#"+-o product  <class IOPlatformDevice, id 0x1000002ec, registered, matched, active, busy 0 (24 ms), retain 17>
    {
      "sandman-support" = <01000000>
      "wifi-chipset" = <"proxima">
      "fdr-product-type" = <"Mac17,4">
      "product-name" = <"MacBook Air (15-inch, M5)">
      "sub-product-type" = <"Mac17,4">
      "product-description" = <"MacBook Air (15-inch, M5)">
      "product-soc-name" = <"Apple M5">
      "unique-model" = <"J815AP">
      "name" = <"product">
    }
"#;

    /// The same in the macOS 27.0 VM (VirtualMac2,1).
    pub(crate) const VM_PRODUCT: &str = r#"+-o product  <class IOPlatformDevice, id 0x10000014b, registered, matched, active, busy 0 (0 ms), retain 8>
    {
      "fdr-product-type" = <"VirtualMac2,1">
      "product-description" = <"virtual machine for ARM-v8.4 with Apple Silicon extensions">
      "sub-product-type" = <"VirtualMac2,1">
      "product-soc-name" = <"Apple M5 (Virtual)">
      "unique-model" = <"VMA2MACOSAP">
      "name" = <"product">
      "product-name" = <"Apple Virtual Machine 1">
    }
"#;

    /// `ioreg -r -c IOBlockStorageDevice -d 3 -l -w 0` on the MacBook Air (Mac17,4, macOS
    /// 27.0.1, its 4 TB SSD; the SSD's serial number left out) and in the macOS 27.0 VM (its
    /// 90 GB tart disk, and a 4 MB disk image the VM had attached).
    pub(crate) const AIR_DISKS: &str =
        include_str!("../fixtures/ioreg/air-27.0.1-block-storage.txt");
    pub(crate) const VM_DISKS: &str = include_str!("../fixtures/ioreg/vm-27.0-block-storage.txt");

    fn facts(model: &str, chip: &str, speed: &str, memory: u64, disk: u64) -> HostFacts {
        HostFacts {
            model_name: model.into(),
            chip: chip.into(),
            processor_speed: speed.into(),
            memory_bytes: memory,
            disk_bytes: disk,
            ..HostFacts::default()
        }
    }

    const SSD: Disk = Disk {
        bytes: 0,
        rotational: false,
    };

    #[test]
    fn the_air_reads_as_system_information_names_it() {
        assert_eq!(
            parse_product_name(AIR_PRODUCT).as_deref(),
            Some("MacBook Air (15-inch, M5)")
        );
        assert_eq!(
            model_name("Mac17,4", parse_product_name(AIR_PRODUCT).as_deref()).as_deref(),
            Some("MacBook Air")
        );
        assert_eq!(
            parse_brand("Apple M5\n"),
            Some(("Apple M5".into(), String::new()))
        );
        assert_eq!(parse_memsize("34359738368\n"), Some(34_359_738_368));
        assert_eq!(
            internal_disk(AIR_DISKS),
            Some(Disk {
                bytes: 4_002_222_325_760,
                ..SSD
            })
        );
        let air = facts(
            "MacBook Air",
            "Apple M5",
            "",
            34_359_738_368,
            4_002_222_325_760,
        );
        assert_eq!(describe(&air).unwrap().spaced(), "MacBook Air M5 32GB 4TB");
    }

    #[test]
    fn the_vm_is_a_virtual_machine_with_its_measured_disk() {
        assert_eq!(
            model_name("VirtualMac2,1", parse_product_name(VM_PRODUCT).as_deref()).as_deref(),
            Some("Virtual Machine")
        );
        assert_eq!(
            internal_disk(VM_DISKS),
            Some(Disk {
                bytes: 90_000_003_072,
                ..SSD
            })
        );
        let vm = facts(
            "Virtual Machine",
            "Apple M5 (Virtual)",
            "",
            8_589_934_592,
            90_000_003_072,
        );
        let d = describe(&vm).unwrap();
        assert_eq!(d.spaced(), "Virtual Machine M5 8GB 90GB");
        assert_eq!(d.labels(), ["Virtual-Machine", "M5", "8GB", "90GB"]);
        assert_eq!(Description::from_parts(d.parts()), Some(d));
        assert_eq!(Description::from_parts(&[]), None);
        assert_eq!(Description::from_parts(&["M5".into(), " ".into()]), None);
    }

    #[test]
    fn an_intel_processor_is_its_family_and_speed() {
        assert_eq!(
            parse_brand("Intel(R) Core(TM) i7-7920HQ CPU @ 3.10GHz\n"),
            Some(("Intel Core i7".into(), "3.1 GHz".into()))
        );
        assert_eq!(
            parse_brand("Intel(R) Xeon(R) W-2140B CPU @ 3.20GHz"),
            Some(("Intel Xeon W".into(), "3.2 GHz".into()))
        );
        assert_eq!(
            parse_brand("Intel(R) Core(TM) i5-1038NG7 CPU @ 2.00GHz"),
            Some(("Intel Core i5".into(), "2.0 GHz".into()))
        );
        assert_eq!(
            model_name("MacBookPro14,3", None).as_deref(),
            Some("MacBook Pro")
        );
        assert_eq!(model_name("Macmini8,1", None).as_deref(), Some("Mac mini"));
        let m4 = facts(
            "MacBook Pro",
            "Intel Core i7",
            "3.1 GHz",
            17_179_869_184,
            1_000_555_581_440,
        );
        let d = describe(&m4).unwrap();
        assert_eq!(d.spaced(), "MacBook Pro Intel Core i7 3.1GHz 16GB 1TB");
        assert_eq!(
            d.labels(),
            ["MacBook-Pro", "Intel-Core-i7-3-1GHz", "16GB", "1TB"]
        );
    }

    #[test]
    fn disks_read_as_the_size_apple_sells() {
        for (bytes, sold) in [
            (121_332_826_112, "128GB"),
            (251_000_193_024, "256GB"),
            (500_277_790_720, "512GB"),
            (1_000_555_581_440, "1TB"),
            (2_001_111_162_880, "2TB"),
            (4_002_222_325_760, "4TB"),
            (8_001_563_222_016, "8TB"),
            (256_060_514_304, "256GB"),
            (1_000_204_886_016, "1TB"),
            (2_000_398_934_016, "2TB"),
        ] {
            assert_eq!(disk_size(bytes, false), sold, "{bytes}");
        }
        for (bytes, measured) in [
            (90_000_003_072, "90GB"),
            (120_034_123_776, "120GB"),
            (240_057_409_536, "240GB"),
            (480_103_981_056, "480GB"),
            (960_197_124_096, "960GB"),
            (1_920_383_410_176, "1.9TB"),
            (3_840_755_982_336, "3.8TB"),
            (7_681_501_126_656, "7.7TB"),
            (1_500_000_000_000, "1.5TB"),
            (3_000_000_000_000, "3TB"),
        ] {
            assert_eq!(disk_size(bytes, false), measured, "{bytes}");
        }
        assert_eq!(disk_size(500_107_862_016, true), "500GB");
        assert_eq!(disk_size(500_107_862_016, false), "512GB");
        assert_eq!(disk_size(1_000_204_886_016, true), "1TB");
        assert_eq!(disk_size(3_000_592_982_016, true), "3TB");
        assert_eq!(memory_size(38_654_705_664), "36GB");
        assert_eq!(memory_size(137_438_953_472), "128GB");
    }

    /// The Air's listing with its SSD's `Medium Type` and `Size` replaced, and its medium's
    /// `Removable`, for the shapes no Mac here has.
    fn air_disk(medium: &str, size: u64, removable: &str, location: &str) -> String {
        AIR_DISKS
            .replace(
                "\"Medium Type\"=\"Solid State\"",
                &format!("\"Medium Type\"={medium:?}"),
            )
            .replace("\"Size\" = 4002222325760", &format!("\"Size\" = {size}"))
            .replace(
                "\"Removable\" = No",
                &format!("\"Removable\" = {removable}"),
            )
            .replace(
                "\"Physical Interconnect Location\"=\"Internal\"",
                &format!("\"Physical Interconnect Location\"={location:?}"),
            )
    }

    #[test]
    fn the_internal_disk_is_the_largest_fixed_one_and_never_an_external_or_removable_one() {
        let fusion = format!(
            "{}{}",
            air_disk("Solid State", 32_000_000_000, "No", "Internal"),
            air_disk("Rotational", 1_000_204_886_016, "No", "Internal")
        );
        let disk = internal_disk(&fusion).unwrap();
        assert_eq!(
            disk,
            Disk {
                bytes: 1_000_204_886_016,
                rotational: true
            }
        );
        assert_eq!(disk_size(disk.bytes, disk.rotational), "1TB");
        let external_first = format!(
            "{}{AIR_DISKS}",
            air_disk("Solid State", 8_001_563_222_016, "No", "External")
        );
        assert_eq!(
            internal_disk(&external_first).unwrap().bytes,
            4_002_222_325_760
        );
        let card = format!(
            "{AIR_DISKS}{}",
            air_disk("Solid State", 8_001_563_222_016, "Yes", "Internal")
        );
        assert_eq!(internal_disk(&card).unwrap().bytes, 4_002_222_325_760);
        assert_eq!(
            internal_disk(&air_disk("Solid State", 1, "No", "External")),
            None
        );
        assert_eq!(internal_disk(""), None);
        assert_eq!(internal_disk("ioreg: no such class\n"), None);
    }

    #[test]
    fn a_mac_missing_a_fact_is_not_described_and_the_answer_names_it() {
        let mut f = facts("MacBook Pro", "Apple M4 Max", "", 38_654_705_664, 0);
        let err = describe(&f).unwrap_err();
        assert!(
            err.contains("IOBlockStorageDevice") && !err.contains("hw.memsize"),
            "{err}"
        );
        f.disk_bytes = 1_000_555_581_440;
        assert_eq!(
            describe(&f).unwrap().spaced(),
            "MacBook Pro M4 Max 36GB 1TB"
        );
        assert_eq!(model_name("Mac16,1", None), None);
        assert_eq!(parse_memsize("sysctl: unknown oid"), None);
        assert_eq!(parse_brand("\n"), None);
    }

    #[test]
    fn labels_follow_macos_own_rule() {
        assert_eq!(label_form("3.1GHz"), "3-1GHz");
        assert_eq!(
            label_form("Managed\u{2019}s Virtual Machine"),
            "Manageds-Virtual-Machine"
        );
        assert_eq!(label_form(" -MacBook  Pro- "), "MacBook-Pro");
        assert_eq!(label_form("lab_03.b"), "lab-03-b");
        assert_eq!(
            label_form_keeping("m1.lab_{hardware}.", PLACEHOLDER),
            "m1-lab-{hardware}"
        );
        assert_eq!(label_form_keeping("{hardware}", PLACEHOLDER), "{hardware}");
    }
}
