// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Stat};

const GIB: u64 = 1 << 30;

/// One look at a 48 GiB, 14-core Mac: every reading present, each class distinct from every
/// other so a figure read from the wrong one shows.
fn full_reading() -> MachineReading {
    MachineReading {
        cores: Some(14),
        load: Some([2.5, 3.0, 4.004]),
        memory: Some(aterm_sysprobe::MemoryDetail {
            total: 48 * GIB,
            available: 7 * GIB,
            free: GIB,
            speculative: Some(GIB / 4),
            active: Some(20 * GIB),
            inactive: Some(10 * GIB),
            wired: 4 * GIB,
            compressed: Some(3 * GIB),
            stored: Some(9 * GIB),
            purgeable: Some(GIB / 2),
            file_backed: 5 * GIB,
            app: 18 * GIB,
            shared: Some(GIB),
            swap_used: GIB / 2,
            swap_total: 2 * GIB,
            pressure: Some(aterm_sysprobe::MemoryLevel::Warn),
            exact: true,
            ..aterm_sysprobe::MemoryDetail::default()
        }),
        gpu: Some(aterm_sysprobe::GpuReading {
            device_pct: Some(37),
            cores: Some(40),
            name: Some(" Apple M5 Max ".into()),
            ..aterm_sysprobe::GpuReading::default()
        }),
        volume: Some(aterm_sysprobe::volumes::Reading {
            mount: "/System/Volumes/Data".into(),
            key: aterm_sysprobe::volumes::VolumeKey::Container("disk3".into()),
            total: 1000 * GIB,
            free: 200 * GIB,
            files_free: None,
            dev: 1,
            read_only: false,
        }),
    }
}

fn owner() -> crate::model::User {
    crate::model::User {
        name: "owner".into(),
        uid: 501,
        gid: 20,
        home: "/Users//owner".into(),
        shell: "/bin/zsh".into(),
        admin: true,
    }
}

#[test]
fn gather_vitals_maps_every_reading_to_the_figure_it_owns() {
    let mut sys = Fake::root();
    sys.machine = Some(full_reading());
    sys.logged_in(&owner());
    let facts = HostFacts {
        boot_time: 1_800_000_000 - 4 * 86_400 - 11 * 3_600,
        ..HostFacts::default()
    };
    let v = gather_vitals(&sys, &facts).expect("a machine read");
    assert_eq!(
        v,
        HostVitals {
            cpu_cores: Some(14),
            load_avg_100: Some([250, 300, 400]),
            gpu_name: Some("Apple M5 Max".into()),
            gpu_cores: Some(40),
            gpu_busy_pct: Some(37),
            mem_total_bytes: Some(48 * GIB),
            mem_available_bytes: Some(7 * GIB),
            mem_used_bytes: Some(26 * GIB),
            mem_wired_bytes: Some(4 * GIB),
            mem_compressed_bytes: Some(3 * GIB),
            swap_used_bytes: Some(GIB / 2),
            mem_pressure: Some("warn".into()),
            disk_total_bytes: Some(1000 * GIB),
            disk_free_bytes: Some(200 * GIB),
            uptime_secs: Some(4 * 86_400 + 11 * 3_600),
            rtt_us: None,
        }
    );
    assert_eq!(v.disk_used(), Some(800 * GIB));
    assert_eq!(
        sys.machine_homes(),
        [Some(PathBuf::from("/Users//owner"))],
        "the volume is the one holding the logged-in account's home, not the root's"
    );
    assert!(
        sys.calls().is_empty(),
        "no child process: {:?}",
        sys.calls()
    );
    let wire = aterm_json::to_string(&v).unwrap();
    assert_eq!(aterm_json::from_str::<HostVitals>(&wire).unwrap(), v);
}

