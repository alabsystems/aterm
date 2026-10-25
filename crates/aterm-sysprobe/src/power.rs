// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which wake the machine is in: a full one, with graphics, or macOS's dark wake.
//!
//! macOS wakes a sleeping Mac with its lid closed for seconds at a time (DarkWake, Power Nap)
//! without lighting the display; its network may still be coming up, and a program that acts
//! there acts unseen. The kernel's power manager says which wake this is by its capability bits,
//! which its root domain publishes in the registry as `System Capabilities` (`IOPMrootDomain`;
//! the bits are `IOPMSystemCapabilities`'s, the ones `IOPMIsADarkWake` and `IOPMIsAUserWake` read).
//! The calls that return them directly are not in the public SDK, so the registry is read.

/// The machine's wake, as its power manager says it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Awake with graphics: the lid opened, a key, or a wake that lights the display.
    Full,
    /// Awake without graphics: a dark wake, the display off and the lid most likely closed.
    Dark,
}

/// `kIOPMSystemCapabilityCPU`.
const CAPABILITY_CPU: u32 = 0x1;
/// `kIOPMSystemCapabilityGraphics`.
const CAPABILITY_GRAPHICS: u32 = 0x2;

/// The wake `caps` (the root domain's `System Capabilities`) names: graphics up is
/// [`Wake::Full`], the CPU without graphics [`Wake::Dark`], and bits without the CPU neither.
#[must_use]
pub const fn wake_of(caps: u32) -> Option<Wake> {
    if caps & CAPABILITY_CPU == 0 {
        None
    } else if caps & CAPABILITY_GRAPHICS == 0 {
        Some(Wake::Dark)
    } else {
        Some(Wake::Full)
    }
}

/// The wake the machine is in now; `None` off macOS, which has no dark wake to tell apart.
#[must_use]
pub fn wake() -> Option<Wake> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::system_capabilities().and_then(wake_of)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capability_bits_name_the_wake() {
        assert_eq!(wake_of(0x1f), Some(Wake::Full), "a full wake: every bit");
        assert_eq!(
            wake_of(0x0f),
            Some(Wake::Full),
            "measured awake: CPU, graphics, audio, network"
        );
        assert_eq!(
            wake_of(0x03),
            Some(Wake::Full),
            "the CPU and graphics alone"
        );
        assert_eq!(
            wake_of(0x09),
            Some(Wake::Dark),
            "CPU and network: a dark wake"
        );
        assert_eq!(wake_of(0x01), Some(Wake::Dark), "the CPU alone");
        assert_eq!(wake_of(0x00), None, "asleep, or nothing read");
        assert_eq!(wake_of(0x1e), None, "no CPU named");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_running_mac_reads_a_wake() {
        let caps = crate::macos::system_capabilities().expect("the root domain's capabilities");
        assert_ne!(
            caps & CAPABILITY_CPU,
            0,
            "a process that runs has the CPU: {caps:#x}"
        );
        assert!(wake().is_some(), "{caps:#x}");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_systems_read_none() {
        assert_eq!(wake(), None);
    }
}
