// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Bonjour browse of this process's, made by the root LAN broker (`aterm_uds::lan::browse`,
//! `docs/DESIGN-lan-broker-2026-11-05.md`): mDNSResponder applies the policy of the process that
//! calls it, and macOS's Local Network privacy exempts root alone, so the broker makes the
//! `DNSServiceBrowse` call and streams what it hears here. The blocking reader and its callbacks
//! stay on the calling thread. A callback can end the browse; every exit closes its connection to
//! the broker, which ends the browse there.

use std::ffi::{CStr, CString};
use std::io;
use std::ops::ControlFlow;
use std::time::Duration;

use aterm_uds::lan::{Event as Heard, Instance, LanError};

/// One service instance added or removed on an interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    pub added: bool,
    pub interface: u32,
    /// The unescaped instance name, borrowed only for the callback.
    pub name: &'a CStr,
}

/// How long one wait for the broker's next event lasts in [`browse`], which has no `going` to
/// ask between waits: a wait that hears nothing simply waits again.
const QUIET: Duration = Duration::from_secs(60);

/// Browse `service` in `domain` until the callback returns `Break`, or an error.
/// This blocks while there is no reply; call it on a background reader thread.
///
/// # Errors
/// The debug loopback guard refuses network browsing, a name that is not UTF-8, the LAN broker
/// is not there to ask (`NotConnected`, its words naming what installs it), off macOS
/// (`Unsupported`), or the broker reports the browse's error or ends it.
pub fn browse(
    service: &CStr,
    domain: &CStr,
    heard: impl FnMut(Event<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    browse_until(service, domain, None, heard)
}

/// [`browse`] while `going` says so: with no reply for `look`, and before each reply it reads,
/// it asks `going`, and returns `Ok(())` once that says no (a person turned sharing off), so a
/// browse that hears nothing still ends within `look` of it.
///
/// # Errors
/// As [`browse`].
pub fn browse_while(
    service: &CStr,
    domain: &CStr,
    look: Duration,
    mut going: impl FnMut() -> bool,
    heard: impl FnMut(Event<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    browse_until(service, domain, Some((look, &mut going)), heard)
}

/// The browse: events read until a callback ends it, an error, or `until`'s `going` says no
/// (asked before each read, and after each `look` with no event).
fn browse_until(
    service: &CStr,
    domain: &CStr,
    mut until: Option<(Duration, &mut dyn FnMut() -> bool)>,
    mut heard: impl FnMut(Event<'_>) -> ControlFlow<()>,
) -> io::Result<()> {
    let service = text(service, "service")?;
    let domain = text(domain, "domain")?;
    crate::loopback::check(crate::loopback::Reach::Browse(service))?;
    let mut events = aterm_uds::lan::browse(service, domain).map_err(LanError::into_io)?;
    loop {
        let look = match &mut until {
            Some((look, going)) => {
                if !going() {
                    return Ok(());
                }
                *look
            }
            None => QUIET,
        };
        let (
            added,
            Instance {
                name, interface, ..
            },
        ) = match events.next_within(look).map_err(LanError::into_io)? {
            None => continue,
            Some(Heard::Added(instance)) => (true, instance),
            Some(Heard::Removed(instance)) => (false, instance),
            // A browse hears instances alone; anything else is no instance of the service.
            Some(_) => continue,
        };
        let Ok(name) = CString::new(name) else {
            // DNS-SD names are C strings: one with a NUL is no name its daemon could give.
            continue;
        };
        if heard(Event {
            added,
            interface,
            name: &name,
        })
        .is_break()
        {
            return Ok(());
        }
    }
}

/// `value` (the browse's `what`) as UTF-8, which the broker's wire carries.
fn text<'a>(value: &'a CStr, what: &str) -> io::Result<&'a str> {
    value.to_str().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the Bonjour {what} is not UTF-8"),
        )
    })
}

#[cfg(test)]
mod tests;