/// With no account home, or one that is not an absolute path, the machine is read for no
/// volume, and the disk figures are absent while the rest are read.
#[test]
fn without_an_absolute_home_no_volume_is_read() {
    let relative = crate::model::User {
        home: "owner".into(),
        ..owner()
    };
    for login in [None, Some(relative)] {
        let mut sys = Fake::root();
        sys.machine = Some(full_reading());
        if let Some(user) = &login {
            sys.logged_in(user);
        }
        let v = gather_vitals(&sys, &HostFacts::default()).expect("a machine read");
        assert_eq!(sys.machine_homes(), [None], "{login:?}");
        assert_eq!(
            (v.disk_total_bytes, v.disk_free_bytes),
            (None, None),
            "{login:?}"
        );
        assert_eq!(v.cpu_cores, Some(14), "{login:?}");
    }
}

#[test]
fn a_reading_that_is_absent_leaves_its_figures_absent() {
    let bare = MachineReading {
        cores: Some(14),
        ..MachineReading::default()
    };
    assert_eq!(
        vitals_of(&bare, None),
        HostVitals {
            cpu_cores: Some(14),
            ..HostVitals::default()
        }
    );
    let unread_cores = MachineReading {
        cores: Some(0),
        ..full_reading()
    };
    assert_eq!(
        vitals_of(&unread_cores, None).cpu_cores,
        None,
        "a layout that reads no core says nothing, not one core"
    );
    assert_eq!(
        gather_vitals(&Fake::root(), &HostFacts::default()),
        None,
        "a system that reads no machine has no vitals"
    );
    let mut unbooted = Fake::root();
    unbooted.machine = Some(full_reading());
    assert_eq!(
        gather_vitals(&unbooted, &HostFacts::default())
            .unwrap()
            .uptime_secs,
        None
    );
}

/// LAW: available memory is the memory classes' own figure (`MemoryDetail::available`, which
/// `sys mem` and admission read), never the free, inactive and purgeable pages' sum; memory
/// used is `MemoryDetail::used`, app (shared included), wired and compressed, at most the
/// total, never the active queue. CONTROL: on this reading each rejected formula gives
/// another figure, so a mapping that used it fails.
#[test]
fn available_and_used_memory_each_have_one_definition() {
    let r = full_reading();
    let v = vitals_of(&r, None);
    let m = r.memory.unwrap();
    let overstated = m.free + m.inactive.unwrap() + m.purgeable.unwrap();
    assert_eq!(v.mem_available_bytes, Some(m.available));
    assert_ne!(
        v.mem_available_bytes,
        Some(overstated),
        "CONTROL: free + inactive + purgeable overstates"
    );
    assert_eq!(
        v.mem_used_bytes,
        Some(m.app + m.shared.unwrap() + m.wired + 3 * GIB)
    );
    assert_ne!(
        v.mem_used_bytes,
        Some(m.active.unwrap() + m.wired + 3 * GIB),
        "CONTROL: the active queue is not memory used"
    );
    let mut over = r.clone();
    if let Some(memory) = over.memory.as_mut() {
        memory.total = 20 * GIB;
    }
    assert_eq!(vitals_of(&over, None).mem_used_bytes, Some(20 * GIB));
}

const VM_HIDUTIL: &str = "Services:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID       Transport                     Class                    Product              UserClass               Built-In \n\
0x5ac    0x1       0x0        1         6     0x7c485324000001 AppleVirtualPlatformHIDBridge (null)                   Virtual Keyboard     (null)                  1        \n\
0x5ac    0x8105    0x10900000 1         6     0x100000335      USB                           AppleUserHIDEventService Virtual USB Keyboard AppleUserHIDEventDriver 0        \n\
\n\
Devices:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class              Product              UserClass                 Built-In \n\
0x5ac    0x8105    0x10900000 1         6     0x100000313 USB       AppleUserHIDDevice Virtual USB Keyboard AppleUserUSBHostHIDDevice 0        \n\
\n\
";

