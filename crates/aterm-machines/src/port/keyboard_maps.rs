// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live keyboard-filter writer. Public hidutil uses a Simple client, whose server
//! whitelist excludes the native modifier table. A Passive client can query and set
//! service properties without event-monitor entitlements or an event subscription.
//!
//! Apple documents Passive's property access and lack of entitlement requirements in
//! [HIDEventSystemClient.h](https://github.com/apple-oss-distributions/IOHIDFamily/blob/777ccd9698845aadf711e32d843c8c9b777431d9/HID/Headers/HIDEventSystemClient.h).
//! The installed IOKit server was also measured to whitelist only Simple (type 4), with
//! UserKeyMapping admitted and HIDKeyboardModifierMappingPairs excluded.

use super::KeyboardMapChange;
use crate::model::PValue;
use std::ffi::{CStr, c_void};
use std::os::unix::fs::MetadataExt as _;
use std::ptr;

type Ref = *const c_void;
type Mapping = Vec<(i64, i64)>;
type RestrictedRemapping = unsafe extern "C" fn(Ref) -> bool;

#[cfg(test)]
const NATIVE: &str = "HIDKeyboardModifierMappingPairs";
#[cfg(test)]
const USER: &str = "UserKeyMapping";
const INVALID: &str = "unrecognized live keyboard filter state; existing remaps left untouched";
const STALE: &str = "keyboard mappings changed during activation; remaining remaps left untouched";

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(value: Ref) -> isize;
    fn CFArrayGetValueAtIndex(value: Ref, index: isize) -> Ref;
    fn CFStringCreateWithCString(allocator: Ref, text: *const i8, encoding: u32) -> Ref;
    fn CFDataCreate(allocator: Ref, bytes: *const u8, length: isize) -> Ref;
    fn CFDataGetLength(value: Ref) -> isize;
    fn CFDataGetBytePtr(value: Ref) -> *const u8;
    fn CFPropertyListCreateWithData(
        allocator: Ref,
        data: Ref,
        options: usize,
        format: *mut isize,
        error: *mut Ref,
    ) -> Ref;
    fn CFPropertyListCreateData(
        allocator: Ref,
        value: Ref,
        format: isize,
        options: usize,
        error: *mut Ref,
    ) -> Ref;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreateWithType(allocator: Ref, kind: i32, attributes: Ref) -> Ref;
    fn IOHIDEventSystemClientCopyServices(client: Ref) -> Ref;
    fn IOHIDEventSystemClientCopyProperty(client: Ref, key: Ref) -> Ref;
    fn IOHIDServiceClientGetRegistryID(service: Ref) -> Ref;
    fn IOHIDServiceClientConformsTo(service: Ref, page: u32, usage: u32) -> i32;
    fn IOHIDServiceClientCopyProperty(service: Ref, key: Ref) -> Ref;
    fn IOHIDServiceClientSetProperty(service: Ref, key: Ref, value: Ref) -> u8;
    fn IOHIDCheckAccess(request: i32) -> i32;
}

fn restricted_remapping() -> Option<RestrictedRemapping> {
    // SAFETY: RTLD_DEFAULT searches loaded images; IOKit is linked by this module,
    // and the export name is a static NUL-terminated C string. Dynamic lookup keeps
    // older supported macOS versions loadable when this private helper is absent.
    let symbol = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"_IOHIDIsRestrictedRemappingProperty".as_ptr(),
        )
    };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: the resolved IOKit helper accepts a CF property list and returns C bool,
    // matching its calls in IOHIDKeyboardFilter. The linked framework stays loaded.
    Some(unsafe { std::mem::transmute::<*mut c_void, RestrictedRemapping>(symbol) })
}

