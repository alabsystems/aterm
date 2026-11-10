// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The computer's type, named (stats deck A4 §9, `docs/DESIGN-stats-deck-instruments-2026-11-06.md`),
//! read in process once ([`read`]): on macOS the device tree's `product` node's `product-name`
//! and `hw.model`; on Linux the device tree's `model` where there is one, else the firmware's DMI
//! table under `/sys/class/dmi/id`; on Windows the firmware's table as the registry keeps it
//! (`HKLM\HARDWARE\DESCRIPTION\System\BIOS`). No child process, no window server.
//!
//! The name goes through a fallback ladder whose every rung passes one filter ([`friendly`]): a
//! firmware field that is a machine-type code (`21K9CTO1WW`), a bare version (`1.0`) or a
//! known placeholder (`To Be Filled By O.E.M.`) never names the machine; where nothing passes,
//! the name is `None` and the OS's machine noun stands. The ladder's Mac rung
//! ([`model_name`]) is the one every reader of a Mac's name uses (`aterm-machines` re-exports it).

/// The computer's type, as [`read`] names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachineType {
    /// The short name a person knows the machine by (`MacBook Air`, `Lenovo ThinkPad X1 Carbon
    /// Gen 12`); `None` where no field passed the ladder.
    pub name: Option<String>,
    /// The family form, shorter, where the firmware gives one apart from the name (`Dell
    /// Latitude`); `None` on a Mac, whose name is its family.
    pub family: Option<String>,
    /// The long form (`MacBook Air (15-inch, M5)`), on a Mac; `None` elsewhere, where the
    /// firmware gives no longer form than the name.
    pub model: Option<String>,
    /// The firmware's identifier for the model (`Mac17,4`, `21K9CTO1WW`), which may be a code.
    pub model_id: Option<String>,
}

/// Placeholders firmware ships in its product and vendor fields, compared without case: A4 §9's
/// list and the others seen in the same fields (`System manufacturer`, `Not Specified`, …).
const PLACEHOLDERS: &[&str] = &[
    "to be filled by o.e.m.",
    "to be filled by oem",
    "system product name",
    "system manufacturer",
    "system version",
    "default string",
    "not applicable",
    "not specified",
    "o.e.m.",
    "oem",
    "none",
    "type1productconfigid",
    "unknown",
    "undefined",
    "invalid",
    "n/a",
    "x.x",
];

/// Whether `field` is a machine-type code rather than a name: one token (no space) of letters
/// and digits (an underscore between them allowed, as HP's `103C_5336AN`) with at least six
/// digits; or with a configure-to-order `CTO` or a world-wide `WW` in it, as Lenovo writes its
/// machine type (`21K9CTO1WW`); or set in capitals throughout, eight characters or more with
/// three digits or more, as Lenovo's other machine types (`81YK00EUUS`).
#[must_use]
pub fn is_code(field: &str) -> bool {
    let t = field.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return false;
    }
    let digits = t.bytes().filter(u8::is_ascii_digit).count();
    let upper = t.to_ascii_uppercase();
    let capitals = !t.bytes().any(|b| b.is_ascii_lowercase());
    digits >= 6
        || (digits > 0 && (upper.contains("CTO") || upper.ends_with("WW")))
        || (capitals && t.len() >= 8 && digits >= 3)
}

/// Whether `field` is one of the firmware's placeholders, or empty.
#[must_use]
pub fn is_placeholder(field: &str) -> bool {
    let t = field.trim();
    t.is_empty() || PLACEHOLDERS.iter().any(|p| t.eq_ignore_ascii_case(p))
}

/// `field`, trimmed and less any code that leads it (HP's family `103C_5336AN HP EliteBook`
/// reads `HP EliteBook`), where what is left can name a machine: not empty, not a placeholder,
/// not a code ([`is_code`]) and not a bare number or version (`1.0`: no letter in it).
#[must_use]
pub fn friendly(field: &str) -> Option<&str> {
    let mut t = field.trim();
    while let Some((head, rest)) = t.split_once(char::is_whitespace) {
        if !is_code(head) {
            break;
        }
        t = rest.trim_start();
    }
    (!is_placeholder(t) && !is_code(t) && t.bytes().any(|b| b.is_ascii_alphabetic())).then_some(t)
}

/// A firmware vendor field (`LENOVO`, `Dell Inc.`, `Micro-Star International Co., Ltd.`) as the
/// word a name is prefixed with (`Lenovo`, `Dell`, `Micro-Star`): its corporate suffixes dropped
/// and a shouted name of five letters or more (`LENOVO`) set in title case; `None` for a
/// placeholder vendor.
#[must_use]
pub fn vendor_word(field: &str) -> Option<String> {
    const SUFFIXES: &[&str] = &[
        "inc.",
        "inc",
        "co.",
        "co",
        "ltd.",
        "ltd",
        "corporation",
        "corp.",
        "corp",
        "gmbh",
        "llc",
        "limited",
        "international",
        "computer",
        "technology",
        "technologies",
    ];
    let field = friendly(field)?;
    let mut words: Vec<&str> = field.split_whitespace().collect();
    while words.len() > 1 {
        let last = words[words.len() - 1].trim_end_matches(',');
        if SUFFIXES.iter().any(|s| last.eq_ignore_ascii_case(s)) {
            words.pop();
        } else {
            break;
        }
    }
    let word = words.join(" ");
    let word = word.trim_end_matches(',');
    let shouted = word.len() >= 5 && word.bytes().all(|b| b.is_ascii_uppercase());
    let word = if shouted {
        let mut out = word[..1].to_owned();
        out.push_str(&word[1..].to_ascii_lowercase());
        out
    } else {
        word.to_owned()
    };
    friendly(&word).map(str::to_owned)
}