const LAPTOP_HIDUTIL: &str = "Services:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class                         Product                            UserClass Built-In \n\
0x0      0x0       0xe9       1         6     0x100000b63 FIFO      AppleHIDKeyboardEventDriverV2 Apple Internal Keyboard / Trackpad (null)    1        \n\
\n\
Devices:\n\
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class                      Product                            UserClass Built-In \n\
0x0      0x0       0xe9       1         6     0x100000b51 FIFO      AppleHIDTransportHIDDevice Apple Internal Keyboard / Trackpad (null)    1        \n\
\n\
";

const VM_IOREG: &str = r##"+-o VMA2MACOSAP  <class IOPlatformExpertDevice, id 0x10000012f, registered, matched, active, busy 0 (9281 ms), retain 28>
    {
      "#address-cells" = <02000000>
      "serial-number" = <5a4e59564e54524a4e3500000000000000000000000000000000000000000000>
      "IOBusyInterest" = "IOCommand is not serializable"
      "compatible" = <"VMA2MACOSAP","VirtualMac2,1","AppleVirtualPlatformARM">
      "IOPlatformSerialNumber" = "ZNYVNTRJN5"
      "model" = <"VirtualMac2,1">
      "mlb-serial-number" = <0000000000000000000000000000000000000000000000000000000000000000>
      "IOPlatformUUID" = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"
    }
"##;

const VM_DSCL_LIST: &str = "_amavisd                 83
_analyticsd              263
_oahd                    441
_xserverdocs             251
admin                    501
daemon                   1
nobody                   -2
root                     0
";

const VM_DSCL_ADMIN: &str = "NFSHomeDirectory: /Users//admin
PrimaryGroupID: 20
UniqueID: 501
UserShell: /bin/zsh
";

const VM_ADMIN_GROUP: &str = "GroupMembership: root _mbsetupuser admin\n";

const VM_IDLE: &str = r#"+-o IOHIDSystem  <class IOHIDSystem, id 0x1000001a2, registered, matched, active, busy 0 (1268 ms), retain 12>
    {
      "IOClass" = "IOHIDSystem"
      "HIDScrollCountMaxTimeDeltaBetween" = 600
      "HIDIdleTime" = 1748058687083
      "HIDScrollCountIgnoreMomentumScrolls" = Yes
    }
"#;

fn vm() -> Fake {
    let sys = Fake::root();
    sys.on(
        "/usr/sbin/scutil --get ComputerName",
        Ran::ok("Managed\u{2019}s Virtual Machine\n"),
    )
    .on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("Manageds-Virtual-Machine\n"),
    )
    .on(
        "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
        Ran::ok(VM_IOREG),
    )
    .on("/usr/sbin/sysctl -n hw.model", Ran::ok("VirtualMac2,1\n"))
    .on(
        &hardware::product_cmd().display(),
        Ran::ok(crate::hardware::tests::VM_PRODUCT),
    )
    .on(
        "/usr/sbin/sysctl -n machdep.cpu.brand_string",
        Ran::ok("Apple M5 (Virtual)\n"),
    )
    .on("/usr/sbin/sysctl -n hw.memsize", Ran::ok("8589934592\n"))
    .on(
        &hardware::disk_cmd().display(),
        Ran::ok(crate::hardware::tests::VM_DISKS),
    )
    .on("/usr/bin/uname -m", Ran::ok("arm64\n"))
    .on("/usr/bin/sw_vers -productVersion", Ran::ok("27.0\n"))
    .on("/usr/bin/sw_vers -buildVersion", Ran::ok("26A428\n"))
    .on(
        "/usr/sbin/sysctl -n kern.boottime",
        Ran::ok("{ sec = 1792190170, usec = 411172 } Fri Oct 16 22:36:10 2026\n"),
    )
    .on(
        "/usr/sbin/ioreg -r -d 1 -k HIDIdleTime -c IOHIDSystem",
        Ran::ok(VM_IDLE),
    )
    .on(
        "/usr/bin/fdesetup isactive",
        Ran {
            status: Some(1),
            stdout: b"false\n".to_vec(),
            stderr: Vec::new(),
        },
    )
    .on(
        "/usr/bin/dscl . -list /Users UniqueID",
        Ran::ok(VM_DSCL_LIST),
    )
    .on(
        "/usr/bin/dscl . -read /Groups/admin GroupMembership",
        Ran::ok(VM_ADMIN_GROUP),
    )
    .on(
        "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
        Ran::ok(VM_DSCL_ADMIN),
    )
    .on(&hidutil_keyboards().display(), Ran::ok(VM_HIDUTIL))
    .file(SYSTEM_TCC_DB, b"SQLite format 3\0")
    .file(
        "/c/0b1f8c6a2d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeef",
        b"x",
    )
    .file("/c/.partial.aterm-machines-77", b"x")
    .stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 0,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    sys
}

