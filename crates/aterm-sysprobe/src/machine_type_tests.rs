// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The type's fallback ladder (A4 §9) over firmware fields seen on real machines, and the law
//! that holds it: no rung ever names a machine by a code or a placeholder.

use super::*;

/// The codes firmware writes where a name belongs: Lenovo's machine types, HP's board ids and
/// a few SKU shapes.
const CODES: &[&str] = &[
    "21K9CTO1WW",
    "20XW0026US",
    "81YK00EUUS",
    "103C_5336AN",
    "0K8J2R123456",
    "MT21K9WW",
];

/// A Lenovo ThinkPad as its DMI table reads (`/sys/class/dmi/id`): the machine-type code as the
/// product name and the name a person knows as the version.
const THINKPAD: Dmi<'static> = Dmi {
    vendor: Some("LENOVO"),
    name: Some("21K9CTO1WW"),
    version: Some("ThinkPad X1 Carbon Gen 12"),
    family: Some("ThinkPad X1 Carbon Gen 12"),
};

fn named(t: &MachineType) -> Option<&str> {
    t.name.as_deref()
}

/// A4 §15 `the_machine_row_names_the_type`, the reader's half, over `hardware.rs`'s Mac
/// fixtures: the M5 Air reads its product name less its parenthesis, with the long form and
/// `hw.model` beside it; a VirtualMac reads `Virtual Machine`; an Intel Mac with no product node
/// its `hw.model` family; an Apple silicon `hw.model` alone (a code) no name at all.
/// NEGATIVE CONTROL: returning `hw_model` itself as the last rung (`_ => return
/// Some(hw_model.to_owned())`) names the bare Mac16,1 `Mac16,1`, and the last assertion fails.
#[test]
fn a_mac_is_named_by_its_product_name_then_its_family_and_never_its_code() {
    let air = mac_type(Some("Mac17,4"), Some("MacBook Air (15-inch, M5)"));
    assert_eq!(
        air,
        MachineType {
            name: Some("MacBook Air".into()),
            family: None,
            model: Some("MacBook Air (15-inch, M5)".into()),
            model_id: Some("Mac17,4".into()),
        }
    );
    let vm = mac_type(Some("VirtualMac2,1"), Some("Apple Virtual Machine 1"));
    assert_eq!(named(&vm), Some("Virtual Machine"));
    assert_eq!(vm.model.as_deref(), Some("Apple Virtual Machine 1"));
    let intel = mac_type(Some("MacBookPro14,3"), None);
    assert_eq!(named(&intel), Some("MacBook Pro"));
    assert_eq!(intel.model, None);
    assert_eq!(named(&mac_type(Some("Macmini8,1"), None)), Some("Mac mini"));
    let bare = mac_type(Some("Mac16,1"), None);
    assert_eq!(bare.model_id.as_deref(), Some("Mac16,1"));
    assert_eq!(named(&bare), None, "an Apple silicon hw.model is a code");
}

/// A4 §15 `the_machine_row_names_the_type`, Linux DMI fixtures: the ThinkPad reads the name in
/// its version, prefixed by its vendor in title case, with the code kept as the identifier; an
/// unconfigured whitebox board (`To Be Filled By O.E.M.` throughout) and an ASUS board's
/// placeholders read no name, so the OS noun stands.
/// NEGATIVE CONTROL: dropping `!is_code(t)` from [`friendly`] names the ThinkPad
/// `Lenovo 21K9CTO1WW`, and the first assertion fails.
#[test]
fn a_thinkpad_reads_its_version_and_a_whitebox_board_reads_no_name() {
    let thinkpad = dmi_type(&THINKPAD);
    assert_eq!(named(&thinkpad), Some("Lenovo ThinkPad X1 Carbon Gen 12"));
    assert_eq!(thinkpad.family, None, "the family is the name");
    assert_eq!(thinkpad.model_id.as_deref(), Some("21K9CTO1WW"));
    let oem = "To Be Filled By O.E.M.";
    let whitebox = dmi_type(&Dmi {
        vendor: Some(oem),
        name: Some(oem),
        version: Some(oem),
        family: Some(oem),
    });
    assert_eq!(whitebox, MachineType::default());
    let asus_board = dmi_type(&Dmi {
        vendor: Some("System manufacturer"),
        name: Some("System Product Name"),
        version: Some("System Version"),
        family: Some("To be filled by O.E.M."),
    });
    assert_eq!(asus_board, MachineType::default());
}

