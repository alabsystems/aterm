// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows power decoders on every host, and on Windows the readers held to calls they do
//! not make themselves, a power request made and released, and a listener registered and told
//! by hand (a real suspend cannot be asked for from a test).

use super::*;

fn status(ac_line: u8, battery_flag: u8, battery_percent: u8) -> SystemPowerStatus {
    SystemPowerStatus {
        ac_line,
        battery_flag,
        battery_percent,
        system_status: 0,
        battery_life_secs: u32::MAX,
        battery_full_life_secs: u32::MAX,
    }
}

#[test]
fn the_power_status_names_the_source_and_the_battery() {
    let desktop = reading_of(&status(1, BATTERY_NONE, STATUS_UNKNOWN)).expect("on AC");
    assert_eq!(desktop.source, PowerSource::Ac);
    assert_eq!(desktop.battery, None, "no system battery");
    assert_eq!(desktop.adapter_w, None);

    let charging = reading_of(&status(1, BATTERY_CHARGING, 61)).expect("on AC");
    assert_eq!(charging.source, PowerSource::Ac);
    assert_eq!(
        charging.battery,
        Some(BatteryReading {
            pct: 61,
            charging: true
        })
    );

    let draining = reading_of(&status(0, 1, 100)).expect("on battery");
    assert_eq!(draining.source, PowerSource::Battery);
    assert_eq!(
        draining.battery,
        Some(BatteryReading {
            pct: 100,
            charging: false
        })
    );
}

#[test]
fn an_unknown_status_is_no_reading_and_an_unknown_charge_no_battery() {
    assert_eq!(
        reading_of(&status(STATUS_UNKNOWN, 1, 50)),
        None,
        "Windows does not know the source"
    );
    assert_eq!(
        reading_of(&status(2, 1, 50)),
        None,
        "an undefined line word"
    );
    let unknown_flag = reading_of(&status(0, STATUS_UNKNOWN, 50)).expect("on battery");
    assert_eq!(
        unknown_flag.battery, None,
        "255 is unknown, not every bit set"
    );
    let unknown_charge = reading_of(&status(0, 2, STATUS_UNKNOWN)).expect("on battery");
    assert_eq!(unknown_charge.battery, None);
    let over = reading_of(&status(1, 0, 120)).expect("on AC");
    assert_eq!(over.battery.map(|b| b.pct), Some(100), "never past full");
}

#[test]
fn one_suspend_and_one_resume_word_name_an_event() {
    assert_eq!(event_of(PBT_APMSUSPEND), Some(PowerEvent::Suspend));
    assert_eq!(event_of(PBT_APMRESUMEAUTOMATIC), Some(PowerEvent::Resume));
    assert_eq!(
        event_of(PBT_APMRESUMESUSPEND),
        None,
        "the second word of a resume a person caused: heard once already"
    );
    for other in [0, 1, 0x000A, 0x8013, u32::MAX] {
        assert_eq!(event_of(other), None, "{other:#x}");
    }
}

#[test]
fn a_filetime_is_unix_milliseconds_and_a_zero_one_is_none() {
    // 2026-11-11T00:00:00Z.
    assert_eq!(
        filetime_unix_ms(134_388_288_000_000_000),
        Some(1_794_355_200_000)
    );
    assert_eq!(
        filetime_unix_ms(116_444_736_000_010_000),
        Some(1),
        "one ms past 1970"
    );
    assert_eq!(filetime_unix_ms(116_444_736_000_000_000), None, "1970");
    assert_eq!(filetime_unix_ms(0), None, "a zeroed answer");
    assert_eq!(filetime_unix_ms(-1), None);
    assert_eq!(interrupt_time(10_000_000), Duration::from_secs(1));
    assert_eq!(interrupt_time(1), Duration::from_nanos(100));
}

/// The system's two types hold the system and this process's execution, and on a PC with
/// Modern Standby the display too (Windows does not honour the system request alone there);
/// the display's type holds all three everywhere. NEGATIVE CONTROL: with no Modern Standby
/// the system's types leave the display alone.
#[test]
fn a_hold_maps_macos_assertion_types_to_power_requests() {
    let system = &[Request::System, Request::Execution][..];
    let with_display = &[Request::System, Request::Execution, Request::Display][..];
    for modern_standby in [false, true] {
        let want = if modern_standby { with_display } else { system };
        assert_eq!(
            requests_of(crate::power::PREVENT_SYSTEM_SLEEP, modern_standby),
            Some(want),
            "{modern_standby}"
        );
        assert_eq!(
            requests_of("PreventUserIdleSystemSleep", modern_standby),
            Some(want),
            "{modern_standby}"
        );
        assert_eq!(
            requests_of("PreventUserIdleDisplaySleep", modern_standby),
            Some(with_display),
            "{modern_standby}"
        );
        assert_eq!(requests_of("NoIdleSleepAssertion", modern_standby), None);
        assert_eq!(requests_of("", modern_standby), None);
    }
    assert_eq!(
        (
            Request::Display as i32,
            Request::System as i32,
            Request::Execution as i32
        ),
        (0, 1, 3),
        "POWER_REQUEST_TYPE's values"
    );
}

