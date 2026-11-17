// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Firmware, names_a_guest, names_a_guest_hypervisor, vm_guest_of};

fn firmware(bios_vendor: &str, manufacturer: &str, product: &str) -> Firmware {
    let field = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    Firmware {
        bios_vendor: field(bios_vendor),
        manufacturer: field(manufacturer),
        product: field(product),
    }
}

/// Each hypervisor's virtual firmware, as its guests read it, is a guest: QEMU (UTM, Proxmox),
/// libvirt's KVM, VMware, VirtualBox, Hyper-V, Parallels, Xen, Bochs.
/// NEGATIVE CONTROL (2026-11-16): dropping the Hyper-V clause: Hyper-V's firmware (Microsoft
/// Corporation's `Virtual Machine`) reads as no guest.
#[test]
fn every_hypervisors_virtual_firmware_names_a_guest() {
    for (bios, maker, product) in [
        (
            "EFI Development Kit II / OVMF",
            "QEMU",
            "Standard PC (Q35 + ICH9, 2009)",
        ),
        ("SeaBIOS", "QEMU", "Standard PC (i440FX + PIIX, 1996)"),
        ("SeaBIOS", "Red Hat", "KVM"),
        ("VMware, Inc.", "VMware, Inc.", "VMware7,1"),
        ("innotek GmbH", "innotek GmbH", "VirtualBox"),
        (
            "Microsoft Corporation",
            "Microsoft Corporation",
            "Virtual Machine",
        ),
        ("", "Microsoft Corporation", "Virtual Machine"),
        (
            "Parallels Software International Inc.",
            "Parallels International GmbH.",
            "Parallels ARM Virtual Machine",
        ),
        ("Xen", "Xen", "HVM domU"),
        ("Bochs", "Bochs", "Bochs"),
    ] {
        let f = firmware(bios, maker, product);
        assert!(names_a_guest(&f), "{f:?}");
        assert_eq!(
            vm_guest_of(|| Some(f.clone()), Vec::new),
            Some(true),
            "{f:?}"
        );
    }
}

/// A machine's own firmware, naming its maker and product, is no guest; a Surface (Microsoft's,
/// like Hyper-V's virtual firmware) included, and a word that only holds a guest word inside it.
/// NEGATIVE CONTROL (2026-11-16): the Hyper-V clause taking any product of Microsoft
/// Corporation's: a Surface Laptop reads as a guest.
#[test]
fn a_makers_own_firmware_names_no_guest() {
    for (bios, maker, product) in [
        ("AMI", "HP", "HP EliteDesk 800 G6"),
        ("Dell Inc.", "Dell Inc.", "OptiPlex 7090"),
        ("LENOVO", "LENOVO", "21K9CTO1WW"),
        (
            "American Megatrends International, LLC.",
            "ASUS",
            "System Product Name",
        ),
        (
            "Microsoft Corporation",
            "Microsoft Corporation",
            "Surface Laptop 5",
        ),
        ("Apple Inc.", "Apple Inc.", "MacBookPro16,1"),
        ("INSYDE Corp.", "Xenon Systems", "Qemunator Workstation"),
    ] {
        let f = firmware(bios, maker, product);
        assert!(!names_a_guest(&f), "{f:?}");
        assert_eq!(
            vm_guest_of(|| Some(f.clone()), Vec::new),
            Some(false),
            "{f:?}"
        );
    }
}

/// A firmware that reads nothing answers nothing, so a failed read never makes a guest.
/// NEGATIVE CONTROL (2026-11-16): `vm_guest_of` judging a firmware that names nothing: it answers
/// `Some(false)`.
#[test]
fn an_unread_firmware_answers_nothing() {
    assert_eq!(vm_guest_of(|| None, Vec::new), None);
    assert_eq!(vm_guest_of(|| Some(Firmware::default()), Vec::new), None);
}

