// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS readers behind [`crate::situation`]: the notify check tokens, the power sources
//! (public `IOKit/ps/IOPowerSources.h`), the root domain's lid and last sleep reason, and the
//! watch thread whose run loop holds the root domain's general interest and the display pipes'
//! matching notifications.

use std::ffi::{CStr, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, PoisonError};

use crate::iokit::{
    self, CFRunLoopRef, CFRunLoopSourceContext, CFRunLoopSourceRef, CFTypeRef,
    IONotificationPortRef, MAIN_PORT_DEFAULT, io_iterator_t, io_object_t, io_service_t,
};
use crate::macos::{Cf, cf_text, cf_u64};
use crate::situation::{
    BatteryReading, Held, NOTIFY_CHARGE, NOTIFY_POWER_SOURCE, Posted, PowerReading, PowerSource,
    Shared, unix_ms_now,
};

/// `kIOPMMessageClamshellStateChange` (`IOKit/pwr_mgt/IOPMLib.h`:
/// `iokit_family_msg(sub_iokit_powermanagement, 0x100)`): the root domain's general-interest
/// message when either clamshell bit moves, its argument the bits.
const CLAMSHELL_MESSAGE: u32 = 0xE003_4100;

/// How long one run of the watch's run loop may last: CoreFoundation's own "forever"
/// (`CFRunLoopRun`'s), so it returns only when [`Stop`] stops it.
const FOREVER: f64 = 1.0e10;

/// The notify check tokens a [`crate::situation::PowerWatch`] checks each reading, the power
/// source's and the charge's; each cancelled on drop.
#[derive(Debug)]
pub(crate) struct Tokens {
    source: Option<c_int>,
    charge: Option<c_int>,
}

/// A check token for `name`, where notify registered one.
fn register(name: &CStr) -> Option<c_int> {
    let mut token: c_int = 0;
    // SAFETY: `name` is NUL-terminated; `token` is a writable out-parameter.
    let status = unsafe { iokit::notify_register_check(name.as_ptr(), &raw mut token) };
    (status == 0).then_some(token)
}

/// Whether `token`'s name was posted since its last check (and on its first check after
/// registering); a check that fails, or no token, says not.
fn checked(token: Option<c_int>) -> bool {
    let Some(token) = token else {
        return false;
    };
    let mut check: c_int = 0;
    // SAFETY: `token` is a registered check token its owner holds; `check` is a writable
    // out-parameter.
    let status = unsafe { iokit::notify_check(token, &raw mut check) };
    status == 0 && check != 0
}

impl Tokens {
    /// The power source's and the charge's check tokens.
    pub(crate) fn register() -> Self {
        Self {
            source: register(NOTIFY_POWER_SOURCE),
            charge: register(NOTIFY_CHARGE),
        }
    }

    /// Which names were posted since the last check (the first check after registering says
    /// both, which reads the sources and the adapter once at the start); `None` where neither
    /// token is registered. Both are checked, so neither carries a post into the next reading.
    pub(crate) fn posted(&self) -> Option<Posted> {
        if self.source.is_none() && self.charge.is_none() {
            return None;
        }
        Some(Posted {
            source: checked(self.source),
            charge: checked(self.charge),
        })
    }
}

impl Drop for Tokens {
    fn drop(&mut self) {
        for token in [self.source, self.charge].into_iter().flatten() {
            // SAFETY: a token this owner registered, cancelled once.
            unsafe { iokit::notify_cancel(token) };
        }
    }
}

/// `dict`'s value for `key`, borrowed while `dict` lives; NULL where it has none.
fn value(dict: iokit::CFDictionaryRef, key: &CStr) -> CFTypeRef {
    let Some(key) = Cf::string(key) else {
        return std::ptr::null();
    };
    // SAFETY: `dict` is a live dictionary the caller holds and `key` a live CFString; the value
    // is borrowed (the Get rule).
    unsafe { iokit::CFDictionaryGetValue(dict, key.0) }
}

/// A borrowed CF value read as a boolean; `None` for any other type.
fn cf_bool(value: CFTypeRef) -> Option<bool> {
    if value.is_null() {
        return None;
    }
    // SAFETY: `value` is a live CF object (borrowed); its type is checked before it is read as a
    // boolean.
    unsafe {
        (iokit::CFGetTypeID(value) == iokit::CFBooleanGetTypeID())
            .then(|| iokit::CFBooleanGetValue(value.cast()) != 0)
    }
}