/// Other firmware tables seen in the field read as a person names the machine.
#[test]
fn dell_hp_framework_and_virtual_machines_read_as_named() {
    let dell = dmi_type(&Dmi {
        vendor: Some("Dell Inc."),
        name: Some("Latitude 7440"),
        version: Some(""),
        family: Some("Latitude"),
    });
    assert_eq!(named(&dell), Some("Dell Latitude 7440"));
    assert_eq!(dell.family.as_deref(), Some("Dell Latitude"));
    let hp = dmi_type(&Dmi {
        vendor: Some("HP"),
        name: Some("HP EliteBook 840 14 inch G10 Notebook PC"),
        version: Some("SBKPF"),
        family: Some("103C_5336AN HP EliteBook"),
    });
    assert_eq!(
        named(&hp),
        Some("HP EliteBook 840 14 inch G10 Notebook PC"),
        "the vendor is not said twice"
    );
    assert_eq!(
        hp.family.as_deref(),
        Some("HP EliteBook"),
        "a leading code goes"
    );
    let framework = dmi_type(&Dmi {
        vendor: Some("Framework"),
        name: Some("Laptop (13th Gen Intel Core)"),
        version: Some("A6"),
        family: Some("Laptop"),
    });
    assert_eq!(
        named(&framework),
        Some("Framework Laptop (13th Gen Intel Core)")
    );
    assert_eq!(framework.family.as_deref(), Some("Framework Laptop"));
    let qemu = dmi_type(&Dmi {
        vendor: Some("QEMU"),
        name: Some("Standard PC (Q35 + ICH9, 2009)"),
        version: Some("pc-q35-8.2"),
        family: None,
    });
    assert_eq!(named(&qemu), Some("QEMU Standard PC (Q35 + ICH9, 2009)"));
    let msi = dmi_type(&Dmi {
        vendor: Some("Micro-Star International Co., Ltd."),
        name: Some("MS-7D25"),
        version: Some("1.0"),
        family: Some("To be filled by O.E.M."),
    });
    assert_eq!(named(&msi), Some("Micro-Star MS-7D25"));
}

/// The vendor's word drops its corporate suffixes and is set in title case only where it is
/// shouted at length.
#[test]
fn a_vendor_reads_as_its_word() {
    for (field, word) in [
        ("LENOVO", Some("Lenovo")),
        ("Dell Inc.", Some("Dell")),
        ("Micro-Star International Co., Ltd.", Some("Micro-Star")),
        ("ASUSTeK COMPUTER INC.", Some("ASUSTeK")),
        ("VMware, Inc.", Some("VMware")),
        ("HP", Some("HP")),
        ("QEMU", Some("QEMU")),
        ("innotek GmbH", Some("innotek")),
        ("System manufacturer", None),
        ("To Be Filled By O.E.M.", None),
        ("", None),
    ] {
        assert_eq!(vendor_word(field).as_deref(), word, "{field:?}");
    }
}

/// A Raspberry Pi's device tree names it by its model (its NUL kept by the kernel's string),
/// its first `compatible` entry the identifier.
#[test]
fn a_device_tree_names_the_board_by_its_model() {
    let pi = device_tree_type(
        Some("Raspberry Pi 5 Model B Rev 1.0\0"),
        Some("raspberrypi,5-model-b\0brcm,bcm2712\0"),
    );
    assert_eq!(named(&pi), Some("Raspberry Pi 5 Model B Rev 1.0"));
    assert_eq!(pi.model_id.as_deref(), Some("raspberrypi,5-model-b"));
    assert_eq!(device_tree_type(None, None), MachineType::default());
}