fn mapping_restricted(
    mapping: &Mapping,
    value: Ref,
    predicate: Option<RestrictedRemapping>,
) -> Result<bool, String> {
    if let Some(predicate) = predicate {
        // SAFETY: value is the owned CFArray created from this complete mapping.
        return Ok(unsafe { predicate(value) });
    }
    // Apple's IOKitUser IOHIDLibPrivate.c restriction implementation excludes Caps39 and
    // keyboard modifiers E0–E7 from alphanumeric usages. When the newer private helper
    // is absent, accept only these known basic modifier endpoints, never unknown keys.
    // https://github.com/apple-oss-distributions/IOKitUser/blob/323ead896d04424f87184d8f6ff0cce811aab106/hid.subproj/IOHIDLibPrivate.c#L511
    let basic = |key| matches!(key, 0x7_0000_0039 | 0x7_0000_00e0..=0x7_0000_00e7);
    if mapping
        .iter()
        .all(|(source, destination)| basic(*source) && basic(*destination))
    {
        Ok(false)
    } else {
        Err("macOS cannot preflight these keyboard remaps; existing remaps left untouched".into())
    }
}

fn current_login(target: u32, actor: u32, console: u32) -> Result<(), String> {
    if actor != target || console != target {
        return Err("keyboard activation must run as the user at this Mac's console; existing remaps left untouched".into());
    }
    Ok(())
}

fn check_current_login(target: u32) -> Result<(), String> {
    // SAFETY: geteuid only reads this process's actor; no identity is changed.
    let actor = unsafe { libc::geteuid() };
    let console = std::fs::metadata("/dev/console")
        .map_err(|error| error.to_string())?
        .uid();
    current_login(target, actor, console)
}

/// Read keyboard-capable service identities without events, listeners or child processes.
pub(super) fn service_ids() -> Result<Vec<u64>, String> {
    // SAFETY: Passive (2) permits property access and registers no event subscription.
    let client =
        Owned::new(unsafe { IOHIDEventSystemClientCreateWithType(ptr::null(), 2, ptr::null()) })?;
    // SAFETY: client is owned and live; CopyServices returns an owned CFArray.
    let services = Owned::new(unsafe { IOHIDEventSystemClientCopyServices(client.0) })?;
    identities(services.0)
}

/// One Passive client supplies both the current cohort and its filter properties.
pub(super) fn observations() -> Result<Vec<super::KeyboardServiceObservation>, String> {
    // SAFETY: Passive (2) permits property reads without events or a subscription.
    let client =
        Owned::new(unsafe { IOHIDEventSystemClientCreateWithType(ptr::null(), 2, ptr::null()) })?;
    // SAFETY: client is live; CopyServices returns an owned CFArray.
    let services = Owned::new(unsafe { IOHIDEventSystemClientCopyServices(client.0) })?;
    super::keyboard_observations(identities(services.0)?, &records(client.0)?)
}

fn identities(services: Ref) -> Result<Vec<u64>, String> {
    // SAFETY: services is a non-null CF object, checked before array operations.
    if unsafe { CFGetTypeID(services) != CFArrayGetTypeID() } {
        return Err(INVALID.into());
    }
    let mut ids = Vec::new();
    // SAFETY: services is an owned CFArray and every index is in its bounds.
    let count = unsafe { CFArrayGetCount(services) };
    if count < 0 {
        return Err(INVALID.into());
    }
    for index in 0..count {
        // SAFETY: CopyServices supplies live service clients borrowed from this array.
        let service = unsafe { CFArrayGetValueAtIndex(services, index) };
        // SAFETY: the public IOKit conformance query accepts a live service and HID usages.
        if unsafe { IOHIDServiceClientConformsTo(service, 1, 6) } == 0 {
            continue;
        }
        // SAFETY: GetRegistryID borrows a number from the live service.
        let PValue::Int(id) = value(unsafe { IOHIDServiceClientGetRegistryID(service) })? else {
            return Err(INVALID.into());
        };
        let id = u64::try_from(id).map_err(|_| INVALID)?;
        if id == 0 {
            return Err(INVALID.into());
        }
        ids.push(id);
        if ids.len() > 4096 {
            return Err(INVALID.into());
        }
    }
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(INVALID.into());
    }
    Ok(ids)
}

