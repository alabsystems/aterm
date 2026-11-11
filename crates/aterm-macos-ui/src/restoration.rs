// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Keep AppKit restoration separate from the application's journal and live terminal custody.

use std::sync::OnceLock;

use aterm_objc::send::{send_bool_id, send_id, send_id_id, send_v_id_id};
use aterm_objc::{Bool, Id, MainThread, Obj, Sel, autoreleasepool, ns_string, sel};

use crate::framework::{Framework, Variable};

static FOUNDATION: Framework =
    Framework::new(&[c"/System/Library/Frameworks/Foundation.framework/Foundation"]);

fn argument_domain() -> Id {
    static DOMAIN: OnceLock<Variable<Id>> = OnceLock::new();
    // SAFETY: Foundation exports NSArgumentDomain as an immutable NSString pointer.
    let domain = DOMAIN.get_or_init(|| unsafe { FOUNDATION.variable(c"NSArgumentDomain") });
    // SAFETY: the exported constant remains initialized and immutable for the process lifetime.
    unsafe { domain.read() }
}

/// Disable AppKit's saved-window restoration before creating NSApplication.
///
/// The application restores its own journal and terminal handles. Native restoration can put
/// a crash-history modal ahead of applicationDidFinishLaunching and prevent that recovery.
/// These values live only in this process's argument domain; existing preferences and saved
/// state are left on disk. Administrative forced preferences remain authoritative.
///
/// # Errors
/// Returns an error when Foundation cannot install or observe the process-local policy.
pub fn use_application_owned_restoration(_main: MainThread) -> Result<(), &'static str> {
    autoreleasepool(|_| {
        aterm_objc::exception::contain("application-owned restoration policy", || {
            // SAFETY: standardUserDefaults is a process-lifetime Foundation singleton.
            let defaults = unsafe {
                send_id(
                    FOUNDATION.class(c"NSUserDefaults").as_id(),
                    sel!(standardUserDefaults),
                )
            };
            apply_to(defaults)
        })
        .map_err(|_| "Foundation raised while setting the native restoration policy")?
    })
}

fn apply_to(defaults: Id) -> Result<(), &'static str> {
    if defaults.is_null() {
        return Err("NSUserDefaults is unavailable for the native restoration policy");
    }
    let domain = argument_domain();
    let keep = ns_string("NSQuitAlwaysKeepsWindows").ok_or("could not make restoration key")?;
    let ignore =
        ns_string("ApplePersistenceIgnoreState").ok_or("could not make restoration key")?;
    // SAFETY: defaults is live; volatileDomainForName: takes the Foundation domain name.
    let prior = unsafe { send_id_id(defaults, sel!(volatileDomainForName:), domain) };
    // SAFETY: mutableCopy returns an owned mutable dictionary; +new constructs one when absent.
    let values = unsafe {
        Obj::from_owned(if prior.is_null() {
            send_id(FOUNDATION.class(c"NSMutableDictionary").as_id(), sel!(new))
        } else {
            send_id(prior, sel!(mutableCopy))
        })
    }
    .ok_or("could not copy the native argument domain")?;
    // SAFETY: NSNumber's factory takes BOOL and returns an autoreleased NSNumber. Dictionary
    // setters retain their arguments, and setVolatileDomain:forName: copies this dictionary.
    unsafe {
        let number: unsafe extern "C-unwind" fn(Id, Sel, Bool) -> Id = aterm_objc::checked_msg();
        let class = FOUNDATION.class(c"NSNumber").as_id();
        let no = number(class, sel!(numberWithBool:), Bool::NO);
        let yes = number(class, sel!(numberWithBool:), Bool::YES);
        if no.is_null() || yes.is_null() {
            return Err("could not make the native restoration values");
        }
        send_v_id_id(values.id(), sel!(setObject:forKey:), no, keep.id());
        send_v_id_id(values.id(), sel!(setObject:forKey:), yes, ignore.id());
        send_v_id_id(
            defaults,
            sel!(setVolatileDomain:forName:),
            values.id(),
            domain,
        );
        if send_bool_id(defaults, sel!(boolForKey:), keep.id())
            || !send_bool_id(defaults, sel!(boolForKey:), ignore.id())
        {
            return Err("native restoration policy is overridden by forced preferences");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "restoration_tests.rs"]
mod tests;