/// Each cloud's virtual firmware, as a Windows guest there reads it, is a guest: Amazon EC2's
/// (Nitro), Google Compute Engine's, DigitalOcean's, Hetzner's, Alibaba Cloud's, OpenStack's,
/// Vultr's, Tencent Cloud's, Nutanix's, oVirt's, bhyve's. A Chromebook's own firmware, which names
/// Google alone, is no guest.
/// NEGATIVE CONTROL (2026-11-16): `names_a_guest` with no phrase matched (`GUEST_PHRASES` empty):
/// Amazon EC2's firmware reads as no guest.
#[test]
fn every_clouds_virtual_firmware_names_a_guest() {
    for (bios, maker, product) in [
        ("Amazon EC2", "Amazon EC2", "m5.large"),
        ("Google", "Google", "Google Compute Engine"),
        ("DigitalOcean", "DigitalOcean", "Droplet"),
        ("Hetzner", "Hetzner", "vServer"),
        ("SeaBIOS", "Alibaba Cloud", "Alibaba Cloud ECS"),
        ("SeaBIOS", "OpenStack Foundation", "OpenStack Nova"),
        ("SeaBIOS", "Vultr", "VC2"),
        ("SeaBIOS", "Tencent Cloud", "CVM"),
        ("SeaBIOS", "Nutanix", "AHV"),
        ("SeaBIOS", "oVirt", "oVirt Node"),
        ("BHYVE", "FreeBSD", "BHYVE"),
    ] {
        let f = firmware(bios, maker, product);
        assert!(names_a_guest(&f), "{f:?}");
        assert_eq!(
            vm_guest_of(|| Some(f.clone()), Vec::new),
            Some(true),
            "{f:?}"
        );
    }
    for (bios, maker, product) in [
        ("coreboot", "Google", "Eve"),
        ("Google", "Google", "Pixelbook Go"),
        ("Amazon", "Amazon", "Engine Compute"),
    ] {
        let f = firmware(bios, maker, product);
        assert!(!names_a_guest(&f), "{f:?}");
    }
}

/// A guest hypervisor's CPUID signature makes a guest whatever the firmware says: KVM's (every
/// KVM cloud), at its own base or past Hyper-V's interface, VMware's, Xen's, VirtualBox's, QEMU's
/// TCG's, bhyve's, Parallels', ACRN's, under a firmware copied from a real machine (QEMU's host
/// passthrough, VMware's `SMBIOS.reflectHost`) or one never read. `Microsoft Hv`, which Windows
/// reads of itself on bare metal under virtualization-based security, and an empty or unknown
/// signature make none, and leave the firmware's verdict standing.
/// NEGATIVE CONTROL (2026-11-16): `vm_guest_of` not asking `hypervisors`: KVM under Dell's copied
/// firmware reads `Some(false)`.
#[test]
fn a_guest_hypervisors_signature_names_a_guest_whatever_the_firmware_says() {
    let copied = firmware("Dell Inc.", "Dell Inc.", "OptiPlex 7090");
    let unread = Firmware::default();
    for signature in [
        b"KVMKVMKVM\0\0\0",
        b"Linux KVM Hv",
        b"VMwareVMware",
        b"XenVMMXenVMM",
        b"VBoxVBoxVBox",
        b"TCGTCGTCGTCG",
        b"bhyve bhyve ",
        b" lrpepyh  vr",
        b"ACRNACRNACRN",
    ] {
        assert!(names_a_guest_hypervisor(signature), "{signature:?}");
        for f in [&copied, &unread] {
            assert_eq!(
                vm_guest_of(|| Some(f.clone()), || vec![*signature]),
                Some(true),
                "{signature:?} {f:?}"
            );
        }
        let behind_hyper_v = || vec![*b"Microsoft Hv", [0; 12], *signature];
        assert_eq!(
            vm_guest_of(|| Some(copied.clone()), behind_hyper_v),
            Some(true)
        );
    }
    for signature in [b"Microsoft Hv", &[0; 12], b"KVMKVMKVMKVM", b"VMwareVMwar\0"] {
        assert!(!names_a_guest_hypervisor(signature), "{signature:?}");
        assert_eq!(
            vm_guest_of(|| Some(copied.clone()), || vec![*signature; 2]),
            Some(false),
            "{signature:?}"
        );
        assert_eq!(vm_guest_of(|| None, || vec![*signature]), None);
    }
}

/// Windows reads its firmware's names and its hypervisor's signatures, whatever they say: this
/// host's answer is printed, not asserted, so the test holds on a guest and on bare metal alike,
/// and it is the two read here.
#[cfg(all(windows, target_pointer_width = "64"))]
#[test]
fn windows_reads_the_firmware_it_judges() {
    let firmware = crate::win::firmware().expect("the firmware's names");
    assert!(
        firmware.manufacturer.is_some()
            || firmware.product.is_some()
            || firmware.bios_vendor.is_some(),
        "no field read: {firmware:?}"
    );
    #[cfg(target_arch = "x86_64")]
    let signatures = super::hypervisor_signatures();
    #[cfg(not(target_arch = "x86_64"))]
    let signatures: Vec<[u8; 12]> = Vec::new();
    for signature in signatures.iter().filter(|s| **s != [0; 12]) {
        println!(
            "hypervisor signature {:?}: a guest's {}",
            String::from_utf8_lossy(signature),
            names_a_guest_hypervisor(signature)
        );
    }
    println!("this host is a guest: {:?}", super::vm_guest());
    assert_eq!(
        super::vm_guest(),
        Some(names_a_guest(&firmware) || signatures.iter().any(names_a_guest_hypervisor))
    );
}