/// One non-null Create/Copy result. Get results are borrowed from their owning array.
struct Owned(Ref);

impl Owned {
    fn new(value: Ref) -> Result<Self, String> {
        if value.is_null() {
            Err("cannot access the live keyboard service".into())
        } else {
            Ok(Self(value))
        }
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: new admits only non-null Create/Copy results; this owns their +1.
        unsafe { CFRelease(self.0) };
    }
}

fn key(text: &CStr) -> Result<Owned, String> {
    // SAFETY: text is NUL terminated; CoreFoundation copies it as UTF-8.
    Owned::new(unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), 0x0800_0100) })
}

fn value(value: Ref) -> Result<PValue, String> {
    if value.is_null() {
        return Err(INVALID.into());
    }
    // SAFETY: callers supply a live CF value. XML format 100 emits a property list;
    // no CFError is requested, and data owns the returned +1.
    let data = Owned::new(unsafe {
        CFPropertyListCreateData(ptr::null(), value, 100, 0, ptr::null_mut())
    })?;
    // SAFETY: the successful CreateData result is a CFData, kept alive through parse.
    let length = usize::try_from(unsafe { CFDataGetLength(data.0) }).map_err(|_| INVALID)?;
    // SAFETY: data is a live CFData; its byte pointer remains valid while data is owned.
    let bytes = unsafe { CFDataGetBytePtr(data.0) };
    if length == 0 || length > aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES || bytes.is_null() {
        return Err(INVALID.into());
    }
    // SAFETY: CFData supplies length initialized bytes, and its owner is still alive.
    crate::plist::parse(unsafe { std::slice::from_raw_parts(bytes, length) })
        .map_err(|_| INVALID.into())
}

fn property(service: Ref, text: &CStr) -> Result<PValue, String> {
    let key = key(text)?;
    // SAFETY: service is borrowed from the live services array; key is a live CFString.
    let property = Owned::new(unsafe { IOHIDServiceClientCopyProperty(service, key.0) })?;
    value(property.0)
}

fn records(client: Ref) -> Result<PValue, String> {
    let key = key(c"ServiceRecords")?;
    // SAFETY: client is the owned Passive client and key is a live CFString.
    let records = Owned::new(unsafe { IOHIDEventSystemClientCopyProperty(client, key.0) })?;
    value(records.0)
}

fn mapping_value(mapping: &Mapping) -> Result<Owned, String> {
    unique_sources(mapping)?;
    let value = PValue::Array(
        mapping
            .iter()
            .map(|(source, destination)| {
                PValue::Dict(vec![
                    ("HIDKeyboardModifierMappingSrc".into(), PValue::Int(*source)),
                    (
                        "HIDKeyboardModifierMappingDst".into(),
                        PValue::Int(*destination),
                    ),
                ])
            })
            .collect(),
    );
    let xml = crate::plist::document(&value);
    let length = isize::try_from(xml.len()).map_err(|_| INVALID)?;
    // SAFETY: xml supplies length valid bytes, copied by CFDataCreate.
    let data = Owned::new(unsafe { CFDataCreate(ptr::null(), xml.as_ptr(), length) })?;
    // SAFETY: data is a live CFData containing our XML array; immutable options 0,
    // no format/error output requested. The returned property list owns its +1.
    Owned::new(unsafe {
        CFPropertyListCreateWithData(ptr::null(), data.0, 0, ptr::null_mut(), ptr::null_mut())
    })
}

#[cfg(test)]
fn field<'a>(value: &'a PValue, key: &str) -> Option<&'a PValue> {
    let PValue::Dict(fields) = value else {
        return None;
    };
    let mut matching = fields.iter().filter(|(name, _)| name == key);
    let value = &matching.next()?.1;
    matching.next().is_none().then_some(value)
}

