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

const NATIVE: &str = "HIDKeyboardModifierMappingPairs";
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
    // SAFETY: services is a non-null CF object, checked before array operations.
    if unsafe { CFGetTypeID(services.0) != CFArrayGetTypeID() } {
        return Err(INVALID.into());
    }
    let mut ids = Vec::new();
    // SAFETY: services is an owned CFArray and every index is in its bounds.
    for index in 0..unsafe { CFArrayGetCount(services.0) } {
        // SAFETY: CopyServices supplies live service clients borrowed from this array.
        let service = unsafe { CFArrayGetValueAtIndex(services.0, index) };
        // SAFETY: the public IOKit conformance query accepts a live service and HID usages.
        if unsafe { IOHIDServiceClientConformsTo(service, 1, 6) } == 0 {
            continue;
        }
        // SAFETY: GetRegistryID borrows a number from the live service.
        let PValue::Int(id) = value(unsafe { IOHIDServiceClientGetRegistryID(service) })? else {
            return Err(INVALID.into());
        };
        ids.push(u64::try_from(id).map_err(|_| INVALID)?);
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
    if length == 0 || bytes.is_null() {
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

fn filter_mapping(filter: &PValue, name: &str) -> Result<Mapping, String> {
    let Some(PValue::Array(pairs)) = field(filter, name) else {
        return Err(INVALID.into());
    };
    let mapping = pairs
        .iter()
        .map(|pair| match (field(pair, "Src"), field(pair, "Dst")) {
            (Some(PValue::Int(source)), Some(PValue::Int(destination))) => {
                Ok((*source, *destination))
            }
            _ => Err(INVALID.to_owned()),
        })
        .collect::<Result<Mapping, String>>()?;
    unique_sources(&mapping)?;
    Ok(mapping)
}

fn check_snapshot(records: &PValue, change: &KeyboardMapChange) -> Result<(), String> {
    let PValue::Array(records) = records else {
        return Err(INVALID.into());
    };
    let mut services = records.iter().filter(|record| {
        matches!(field(record, "IORegistryEntryID"), Some(PValue::Int(id)) if u64::try_from(*id).ok() == Some(change.registry))
    });
    let service = services.next().ok_or(INVALID)?;
    if services.next().is_some() || !super::keyboard_usage::is_keyboard_service(service) {
        return Err(INVALID.into());
    }
    let Some(PValue::Array(filters)) = field(service, "ServiceFilterDebug") else {
        return Err(INVALID.into());
    };
    let mut filters = filters.iter().filter_map(|filter| {
        let plugin = field(filter, "plugin")?;
        matches!(field(plugin, "Class"), Some(PValue::Str(name)) if name == "IOHIDKeyboardFilter")
            .then_some(plugin)
    });
    let filter = filters.next().ok_or(INVALID)?;
    if filters.next().is_some() {
        return Err(INVALID.into());
    }
    if super::KeyboardPrefix::read(filter)? != change.before_prefix {
        return Err(STALE.into());
    }
    for (name, expected) in [(NATIVE, &change.before_native), (USER, &change.before_user)] {
        unique_sources(expected)?;
        let mut actual = filter_mapping(filter, name)?;
        let mut expected = expected.clone();
        actual.sort_unstable();
        expected.sort_unstable();
        if actual != expected {
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
mod tests {
    use super::*;

    #[test]
    fn the_passive_census_matches_live_keyboard_conformance_without_events() {
        // SAFETY: this Passive client reads service records only, registering no listener.
        let client = Owned::new(unsafe {
            IOHIDEventSystemClientCreateWithType(ptr::null(), 2, ptr::null())
        })
        .unwrap();
        let PValue::Array(records) = records(client.0).unwrap() else {
            panic!("service records")
        };
        let mut expected: Vec<_> = records
            .iter()
            .filter(|record| super::super::keyboard_usage::is_keyboard_service(record))
            .map(|record| match field(record, "IORegistryEntryID") {
                Some(PValue::Int(id)) => u64::try_from(*id).unwrap(),
                _ => panic!("service identity"),
            })
            .collect();
        expected.sort_unstable();
        assert_eq!(service_ids().unwrap(), expected);
    }

    fn change() -> KeyboardMapChange {
        KeyboardMapChange {
            registry: 7,
            before_prefix: super::super::KeyboardPrefix::default(),
            before_native: vec![(2, 3), (4, 5)],
            before_user: vec![(6, 7)],
            native: Some(vec![(4, 5)]),
            user: None,
        }
    }

    fn snapshot() -> PValue {
        snapshot_with(&[(4, 5), (2, 3)], &[(6, 7)])
    }

    fn snapshot_with(native: &[(i64, i64)], user: &[(i64, i64)]) -> PValue {
        snapshot_with_prefix(
            native,
            user,
            &super::super::keyboard_prefix::tests::fields(),
        )
    }

    fn snapshot_with_prefix(
        native: &[(i64, i64)],
        user: &[(i64, i64)],
        prefix: &[(String, PValue)],
    ) -> PValue {
        let pairs = |mapping: &[(i64, i64)]| {
            PValue::Array(
                mapping
                    .iter()
                    .map(|(source, destination)| {
                        PValue::Dict(vec![
                            ("Src".into(), PValue::Int(*source)),
                            ("Dst".into(), PValue::Int(*destination)),
                        ])
                    })
                    .collect(),
            )
        };
        PValue::Array(vec![PValue::Dict(vec![
            ("IORegistryEntryID".into(), PValue::Int(7)),
            ("PrimaryUsagePage".into(), PValue::Int(1)),
            ("PrimaryUsage".into(), PValue::Int(6)),
            (
                "ServiceFilterDebug".into(),
                PValue::Array(vec![PValue::Dict(vec![(
                    "plugin".into(),
                    PValue::Dict(
                        [
                            ("Class".into(), PValue::Str("IOHIDKeyboardFilter".into())),
                            (NATIVE.into(), pairs(native)),
                            (USER.into(), pairs(user)),
                        ]
                        .into_iter()
                        .chain(prefix.iter().cloned())
                        .collect(),
                    ),
                )])]),
            ),
        ])])
    }

    #[test]
    fn another_apps_user_change_after_native_activation_is_preserved() {
        let mut change = change();
        change.user = Some(vec![(6, 8)]);
        let mut native = change.before_native.clone();
        let mut user = change.before_user.clone();
        let external = vec![(6, 9), (10, 11)];
        let mut writes = Vec::new();
        let result = apply_checked(&change, |property, expected| {
            check_snapshot(&snapshot_with(&native, &user), expected)?;
            writes.push(property);
            match property {
                MappingProperty::Native => {
                    native.clone_from(change.native.as_ref().unwrap());
                    // A separate app changes the user table after our native setter.
                    user.clone_from(&external);
                }
                MappingProperty::User => user.clone_from(change.user.as_ref().unwrap()),
            }
            Ok(())
        });
        assert_eq!(result, Err(STALE.into()));
        assert_eq!(writes, [MappingProperty::Native]);
        assert_eq!(user, external);
        assert_eq!(native, change.native.unwrap());
    }

    #[test]
    fn upstream_change_after_native_activation_prevents_the_next_write() {
        let mut change = change();
        change.user = Some(vec![(6, 8)]);
        let mut native = change.before_native.clone();
        let mut user = change.before_user.clone();
        let mut prefix = super::super::keyboard_prefix::tests::fields();
        let external =
            super::super::keyboard_prefix::tests::pairs(&[(0x7_0000_003a, 0xc_0000_006f)]);
        let mut writes = Vec::new();
        let result = apply_checked(&change, |property, expected| {
            check_snapshot(&snapshot_with_prefix(&native, &user, &prefix), expected)?;
            writes.push(property);
            match property {
                MappingProperty::Native => {
                    native.clone_from(change.native.as_ref().unwrap());
                    // Even a still-safe upstream edit is newer state. It cannot be
                    // silently carried past the original Caps-passthrough before-image.
                    prefix
                        .iter_mut()
                        .find(|(name, _)| name == "FnFunctionUsageMap")
                        .unwrap()
                        .1 = external.clone();
                }
                MappingProperty::User => user.clone_from(change.user.as_ref().unwrap()),
            }
            Ok(())
        });
        assert_eq!(result, Err(STALE.into()));
        assert_eq!(writes, [MappingProperty::Native]);
        assert_eq!(user, change.before_user);
        assert_eq!(native, change.native.unwrap());
        assert_eq!(
            prefix
                .iter()
                .find(|(name, _)| name == "FnFunctionUsageMap")
                .unwrap()
                .1,
            external
        );
    }

    #[test]
    fn only_our_successful_native_write_advances_the_next_propertys_before_image() {
        let mut change = change();
        change.user = Some(vec![(6, 8)]);
        for native_takes_effect in [true, false] {
            let mut native = change.before_native.clone();
            let mut user = change.before_user.clone();
            let mut writes = Vec::new();
            let result = apply_checked(&change, |property, expected| {
                check_snapshot(&snapshot_with(&native, &user), expected)?;
                writes.push(property);
                match property {
                    MappingProperty::Native if native_takes_effect => {
                        native.clone_from(change.native.as_ref().unwrap());
                    }
                    MappingProperty::Native => {}
                    MappingProperty::User => user.clone_from(change.user.as_ref().unwrap()),
                }
                Ok(())
            });
            if native_takes_effect {
                assert_eq!(result, Ok(()));
                assert_eq!(writes, [MappingProperty::Native, MappingProperty::User]);
                assert_eq!(native, *change.native.as_ref().unwrap());
                assert_eq!(user, *change.user.as_ref().unwrap());
            } else {
                assert_eq!(result, Err(STALE.into()));
                assert_eq!(writes, [MappingProperty::Native]);
                assert_eq!(user, change.before_user);
            }
        }
    }

    #[test]
    fn composite_keyboard_conformance_is_required_at_the_write_boundary() {
        let mut records = snapshot();
        let PValue::Array(services) = &mut records else {
            unreachable!()
        };
        let PValue::Dict(service) = &mut services[0] else {
            unreachable!()
        };
        *service
            .iter_mut()
            .find(|(name, _)| name == "PrimaryUsage")
            .unwrap() = ("PrimaryUsage".into(), PValue::Int(2));
        service.push((
            "DeviceUsagePairs".into(),
            PValue::Array(vec![PValue::Dict(vec![
                ("DeviceUsagePage".into(), PValue::Int(1)),
                ("DeviceUsage".into(), PValue::Int(6)),
            ])]),
        ));
        assert_eq!(check_snapshot(&records, &change()), Ok(()));
        let PValue::Array(services) = &mut records else {
            unreachable!()
        };
        let PValue::Dict(service) = &mut services[0] else {
            unreachable!()
        };
        service
            .iter_mut()
            .find(|(name, _)| name == "DeviceUsagePairs")
            .unwrap()
            .1 = PValue::Array(vec![PValue::Dict(vec![
            ("DeviceUsagePage".into(), PValue::Int(1)),
            ("DeviceUsage".into(), PValue::Int(2)),
        ])]);
        assert!(
            check_snapshot(&records, &change()).is_err(),
            "a service that lost keyboard conformance is not written"
        );
    }

    #[test]
    fn snapshots_bind_identity_and_both_live_tables_before_writing() {
        let records = snapshot();
        let change = change();
        assert_eq!(check_snapshot(&records, &change), Ok(()));
        let historical = super::super::keyboard_prefix::tests::historical_fields();
        let mut historical_change = change.clone();
        historical_change.before_prefix =
            super::super::KeyboardPrefix::read(&PValue::Dict(historical.clone())).unwrap();
        assert_eq!(
            check_snapshot(
                &snapshot_with_prefix(&change.before_native, &change.before_user, &historical),
                &historical_change
            ),
            Ok(())
        );
        assert_eq!(
            check_snapshot(&records, &historical_change),
            Err(STALE.into()),
            "a schema change cannot reuse a historical before-image"
        );
        let mut stale = change.clone();
        stale.before_native.pop();
        assert_eq!(check_snapshot(&records, &stale), Err(STALE.into()));
        stale = change.clone();
        stale.before_user.clear();
        assert_eq!(check_snapshot(&records, &stale), Err(STALE.into()));
        stale = change.clone();
        stale.registry = 8;
        assert!(check_snapshot(&records, &stale).is_err());
        let PValue::Array(mut duplicate) = records else {
            unreachable!()
        };
        duplicate.push(duplicate[0].clone());
        assert!(check_snapshot(&PValue::Array(duplicate), &change).is_err());
        let PValue::Array(mut ambiguous) = snapshot() else {
            unreachable!()
        };
        let PValue::Dict(service) = &mut ambiguous[0] else {
            unreachable!()
        };
        let (_, PValue::Array(filters)) = service
            .iter_mut()
            .find(|(name, _)| name == "ServiceFilterDebug")
            .unwrap()
        else {
            unreachable!()
        };
        filters.push(filters[0].clone());
        assert!(check_snapshot(&PValue::Array(ambiguous), &change).is_err());
        for invalid in [
            crate::plist::document(&snapshot()).replace("IOHIDKeyboardFilter", "UnknownFilter"),
            crate::plist::document(&snapshot()).replace("<key>Dst</key>", "<key>Unknown</key>"),
            crate::plist::document(&snapshot()).replace(NATIVE, "UnknownMapping"),
        ] {
            let invalid = crate::plist::parse(invalid.as_bytes()).unwrap();
            assert!(check_snapshot(&invalid, &change).is_err());
        }
    }

    #[test]
    fn permission_preflight_never_asks_and_covers_every_changed_array() {
        assert!(
            check_access([false, false], || panic!(
                "modifier-only maps need no grant"
            ))
            .is_ok()
        );
        assert!(check_access([false, true], || 0).is_ok());
        assert!(check_access([false, true], || 1).is_err());
        assert!(check_access([true, false], || 2).is_err());
    }

    #[test]
    fn native_payloads_round_trip_and_classify_without_opening_a_hid_client() {
        let restricted_remapping = restricted_remapping();
        let caps = 0x7_0000_0039;
        let control = 0x7_0000_00e0;
        let modifier = mapping_value(&vec![(caps, control)]).unwrap();
        let PValue::Array(pairs) = value(modifier.0).unwrap() else {
            panic!("mapping payload must be an array")
        };
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            field(&pairs[0], "HIDKeyboardModifierMappingSrc"),
            Some(&PValue::Int(caps))
        );
        assert_eq!(
            field(&pairs[0], "HIDKeyboardModifierMappingDst"),
            Some(&PValue::Int(control))
        );
        // SAFETY: these are owned CFArray property lists; the predicate only examines
        // their pairs. This test creates no HID client and invokes no property setter.
        assert!(
            !mapping_restricted(&vec![(caps, control)], modifier.0, restricted_remapping).unwrap()
        );
        let alphanumeric = mapping_value(&vec![(0x7_0000_0004, control)]).unwrap();
        // SAFETY: alphanumeric owns the complete CFArray passed to the pure predicate.
        if let Some(predicate) = restricted_remapping {
            assert!(unsafe { predicate(alphanumeric.0) });
        } else {
            assert!(
                mapping_restricted(&vec![(0x7_0000_0004, control)], alphanumeric.0, None).is_err()
            );
        }
    }

    #[test]
    fn absent_private_predicate_keeps_basic_modifiers_available_and_other_keys_unavailable() {
        let caps = 0x7_0000_0039;
        let control = 0x7_0000_00e0;
        assert_eq!(
            mapping_restricted(&Vec::new(), ptr::null(), None),
            Ok(false)
        );
        let predicate = restricted_remapping();
        let basic = [
            caps,
            0x7_0000_00e0,
            0x7_0000_00e1,
            0x7_0000_00e2,
            0x7_0000_00e3,
            0x7_0000_00e4,
            0x7_0000_00e5,
            0x7_0000_00e6,
            0x7_0000_00e7,
        ];
        for source in basic {
            for destination in basic {
                let mapping = vec![(source, destination)];
                assert_eq!(mapping_restricted(&mapping, ptr::null(), None), Ok(false));
                if let Some(predicate) = predicate {
                    let payload = mapping_value(&mapping).unwrap();
                    // SAFETY: the predicate examines only this owned in-memory CFArray.
                    assert!(!unsafe { predicate(payload.0) });
                }
            }
        }
        for mapping in [
            vec![(0x7_0000_0004, control)],
            vec![(caps, 0x7_0000_0004)],
            vec![(0x8_0000_0039, control)],
            vec![(caps, -1)],
            vec![(caps, 0x7_0000_003a)],
        ] {
            assert!(mapping_restricted(&mapping, ptr::null(), None).is_err());
        }
    }

    #[test]
    fn only_the_target_user_in_the_current_console_login_can_activate() {
        assert_eq!(current_login(501, 501, 501), Ok(()));
        for (actor, console) in [(0, 501), (502, 501), (501, 502), (501, 0)] {
            assert!(current_login(501, actor, console).is_err());
        }
    }
}
