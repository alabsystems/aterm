// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::metal::test_support::{C_STRING_HOLES, ns_string_of_units};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn headless_listing_refuses_absent_and_ambiguous_devices_without_a_window_fallback() {
    let display = ListedGpu {
        low_power: false,
        headless: false,
    };
    for (gpus, expected) in [
        (vec![Some(display)], Some(0)),
        (vec![], None),
        (vec![None], None),
        (vec![Some(display), Some(display)], None),
    ] {
        let listed = || choose_listed(&gpus, true).map(|(i, _)| i);
        assert_eq!(
            crate::DevicePolicy::Headless.select(listed, || panic!("WindowServer fallback")),
            expected
        );
        let calls = std::cell::Cell::new(0);
        let windowed = crate::DevicePolicy::Windowed.select(listed, || {
            calls.set(calls.get() + 1);
            Some(99)
        });
        assert_eq!(windowed, expected.or(Some(99)));
        assert_eq!(calls.get(), usize::from(expected.is_none()));
    }
}

/// How many times this process has called [`Device::system_default_asks_window_server`].
pub(super) static WINDOW_SERVER_ASKS: AtomicUsize = AtomicUsize::new(0);

fn listing_read_independently() -> Option<Vec<(String, bool, bool)>> {
    // SAFETY: as in `Device::listing`: a +1 array released by `Obj`, +0 elements retained for the name while the array lives, `BOOL` getters through `ObjcBool`.
    unsafe {
        let all = Obj::from_owned(MTLCopyAllDevices())?;
        let count: unsafe extern "C-unwind" fn(Id, Sel) -> usize = checked_msg();
        let at: unsafe extern "C-unwind" fn(Id, Sel, usize) -> Id = checked_msg();
        let flag: unsafe extern "C-unwind" fn(Id, Sel) -> ObjcBool = checked_msg();
        let mut out = Vec::new();
        for i in 0..count(all.id(), sel(c"count")) {
            let d = at(all.id(), sel(c"objectAtIndexedSubscript:"), i);
            let Some(dev) = Obj::retain(d).map(Device) else {
                continue;
            };
            out.push((
                dev.name(),
                flag(d, sel(c"isLowPower")).as_bool(),
                flag(d, sel(c"isHeadless")).as_bool(),
            ));
        }
        Some(out)
    }
}

#[test]
fn preferred_asks_the_window_server_only_when_the_listing_cannot_decide() {
    let _pool = AutoreleasePool::new();
    let Some(gpus) = listing_read_independently() else {
        aterm_spec::host::could_not_run("no Metal device on this machine");
    };
    let want_high =
        aterm_types::dev_seam!("ATERM_GPU_POWER").is_some_and(|v| v.eq_ignore_ascii_case("high"));
    let display: Vec<&(String, bool, bool)> = gpus.iter().filter(|g| !g.2).collect();
    let expected = match display.iter().find(|g| g.1) {
        Some(low) if !want_high => Some(low.0.clone()),
        _ => (display.len() == 1).then(|| display[0].0.clone()),
    };

    let before = WINDOW_SERVER_ASKS.load(Ordering::SeqCst);
    let picked = Device::preferred().map(|d| d.name());
    let asked = WINDOW_SERVER_ASKS.load(Ordering::SeqCst) - before;
    crate::stderr_line!(
        "aterm-gpu: listing {gpus:?} (want_high={want_high}); preferred() = {picked:?}, \
         window-server asks {asked}"
    );
    match expected {
        Some(name) => {
            assert_eq!(
                asked, 0,
                "the listing names the GPU ({name}), so nothing may ask the display server"
            );
            assert_eq!(picked, Some(name), "and preferred() is that GPU");
        }
        None => assert!(
            asked >= 1,
            "the listing cannot decide here, so the choice is the display server's"
        ),
    }
}