#[test]
fn the_vm_facts_come_from_the_tools_real_answers() {
    let f = gather_with_cache(&vm(), Path::new("/c"));
    assert_eq!(f.computer_name, "Managed\u{2019}s Virtual Machine");
    assert_eq!(f.local_host_name, "Manageds-Virtual-Machine");
    assert_eq!(f.uuid, "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43");
    assert_eq!(f.serial, "ZNYVNTRJN5");
    assert_eq!(f.model, "VirtualMac2,1");
    assert_eq!(f.model_name, "Virtual Machine");
    assert_eq!(f.chip, "Apple M5 (Virtual)");
    assert_eq!(f.processor_speed, "");
    assert_eq!(f.memory_bytes, 8_589_934_592);
    assert_eq!((f.disk_bytes, f.disk_rotational), (90_000_003_072, false));
    assert_eq!(f.arch, "arm64");
    assert_eq!(f.os_version, "27.0");
    assert_eq!(f.os_build, "26A428");
    assert_eq!(f.form, Form::Virtual);
    assert_eq!(f.filevault, Some(false));
    assert_eq!(f.boot_time, 1_792_190_170);
    assert_eq!(f.idle_secs, Some(1748));
    assert_eq!(f.remote_full_disk_access, Some(true));
    assert_eq!(f.console_user.as_deref(), Some("admin"));
    assert_eq!(
        f.users,
        vec![User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }]
    );
    assert_eq!(f.keyboards.len(), 2);
    assert_eq!(
        f.cached,
        vec!["0b1f8c6a2d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeef".to_owned()]
    );
    assert_eq!(f.agent_version, aterm_stamp::app_version());
}

#[test]
fn facts_without_keyboards_never_search_live_devices() {
    let sys = vm();
    let f = gather_with_cache_without_keyboards(&sys, Path::new("/c"));
    assert!(f.keyboards.is_empty());
    assert!(
        !sys.calls().iter().any(|call| call.contains("hidutil")),
        "calls: {:?}",
        sys.calls()
    );
    assert_eq!(inventory_keyboards(&sys).len(), 2);
}

#[test]
fn the_laptops_hardware_comes_from_sysctl_and_ioreg_never_the_slow_tools() {
    let sys = Fake::root();
    sys.on("/usr/sbin/sysctl -n hw.model", Ran::ok("Mac17,4\n"))
        .on(
            &hardware::product_cmd().display(),
            Ran::ok(crate::hardware::tests::AIR_PRODUCT),
        )
        .on(
            "/usr/sbin/sysctl -n machdep.cpu.brand_string",
            Ran::ok("Apple M5\n"),
        )
        .on("/usr/sbin/sysctl -n hw.memsize", Ran::ok("34359738368\n"))
        .on(
            &hardware::disk_cmd().display(),
            Ran::ok(crate::hardware::tests::AIR_DISKS),
        );
    let f = gather_with_cache(&sys, Path::new("/c"));
    assert_eq!(
        crate::hardware::describe(&f).unwrap().spaced(),
        "MacBook Air M5 32GB 4TB"
    );
    assert!(
        !sys.calls().iter().any(|c| c.contains("system_profiler")
            || c.contains("diskutil")
            || c.contains("/bin/df")),
        "{:?}",
        sys.calls()
    );
}

