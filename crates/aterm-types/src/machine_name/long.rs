// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The long OS name derived from a machine's number and its hardware
//! (`m8-macbook-pro-m5-128gb-8tb`), and the OS slots each name goes into
//! (`docs/DESIGN-machine-names-2026-11-04.md` §4).

use super::{Number, disk_size, label_form, memory_size};

/// A DNS label's limit, and so the long form's: macOS's ComputerName is a DNS-SD
/// instance label, and Linux's pretty name is kept to the same.
pub const DNS_LABEL_MAX: usize = 63;

/// The Windows computer description's limit (`net config server /srvcomment`).
pub const WINDOWS_DESCRIPTION_MAX: usize = 48;

/// Whether the long form names the chip's tier (`m5-max`). Off, as the owner's own
/// example `m8-macbook-pro-m5-128gb-8tb` leaves it out: the number already tells every
/// machine apart, and the long form only describes the hardware.
pub const LONG_CHIP_TIER: bool = false;

/// The model words for Apple's marketing names. A name not in the table is slugged
/// and cut to three words.
pub const MODEL_WORDS: [(&str, &str); 8] = [
    ("MacBook Pro", "macbook-pro"),
    ("MacBook Air", "macbook-air"),
    ("Mac Studio", "mac-studio"),
    ("Mac mini", "mac-mini"),
    ("iMac", "imac"),
    ("iMac Pro", "imac-pro"),
    ("Mac Pro", "mac-pro"),
    ("MacBook", "macbook"),
];

/// The most words a slugged model name keeps (`thinkpad-x1-carbon`).
const MODEL_WORDS_KEPT: usize = 3;

/// Words a hypervisor writes into the machine's identity (macOS's `VirtualMac` aside),
/// matched as whole words in the vendor, product and family.
const HYPERVISOR_WORDS: [&str; 8] = [
    "qemu",
    "kvm",
    "vmware",
    "virtualbox",
    "innotek",
    "parallels",
    "xen",
    "apple virtualization",
];

/// What firmware writes where a model belongs, read as no model.
const PLACEHOLDERS: [&str; 13] = [
    "to be filled by o.e.m.",
    "system product name",
    "system version",
    "system manufacturer",
    "default string",
    "not applicable",
    "not specified",
    "none",
    "o.e.m.",
    "oem",
    "invalid",
    "unknown",
    "type1productconfigid",
];

/// The internal disk aterm reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disk {
    /// Its size in bytes, as the OS reports the whole disk.
    pub bytes: u64,
    /// Whether it spins, so it is named by its measured size, not a size Apple sells.
    pub rotational: bool,
}

/// The facts a machine's long OS name is derived from, each `None` until it is read.
/// Each OS's reader fills the fields it has; nothing here reads the machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineFacts {
    /// macOS `hw.model` (`Mac17,6`, `VirtualMac2,1`).
    pub hw_model: Option<String>,
    /// The marketing name macOS reports (`MacBook Pro`).
    pub marketing: Option<String>,
    /// Linux DMI `product_family` (`ThinkPad X1 Carbon Gen 11`).
    pub family: Option<String>,
    /// Linux DMI `product_name`, or Windows `Win32_ComputerSystem.Model`.
    pub product: Option<String>,
    /// Linux DMI `sys_vendor`, or Windows `Win32_ComputerSystem.Manufacturer`.
    pub vendor: Option<String>,
    /// The processor's brand string (`Apple M5 Max`, `Intel(R) Core(TM) i7-8559U …`).
    pub chip: Option<String>,
    /// Installed memory in bytes.
    pub memory_bytes: Option<u64>,
    /// The internal disk.
    pub disk: Option<Disk>,
}

/// The OS whose name slots [`os_slots`] fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameOs {
    /// macOS: HostName, LocalHostName and ComputerName.
    MacOs,
    /// Linux: the static and pretty host names and `/etc/hosts`' `127.0.1.1` line.
    Linux,
    /// Windows: the DNS host name and the computer description.
    Windows,
}