#[test]
fn listing_decides_every_shape_it_can() {
    type Pick = Option<(usize, &'static str)>;
    let gpu = |low_power, headless| {
        Some(ListedGpu {
            low_power,
            headless,
        })
    };
    let apple = gpu(false, false);
    let integrated = gpu(true, false);
    let discrete = gpu(false, false);
    let compute = gpu(false, true);
    let only = "the only display GPU";
    #[rustfmt::skip]
    let rows: [(&str, Vec<Option<ListedGpu>>, Pick, Pick); 11] = [
        ("no Metal",                     vec![],                          None,                    None),
        ("a nil slot only",              vec![None],                      None,                    None),
        ("Apple silicon",                vec![apple],                     Some((0, only)),         Some((0, only))),
        ("nil slot, then Apple silicon", vec![None, apple],               Some((1, only)),         Some((1, only))),
        ("dual-GPU laptop",              vec![integrated, discrete],      Some((0, "low-power")),  None),
        ("dual-GPU, discrete first",     vec![discrete, integrated],      Some((1, "low-power")),  None),
        ("integrated only",              vec![integrated],                Some((0, "low-power")),  Some((0, only))),
        ("one display GPU + compute",    vec![discrete, compute],         Some((0, only)),         Some((0, only))),
        ("compute only",                 vec![compute],                   None,                    None),
        ("two discrete (Mac Pro)",       vec![discrete, discrete],        None,                    None),
        ("laptop + eGPU",                vec![integrated, discrete, discrete], Some((0, "low-power")), None),
    ];
    let mut wrong = Vec::new();
    for (shape, listing, default, high) in rows {
        for (policy, want_high, want) in [("default", false, default), ("high", true, high)] {
            let got = choose_listed(&listing, want_high);
            if got != want {
                wrong.push(format!("{shape}, {policy}: got {got:?}, want {want:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "choose_listed moved:\n{}",
        wrong.join("\n")
    );
}

#[test]
fn an_error_description_is_read_whole() {
    let _pool = AutoreleasePool::new();
    let key = ns_string("NSLocalizedDescription").expect("key");
    let domain = ns_string("aterm.test").expect("domain");
    let read = C_STRING_HOLES.map(|(units, _)| {
        let description = ns_string_of_units(units);
        // SAFETY: `+dictionaryWithObject:forKey:` (`@32@0:8@16@24`) and `+errorWithDomain:code:userInfo:` (`@40@0:8@16q24@32`) return autoreleased objects the pool above owns; every argument is a live object for the call, and the dictionary retains its value.
        let error = unsafe {
            let dict: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, Id) -> Id = checked_msg();
            let info = dict(
                class(c"NSDictionary"),
                sel(c"dictionaryWithObject:forKey:"),
                description.id(),
                key.id(),
            );
            let make: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, isize, Id) -> Id =
                checked_msg();
            make(
                class(c"NSError"),
                sel(c"errorWithDomain:code:userInfo:"),
                domain.id(),
                1,
                info,
            )
        };
        assert!(!error.is_null(), "NSError for {units:04X?}");
        ns_error_string(error)
    });
    assert_eq!(read, C_STRING_HOLES.map(|(_, want)| want));
}

#[test]
fn a_device_name_is_read_whole() {
    let _pool = AutoreleasePool::new();
    let read = C_STRING_HOLES.map(|(units, _)| {
        let name = ns_string_of_units(units);
        // SAFETY: `+notificationWithName:object:` (`@32@0:8@16@24`) returns an autoreleased NSNotification the pool above owns; `Obj::retain` takes it to +1, which the stand-in `Device` releases on drop.
        let stand_in = unsafe {
            let f: unsafe extern "C-unwind" fn(ClassPtr, Sel, Id, Id) -> Id = checked_msg();
            Obj::retain(f(
                class(c"NSNotification"),
                sel(c"notificationWithName:object:"),
                name.id(),
                Id::NIL,
            ))
        }
        .map(Device)
        .expect("NSNotification");
        stand_in.name()
    });
    assert_eq!(read, C_STRING_HOLES.map(|(_, want)| want));
}