/// `name` with `vendor` before it where the name does not already say the vendor.
fn prefixed(vendor: Option<&str>, name: &str) -> String {
    match vendor {
        Some(v) if !name.to_ascii_lowercase().contains(&v.to_ascii_lowercase()) => {
            format!("{v} {name}")
        }
        _ => name.to_owned(),
    }
}

/// A Mac's marketing name from its `hw.model` (`Mac17,4`) and its device tree's `product-name`
/// (`MacBook Air (15-inch, M5)`): the product name less its parenthesis (`MacBook Air`); a
/// virtual Mac reads `Virtual Machine`; with no product name, the `hw.model` family an Intel Mac
/// names (`MacBookPro14,3` reads `MacBook Pro`); `None` where neither gives a name (an Apple
/// silicon `hw.model` alone is a code, never a name).
#[must_use]
pub fn model_name(hw_model: &str, product_name: Option<&str>) -> Option<String> {
    if hw_model.starts_with("VirtualMac") {
        return Some("Virtual Machine".to_owned());
    }
    if let Some(base) = product_name
        .and_then(|p| p.split(" (").next())
        .and_then(friendly)
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

/// A Mac's type from its `hw.model` and its device tree's `product-name`, each where read.
#[must_use]
pub fn mac_type(hw_model: Option<&str>, product_name: Option<&str>) -> MachineType {
    let hw_model = hw_model.map(str::trim).filter(|m| !m.is_empty());
    MachineType {
        name: model_name(hw_model.unwrap_or(""), product_name),
        family: None,
        model: product_name.and_then(friendly).map(str::to_owned),
        model_id: hw_model.map(str::to_owned),
    }
}

/// The firmware's product fields, as Linux's DMI table (`/sys/class/dmi/id`) and Windows'
/// registry copy of it (`SystemManufacturer`, `SystemProductName`, `SystemVersion`,
/// `SystemFamily`) hold them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dmi<'a> {
    /// `sys_vendor`, `SystemManufacturer`.
    pub vendor: Option<&'a str>,
    /// `product_name`, `SystemProductName`.
    pub name: Option<&'a str>,
    /// `product_version`, `SystemVersion`.
    pub version: Option<&'a str>,
    /// `product_family`, `SystemFamily`.
    pub family: Option<&'a str>,
}

/// The type the firmware's product fields give: the name is the first of the product name,
/// version and family that passes [`friendly`] (Lenovo writes its machine-type code as the
/// product name and the name a person knows as the version), with the vendor before it where it
/// lacks it; the family the same, where it differs from the name; the identifier the product
/// name as written, code or not, unless it is a placeholder.
#[must_use]
pub fn dmi_type(dmi: &Dmi<'_>) -> MachineType {
    let vendor = dmi.vendor.and_then(vendor_word);
    let vendor = vendor.as_deref();
    let name = [dmi.name, dmi.version, dmi.family]
        .into_iter()
        .flatten()
        .find_map(friendly)
        .map(|n| prefixed(vendor, n));
    let family = dmi
        .family
        .and_then(friendly)
        .map(|f| prefixed(vendor, f))
        .filter(|f| name.as_ref() != Some(f));
    MachineType {
        name,
        family,
        model: None,
        model_id: dmi
            .name
            .map(str::trim)
            .filter(|n| !is_placeholder(n))
            .map(str::to_owned),
    }
}

/// The type a device tree gives: its `model` (`Raspberry Pi 5 Model B Rev 1.0`) through the
/// filter, and its first `compatible` entry (`raspberrypi,5-model-b`) as the identifier.
#[must_use]
pub fn device_tree_type(model: Option<&str>, compatible: Option<&str>) -> MachineType {
    let first = |s: &str| {
        s.split('\0')
            .map(str::trim)
            .find(|c| !c.is_empty())
            .map(str::to_owned)
    };
    MachineType {
        name: model
            .and_then(|m| m.split('\0').next())
            .and_then(friendly)
            .map(str::to_owned),
        family: None,
        model: None,
        model_id: compatible.and_then(first),
    }
}

/// The type a Linux tree under `root` gives: the device tree's where its model passes the
/// filter, else the DMI table's.
#[must_use]
pub fn linux_type(root: &crate::linux::Root) -> MachineType {
    let tree = device_tree_type(
        root.text("proc/device-tree/model").as_deref(),
        root.text("proc/device-tree/compatible").as_deref(),
    );
    if tree.name.is_some() {
        return tree;
    }
    let field = |f: &str| root.text(&format!("sys/class/dmi/id/{f}"));
    let (vendor, name, version, family) = (
        field("sys_vendor"),
        field("product_name"),
        field("product_version"),
        field("product_family"),
    );
    dmi_type(&Dmi {
        vendor: vendor.as_deref(),
        name: name.as_deref(),
        version: version.as_deref(),
        family: family.as_deref(),
    })
}

/// This computer's type, read in process (once: the host keeps it); `None` on an OS with no
/// reader.
#[must_use]
pub fn read() -> Option<MachineType> {
    #[cfg(target_os = "macos")]
    {
        Some(crate::macos::machine_type())
    }
    #[cfg(target_os = "linux")]
    {
        Some(linux_type(&crate::linux::Root::system()))
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        Some(crate::win::machine_type())
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        all(windows, target_pointer_width = "64")
    )))]
    {
        None
    }
}

#[cfg(test)]
#[path = "machine_type_tests.rs"]
mod tests;