/// The power sources now (`IOPSCopyPowerSourcesInfo`): what provides power, and the internal
/// battery's charge and whether it charges; the adapter's rating is [`adapter_watts`]' (the
/// reading's `adapter_w` is `None` here). `None` where the power manager answered no snapshot, no
/// source word this reads, or a present internal battery whose charge or charging it could not
/// read: unread, never "no battery" or "not charging".
pub(crate) fn power_reading() -> Option<PowerReading> {
    // SAFETY: the snapshot is +1 (released by `Cf`) or NULL.
    let blob = Cf::adopt(unsafe { iokit::IOPSCopyPowerSourcesInfo() })?;
    // SAFETY: `blob` is the live snapshot; the word is borrowed while it lives.
    let providing = unsafe { iokit::IOPSGetProvidingPowerSourceType(blob.0) };
    let source = PowerSource::of(&cf_text(providing.cast())?)?;
    Some(PowerReading {
        source,
        battery: internal_battery(&blob)?,
        adapter_w: None,
    })
}

/// The internal battery in power-source snapshot `blob`: its `Current Capacity` of its `Max
/// Capacity` (the power sources give both as a percentage) and `Is Charging`. `Some(None)` where
/// the snapshot lists no internal battery that is present; `None` where the list could not be
/// read, or a present internal battery's charge or `Is Charging` could not.
fn internal_battery(blob: &Cf) -> Option<Option<BatteryReading>> {
    // SAFETY: `blob` is a live snapshot; the list is +1 (released by `Cf`) or NULL.
    let list = Cf::adopt(unsafe { iokit::IOPSCopyPowerSourcesList(blob.0) }.cast())?;
    // SAFETY: `list` is a live CFArray; each index is below its count, and each element and
    // its description are borrowed while `blob` and `list` live.
    unsafe {
        if iokit::CFGetTypeID(list.0) != iokit::CFArrayGetTypeID() {
            return None;
        }
        let count = iokit::CFArrayGetCount(list.0.cast());
        for i in 0..count {
            let ps = iokit::CFArrayGetValueAtIndex(list.0.cast(), i);
            let desc = iokit::IOPSGetPowerSourceDescription(blob.0, ps);
            if desc.is_null() || iokit::CFGetTypeID(desc.cast()) != iokit::CFDictionaryGetTypeID() {
                continue;
            }
            if cf_text(value(desc, c"Type")).as_deref() != Some("InternalBattery")
                || cf_bool(value(desc, c"Is Present")) == Some(false)
            {
                continue;
            }
            let current = cf_u64(value(desc, c"Current Capacity"))?;
            let max = cf_u64(value(desc, c"Max Capacity")).filter(|m| *m > 0)?;
            let pct = (current.saturating_mul(100) + max / 2) / max;
            return Some(Some(BatteryReading {
                pct: u8::try_from(pct.min(100)).unwrap_or(100),
                charging: cf_bool(value(desc, c"Is Charging"))?,
            }));
        }
        Some(None)
    }
}

/// The wall adapter's rating (`IOPSCopyExternalPowerAdapterDetails`' `Watts`); `None` where no
/// adapter is attached or it gives no rating.
pub(crate) fn adapter_watts() -> Option<u32> {
    // SAFETY: the details are +1 (released by `Cf`) or NULL.
    let details = Cf::adopt(unsafe { iokit::IOPSCopyExternalPowerAdapterDetails() }.cast())?;
    // SAFETY: `details` is a live CF object; its type is checked before it is read as a
    // dictionary.
    if unsafe { iokit::CFGetTypeID(details.0) } != unsafe { iokit::CFDictionaryGetTypeID() } {
        return None;
    }
    cf_u64(value(details.0.cast(), c"Watts")).and_then(|w| u32::try_from(w).ok())
}

/// The lid as the root domain publishes it ([`crate::macos::clamshell_state`]).
pub(crate) fn clamshell() -> Option<Option<bool>> {
    crate::macos::clamshell_state()
}

/// The root domain's `Last Sleep Reason`.
pub(crate) fn sleep_reason() -> Option<String> {
    crate::macos::sleep_reason()
}

