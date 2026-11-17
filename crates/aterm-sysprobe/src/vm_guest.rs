// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether this machine runs as a virtual machine's guest ([`vm_guest`]): an identity aterm's vault
//! makes there is a lab identity (`aterm_secrets::LabMark::VirtualMachine`,
//! `docs/DESIGN-alab-one-mesh-2026-11-05.md` A12).
//!
//! macOS asks its kernel (`kern.hv_vmm_present`, [`crate::hv_vmm_present`]). Windows reads two
//! things, and either one that names a guest makes the machine one:
//! - what the firmware says the machine is: the BIOS vendor and the system's manufacturer and
//!   product name from its SMBIOS tables, as the registry keeps them
//!   (`HKLM\HARDWARE\DESCRIPTION\System\BIOS`, the copy Windows writes at boot), judged by
//!   [`names_a_guest`]: a desktop hypervisor's virtual firmware, or a cloud's (Amazon EC2, Google
//!   Compute Engine, DigitalOcean, Hetzner, Alibaba Cloud, OpenStack and others);
//! - on x86-64, which hypervisor the processor says it runs under: the signatures CPUID names at
//!   `0x4000_0000` and each `0x100` above it, read only where leaf 1 sets the hypervisor bit, as
//!   Linux reads them, judged by [`names_a_guest_hypervisor`]: KVM (every KVM cloud, and a QEMU
//!   guest whose firmware copies the host's), VMware, Xen, VirtualBox, QEMU's TCG, bhyve,
//!   Parallels, ACRN. Never the hypervisor bit alone, and never `Microsoft Hv`: Windows 11 runs
//!   itself under Hyper-V whenever virtualization-based security runs, so its own desktop reads the
//!   bit set and `Microsoft Hv` (measured 2026-11-16), and either would make every such PC a lab
//!   identity that never joins. A Hyper-V guest is told by its firmware instead.
//!
//! What the two cannot tell is a guest that hides from both: a hypervisor that names itself
//! nowhere (KVM's `kvm=off` with the hypervisor bit cleared) under firmware copied from a real
//! machine. Linux: no reading yet.

/// What the firmware names the machine: SMBIOS type 0's vendor and type 1's manufacturer and
/// product name, each `None` where unread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Firmware {
    /// The BIOS vendor (`American Megatrends`, `SeaBIOS`, `innotek GmbH`).
    pub bios_vendor: Option<String>,
    /// The system's manufacturer (`Dell Inc.`, `QEMU`, `VMware, Inc.`).
    pub manufacturer: Option<String>,
    /// The system's product name (`OptiPlex 7090`, `VirtualBox`, `Virtual Machine`).
    pub product: Option<String>,
}

/// The words a hypervisor's or a cloud's virtual firmware names itself by in one of
/// [`Firmware`]'s fields, compared whole and without case: QEMU (UTM, Proxmox and libvirt's KVM
/// guests among them), VMware, VirtualBox (and its maker, innotek), Parallels, Bochs, Xen, KVM,
/// bhyve, QEMU's own machine types (`q35`, `i440FX`), and the clouds that name themselves:
/// DigitalOcean (its `Droplet`), Hetzner, Vultr, OpenStack (`OpenStack Nova`), oVirt and RHEV,
/// Nutanix.
const GUEST_WORDS: &[&str] = &[
    "qemu",
    "vmware",
    "virtualbox",
    "innotek",
    "parallels",
    "bochs",
    "xen",
    "kvm",
    "bhyve",
    "q35",
    "i440fx",
    "digitalocean",
    "droplet",
    "hetzner",
    "vultr",
    "openstack",
    "ovirt",
    "rhev",
    "nutanix",
];

/// The runs of words a cloud's virtual firmware names itself by, where one word alone would also
/// name real hardware (a Chromebook's coreboot firmware is `Google`'s): each matched as
/// consecutive whole words of one field, without case.
const GUEST_PHRASES: &[&[&str]] = &[
    &["amazon", "ec2"],
    &["google", "compute", "engine"],
    &["alibaba", "cloud"],
    &["tencent", "cloud"],
    &["huawei", "cloud"],
];

