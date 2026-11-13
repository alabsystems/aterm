// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_passive_census_matches_live_keyboard_conformance_without_events() {
    // SAFETY: this Passive client reads service records only, registering no listener.
    let client =
        Owned::new(unsafe { IOHIDEventSystemClientCreateWithType(ptr::null(), 2, ptr::null()) })
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
    let external = super::super::keyboard_prefix::tests::pairs(&[(0x7_0000_003a, 0xc_0000_006f)]);
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
    assert!(!mapping_restricted(&vec![(caps, control)], modifier.0, restricted_remapping).unwrap());
    let alphanumeric = mapping_value(&vec![(0x7_0000_0004, control)]).unwrap();
    // SAFETY: alphanumeric owns the complete CFArray passed to the pure predicate.
    if let Some(predicate) = restricted_remapping {
        assert!(unsafe { predicate(alphanumeric.0) });
    } else {
        assert!(mapping_restricted(&vec![(0x7_0000_0004, control)], alphanumeric.0, None).is_err());
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