/// One OS name slot aterm writes. NetBIOS names are not here: macOS and Windows derive
/// them from the short name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsSlot {
    /// macOS `HostName` (`hostname`, shell prompts).
    MacHostName,
    /// macOS `LocalHostName` (Bonjour, `m8.local`).
    MacLocalHostName,
    /// macOS `ComputerName` (Finder, AirDrop, Sharing).
    MacComputerName,
    /// Linux static host name (`hostnamectl --static`).
    LinuxStatic,
    /// Linux pretty host name (`/etc/machine-info`).
    LinuxPretty,
    /// The name on `/etc/hosts`' `127.0.1.1` line (Debian family).
    LinuxHostsLine,
    /// Windows DNS host name (`Rename-Computer`).
    WindowsHostName,
    /// Windows computer description (`srvcomment`).
    WindowsDescription,
}

impl OsSlot {
    /// The macOS slot named by an Apple `scutil` preference.
    #[must_use]
    pub fn from_macos_preference(preference: &str) -> Option<Self> {
        match preference {
            "HostName" => Some(Self::MacHostName),
            "LocalHostName" => Some(Self::MacLocalHostName),
            "ComputerName" => Some(Self::MacComputerName),
            _ => None,
        }
    }

    /// A numeric Bonjour alias of the exact expected DNS label. This diagnoses a
    /// collision without treating it as alignment or asking the setter to fight it.
    #[must_use]
    pub fn collision(self, expected: &str, observed: &str) -> bool {
        self == Self::MacLocalHostName
            && !expected.is_empty()
            && expected.len() <= DNS_LABEL_MAX
            && !expected.starts_with('-')
            && !expected.ends_with('-')
            && expected
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && observed
                .strip_prefix(expected)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|suffix| suffix.parse::<u32>().ok().map(|n| (suffix, n)))
                .is_some_and(|(suffix, n)| n >= 2 && n.to_string() == suffix)
    }

    /// The most bytes the slot holds as aterm writes it.
    #[must_use]
    pub const fn limit(self) -> usize {
        match self {
            Self::WindowsDescription => WINDOWS_DESCRIPTION_MAX,
            _ => DNS_LABEL_MAX,
        }
    }
}

/// Each slot aterm writes on `os` and its value for the machine numbered `number`.
/// Addressing slots get the bare number and description slots the long form, so
/// `m8.local` keeps resolving. macOS's ComputerName gets the long form only once the
/// name rule's second version is in place (`computer_name_long`), the bare number
/// before.
#[must_use]
pub fn os_slots(
    os: NameOs,
    number: Number,
    facts: &MachineFacts,
    computer_name_long: bool,
) -> Vec<(OsSlot, String)> {
    let short = number.to_string();
    match os {
        NameOs::MacOs => vec![
            (OsSlot::MacHostName, short.clone()),
            (OsSlot::MacLocalHostName, short.clone()),
            (
                OsSlot::MacComputerName,
                if computer_name_long {
                    long_name(number, facts)
                } else {
                    short
                },
            ),
        ],
        NameOs::Linux => vec![
            (OsSlot::LinuxStatic, short.clone()),
            (OsSlot::LinuxPretty, long_name(number, facts)),
            (OsSlot::LinuxHostsLine, short),
        ],
        NameOs::Windows => vec![
            (OsSlot::WindowsHostName, short),
            (
                OsSlot::WindowsDescription,
                long_name_within(number, facts, WINDOWS_DESCRIPTION_MAX),
            ),
        ],
    }
}

/// The long OS name: the number, then the model, chip, memory and disk words that are
/// known, within a DNS label (`m8-macbook-pro-m5-128gb-8tb`; `m9` when nothing is).
#[must_use]
pub fn long_name(number: Number, facts: &MachineFacts) -> String {
    long_name_within(number, facts, DNS_LABEL_MAX)
}