#[test]
fn a_fact_no_tool_answered_stays_empty_rather_than_guessed() {
    let f = gather_with_cache(&Fake::root(), Path::new("/c"));
    assert_eq!(f.uuid, "");
    assert_eq!(f.computer_name, "");
    assert_eq!(f.form, Form::Unknown);
    assert_eq!((f.model_name.as_str(), f.chip.as_str()), ("", ""));
    assert_eq!((f.memory_bytes, f.disk_bytes), (0, 0));
    assert_eq!(f.filevault, None);
    assert_eq!(f.boot_time, 0);
    assert_eq!(f.idle_secs, None);
    assert_eq!(f.remote_full_disk_access, None);
    assert_eq!(f.console_user.as_deref(), Some(CONSOLE_UNKNOWN));
    assert!(f.users.is_empty() && f.keyboards.is_empty() && f.cached.is_empty());
}

#[test]
fn a_console_owner_no_account_names_is_someone_never_nobody_and_never_a_user() {
    let sys = Fake::root();
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 501,
            gid: 0,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    let f = gather_with_cache(&sys, Path::new("/c"));
    assert_eq!(f.console_user.as_deref(), Some("(uid 501)"));
    for not_a_name in ["(uid 501)", CONSOLE_UNKNOWN] {
        assert!(user(&sys, not_a_name).is_err(), "{not_a_name}");
    }
}