/// A Linux tree is read through the device tree first and the DMI table after, from the files
/// the kernel writes (`/proc/device-tree`, `/sys/class/dmi/id`, each with its newline).
#[test]
fn a_linux_tree_reads_its_device_tree_then_its_dmi_table() {
    let root = aterm_tempfile::tempdir().expect("scratch root");
    let write = |rel: &str, text: &str| {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("dirs");
        std::fs::write(path, text).expect("a fixture file");
    };
    write("sys/class/dmi/id/sys_vendor", "LENOVO\n");
    write("sys/class/dmi/id/product_name", "21K9CTO1WW\n");
    write(
        "sys/class/dmi/id/product_version",
        "ThinkPad X1 Carbon Gen 12\n",
    );
    write(
        "sys/class/dmi/id/product_family",
        "ThinkPad X1 Carbon Gen 12\n",
    );
    let tree = crate::linux::Root::new(root.path());
    let t = linux_type(&tree);
    assert_eq!(named(&t), Some("Lenovo ThinkPad X1 Carbon Gen 12"));
    assert_eq!(t.model_id.as_deref(), Some("21K9CTO1WW"));
    write("proc/device-tree/model", "Raspberry Pi 5 Model B Rev 1.0\0");
    write(
        "proc/device-tree/compatible",
        "raspberrypi,5-model-b\0brcm,bcm2712\0",
    );
    assert_eq!(
        named(&linux_type(&tree)),
        Some("Raspberry Pi 5 Model B Rev 1.0")
    );
    let empty = aterm_tempfile::tempdir().expect("scratch root");
    assert_eq!(
        linux_type(&crate::linux::Root::new(empty.path())),
        MachineType::default()
    );
}

/// The law (A4 §9, N2): whatever the firmware writes in whichever field, no name and no family
/// the ladder yields is a code or a placeholder, or holds one as a leading word. Every code and
/// placeholder is put in every product field of a table whose other fields are the same kind of
/// junk or a real name, on Linux's and Windows' ladder and on the Mac's.
/// NEGATIVE CONTROL: dropping the leading-code strip from [`friendly`] passes
/// `103C_5336AN HP EliteBook` as a family, and the sweep fails on it.
#[test]
fn the_type_ladder_never_yields_a_code_or_placeholder() {
    let junk: Vec<&str> = CODES
        .iter()
        .copied()
        .chain(PLACEHOLDERS.iter().copied())
        .chain(["", "  ", "1.0", "0001", "103C_5336AN HP EliteBook"])
        .collect();
    let fills: Vec<Option<&str>> = junk
        .iter()
        .map(|j| Some(*j))
        .chain([None, Some("ThinkPad X1 Carbon Gen 12")])
        .collect();
    let clean = |s: &str| {
        let first = s.split_whitespace().next().unwrap_or("");
        let vendorless = s.split_once(' ').map_or(s, |(_, rest)| rest);
        !is_code(s)
            && !is_placeholder(s)
            && !is_code(first)
            && !is_code(vendorless.split_whitespace().next().unwrap_or(""))
            && PLACEHOLDERS
                .iter()
                .all(|p| !s.to_ascii_lowercase().contains(p) || p.len() <= 4)
            && s.bytes().any(|b| b.is_ascii_alphabetic())
    };
    let mut seen = 0usize;
    for vendor in [Some("LENOVO"), Some("To Be Filled By O.E.M."), None] {
        for name in &fills {
            for version in &fills {
                for family in [Some("103C_5336AN HP EliteBook"), Some("O.E.M."), None] {
                    let t = dmi_type(&Dmi {
                        vendor,
                        name: *name,
                        version: *version,
                        family,
                    });
                    for out in [&t.name, &t.family].into_iter().flatten() {
                        seen += 1;
                        assert!(clean(out), "{out:?} from {name:?} {version:?} {family:?}");
                    }
                }
            }
        }
    }
    assert!(seen > 0, "the sweep named something");
    for j in &junk {
        for model in ["Mac16,1", "VirtualMac2,1", "MacBookAir10,1", ""] {
            if let Some(out) = mac_type(Some(model), Some(j)).name {
                assert!(clean(&out), "{out:?} from {model:?} {j:?}");
            }
        }
        if let Some(out) = device_tree_type(Some(j), None).name {
            assert!(clean(&out), "{out:?} from the device tree's {j:?}");
        }
    }
    for j in CODES.iter().chain(PLACEHOLDERS) {
        let tree = device_tree_type(Some(j), None);
        assert!(tree.name.is_none(), "{j:?} names a board");
    }
}

/// This Mac's own type reads as a name, its long form and its identifier, in process.
#[cfg(target_os = "macos")]
#[test]
fn this_mac_reads_a_name_in_process() {
    let t = read().expect("macOS has a reader");
    let id = t.model_id.as_deref().expect("hw.model");
    assert!(!id.is_empty());
    if let Some(name) = t.name.as_deref() {
        assert_eq!(friendly(name), Some(name), "{name:?}");
    }
    if cfg!(target_arch = "aarch64") {
        assert!(
            t.model.is_some(),
            "an Apple silicon Mac has a product name: {t:?}"
        );
    }
}