/// The lid and the last sleep, read again into `held`. The caller holds the lock across the
/// reads, so a clamshell message the watch hears meanwhile is applied after them, never
/// overwritten by an older read.
pub(crate) fn reread(held: &mut Held) {
    if let Some(closed) = clamshell() {
        held.lid.read(closed, unix_ms_now());
    }
    if let Some(awake) = crate::situation::awake() {
        held.awake = Some(awake);
    }
}

/// Ends the watch's thread when dropped: it marks the thread stopped, then signals the thread's
/// stop source and wakes its run loop. A signalled source stays pending until the run loop runs,
/// so a drop before the thread first runs it is not lost (a bare `CFRunLoopStop` then would
/// be). The same teardown as `aterm_uds::power`'s listener, which this crate cannot depend on.
#[derive(Debug)]
pub(crate) struct Stop {
    run_loop: CFRunLoopRef,
    source: CFRunLoopSourceRef,
    stopped: Arc<AtomicBool>,
}

// SAFETY: `Stop` holds its own retained reference to the run loop and the source, and uses them
// only through CoreFoundation calls documented safe from any thread (`CFRunLoopSourceSignal`,
// `CFRunLoopWakeUp`, `CFRelease`).
unsafe impl Send for Stop {}
// SAFETY: as for `Send`; a shared `Stop` exposes no call at all.
unsafe impl Sync for Stop {}

impl Drop for Stop {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // SAFETY: both references are the ones the watch's thread retained for this `Stop`,
        // released once here; signalling an invalidated source does nothing.
        unsafe {
            iokit::CFRunLoopSourceSignal(self.source);
            iokit::CFRunLoopWakeUp(self.run_loop);
            iokit::CFRelease(self.source.cast_const().cast());
            iokit::CFRelease(self.run_loop.cast_const().cast());
        }
    }
}

/// The stop source's perform: ends the current run of the run loop it fires on (the watch's).
extern "C" fn stop_here(_info: *mut c_void) {
    // SAFETY: the calling thread's own run loop, which is running (this is its callout).
    unsafe { iokit::CFRunLoopStop(iokit::CFRunLoopGetCurrent()) };
}

/// The shared state a callback's `refcon` points at.
///
/// # Safety
/// `refcon` is the `Arc<Shared>` the watch thread leaked for its registrations, which it takes
/// back only after the run loop stopped and every registration was released.
unsafe fn state_of<'a>(refcon: *mut c_void) -> &'a Shared {
    // SAFETY: the caller's.
    unsafe { &*refcon.cast::<Shared>() }
}

/// The root domain's general interest: a clamshell message's closed bit is the lid
/// ([`crate::situation::LidLatch::clamshell`]); every other message is not the lid's.
#[cfg_attr(trust_verify, trust::skip)]
unsafe extern "C" fn interest(
    refcon: *mut c_void,
    _service: io_service_t,
    kind: u32,
    argument: *mut c_void,
) {
    if kind != CLAMSHELL_MESSAGE {
        return;
    }
    // SAFETY: IOKit calls this from the watch thread's run loop with the refcon the
    // registration was given ([`state_of`]).
    let shared = unsafe { state_of(refcon) };
    shared
        .state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .lid
        .clamshell(argument.addr(), unix_ms_now());
}

/// The display pipe `entry`'s registry id, where it is an external display's (`Location`
/// `External`).
#[cfg(target_arch = "aarch64")]
fn external_id(entry: io_object_t) -> Option<u64> {
    let key = Cf::string(c"Location")?;
    let location = Cf::property(entry, key.0)?;
    if cf_text(location.0).as_deref() != Some("External") {
        return None;
    }
    entry_id(entry)
}

/// `entry`'s registry-wide id.
#[cfg(target_arch = "aarch64")]
fn entry_id(entry: io_object_t) -> Option<u64> {
    let mut id: u64 = 0;
    // SAFETY: `entry` is a live registry entry; `id` is a writable out-parameter.
    (unsafe { iokit::IORegistryEntryGetRegistryEntryID(entry, &raw mut id) } == 0).then_some(id)
}

/// Each entry `iterator` holds, released after `each` saw it; draining it arms its
/// notification again.
#[cfg(target_arch = "aarch64")]
fn drain(iterator: io_iterator_t, mut each: impl FnMut(io_object_t)) {
    loop {
        // SAFETY: `iterator` is the live iterator the matching notification armed.
        let entry = unsafe { iokit::IOIteratorNext(iterator) };
        if entry == 0 {
            break;
        }
        each(entry);
        // SAFETY: the +1 `IOIteratorNext` returned, released once.
        unsafe { iokit::IOObjectRelease(entry) };
    }
}