#[test]
fn the_vm_keyboards_are_read_from_services_once_each_with_the_built_in_one_marked() {
    let k = parse_hidutil_keyboards(VM_HIDUTIL);
    assert_eq!(
        k,
        vec![
            Keyboard {
                vendor: 0x5ac,
                product: 0x1,
                name: "Virtual Keyboard".into(),
                builtin: true,
            },
            Keyboard {
                vendor: 0x5ac,
                product: 0x8105,
                name: "Virtual USB Keyboard".into(),
                builtin: false,
            },
        ]
    );
    let malformed_id = VM_HIDUTIL.replacen("0x5ac", "bogus", 1);
    assert_eq!(parse_hidutil_keyboards(&malformed_id).len(), 1);
    assert_eq!(
        parse_hidutil_keyboards_complete(VM_HIDUTIL)
            .unwrap()
            .keyboards,
        k
    );
    assert_eq!(
        parse_hidutil_keyboards_complete(LAPTOP_HIDUTIL)
            .unwrap()
            .keyboards,
        parse_hidutil_keyboards(LAPTOP_HIDUTIL)
    );
    for empty in ["Services:\n", "Services:\n\nDevices:\n"] {
        assert!(
            parse_hidutil_keyboards_complete(empty)
                .unwrap()
                .keyboards
                .is_empty()
        );
    }
    let partial = VM_HIDUTIL.replacen("Devices:", "unparsed-service-row\nDevices:", 1);
    for invalid in [
        "Services:\nUnknownHeader\nnonempty unparsed row\n",
        "Services:\n\nServices:\n",
        "Devices:\n",
        "Services:\nVendorID ProductID Product Built-In\n0x1      0x2       kbd     x\n",
        malformed_id.as_str(),
        partial.as_str(),
    ] {
        assert!(
            parse_hidutil_keyboards_complete(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn a_keyboard_census_covers_native_services_without_adopting_device_rows() {
    let census = || parse_hidutil_keyboards_complete(VM_HIDUTIL).unwrap();
    let (_, cohorts) = census().into_parts(Some(&[0x007c_4853_2400_0001])).unwrap();
    assert!(
        cohorts
            .expected(0x5ac, 1)
            .unwrap()
            .covers(&[0x007c_4853_2400_0001])
    );
    assert!(
        !cohorts
            .expected(0x5ac, 0x8105)
            .unwrap()
            .covers(&[0x1_0000_0335])
    );
    assert!(cohorts.expected(0xffff, 0xffff).is_err());
    assert!(
        census()
            .into_parts(Some(&[0x1_0000_0335, 0x1_0000_0335]))
            .is_err()
    );
    assert_eq!(
        census()
            .into_keyboards(&[0x007c_4853_2400_0001, 0x1_0000_0335])
            .unwrap(),
        parse_hidutil_keyboards(VM_HIDUTIL)
    );
    assert!(census().into_keyboards(&[41]).is_err());
    assert!(census().into_keyboards(&[0x1_0000_0313]).is_err());
    assert!(
        parse_hidutil_keyboards_complete("Services:\n\nDevices:\n")
            .unwrap()
            .into_keyboards(&[])
            .unwrap()
            .is_empty()
    );
    let duplicate = VM_HIDUTIL.replacen("0x100000335", "0x7c485324000001", 1);
    let unknown = VM_HIDUTIL.replacen("0x100000335", "not-an-id", 1);
    for invalid in [duplicate, unknown] {
        assert!(parse_hidutil_keyboards_complete(&invalid).is_err());
    }
}

#[test]
fn an_apple_silicon_laptop_keyboard_is_0_0_and_built_in() {
    let k = parse_hidutil_keyboards(LAPTOP_HIDUTIL);
    assert_eq!(
        k,
        vec![Keyboard {
            vendor: 0,
            product: 0,
            name: "Apple Internal Keyboard / Trackpad".into(),
            builtin: true,
        }]
    );
}

#[test]
fn a_keyboard_listed_only_under_devices_is_not_a_service_and_is_skipped() {
    let only_devices = VM_HIDUTIL.split_once("Devices:").unwrap().1;
    let text = format!("Services:\n\nDevices:{only_devices}");
    assert!(parse_hidutil_keyboards(&text).is_empty());
}

#[test]
fn a_table_without_section_headers_is_read_as_services() {
    let services = VM_HIDUTIL
        .split_once("Services:\n")
        .unwrap()
        .1
        .split_once("\n\n")
        .unwrap()
        .0;
    assert_eq!(parse_hidutil_keyboards(services).len(), 2);
}

#[test]
fn a_product_name_with_non_ascii_text_does_not_break_the_ids() {
    let text = "Services:
VendorID ProductID LocationID UsagePage Usage RegistryID  Transport            Class   Product          UserClass Built-In
0x4d9    0x2a7     0x1        1         6     0x100000aaa Bluetooth Low Energy Driver   Clavier \u{e9}t\u{e9}     (null)    0
";
    let k = parse_hidutil_keyboards(text);
    assert_eq!(k.len(), 1);
    assert_eq!(
        (k[0].vendor, k[0].product, k[0].builtin),
        (0x4d9, 0x2a7, false)
    );
    assert!(k[0].name.starts_with("Clavier"), "{:?}", k[0].name);
}

#[test]
fn dscl_values_on_continuation_lines_and_missing_keys_are_read_as_dscl_prints_them() {
    let attrs = parse_dscl_read("No such key: NoSuchKey\nRealName:\n Managed via Tart\n");
    assert_eq!(attrs, vec![("RealName".into(), "Managed via Tart".into())]);
    let attrs = parse_dscl_read(VM_DSCL_ADMIN);
    assert_eq!(attrs.len(), 4);
    assert_eq!(attrs[0], ("NFSHomeDirectory".into(), "/Users//admin".into()));
}

#[test]
fn the_account_list_keeps_negative_uids_so_nobody_is_not_mistaken_for_a_person() {
    let rows = parse_dscl_list(VM_DSCL_LIST);
    assert!(rows.contains(&("nobody".into(), -2)));
    assert!(rows.contains(&("admin".into(), 501)));
    assert_eq!(rows.len(), 8);
}

#[test]
fn a_missing_account_is_named_and_an_unreadable_admin_group_is_not_guessed() {
    let sys = Fake::root();
    sys.on(
        "/usr/bin/dscl . -read /Groups/admin GroupMembership",
        Ran::ok(VM_ADMIN_GROUP),
    )
    .on(
        "/usr/bin/dscl . -read /Users//nosuchuser UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
        Ran::exit(56, "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)"),
    );
    assert_eq!(
        user(&sys, "nosuchuser").unwrap_err(),
        "no account nosuchuser"
    );
    let err = user(&Fake::root(), "admin").unwrap_err();
    assert!(err.contains("/Groups/admin"), "{err}");
}

#[test]
fn one_account_resolves_with_its_admin_bit() {
    let u = user(&vm(), "admin").unwrap();
    assert_eq!((u.uid, u.gid, u.admin), (501, 20, true));
    assert_eq!(u.home, "/Users//admin");
}

#[test]
fn an_account_name_that_would_change_the_record_path_is_refused_before_dscl_runs() {
    let sys = Fake::root();
    for bad in ["", "../Groups/admin", "-x", "a b"] {
        assert!(user(&sys, bad).is_err());
    }
    assert!(sys.calls().is_empty());
}

#[test]
fn nobody_at_the_console_is_root_owning_it() {
    let sys = vm();
    sys.stat_of(
        "/dev/console",
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o600,
            dir: false,
            symlink: false,
            len: 0,
        },
    );
    let f = gather_with_cache(&sys, Path::new("/c"));
    assert_eq!(f.console_user, None);
}

#[test]
fn a_laptop_is_known_by_its_internal_battery_and_a_desktop_by_its_absence() {
    let laptop = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=36044899)\t80%; AC attached; not charging present: true\n";
    let sys = Fake::root();
    sys.on("/usr/bin/pmset -g batt", Ran::ok(laptop));
    assert_eq!(form(&sys, "Mac17,4"), Form::Laptop);
    let sys = Fake::root();
    sys.on(
        "/usr/bin/pmset -g batt",
        Ran::ok("Now drawing from 'AC Power'\n"),
    );
    assert_eq!(form(&sys, "Mac16,10"), Form::Desktop);
    assert_eq!(form(&Fake::root(), "Mac16,10"), Form::Unknown);
}

#[test]
fn filevault_is_read_from_the_exit_status_and_its_word_together() {
    let sys = Fake::root();
    sys.on("/usr/bin/fdesetup isactive", Ran::ok("true\n"));
    assert_eq!(filevault(&sys), Some(true));
    let sys = Fake::root();
    sys.on("/usr/bin/fdesetup isactive", Ran::exit(2, "error"));
    assert_eq!(filevault(&sys), None);
}

struct TccRefuses {
    inner: Fake,
    errno: i32,
}

impl System for TccRefuses {
    fn scratch_dir(
        &self,
        parent: Option<&Path>,
        label: &str,
    ) -> std::io::Result<crate::port::Scratch<'_>> {
        self.inner.scratch_dir(parent, label)
    }

    fn run(&self, cmd: &Cmd) -> Result<Ran, crate::port::RunFailed> {
        self.inner.run(cmd)
    }
    fn run_tty(
        &self,
        cmd: &Cmd,
        before: &mut dyn FnMut(&mut Cmd) -> Result<(), crate::port::RunFailed>,
    ) -> Result<Option<i32>, crate::port::RunFailed> {
        self.inner.run_tty(cmd, before)
    }
    fn read_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if path == Path::new(SYSTEM_TCC_DB) {
            return Err(std::io::Error::from_raw_os_error(self.errno));
        }
        self.inner.read_file(path)
    }
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
        self.inner.write_file(path, bytes, mode)
    }
    fn write_user_file(
        &self,
        user: &User,
        path: &Path,
        bytes: &[u8],
        mode: u32,
    ) -> std::io::Result<()> {
        self.inner.write_user_file(user, path, bytes, mode)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove(path)
    }
    fn create_dir_all(&self, path: &Path, mode: u32) -> std::io::Result<()> {
        self.inner.create_dir_all(path, mode)
    }
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> std::io::Result<()> {
        self.inner.chown(path, uid, gid)
    }
    fn stat(&self, path: &Path) -> std::io::Result<Stat> {
        self.inner.stat(path)
    }
    fn list_dir(&self, path: &Path) -> std::io::Result<Vec<String>> {
        self.inner.list_dir(path)
    }
    fn sha256(&self, path: &Path) -> std::io::Result<String> {
        self.inner.sha256(path)
    }
    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> std::io::Result<()> {
        self.inner.copy_file(from, to, mode)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }
    fn euid(&self) -> u32 {
        self.inner.euid()
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
}