/// `AoAc` and `SystemS3` are read at their `winnt.h` offsets, each from its own byte.
#[test]
fn the_power_capabilities_name_modern_standby_and_s3() {
    let mut bytes = [0_u8; POWER_CAPABILITIES_LEN];
    assert!(!PowerCapabilities(bytes).aoac());
    assert!(!PowerCapabilities(bytes).s3());
    bytes[20] = 1;
    assert!(PowerCapabilities(bytes).aoac());
    assert!(!PowerCapabilities(bytes).s3());
    bytes = [0; POWER_CAPABILITIES_LEN];
    bytes[5] = 1;
    assert!(PowerCapabilities(bytes).s3());
    assert!(!PowerCapabilities(bytes).aoac());
    bytes = [1; POWER_CAPABILITIES_LEN];
    bytes[20] = 0;
    assert!(
        !PowerCapabilities(bytes).aoac(),
        "no neighbour's byte reads as it"
    );
}

#[cfg(not(windows))]
#[test]
fn off_windows_there_is_no_modern_standby_to_read() {
    assert_eq!(modern_standby(), None);
}

#[cfg(not(windows))]
#[test]
fn off_windows_no_sleep_was_heard() {
    assert_eq!(last_sleep(), (None, None));
}

#[cfg(windows)]
mod live {
    #![allow(
        non_snake_case,
        reason = "Win32 names are kept verbatim to check against the SDK"
    )]

    use super::*;
    use std::sync::{Arc, Mutex};

    /// `SYSTEM_BATTERY_STATE` (`winnt.h`), what `CallNtPowerInformation(SystemBatteryState)`
    /// fills: the power manager's own record, which `GetSystemPowerStatus` is not read from
    /// here.
    #[repr(C)]
    #[derive(Default)]
    struct SystemBatteryState {
        ac_on_line: u8,
        battery_present: u8,
        charging: u8,
        discharging: u8,
        spare: [u8; 3],
        tag: u8,
        max_capacity: u32,
        remaining_capacity: u32,
        rate: u32,
        estimated_time: u32,
        default_alert_1: u32,
        default_alert_2: u32,
    }
    const _: () = assert!(std::mem::size_of::<SystemBatteryState>() == 32);

    /// `POWER_INFORMATION_LEVEL::SystemBatteryState`.
    const SYSTEM_BATTERY_STATE: i32 = 5;

    #[link(name = "powrprof.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    unsafe extern "system" {
        fn CallNtPowerInformation(
            InformationLevel: i32,
            InputBuffer: *const std::ffi::c_void,
            InputBufferLength: u32,
            OutputBuffer: *mut std::ffi::c_void,
            OutputBufferLength: u32,
        ) -> i32;
    }

    #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    unsafe extern "system" {
        fn GetSystemPowerStatus(lpSystemPowerStatus: *mut SystemPowerStatus) -> i32;
        fn GetTickCount64() -> u64;
    }

    fn battery_state() -> SystemBatteryState {
        let mut state = SystemBatteryState::default();
        // SAFETY: no input, and a writable output of the size passed.
        let status = unsafe {
            CallNtPowerInformation(
                SYSTEM_BATTERY_STATE,
                std::ptr::null(),
                0,
                (&raw mut state).cast(),
                std::mem::size_of::<SystemBatteryState>() as u32,
            )
        };
        assert_eq!(status, 0, "CallNtPowerInformation(SystemBatteryState)");
        state
    }

    fn unix_ms_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_millis() as i64
    }

    /// The reading names a source, the one `GetSystemPowerStatus` names when this test asks it
    /// itself, and the power manager's battery state agrees on the line and on whether there is
    /// a battery to read.
    #[test]
    fn this_machine_reads_its_power_source() {
        let reading = power_reading().expect("a power source");
        assert_eq!(crate::situation::power_sources(), Some(reading));
        let mut status = SystemPowerStatus::default();
        // SAFETY: one writable `SYSTEM_POWER_STATUS` that outlives the call.
        assert_ne!(unsafe { GetSystemPowerStatus(&raw mut status) }, 0);
        let want = match status.ac_line {
            0 => PowerSource::Battery,
            1 => PowerSource::Ac,
            other => panic!("ACLineStatus {other}: no reading should have come"),
        };
        assert_eq!(reading.source, want, "{status:?}");
        let state = battery_state();
        assert_eq!(
            reading.source == PowerSource::Ac,
            state.ac_on_line != 0,
            "the power manager's own line state"
        );
        if state.battery_present == 0 {
            assert_eq!(reading.battery, None, "no battery present");
        }
        if let Some(battery) = reading.battery {
            assert!(battery.pct <= 100, "{battery:?}");
            assert_ne!(state.battery_present, 0, "{battery:?}");
        }
        assert_eq!(reading.adapter_w, None, "Windows reports no adapter rating");
    }

    /// The situation's power watch reads at its first reading and every one after, and names
    /// no switch of the source while there was none.
    #[test]
    fn the_power_watch_reads_at_every_reading() {
        const { assert!(crate::situation::POWER_READER) };
        let mut watch = crate::situation::PowerWatch::new();
        let first = watch.read(1_000).expect("read at the first reading");
        let second = watch.read(2_000).expect("and at the next");
        assert_eq!(first.reading.source, second.reading.source);
        assert_eq!(first.changed_ms, None, "no switch seen yet");
    }

    /// A request is made for each type the callers name and released at the drop; a type that
    /// maps to no request and a name with a NUL hold nothing. Whether the power manager lists
    /// it (`powercfg /requests`) needs an elevated prompt, so only the calls' answers are held.
    #[test]
    fn a_power_request_is_held_and_released() {
        let name = format!("aterm-sysprobe power request test {}", std::process::id());
        for kind in [
            crate::power::PREVENT_SYSTEM_SLEEP,
            "PreventUserIdleSystemSleep",
            "PreventUserIdleDisplaySleep",
        ] {
            let held = hold(kind, &name).unwrap_or_else(|| panic!("{kind}: refused"));
            drop(held);
            let through_the_door = crate::power::hold(kind, &name);
            assert!(through_the_door.is_some(), "{kind}");
        }
        assert!(hold("NoSuchAssertion", &name).is_none());
        assert!(crate::power::hold("NoSuchAssertion", &name).is_none());
        assert!(hold(crate::power::PREVENT_SYSTEM_SLEEP, "a\0b").is_none());
    }

    /// The power manager names its capabilities, and the reading agrees with itself as
    /// Windows' rule has it: a PC with Modern Standby has no S3 (`powercfg /a`: "disabled when
    /// S0 low power idle is supported"). [`modern_standby`] reads the same, once.
    #[test]
    fn the_power_capabilities_read_and_agree_with_themselves() {
        let caps = power_capabilities().expect("the power capabilities");
        if caps.aoac() {
            assert!(!caps.s3(), "{caps:?}");
        }
        assert_eq!(modern_standby(), Some(caps.aoac()));
        assert_eq!(power_capabilities(), Some(caps), "one machine, one answer");
    }

    /// The kernel's boot time is a time before now, the same at every read, and what the
    /// timeline names; the clock that stops in a suspend never runs past the one that does not.
    #[test]
    fn the_boot_time_and_the_unbiased_clock_read() {
        let boot = boot_unix_ms().expect("the kernel's boot time");
        let now = unix_ms_now();
        // 2015-01-01: before any Windows this runs on was installed.
        assert!((1_420_070_400_000..=now).contains(&boot), "{boot} {now}");
        assert_eq!(boot_unix_ms(), Some(boot), "one boot, one time");
        let timeline = crate::timeline::timeline().expect("a timeline");
        assert_eq!(timeline.boot_ms, boot);
        assert_eq!(timeline.wake_reason, None);
        assert_eq!(crate::timeline::waketime_ms(), timeline.wake_ms);

        let first = unbiased_time().expect("the unbiased interrupt time");
        let second = unbiased_time().expect("again");
        assert!(second >= first, "{first:?} {second:?}");
        // SAFETY: takes nothing, returns a count.
        let ticks = Duration::from_millis(unsafe { GetTickCount64() });
        assert!(
            second <= ticks + Duration::from_secs(1),
            "awake time {second:?} past the time since boot {ticks:?}"
        );
    }

    /// The registration is taken, and what its callback hears reaches the handler: one event
    /// per suspend and per resume word, none for any other, a handler's panic caught. Told by
    /// hand, an event is no note of a real sleep.
    #[test]
    fn a_listener_registers_and_hears_what_its_callback_is_told() {
        let before = last_sleep();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let listening = listen(Box::new(move |event| {
            sink.lock().expect("unpoisoned").push(event);
        }))
        .expect("powrprof takes a suspend and resume registration");
        assert!(
            heard.lock().expect("unpoisoned").is_empty(),
            "nothing announced at the registration"
        );
        listening.deliver(PBT_APMSUSPEND);
        listening.deliver(PBT_APMRESUMEAUTOMATIC);
        listening.deliver(PBT_APMRESUMESUSPEND);
        listening.deliver(0x8013);
        assert_eq!(
            *heard.lock().expect("unpoisoned"),
            [PowerEvent::Suspend, PowerEvent::Resume]
        );
        assert_eq!(last_sleep(), before, "a hand's telling is no sleep");
        drop(listening);

        let loud = listen(Box::new(|_| panic!("a handler that fails"))).expect("registered");
        loud.deliver(PBT_APMSUSPEND);
        drop(loud);
    }
}