/// [`long_name`] within `limit` bytes: the disk, memory, chip tier, chip and model
/// words are dropped in that order until it fits. The number is never dropped.
#[must_use]
pub fn long_name_within(number: Number, facts: &MachineFacts, limit: usize) -> String {
    let (chip, tier) = facts
        .chip
        .as_deref()
        .and_then(chip_parts)
        .map_or((None, None), |(chip, tier)| (Some(chip), tier));
    let mut parts = Parts {
        number: number.to_string(),
        model: model_word(facts),
        chip,
        tier: tier.filter(|_| LONG_CHIP_TIER),
        memory: facts
            .memory_bytes
            .filter(|b| *b > 0)
            .map(|b| memory_size(b).to_ascii_lowercase()),
        disk: facts.disk.filter(|d| d.bytes > 0).map(disk_word),
    };
    loop {
        let name = parts.joined();
        if name.len() <= limit || !parts.drop_one() {
            return name;
        }
    }
}

/// The long form's words, in its order.
struct Parts {
    number: String,
    model: Option<String>,
    chip: Option<String>,
    tier: Option<&'static str>,
    memory: Option<String>,
    disk: Option<String>,
}

impl Parts {
    fn joined(&self) -> String {
        let chip = self.chip.as_ref().map(|chip| match self.tier {
            Some(tier) => format!("{chip}-{tier}"),
            None => chip.clone(),
        });
        [
            Some(self.number.clone()),
            self.model.clone(),
            chip,
            self.memory.clone(),
            self.disk.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("-")
    }

    /// Drops the next word in the fit order; false once only the number is left.
    fn drop_one(&mut self) -> bool {
        if self.disk.take().is_some() || self.memory.take().is_some() {
            return true;
        }
        if self.tier.take().is_some() {
            return true;
        }
        self.chip.take().is_some() || self.model.take().is_some()
    }
}

/// The disk's word: its sold size, lowercase, and in gigabytes when it is not a whole
/// number of terabytes, so the word has no `.` (`1.5TB` is `1500gb`).
fn disk_word(disk: Disk) -> String {
    let size = disk_size(disk.bytes, disk.rotational).to_ascii_lowercase();
    match size.strip_suffix("tb").and_then(|tb| tb.split_once('.')) {
        Some((whole, tenth)) => {
            let whole: u64 = whole.parse().unwrap_or(0);
            let tenth: u64 = tenth.parse().unwrap_or(0);
            format!("{}gb", whole * 1000 + tenth * 100)
        }
        None => size,
    }
}

/// Whether the machine is a virtual one: macOS's `VirtualMac` models, or a hypervisor's
/// word in the vendor, product or family (QEMU, KVM, VMware, VirtualBox, Parallels, Xen,
/// Apple Virtualization), or Hyper-V's `Microsoft Corporation` `Virtual Machine`.
#[must_use]
pub fn is_virtual(facts: &MachineFacts) -> bool {
    if facts
        .hw_model
        .as_deref()
        .is_some_and(|model| model.starts_with("VirtualMac"))
    {
        return true;
    }
    let named = [&facts.vendor, &facts.product, &facts.family]
        .into_iter()
        .flatten()
        .any(|text| {
            let text = text.to_ascii_lowercase();
            HYPERVISOR_WORDS.iter().any(|word| has_word(&text, word))
        });
    let hyper_v = facts
        .vendor
        .as_deref()
        .is_some_and(|v| has_word(&v.to_ascii_lowercase(), "microsoft"))
        && facts
            .product
            .as_deref()
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("virtual machine"));
    named || hyper_v
}

/// Whether `word` stands in `text` with no ASCII letter or digit on either side.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].bytes().next_back();
        let after = text[at + word.len()..].bytes().next();
        before.is_none_or(|b| !b.is_ascii_alphanumeric())
            && after.is_none_or(|b| !b.is_ascii_alphanumeric())
    })
}

/// [`model_word`]'s word for a virtual machine on any OS.
pub const VM_WORD: &str = "vm";