#[test]
fn remote_full_disk_access_is_off_only_when_tcc_refuses_the_open() {
    let refused = TccRefuses {
        inner: Fake::root(),
        errno: libc::EPERM,
    };
    assert_eq!(remote_full_disk_access(&refused), Some(false));
    let other = TccRefuses {
        inner: Fake::root(),
        errno: libc::EIO,
    };
    assert_eq!(remote_full_disk_access(&other), None);
    assert_eq!(remote_full_disk_access(&Fake::root()), None);
}

#[test]
fn this_process_s_full_disk_access_is_the_runners_measurement_else_its_own_open() {
    assert_eq!(
        gather_with_cache(&vm(), Path::new("/c")).full_disk_access,
        Some(true)
    );
    let refused = TccRefuses {
        inner: Fake::root(),
        errno: libc::EPERM,
    };
    assert_eq!(
        gather_with_cache(&refused, Path::new("/c")).full_disk_access,
        Some(false)
    );
    assert_eq!(
        gather_with_cache(&Fake::root(), Path::new("/c")).full_disk_access,
        None
    );
    let mut measured = Fake::root();
    measured.full_disk_access = Some(true);
    assert_eq!(
        gather_with_cache(&measured, Path::new("/c")).full_disk_access,
        Some(true)
    );
}