/// Whether `firmware` is a hypervisor's virtual hardware: a field holds one of [`GUEST_WORDS`] as
/// a whole word or one of [`GUEST_PHRASES`] as consecutive whole words, or the system is
/// Hyper-V's (`Microsoft Corporation`'s `Virtual Machine`, Azure's among them; a Surface, also
/// Microsoft's, is no guest). A machine whose firmware names its maker and product (`HP`,
/// `Dell Inc.`, `LENOVO`) is no guest.
#[must_use]
pub fn names_a_guest(firmware: &Firmware) -> bool {
    let fields = [
        firmware.bios_vendor.as_deref(),
        firmware.manufacturer.as_deref(),
        firmware.product.as_deref(),
    ];
    let named = fields.into_iter().flatten().any(|field| {
        let words: Vec<&str> = field
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect();
        words
            .iter()
            .any(|word| GUEST_WORDS.iter().any(|g| word.eq_ignore_ascii_case(g)))
            || GUEST_PHRASES.iter().any(|phrase| {
                words.windows(phrase.len()).any(|run| {
                    run.iter()
                        .zip(phrase.iter())
                        .all(|(word, g)| word.eq_ignore_ascii_case(g))
                })
            })
    });
    let is = |field: Option<&str>, value: &str| {
        field.is_some_and(|f| f.trim().eq_ignore_ascii_case(value))
    };
    named
        || (is(firmware.manufacturer.as_deref(), "Microsoft Corporation")
            && is(firmware.product.as_deref(), "Virtual Machine"))
}

/// The hypervisor signatures a guest's processor names (CPUID leaf `0x4000_0000`'s, or a later
/// base's, `EBX`, `ECX` and `EDX` in that order): KVM's, VMware's, Xen's, VirtualBox's, QEMU's
/// TCG's, bhyve's, Parallels', ACRN's, and KVM's when it offers Hyper-V's interface first
/// (`Linux KVM Hv`). Never `Microsoft Hv`, which Windows reads of itself on bare metal under
/// virtualization-based security.
const GUEST_HYPERVISORS: &[&[u8; 12]] = &[
    b"KVMKVMKVM\0\0\0",
    b"Linux KVM Hv",
    b"VMwareVMware",
    b"XenVMMXenVMM",
    b"VBoxVBoxVBox",
    b"TCGTCGTCGTCG",
    b"bhyve bhyve ",
    b" lrpepyh  vr",
    b"ACRNACRNACRN",
];

/// Whether `signature`, as CPUID names a hypervisor, is one of [`GUEST_HYPERVISORS`]: a
/// hypervisor that runs this machine as its guest.
#[must_use]
pub fn names_a_guest_hypervisor(signature: &[u8; 12]) -> bool {
    GUEST_HYPERVISORS.contains(&signature)
}

/// [`vm_guest`] from the firmware `read` gives and the hypervisor signatures `hypervisors` gives:
/// `Some(true)` where a signature names a guest ([`names_a_guest_hypervisor`]); else the
/// firmware's verdict ([`names_a_guest`]), `None` where it gives none (no field read), so an
/// unread firmware never makes a machine a guest.
#[must_use]
pub fn vm_guest_of(
    read: impl FnOnce() -> Option<Firmware>,
    hypervisors: impl FnOnce() -> Vec<[u8; 12]>,
) -> Option<bool> {
    if hypervisors().iter().any(names_a_guest_hypervisor) {
        return Some(true);
    }
    read()
        .filter(|f| f.bios_vendor.is_some() || f.manufacturer.is_some() || f.product.is_some())
        .map(|firmware| names_a_guest(&firmware))
}

/// The hypervisor signatures this processor names: none where CPUID leaf 1 clears the hypervisor
/// bit; else each base `0x4000_0000 + 0x100 n` below `0x4001_0000`'s signature (`EBX`, `ECX`,
/// `EDX`), as Linux's `hypervisor_cpuid_base` walks them (KVM moves its own to `0x4000_0100` when
/// it offers Hyper-V's interface at the first).
#[cfg(target_arch = "x86_64")]
#[must_use]
pub fn hypervisor_signatures() -> Vec<[u8; 12]> {
    use std::arch::x86_64::__cpuid;
    const HYPERVISOR_BIT: u32 = 1 << 31;
    if __cpuid(1).ecx & HYPERVISOR_BIT == 0 {
        return Vec::new();
    }
    (0x4000_0000_u32..0x4001_0000)
        .step_by(0x100)
        .map(|base| {
            let leaf = __cpuid(base);
            let mut signature = [0; 12];
            signature[..4].copy_from_slice(&leaf.ebx.to_le_bytes());
            signature[4..8].copy_from_slice(&leaf.ecx.to_le_bytes());
            signature[8..].copy_from_slice(&leaf.edx.to_le_bytes());
            signature
        })
        .collect()
}

/// Whether this machine runs as a virtual machine's guest: macOS's `kern.hv_vmm_present`; on
/// Windows the hypervisor's CPUID signature (x86-64) and the firmware's names
/// ([`vm_guest_of`]); `None` where unread, and on Linux.
#[must_use]
pub fn vm_guest() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::hv_vmm_present()
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        vm_guest_of(crate::win::firmware, hypervisor_signatures)
    }
    #[cfg(all(windows, target_pointer_width = "64", not(target_arch = "x86_64")))]
    {
        vm_guest_of(crate::win::firmware, Vec::new)
    }
    #[cfg(not(any(target_os = "macos", all(windows, target_pointer_width = "64"))))]
    {
        None
    }
}

#[cfg(test)]
#[path = "vm_guest_tests.rs"]
mod tests;
