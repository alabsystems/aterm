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
//! Which power assertions a process holds ([`assertions_of`]), as the power manager lists them;
//! one of this process's own to hold ([`hold`]); and whether the lid is closed ([`lid_closed`]).
//!
//! On Windows [`hold`] holds a power request (`PowerCreateRequest`), and [`windows`] has that
//! system's other power readers: the power source and battery, the boot time, the clock that
//! stops in a suspend, and the suspend and resume announcements.

/// Windows' power readers and its keep-awake request; what they decode compiles on every host.
#[path = "win_power.rs"]
pub mod windows;

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

/// Whether this Mac's lid is closed now, as the power manager's root domain publishes it
/// (`AppleClamshellState`): `None` on a Mac with no lid, where the registry has no answer, and
/// off macOS. A read of the registry: no window server, no child process.
#[must_use]
pub fn lid_closed() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::clamshell_closed()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// The power assertion type that keeps the system from sleeping at all, the lid's close
/// included, while the Mac runs on AC power (`kIOPMAssertionTypePreventSystemSleep`; on battery
/// macOS does not honour it).
pub const PREVENT_SYSTEM_SLEEP: &str = "PreventSystemSleep";

/// A power assertion this process holds ([`hold`]); dropping it releases it. On Windows it is a
/// power request ([`windows::Held`]), which its own drop clears and closes.
#[derive(Debug)]
pub struct Hold {
    /// The power manager's id for it.
    #[cfg(target_os = "macos")]
    id: u32,
    /// The power request and the kinds set on it.
    #[cfg(windows)]
    _request: windows::Held,
}

impl Drop for Hold {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        crate::macos::release_assertion(self.id);
    }
}

/// Hold a power assertion of type `kind` ([`PREVENT_SYSTEM_SLEEP`], say) named `name`, which
/// `pmset -g assertions` and [`assertions_of`] list it by, until the [`Hold`] is dropped; `None`
/// when the power manager refused it, and on Linux. On Windows `kind` is one of macOS's
/// assertion types, held as the power requests [`windows::requests_of`] maps it to (the system
/// kept from its idle sleep and this process running, and for the display's type, or on a PC
/// with Modern Standby for every type, the display kept on), under `name`,
/// which `powercfg /requests` lists it by; a closing lid or a person's own Sleep still sleeps
/// the machine there, whatever the type.
#[must_use]
pub fn hold(kind: &str, name: &str) -> Option<Hold> {
    #[cfg(target_os = "macos")]
    {
        crate::macos::create_assertion(kind, name).map(|id| Hold { id })
    }
    #[cfg(windows)]
    {
        windows::hold(kind, name).map(|request| Hold { _request: request })
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (kind, name);
        None
    }
}

#[cfg(test)]
#[path = "power_tests.rs"]
mod tests;