#[test]
fn boot_time_and_idle_time_parse_the_tools_own_formats() {
    assert_eq!(
        parse_boottime("{ sec = 1792190170, usec = 411172 } Fri Oct 16 22:36:10 2026\n"),
        Some(1_792_190_170)
    );
    assert_eq!(parse_boottime("garbage"), None);
    assert_eq!(parse_idle_ns(VM_IDLE), Some(1_748_058_687_083));
    assert_eq!(parse_idle_ns("{}"), None);
    assert_eq!(
        ioreg_string(VM_IOREG, "IOPlatformUUID").as_deref(),
        Some("C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43")
    );
    assert_eq!(
        ioreg_string(VM_IOREG, "IOBusyInterest").as_deref(),
        Some("IOCommand is not serializable")
    );
    assert_eq!(ioreg_string(VM_IOREG, "missing"), None);
    assert!(parse_dscl_list("admin +501\n").is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn the_live_keyboard_list_parses_where_hidutil_answers() {
    let ran = match crate::port::Live::new().run(&hidutil_keyboards()) {
        Ok(ran) if ran.success() => ran,
        Ok(ran) => aterm_uds::host::could_not_run(format!(
            "hidutil refused the keyboard list: {}",
            ran.describe()
        )),
        Err(e) => aterm_uds::host::could_not_run(format!("hidutil did not run: {e:?}")),
    };
    let text = ran.out();
    let rows = text
        .split_once("Services:")
        .map_or("", |(_, s)| s.split("\n\n").next().unwrap_or(""))
        .lines()
        .filter(|l| l.starts_with("0x"))
        .count();
    let parsed = parse_hidutil_keyboards(&text);
    assert_eq!(parsed.is_empty(), rows == 0, "{text}");
    assert!(parsed.len() <= rows, "{text}");
}
