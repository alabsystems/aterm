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
//!
//! Which power assertions a process holds ([`assertions_of`]), as the power manager lists them.

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

/// One power assertion a process holds, as macOS's power manager lists it (the rows
/// `pmset -g assertions` prints under "Listed by owning process").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assertion {
    /// Its type: `PreventUserIdleSystemSleep`, `PreventUserIdleDisplaySleep`, ….
    pub kind: String,
    /// The name its holder gave it; `None` when it has none, or one longer than 127 bytes.
    pub name: Option<String>,
}

/// The power assertions `pid` holds now, read in-process from the power manager, with no
/// window server and no child process.
///
/// # Errors
///
/// Off macOS, which has no power assertions to list; on macOS, the power manager did not
/// answer.
pub fn assertions_of(pid: u32) -> Result<Vec<Assertion>, String> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::assertions_of(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Err("power assertions are macOS's".to_owned())
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

    /// An assertion this test holds is listed under its pid, with its type and name, until it is
    /// released; the pid after this one lists none of it.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_assertion_held_is_listed_under_its_pid_until_released() {
        use crate::iokit;
        use crate::macos::Cf;
        let name = format!("aterm-sysprobe assertion test {}", std::process::id());
        let name_c = std::ffi::CString::new(name.clone()).expect("no NUL");
        let kind = Cf::string(c"PreventUserIdleSystemSleep").expect("type string");
        let named = Cf::string(&name_c).expect("name string");
        let mut id = 0u32;
        // SAFETY: Two live CFStrings and a writable id; level 255 is kIOPMAssertionLevelOn.
        let rc = unsafe {
            iokit::IOPMAssertionCreateWithName(kind.0.cast(), 255, named.0.cast(), &raw mut id)
        };
        assert_eq!(rc, 0, "IOPMAssertionCreateWithName");
        let ours = |list: Vec<Assertion>| -> Vec<String> {
            list.into_iter()
                .filter(|a| a.name.as_deref() == Some(name.as_str()))
                .map(|a| a.kind)
                .collect()
        };
        let while_held = ours(assertions_of(std::process::id()).expect("listed"));
        let elsewhere = ours(assertions_of(std::process::id() + 1).expect("listed"));
        // SAFETY: The id the create above wrote, released once.
        assert_eq!(unsafe { iokit::IOPMAssertionRelease(id) }, 0, "released");
        let after = ours(assertions_of(std::process::id()).expect("listed"));
        assert_eq!(while_held, ["PreventUserIdleSystemSleep"]);
        assert!(elsewhere.is_empty(), "{elsewhere:?}");
        assert!(after.is_empty(), "{after:?}");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn off_macos_no_assertion_is_listed() {
        assert!(assertions_of(1).is_err());
    }
}