/// The model's word in the long form: [`VM_WORD`] for a virtual machine; otherwise Apple's
/// marketing name through [`MODEL_WORDS`], else the first of the marketing name, DMI
/// family and product (Windows' model) that is not a firmware placeholder, its
/// vendor's words dropped, slugged and cut to three words.
#[must_use]
pub fn model_word(facts: &MachineFacts) -> Option<String> {
    if is_virtual(facts) {
        return Some(VM_WORD.to_owned());
    }
    let vendor = facts.vendor.as_deref().map(slug_words).unwrap_or_default();
    [&facts.marketing, &facts.family, &facts.product]
        .into_iter()
        .flatten()
        .map(|source| source.trim())
        .filter(|source| !is_placeholder(source))
        .find_map(|source| {
            if let Some((_, word)) = MODEL_WORDS
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(source))
            {
                return Some((*word).to_owned());
            }
            let words: Vec<String> = slug_words(source)
                .into_iter()
                .filter(|word| !vendor.contains(word))
                .take(MODEL_WORDS_KEPT)
                .collect();
            (!words.is_empty()).then(|| words.join("-"))
        })
}

fn is_placeholder(text: &str) -> bool {
    text.is_empty()
        || text.bytes().all(|b| b.is_ascii_digit())
        || PLACEHOLDERS.iter().any(|p| p.eq_ignore_ascii_case(text))
}

/// `text`'s words as macOS's label rule cuts them, lowercase.
fn slug_words(text: &str) -> Vec<String> {
    label_form(text)
        .to_ascii_lowercase()
        .split('-')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The chip's word in the long form: Apple `m<gen>` (with `-pro`, `-max` or `-ultra`
/// under [`LONG_CHIP_TIER`]), Intel `i3`/`i5`/`i7`/`i9` or `xeon`, AMD `ryzen-<n>`,
/// `threadripper` or `epyc`; never a clock speed, and `None` for any other chip.
#[must_use]
pub fn chip_word(brand: &str) -> Option<String> {
    let (chip, tier) = chip_parts(brand)?;
    Some(match tier.filter(|_| LONG_CHIP_TIER) {
        Some(tier) => format!("{chip}-{tier}"),
        None => chip,
    })
}

/// The chip's word and its Apple tier, which the fit can drop on its own.
fn chip_parts(brand: &str) -> Option<(String, Option<&'static str>)> {
    let words = slug_words(brand);
    let at = |i: usize| words.get(i).map(String::as_str);
    let apple_gen = |word: &str| {
        word.strip_prefix('m')
            .is_some_and(|n| (1..=2).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let apple = match at(0) {
        Some("apple") => Some(1),
        Some(first) if apple_gen(first) => Some(0),
        _ => None,
    };
    if let Some(i) = apple
        && let Some(generation) = at(i).filter(|w| apple_gen(w))
    {
        let tier = match at(i + 1) {
            Some("pro") => Some("pro"),
            Some("max") => Some("max"),
            Some("ultra") => Some("ultra"),
            _ => None,
        };
        return Some((generation.to_owned(), tier));
    }
    if words.iter().any(|w| w == "intel") {
        if let Some(core) = words
            .iter()
            .find(|w| matches!(w.as_str(), "i3" | "i5" | "i7" | "i9"))
        {
            return Some((core.clone(), None));
        }
        return words
            .iter()
            .any(|w| w == "xeon")
            .then(|| ("xeon".to_owned(), None));
    }
    if words.iter().any(|w| w == "amd") {
        if words.iter().any(|w| w == "threadripper") {
            return Some(("threadripper".to_owned(), None));
        }
        if words.iter().any(|w| w == "epyc") {
            return Some(("epyc".to_owned(), None));
        }
        let ryzen = words.iter().position(|w| w == "ryzen")?;
        let n = at(ryzen + 1).filter(|n| n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()))?;
        return Some((format!("ryzen-{n}"), None));
    }
    None
}