/// External display pipes attached now, by one listing ([`crate::situation::external_displays`]).
#[cfg(target_arch = "aarch64")]
pub(crate) fn external_census() -> Option<u16> {
    let mut iterator: io_iterator_t = 0;
    // SAFETY: the class name is NUL-terminated; the dictionary is +1 and consumed by the call;
    // the iterator is +1 on success, released below.
    let status = unsafe {
        let matching = iokit::IOServiceMatching(c"DCPAVServiceProxy".as_ptr());
        if matching.is_null() {
            return None;
        }
        iokit::IOServiceGetMatchingServices(
            MAIN_PORT_DEFAULT,
            matching.cast_const(),
            &raw mut iterator,
        )
    };
    if status != 0 || iterator == 0 {
        return None;
    }
    let mut external = 0u16;
    drain(iterator, |entry| {
        if external_id(entry).is_some() {
            external = external.saturating_add(1);
        }
    });
    // SAFETY: the +1 iterator the listing returned, released once.
    unsafe { iokit::IOObjectRelease(iterator) };
    Some(external)
}

/// A display pipe appeared: an external one joins the set.
#[cfg(target_arch = "aarch64")]
#[cfg_attr(trust_verify, trust::skip)]
unsafe extern "C" fn appeared(refcon: *mut c_void, iterator: io_iterator_t) {
    // SAFETY: as for [`interest`].
    let shared = unsafe { state_of(refcon) };
    let mut held = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    let set = held.external.get_or_insert_default();
    drain(iterator, |entry| {
        if let Some(id) = external_id(entry) {
            set.insert(id);
        }
    });
}

/// A display pipe went: it leaves the set (only external ones are in it).
#[cfg(target_arch = "aarch64")]
#[cfg_attr(trust_verify, trust::skip)]
unsafe extern "C" fn gone(refcon: *mut c_void, iterator: io_iterator_t) {
    // SAFETY: as for [`interest`].
    let shared = unsafe { state_of(refcon) };
    let mut held = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    drain(iterator, |entry| {
        if let (Some(set), Some(id)) = (held.external.as_mut(), entry_id(entry)) {
            set.remove(&id);
        }
    });
}

/// The display pipes' matching notifications on `port`: their end first, so a pipe that goes
/// between the two registrations is never left in the set, then their arrival, whose first
/// drain lists the external pipes attached now. The notifications' iterators, which the caller
/// releases at teardown.
#[cfg(target_arch = "aarch64")]
fn watch_displays(port: IONotificationPortRef, refcon: *mut c_void) -> Vec<io_iterator_t> {
    let mut iterators = Vec::new();
    for (kind, callback) in [
        (
            c"IOServiceTerminate",
            gone as unsafe extern "C" fn(*mut c_void, io_iterator_t),
        ),
        (c"IOServiceFirstMatch", appeared),
    ] {
        let mut iterator: io_iterator_t = 0;
        // SAFETY: the class name is NUL-terminated; the dictionary is +1 and consumed by the
        // registration; `port` is live and `refcon` outlives the registration (the caller's).
        let status = unsafe {
            let matching = iokit::IOServiceMatching(c"DCPAVServiceProxy".as_ptr());
            if matching.is_null() {
                continue;
            }
            iokit::IOServiceAddMatchingNotification(
                port,
                kind.as_ptr(),
                matching.cast_const(),
                Some(callback),
                refcon,
                &raw mut iterator,
            )
        };
        if status != 0 || iterator == 0 {
            continue;
        }
        // SAFETY: the registration's own iterator, drained on the thread whose run loop will
        // run its callback; `refcon` is live (the caller's).
        unsafe { callback(refcon, iterator) };
        iterators.push(iterator);
    }
    iterators
}

/// Off Apple silicon there are no display pipes to watch.
#[cfg(not(target_arch = "aarch64"))]
fn watch_displays(_port: IONotificationPortRef, _refcon: *mut c_void) -> Vec<io_iterator_t> {
    Vec::new()
}