fn unique_sources(mapping: &Mapping) -> Result<(), String> {
    let mut sources: Vec<_> = mapping.iter().map(|(source, _)| *source).collect();
    sources.sort_unstable();
    if sources.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("duplicate keyboard mapping source; existing remaps left untouched".into());
    }
    Ok(())
}

fn check_snapshot(records: &PValue, change: &KeyboardMapChange) -> Result<(), String> {
    let state = super::KeyboardServiceState::read(records, change.registry)?;
    if state.prefix != change.before_prefix {
        return Err(STALE.into());
    }
    for (actual, expected) in [
        (&state.native, &change.before_native),
        (&state.user, &change.before_user),
    ] {
        unique_sources(expected)?;
        let mut expected = expected.clone();
        expected.sort_unstable();
        if *actual != expected {
            return Err(STALE.into());
        }
    }
    Ok(())
}

fn check_model(service: Ref, vendor: u32, product: u32) -> Result<(), String> {
    for (name, expected) in [
        (c"VendorID", i64::from(vendor)),
        (c"ProductID", i64::from(product)),
    ] {
        if property(service, name)? != PValue::Int(expected) {
            return Err("keyboard service identity changed; existing remaps left untouched".into());
        }
    }
    let primary = property(service, c"PrimaryUsagePage").ok() == Some(PValue::Int(1))
        && property(service, c"PrimaryUsage").ok() == Some(PValue::Int(6));
    if !primary
        && !super::keyboard_usage::has_keyboard_usage(&property(service, c"DeviceUsagePairs")?)
    {
        return Err("keyboard service identity changed; existing remaps left untouched".into());
    }
    Ok(())
}

fn check_access(
    restricted: impl IntoIterator<Item = bool>,
    access: impl FnOnce() -> i32,
) -> Result<(), String> {
    if restricted.into_iter().any(|restricted| restricted) && access() != 0 {
        return Err("keyboard remaps require an existing Input Monitoring grant; left untouched without requesting access".into());
    }
    Ok(())
}

struct Prepared<'a> {
    change: &'a KeyboardMapChange,
    service: Ref,
    native: Option<PreparedMapping>,
    user: Option<PreparedMapping>,
}

struct PreparedMapping {
    value: Owned,
    restricted: bool,
}

