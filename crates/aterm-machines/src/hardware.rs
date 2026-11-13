// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mac hardware discovery ([`internal_disk`], [`HostFacts`], [`describe`]).

use crate::model::HostFacts;
use crate::port::Cmd;

/// Extracts the string value of `"<key>" = "<value>"` in `ioreg` output.
#[must_use]
pub fn ioreg_value(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\" = \"");
    text.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix(&needle)?;
        let value = rest.strip_suffix('"')?;
        (!value.is_empty() && !value.contains('"')).then(|| value.to_owned())
    })
}

/// Whether `s` has an IOPlatformUUID's shape: `8-4-4-4-12` hex digits.
#[must_use]
pub fn uuid_shaped(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

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
    crate::parse_uint(text.trim()).filter(|b| *b > 0)
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

/// The model's marketing name. It is the one definition every reader of a Mac's name uses, the
/// in-process type reader's Mac rung (stats deck A4 §9), whose filter refuses a product name that
/// is a code or a placeholder.
pub use aterm_sysprobe::machine_type::model_name;

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
        // A device starts each subtree at the first column; its driver and medium are drawn
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
                    "\"Size\"" => m.bytes = crate::parse_uint(value).unwrap_or(0),
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

pub use aterm_types::machine_name::{disk_size, memory_size};

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

pub use aterm_types::machine_name::{label_form, label_form_keeping};

#[cfg(test)]
#[path = "hardware_tests.rs"]
pub(crate) mod tests;