/// Starts the watch's thread: it registers the root domain's general interest and the display
/// pipes' matching notifications on one notification port on its run loop, reads the lid and
/// the last sleep, and hears until the returned [`Stop`] is dropped. `None` where the thread or
/// its port could not start; the caller then reads the lid and the last sleep itself.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn listen(shared: Arc<Shared>) -> Option<Stop> {
    let (told, handed) = mpsc::sync_channel::<Option<Stop>>(1);
    let spawned = std::thread::Builder::new()
        .name("aterm-situation".to_owned())
        .spawn(move || {
            // SAFETY: `MAIN_PORT_DEFAULT` names the default main port; the port is this
            // thread's, destroyed below.
            let port = unsafe { iokit::IONotificationPortCreate(MAIN_PORT_DEFAULT) };
            if port.is_null() {
                let _ = told.send(None);
                return;
            }
            let refcon = Arc::into_raw(shared).cast_mut().cast::<c_void>();
            let mut notifier: io_object_t = 0;
            if let Some(root) = crate::macos::root_service() {
                // SAFETY: `port` is live, `root` the root domain's service the process keeps,
                // and `refcon` outlives the registration (it is taken back below, after the
                // notifier is released).
                let status = unsafe {
                    iokit::IOServiceAddInterestNotification(
                        port,
                        root,
                        c"IOGeneralInterest".as_ptr(),
                        Some(interest),
                        refcon,
                        &raw mut notifier,
                    )
                };
                if status != 0 {
                    notifier = 0;
                }
            }
            let iterators = watch_displays(port, refcon);
            // SAFETY: as for the callbacks: the refcon is live, and no callback runs before
            // this thread runs its run loop, so this read and a message are never interleaved.
            reread(
                &mut unsafe { state_of(refcon) }
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            let mut context = CFRunLoopSourceContext {
                version: 0,
                info: std::ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: stop_here,
            };
            // SAFETY: `port` is live; its source is borrowed while the port lives, and this
            // thread's run loop is its own. The stop source is this thread's (the Create rule),
            // its context copied at creation; the run loop and it are each retained once more
            // for the `Stop`, which releases them.
            let (run_loop, source, stop) = unsafe {
                let source = iokit::IONotificationPortGetRunLoopSource(port);
                let run_loop = iokit::CFRunLoopGetCurrent();
                iokit::CFRunLoopAddSource(run_loop, source, iokit::kCFRunLoopDefaultMode);
                let stop = iokit::CFRunLoopSourceCreate(std::ptr::null(), 0, &raw mut context);
                if !stop.is_null() {
                    iokit::CFRunLoopAddSource(run_loop, stop, iokit::kCFRunLoopDefaultMode);
                    iokit::CFRetain(run_loop.cast_const().cast());
                    iokit::CFRetain(stop.cast_const().cast());
                }
                (run_loop, source, stop)
            };
            if stop.is_null() {
                let _ = told.send(None);
            } else {
                let stopped = Arc::new(AtomicBool::new(false));
                // A `Stop` nobody received is dropped here, which marks this thread stopped: it
                // then never runs its run loop.
                let _ = told.send(Some(Stop {
                    run_loop,
                    source: stop,
                    stopped: Arc::clone(&stopped),
                }));
                while !stopped.load(Ordering::Acquire) {
                    // SAFETY: this thread's own run loop, in the mode its sources were added to.
                    unsafe { iokit::CFRunLoopRunInMode(iokit::kCFRunLoopDefaultMode, FOREVER, 0) };
                }
            }
            // SAFETY: no callback runs once the run loop stopped running: the sources leave it,
            // each registration is released, the port is destroyed, and the refcon is taken back
            // last, by this thread alone. The stop source is released by its creator (the
            // `Stop` released its own reference, or never got one).
            unsafe {
                if !stop.is_null() {
                    iokit::CFRunLoopRemoveSource(run_loop, stop, iokit::kCFRunLoopDefaultMode);
                    iokit::CFRunLoopSourceInvalidate(stop);
                    iokit::CFRelease(stop.cast_const().cast());
                }
                iokit::CFRunLoopRemoveSource(run_loop, source, iokit::kCFRunLoopDefaultMode);
                if notifier != 0 {
                    iokit::IOObjectRelease(notifier);
                }
                for iterator in iterators {
                    iokit::IOObjectRelease(iterator);
                }
                iokit::IONotificationPortDestroy(port);
                drop(Arc::from_raw(refcon.cast_const().cast::<Shared>()));
            }
        });
    spawned.ok()?;
    handed.recv().ok().flatten()
}