impl PreparedMapping {
    fn new(mapping: &Mapping, predicate: Option<RestrictedRemapping>) -> Result<Self, String> {
        let value = mapping_value(mapping)?;
        let restricted = mapping_restricted(mapping, value.0, predicate)?;
        Ok(Self { value, restricted })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MappingProperty {
    Native,
    User,
}

/// Each write checks the upstream state and both tables against the initial state plus
/// only our successful preceding writes. An observed competing edit stops the next write.
fn apply_checked(
    change: &KeyboardMapChange,
    mut write: impl FnMut(MappingProperty, &KeyboardMapChange) -> Result<(), String>,
) -> Result<(), String> {
    let mut expected = change.clone();
    for (property, wanted) in [
        (MappingProperty::Native, &change.native),
        (MappingProperty::User, &change.user),
    ] {
        if let Some(wanted) = wanted {
            write(property, &expected)?;
            match property {
                MappingProperty::Native => expected.before_native.clone_from(wanted),
                MappingProperty::User => expected.before_user.clone_from(wanted),
            }
        }
    }
    Ok(())
}

pub(super) fn set(
    target: u32,
    vendor: u32,
    product: u32,
    changes: &[KeyboardMapChange],
) -> Result<(), String> {
    if changes.is_empty() {
        return Ok(());
    }
    check_current_login(target)?;
    let mut identities: Vec<_> = changes.iter().map(|change| change.registry).collect();
    identities.sort_unstable();
    if identities.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("duplicate keyboard service identity; existing remaps left untouched".into());
    }
    let restricted_remapping = restricted_remapping();
    // SAFETY: type 2 is Passive: property access without event subscriptions or monitor
    // entitlements. Null allocator/attributes select defaults. No callback is registered.
    let client =
        Owned::new(unsafe { IOHIDEventSystemClientCreateWithType(ptr::null(), 2, ptr::null()) })?;
    // SAFETY: client is a live event-system client; CopyServices returns an owned CFArray.
    let services = Owned::new(unsafe { IOHIDEventSystemClientCopyServices(client.0) })?;
    // SAFETY: services is a non-null CF object, checked before CFArray operations.
    if unsafe { CFGetTypeID(services.0) != CFArrayGetTypeID() } {
        return Err(INVALID.into());
    }
    let mut prepared = Vec::new();
    for change in changes {
        let mut matching = None;
        // SAFETY: services has been checked as an array and remains owned throughout set.
        for index in 0..unsafe { CFArrayGetCount(services.0) } {
            // SAFETY: index is in the array's bounds; service is borrowed from services.
            let service = unsafe { CFArrayGetValueAtIndex(services.0, index) };
            // SAFETY: CopyServices supplies service clients; GetRegistryID borrows a number.
            let registry = value(unsafe { IOHIDServiceClientGetRegistryID(service) })?;
            if matches!(registry, PValue::Int(id) if u64::try_from(id).ok() == Some(change.registry))
                && matching.replace(service).is_some()
            {
                return Err(INVALID.into());
            }
        }
        let service =
            matching.ok_or("keyboard service disappeared; existing remaps left untouched")?;
        check_model(service, vendor, product)?;
        prepared.push(Prepared {
            change,
            service,
            native: change
                .native
                .as_ref()
                .map(|mapping| PreparedMapping::new(mapping, restricted_remapping))
                .transpose()?,
            user: change
                .user
                .as_ref()
                .map(|mapping| PreparedMapping::new(mapping, restricted_remapping))
                .transpose()?,
        });
    }
    let arrays: Vec<_> = prepared
        .iter()
        .flat_map(|change| [change.native.as_ref(), change.user.as_ref()])
        .flatten()
        .collect();
    // Every complete array has already been classified without requesting access.
    check_access(arrays.iter().map(|array| array.restricted), || {
        // SAFETY: 1 is ListenEvent; CheckAccess only reads the existing grant.
        unsafe { IOHIDCheckAccess(1) }
    })?;
    let snapshot = records(client.0)?;
    for change in &prepared {
        check_snapshot(&snapshot, change.change)?;
    }
    let native_key = key(c"HIDKeyboardModifierMappingPairs")?;
    let user_key = key(c"UserKeyMapping")?;
    for change in &prepared {
        apply_checked(change.change, |property, expected| {
            let (key, mapping) = match property {
                MappingProperty::Native => (&native_key, &change.native),
                MappingProperty::User => (&user_key, &change.user),
            };
            let mapping = mapping.as_ref().ok_or(INVALID)?;
            check_current_login(target)?;
            check_model(change.service, vendor, product)?;
            check_snapshot(&records(client.0)?, expected)?;
            // Recheck existing access immediately before a restricted write as well
            // as batch preflight; classification belongs to this immutable array.
            check_access([mapping.restricted], || {
                // SAFETY: CheckAccess reads ListenEvent's current grant without asking.
                unsafe { IOHIDCheckAccess(1) }
            })?;
            check_current_login(target)?;
            // SAFETY: service is borrowed from services, both key and complete array
            // are owned; this property's target, complete before-image and permission
            // were rechecked at this write's boundary. The caller independently probes.
            if unsafe { IOHIDServiceClientSetProperty(change.service, key.0, mapping.value.0) } == 0
            {
                return Err("the live keyboard service refused a mapping update".into());
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "keyboard_maps_tests.rs"]
mod tests;
