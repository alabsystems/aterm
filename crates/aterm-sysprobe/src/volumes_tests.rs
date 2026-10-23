// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows volume table.

use super::*;

const C_VOLUME: &str = "Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}";
const RECOVERY: &str = "Volume{732b011f-a067-4b03-83bf-ba4197adcb43}";
const DATA: &str = "Volume{0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d}";
const DVD: &str = "Volume{f0e1d2c3-b4a5-4968-8776-655443322110}";

fn seen(name: &str, device: Option<&str>, paths: &[&str]) -> VolumeSeen {
    VolumeSeen {
        name: name.to_owned(),
        device: device.map(str::to_owned),
        paths: Some(paths.iter().map(PathBuf::from).collect()),
    }
}

fn letters(l: &[(char, Option<&str>)]) -> Vec<(char, Option<String>)> {
    l.iter().map(|(c, t)| (*c, t.map(str::to_owned))).collect()
}

fn box_inputs() -> (Vec<VolumeSeen>, Vec<(char, Option<String>)>) {
    (
        vec![
            seen(C_VOLUME, Some(r"\Device\HarddiskVolume3"), &[r"C:\"]),
            seen(RECOVERY, Some(r"\Device\HarddiskVolume4"), &[]),
            seen(
                DATA,
                Some(r"\Device\HarddiskVolume7"),
                &[r"C:\mnt\data\", r"D:\"],
            ),
            seen(DVD, Some(r"\Device\CdRom0"), &[r"E:\"]),
        ],
        letters(&[
            ('C', Some(r"\Device\HarddiskVolume3")),
            ('D', Some(r"\Device\HarddiskVolume7")),
            ('E', Some(r"\Device\CdRom0")),
            ('S', Some(r"\??\C:\work")),
            (
                'Z',
                Some(r"\Device\LanmanRedirector\;Z:00000000000a1b2c\srv\share"),
            ),
        ]),
    )
}

fn box_table() -> Table {
    let (v, l) = box_inputs();
    windows_table(&v, &l, true)
}

fn on_of(t: &Table, p: &str) -> Option<String> {
    mount_in(Path::new(p), t, true).map(|m| m.on.to_string_lossy().into_owned())
}

#[test]
fn a_multi_sz_lists_its_strings() {
    let w = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    let buf = w("C:\\\0C:\\mnt\\data\\\0\0junk\0");
    let got: Vec<String> = multi_sz(&buf)
        .into_iter()
        .map(String::from_utf16_lossy)
        .collect();
    assert_eq!(got, [r"C:\", r"C:\mnt\data\"]);
    assert!(multi_sz(&w("\0\0")).is_empty(), "a volume mounted nowhere");
    assert!(multi_sz(&[]).is_empty());
    assert_eq!(multi_sz(&w("unterminated")).len(), 1);
}

#[test]
fn a_dos_target_is_classed_by_an_allowlist() {
    for (target, want) in [
        (r"\Device\HarddiskVolume3", Device::Volume),
        (r"\device\harddiskvolume12", Device::Volume),
        (r"\Device\HarddiskVolume", Device::Remote),
        (r"\Device\HarddiskVolume3x", Device::Remote),
        (r"\Device\HarddiskVolume3\x", Device::Remote),
        (r"\Device\CdRom0", Device::Removable),
        (r"\Device\Floppy0", Device::Removable),
        (r"\??\C:\work", Device::Subst(r"C:\work")),
        (r"\??\C:\", Device::Subst(r"C:\")),
        (r"\??\UNC\srv\share", Device::Remote),
        (r"\??\C:", Device::Remote),
        (
            r"\Device\LanmanRedirector\;Z:00000000000a1b2c\srv\share",
            Device::Remote,
        ),
        (
            r"\Device\Mup\;WebDavRedirector\srv\DavWWWRoot",
            Device::Remote,
        ),
        (r"\Device\WebDavRedirector\;W:000\srv", Device::Remote),
        (r"\Device\RdpDr\;D:1\tsclient\D", Device::Remote),
        (r"\Device\P9Rdr\;W:000\wsl.localhost\Ubuntu", Device::Remote),
        (r"\Device\ImDisk0", Device::Remote),
        ("", Device::Remote),
    ] {
        assert_eq!(device_class(target), want, "{target}");
    }
}

#[test]
fn the_windows_table_lists_every_mount_point_and_letter() {
    let t = box_table();
    assert!(t.complete, "{t:?}");
    let ons: Vec<&str> = t.mounts.iter().map(|m| m.on.to_str().unwrap()).collect();
    assert_eq!(
        ons,
        [
            NETWORK,
            r"C:\",
            r"C:\mnt\data",
            r"D:\",
            r"E:\",
            r"S:\",
            r"Z:\"
        ]
    );
    let m = |on: &str| t.mounts.iter().find(|m| m.on == Path::new(on)).unwrap();
    assert!(m(r"C:\").local && !m(r"C:\").automounted);
    assert_eq!(m(r"C:\").container.as_deref(), Some(C_VOLUME));
    assert_eq!(m(r"C:\").from, r"\Device\HarddiskVolume3");
    assert_eq!(m(r"C:\").fstype, "", "no file system is asked");
    assert_eq!(m(r"C:\mnt\data").container.as_deref(), Some(DATA));
    assert_eq!(m(r"D:\").container.as_deref(), Some(DATA));
    assert!(m(r"E:\").automounted && !m(r"E:\").local);
    assert_eq!(m(r"S:\").fstype, SUBST);
    assert!(m(r"S:\").local);
    assert_eq!(m(r"S:\").container.as_deref(), Some(C_VOLUME));
    assert!(!m(r"Z:\").local && !m(r"Z:\").automounted);
    assert!(!m(NETWORK).local);
    assert_eq!(read(m(r"Z:\")), Err(Unread::Remote));
    assert_eq!(read(m(NETWORK)), Err(Unread::Remote));
    assert_eq!(read(m(r"E:\")), Err(Unread::Automounted));
    assert_eq!(m(r"C:\").key(), m(r"S:\").key(), "one free figure, one key");
}

#[test]
fn a_letter_this_session_points_elsewhere_is_read_as_it_points() {
    let (v, _) = box_inputs();
    let l = letters(&[
        ('C', Some(r"\Device\HarddiskVolume3")),
        (
            'D',
            Some(r"\Device\LanmanRedirector\;D:00000000000a1b2c\srv\share"),
        ),
    ]);
    let t = windows_table(&v, &l, true);
    let d = t.mounts.iter().find(|m| m.on == Path::new(r"D:\")).unwrap();
    assert!(
        !d.local,
        "the session's network letter, not the data volume"
    );
    assert_eq!(d.container, None);
    assert_eq!(on_of(&t, r"C:\mnt\data\x").as_deref(), Some(r"C:\mnt\data"));
}

#[test]
fn an_unread_volume_or_letter_makes_the_table_incomplete() {
    let (v, l) = box_inputs();
    assert!(windows_table(&v, &l, true).complete);
    assert!(
        !windows_table(&v, &l, false).complete,
        "the enumeration was cut"
    );

    let mut unread = v.clone();
    unread[2].paths = None;
    let t = windows_table(&unread, &l, true);
    assert!(!t.complete, "a volume's mount points unread");
    assert!(t.mounts.iter().all(|m| m.on != Path::new(r"C:\mnt\data")));

    let mut nameless = v.clone();
    nameless[2].device = None;
    let t = windows_table(&nameless, &l, true);
    assert!(!t.complete, "a volume's device unread");
    let data = t
        .mounts
        .iter()
        .find(|m| m.on == Path::new(r"C:\mnt\data"))
        .unwrap();
    assert!(!data.local, "an unclassed device is never read");

    let mut dark = l.clone();
    dark.push(('Y', None));
    let t = windows_table(&v, &dark, true);
    assert!(!t.complete, "a letter's target unread");
    assert!(t.mounts.iter().all(|m| m.on != Path::new(r"Y:\")));

    let mut claimed = l.clone();
    claimed[0].1 = None;
    let t = windows_table(&v, &claimed, true);
    assert!(
        !t.complete,
        "a volume's letter whose session target is unread"
    );
    assert!(
        t.mounts
            .iter()
            .any(|m| m.on == Path::new(r"C:\") && m.local)
    );

    let many: Vec<VolumeSeen> = (0..=MAX_MOUNTS)
        .map(|i| {
            seen(
                &format!("Volume{{{i:08x}-0000-0000-0000-000000000000}}"),
                Some(&format!(r"\Device\HarddiskVolume{}", i + 10)),
                &[&format!(r"C:\m{i}\")],
            )
        })
        .collect();
    let t = windows_table(&many, &[], true);
    assert!(!t.complete && t.mounts.len() == MAX_MOUNTS);
}

#[test]
fn a_windows_path_finds_its_mount_in_any_spelling() {
    let t = box_table();
    let on = |p: &str| on_of(&t, p);
    let c = Some(r"C:\".to_owned());
    let data = Some(r"C:\mnt\data".to_owned());
    assert_eq!(on(r"\\?\C:\Users\m6-an\aterm"), c);
    assert_eq!(on(r"c:\USERS\m6-an\"), c);
    assert_eq!(on(r"\\?\c:\MNT\Data\x"), data);
    assert_eq!(on(r"C:\mnt\data"), data, "at the mount point");
    assert_eq!(on(r"C:\mntx\data"), c, "a component, not a prefix");
    assert_eq!(on(&format!(r"\\?\{DATA}\x")), data);
    assert_eq!(on(&format!(r"\\?\{C_VOLUME}\Users")), c);
    assert_eq!(
        on(&format!(r"\\?\{RECOVERY}\x")),
        None,
        "a volume mounted nowhere"
    );
    assert_eq!(on(r"D:\x"), Some(r"D:\".to_owned()));
    assert_eq!(on(r"S:\x"), c, "a subst letter is followed");
    assert_eq!(on(r"\\?\S:\"), c);
    assert_eq!(on(r"Z:\x"), Some(r"Z:\".to_owned()));
    assert_eq!(on(r"\\srv\share\x"), Some(NETWORK.to_owned()));
    assert_eq!(on(r"\\?\UNC\srv\share\x"), Some(NETWORK.to_owned()));
    for none in ["rel", r"\x", "C:x", r"\\.\C:\x", r"C:\a\..\b", r"Q:\x"] {
        assert_eq!(on(none), None, "{none}");
    }
    assert_eq!(
        mount_in(Path::new(r"\\?\C:\Users\x"), &t, false).map(|m| m.on.clone()),
        None,
        "the unix rule cannot place a verbatim spelling"
    );
}

#[test]
fn mounts_under_sees_a_folder_mount_inside_in_any_spelling() {
    let (mut v, mut l) = box_inputs();
    v.push(seen(
        "Volume{12345678-9abc-4def-8123-456789abcdef}",
        Some(r"\Device\HarddiskVolume9"),
        &[r"C:\repo\target\mnt\"],
    ));
    l.push(('R', Some(r"\??\C:\repo".to_owned())));
    let t = windows_table(&v, &l, true);
    let under = |p: &str| -> Vec<String> {
        mounts_in(Path::new(p), &t, true)
            .map(|m| m.on.to_string_lossy().into_owned())
            .collect()
    };
    let inner = vec![r"C:\repo\target\mnt".to_owned()];
    assert_eq!(under(r"\\?\C:\REPO\target"), inner);
    assert_eq!(under(r"c:\repo\TARGET\"), inner);
    assert_eq!(under(r"C:\repo\target\mnt"), inner, "at it");
    assert_eq!(under(r"R:\target"), inner, "through the subst letter");
    assert!(under(r"C:\repo\targetx").is_empty());
    assert!(under(r"C:\other").is_empty());
    let every = t.mounts.iter().filter(|m| m.fstype != SUBST).count();
    for unplaced in [
        "rel",
        r"\\?\GLOBALROOT\Device\HarddiskVolume3\repo",
        r"\\?\Volume{00000000-0000-0000-0000-000000000000}\repo",
    ] {
        assert_eq!(under(unplaced).len(), every, "{unplaced}");
    }
    assert!(under(r"C:\").iter().all(|on| !on.starts_with(['R', 'S'])));
    if cfg!(windows) {
        assert_eq!(
            mounts_in(Path::new(r"\\?\C:\REPO\target"), &t, false).count(),
            0,
            "the unix rule misses it"
        );
    }
}

#[test]
fn the_tables_rule_relates_every_spelling_of_a_directory() {
    let (mut v, l) = box_inputs();
    v[2].paths = Some(
        [r"C:\mnt\data\", r"D:\", r"C:\alias\"]
            .iter()
            .map(PathBuf::from)
            .collect(),
    );
    let t = windows_table(&v, &l, true);
    assert!(t.complete);
    let under = |p: &str, d: &str| under_in(Path::new(p), Path::new(d), &t, true);
    for (p, d) in [
        (r"S:\proj\target\debug\app.exe", r"\\?\C:\work\proj\target"),
        (r"\\?\s:\PROJ", r"c:\WORK"),
        (r"C:\work\proj\x", r"S:\proj"),
        (r"D:\x", r"C:\mnt\data"),
        (r"C:\mnt\data\x", r"D:\"),
        (r"C:\alias\x\y", r"\\?\C:\MNT\data\x"),
        (r"D:\x", r"C:\mnt"),
        (&format!(r"\\?\{DATA}\x"), r"C:\alias"),
        (r"Z:\x\y", r"z:\X"),
        (r"\\srv\share\x", r"\\?\UNC\SRV\share"),
    ] {
        assert_eq!(under(p, d), Some(true), "{p} under {d}");
    }
    for (p, d) in [
        (r"D:\x", r"C:\work"),
        (r"C:\work2", r"C:\work"),
        (r"S:\other", r"C:\work\proj"),
        (r"C:\Users", r"D:\"),
    ] {
        assert_eq!(under(p, d), Some(false), "{p} not under {d}");
    }
    for (p, d) in [
        (r"\\localhost\C$\work\proj\target\x", r"C:\work\proj\target"),
        (r"\\?\UNC\localhost\c$\work", r"C:\"),
        (r"Z:\proj", r"C:\work"),
        (r"C:\work", r"Z:\"),
        (r"\\srv\share\x", r"\\srv\other"),
        (r"Q:\x", r"C:\"),
        (r"C:\x", r"Q:\"),
        (r"C:\PROGRA~1\x", r"C:\Program Files"),
        ("rel", r"C:\"),
    ] {
        assert_eq!(under(p, d), None, "{p} beside {d}");
    }
    let mut cut = t.clone();
    cut.complete = false;
    assert_eq!(
        under_in(Path::new(r"D:\x"), Path::new(r"C:\work"), &cut, true),
        None
    );
    assert_eq!(
        under_in(
            Path::new(r"\\?\C:\work\x"),
            Path::new(r"c:\work"),
            &cut,
            true
        ),
        Some(true),
        "lexically under is under whatever the table missed"
    );

    assert_eq!(
        under_in(
            Path::new(r"S:\proj\x"),
            Path::new(r"C:\work\proj"),
            &t,
            false
        ),
        Some(false),
        "the unix rule: a false not-under"
    );
    assert_eq!(
        crate::paths::windows_under(Path::new(r"C:\alias\x"), Path::new(r"C:\mnt\data")),
        Some(false),
        "the table-less key cannot see a volume's second mount point"
    );
    assert_eq!(under(r"C:\alias\x", r"C:\mnt\data"), Some(true));
    assert_eq!(
        crate::paths::windows_under(Path::new(r"S:\proj"), Path::new(r"C:\work")),
        None
    );
}

#[test]
fn a_mount_point_is_found_in_any_spelling() {
    let (mut v, mut l) = box_inputs();
    v.push(seen(
        "Volume{12345678-9abc-4def-8123-456789abcdef}",
        Some(r"\Device\HarddiskVolume9"),
        &[r"D:\inner\"],
    ));
    l.push(('R', Some(r"\??\C:\mnt".to_owned())));
    let t = windows_table(&v, &l, true);
    let point = |p: &str| point_in(Path::new(p), &t, true);
    for p in [
        r"C:\mnt\data",
        r"\\?\c:\MNT\Data",
        r"C:\mnt\data\",
        r"R:\data",
        r"C:\",
        r"D:\",
        r"D:\inner",
        r"\\?\C:\mnt\data\INNER",
        NETWORK,
    ] {
        assert_eq!(point(p), Some(true), "{p}");
    }
    for p in [r"C:\mnt", r"C:\mnt\data\x", r"S:\", r"R:\", r"C:\work"] {
        assert_eq!(point(p), Some(false), "{p}");
    }
    for p in ["rel", r"Q:\mnt", r"\\srv\share\x", r"C:\PROGRA~1"] {
        assert_eq!(point(p), None, "{p}");
    }
    let mut cut = t.clone();
    cut.complete = false;
    assert_eq!(point_in(Path::new(r"C:\mnt"), &cut, true), None);
    assert_eq!(
        point_in(Path::new(r"\\?\C:\MNT\DATA"), &cut, true),
        Some(true)
    );
    assert!(t.points().contains(&PathBuf::from(r"C:\mnt\data")));
    assert!(!t.points().contains(&PathBuf::from(r"\\?\c:\MNT\Data")));
    assert!(!t.points().contains(&PathBuf::from(r"C:\mnt\data\inner")));
}

#[test]
fn an_unkeyable_mount_point_places_nothing_under_the_drive() {
    let mut v = vec![
        seen(C_VOLUME, Some(r"\Device\HarddiskVolume3"), &[r"C:\"]),
        seen(DATA, Some(r"\Device\HarddiskVolume7"), &[r"C:\data.\"]),
    ];
    let l = letters(&[('C', Some(r"\Device\HarddiskVolume3"))]);
    let t = windows_table(&v, &l, true);
    let on = |t: &Table, p: &str| on_of(t, p);
    assert_eq!(on(&t, r"C:\x").as_deref(), Some(r"C:\"));
    assert_eq!(on(&t, r"\\?\C:\data.\x").as_deref(), Some(r"C:\data."));
    assert_eq!(on(&t, r"C:\data.\x"), None, "Win32 trims a plain `data.`");
    assert_eq!(on(&t, r"\\?\C:\data\x").as_deref(), Some(r"C:\"));

    v.push(seen(
        RECOVERY,
        Some(r"\Device\HarddiskVolume4"),
        &[r"C:\a/b\"],
    ));
    let t = windows_table(&v, &l, true);
    assert_eq!(on(&t, r"C:\x"), None);
    assert_eq!(on(&t, r"\\?\C:\a/b\x"), None);
    assert!(
        mounts_in(Path::new(r"C:\elsewhere"), &t, true).any(|m| m.on == Path::new(r"C:\a/b")),
        "an unkeyable point is never ruled out"
    );
}

#[test]
fn a_folder_mount_on_a_letter_this_session_overrides_is_read_as_it_points() {
    let x = "Volume{12345678-9abc-4def-8123-456789abcdef}";
    let v = vec![
        seen(C_VOLUME, Some(r"\Device\HarddiskVolume3"), &[r"C:\"]),
        seen(DATA, Some(r"\Device\HarddiskVolume7"), &[r"D:\"]),
        seen(x, Some(r"\Device\HarddiskVolume9"), &[r"D:\mnt\x\"]),
    ];
    let own = letters(&[
        ('C', Some(r"\Device\HarddiskVolume3")),
        ('D', Some(r"\Device\HarddiskVolume7")),
    ]);
    let t = windows_table(&v, &own, true);
    let m = mount_in(Path::new(r"D:\mnt\x\f"), &t, true).unwrap();
    assert_eq!((m.on.to_str(), m.local), (Some(r"D:\mnt\x"), true));

    let netted = letters(&[
        ('C', Some(r"\Device\HarddiskVolume3")),
        (
            'D',
            Some(r"\Device\LanmanRedirector\;D:00000000000a1b2c\srv\share"),
        ),
    ]);
    let t = windows_table(&v, &netted, true);
    assert!(t.complete, "{t:?}");
    assert!(t.mounts.iter().all(|m| m.on != Path::new(r"D:\mnt\x")));
    let m = mount_in(Path::new(r"D:\mnt\x\f"), &t, true).unwrap();
    assert_eq!((m.on.to_str(), m.local), (Some(r"D:\"), false));

    let orphan = vec![
        seen(C_VOLUME, Some(r"\Device\HarddiskVolume3"), &[r"C:\"]),
        seen(x, Some(r"\Device\HarddiskVolume9"), &[r"Q:\mnt\x\"]),
    ];
    let ql = letters(&[
        ('C', Some(r"\Device\HarddiskVolume3")),
        ('Q', Some(r"\Device\HarddiskVolume12")),
    ]);
    let t = windows_table(&orphan, &ql, true);
    assert!(!t.complete, "a folder on a letter no volume owns");
    assert!(t.mounts.iter().any(|m| m.on == Path::new(r"Q:\mnt\x")));
}

#[test]
fn a_drive_mask_with_no_letter_is_a_failed_read() {
    assert_eq!(drive_letters(0), None);
    assert_eq!(drive_letters(0xFC00_0000), None, "no letter's bit");
    assert_eq!(drive_letters(1 << 2), Some(vec!['C']));
    assert_eq!(
        drive_letters(1 | (1 << 2) | (1 << 25)),
        Some(vec!['A', 'C', 'Z'])
    );
}

#[test]
fn a_mount_keys_its_volume_by_guid() {
    let mut m = box_table().mounts[1].clone();
    m.container = Some("Volume{6140FC3B-044F-43D7-B060-CB98170C7A63}".to_owned());
    let key = m.key().expect("a volume key");
    assert_eq!(
        key,
        VolumeKey::Volume("{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned())
    );
    assert_eq!(
        key.spelled(),
        "volume:{6140fc3b-044f-43d7-b060-cb98170c7a63}"
    );
    m.container = Some("disk3".to_owned());
    assert_eq!(m.key(), Some(VolumeKey::Container("disk3".to_owned())));
    m.container = Some("Volume{short}".to_owned());
    assert_eq!(
        m.key(),
        Some(VolumeKey::Container("Volume{short}".to_owned()))
    );
    m.container = None;
    assert_eq!(m.key(), None);
}

fn long_of(p: &str, t: &Table, answer: Option<&str>) -> (String, Option<String>) {
    let mut asked = None;
    let got = long_spelling_in(Path::new(p), t, true, |through| {
        asked = Some(through.to_str().expect("unicode").to_owned());
        answer.map(PathBuf::from)
    });
    (got.to_str().expect("unicode").to_owned(), asked)
}

#[test]
fn a_short_spelled_path_is_expanded_only_on_a_local_volume() {
    let t = box_table();
    let long = r"\\?\C:\Users\Andrew Yates";
    for (short, through, want) in [
        (
            r"C:\Users\ANDREW~1\x",
            r"\\?\C:\Users\ANDREW~1",
            r"\\?\C:\Users\Andrew Yates\x",
        ),
        (
            r"\\?\C:\Users\ANDREW~1\x\not-yet\",
            r"\\?\C:\Users\ANDREW~1",
            r"\\?\C:\Users\Andrew Yates\x\not-yet",
        ),
        (
            r"S:\PROGRA~1",
            r"\\?\S:\PROGRA~1",
            r"\\?\C:\Users\Andrew Yates",
        ),
    ] {
        assert_eq!(
            long_of(short, &t, Some(long)),
            (want.to_owned(), Some(through.to_owned())),
            "{short}"
        );
    }
    for untouched in [
        r"C:\Users\Andrew Yates\x",
        r"Z:\LONGNA~2\x",
        r"\\srv\share\LONGNA~2\x",
        r"E:\PROGRA~1\setup.exe",
        r"PROGRA~1\x",
        r"C:\PROGRA~1\..\x",
    ] {
        assert_eq!(
            long_of(untouched, &t, Some(long)),
            (untouched.to_owned(), None),
            "{untouched}"
        );
    }
    let short = r"C:\Users\ANDREW~1\x";
    let asked = Some(r"\\?\C:\Users\ANDREW~1".to_owned());
    assert_eq!(long_of(short, &t, None), (short.to_owned(), asked.clone()));
    let still_short = r"\\?\C:\Users\ANDREW~2";
    assert_eq!(
        long_of(short, &t, Some(still_short)),
        (short.to_owned(), asked)
    );
    let mut asked = false;
    let unix = long_spelling_in(Path::new(short), &t, false, |_| {
        asked = true;
        Some(PathBuf::from(long))
    });
    assert_eq!((unix.as_ref(), asked), (Path::new(short), false));
}

#[cfg(windows)]
fn system_root() -> PathBuf {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
    PathBuf::from(format!(r"{drive}\"))
}

#[cfg(windows)]
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("scratch dir")
}

/// The live table is read whole and lists the system drive's root.
#[cfg(windows)]
#[test]
fn the_live_table_lists_the_system_drive_local_and_whole() {
    let t = table();
    assert!(t.complete, "{t:?}");
    let root = system_root();
    let m = t
        .mounts
        .iter()
        .find(|m| m.on == root)
        .expect("the system drive");
    assert!(m.local && !m.automounted, "{m:?}");
    assert!(matches!(m.key(), Some(VolumeKey::Volume(_))), "{m:?}");
    assert!(m.from.starts_with(r"\Device\HarddiskVolume"), "{m:?}");
    assert!(
        t.mounts
            .iter()
            .any(|m| m.on == Path::new(NETWORK) && !m.local)
    );
    assert!(t.points().contains(&root));
}

/// The system drive is a fixed disk.
#[cfg(windows)]
#[test]
fn the_system_drive_is_fixed_and_the_network_is_not() {
    let t = table();
    let root = system_root();
    let m = t
        .mounts
        .iter()
        .find(|m| m.on == root)
        .expect("the system drive");
    assert_eq!(is_fixed(m), Some(true), "{m:?}");
    let windows = std::fs::canonicalize(root.join("Windows")).expect("Windows");
    let placed = mount_of(&windows, &t).expect("placed");
    assert_eq!(placed.on, root);
    assert_eq!(is_fixed(placed), Some(true), "{placed:?}");
    let network = t
        .mounts
        .iter()
        .find(|m| m.on == Path::new(NETWORK))
        .expect("the network mount");
    assert_eq!(is_fixed(network), Some(false));
}

/// The home directory is on a local volume, and every spelling of it reads that volume.
#[cfg(windows)]
#[test]
fn the_home_volume_reads_through_any_spelling() {
    let t = table();
    let home = std::env::var_os("USERPROFILE").expect("USERPROFILE");
    let canonical = std::fs::canonicalize(&home).expect("canonical home");
    assert!(
        canonical.to_string_lossy().starts_with(r"\\?\"),
        "{canonical:?}"
    );
    let r = read_path(&canonical, &t).expect("the home volume");
    assert!(r.free > 0 && r.total > r.free, "{r:?}");
    assert_eq!(r.files_free, None);
    assert!(matches!(r.key, VolumeKey::Volume(_)), "{r:?}");
    assert!(t.points().contains(&r.mount), "{r:?}");
    let plain = crate::paths::plain(&canonical).into_owned();
    let lower = PathBuf::from(plain.to_string_lossy().to_lowercase());
    for spelling in [plain, lower] {
        let again = read_path(&spelling, &t).expect("the same volume");
        assert_eq!(
            (&again.mount, &again.key),
            (&r.mount, &r.key),
            "{spelling:?}"
        );
    }
    assert_eq!(file_id(&canonical, true).map(|id| id.dev), Some(r.dev));
}

/// A file's identity is the same through every open and every spelling.
#[cfg(windows)]
#[test]
fn a_file_id_is_stable_and_on_its_volumes_device() {
    let dir = scratch();
    let f = dir.path().join("identity-probe.txt");
    std::fs::write(&f, "x").expect("write");
    let id = file_id(&f, true).expect("an identity");
    assert_eq!(file_id(&f, true), Some(id), "a second open");
    assert_eq!(file_id(&f, false), Some(id), "a plain file is no link");
    let canonical = std::fs::canonicalize(&f).expect("canonical");
    assert_eq!(file_id(&canonical, true), Some(id), "verbatim");
    let upper = PathBuf::from(f.to_string_lossy().to_uppercase());
    assert_eq!(file_id(&upper, true), Some(id), "another case");
    let open = std::fs::File::open(&f).expect("open");
    assert_eq!(
        file_id_of_handle(std::os::windows::io::AsHandle::as_handle(&open)),
        Some(id)
    );
    let g = dir.path().join("identity-probe-2.txt");
    std::fs::write(&g, "y").expect("write");
    let other = file_id(&g, true).expect("an identity");
    assert_eq!(other.dev, id.dev);
    assert_ne!(other.ino, id.ino);
    assert_ne!(file_id(dir.path(), true).map(|d| d.ino), Some(id.ino));
    let r = read_path(&canonical, &table()).expect("its volume");
    assert_eq!(
        r.dev, id.dev,
        "one width: the reading's device is the file's"
    );
}

/// A path past `MAX_PATH` (`LongPathsEnabled` is 0 on most machines; a cargo profile's paths run
/// past it) has an identity in its plain spelling and its verbatim one alike.
#[cfg(windows)]
#[test]
fn a_path_past_max_path_has_an_identity_and_a_volume() {
    let dir = scratch();
    let mut deep = dir.path().to_path_buf();
    while deep.as_os_str().len() < 300 {
        deep.push("a-directory-name-of-forty-characters-xx");
    }
    std::fs::create_dir_all(&deep).expect("a deep tree");
    let f = deep.join("deep-file.txt");
    std::fs::write(&f, "x").expect("write");
    assert!(f.as_os_str().len() > 260, "{f:?}");
    let id = file_id(&f, true).expect("an identity past MAX_PATH");
    let canonical = std::fs::canonicalize(&f).expect("canonical");
    assert_eq!(file_id(&canonical, false), Some(id));
    let t = table();
    let r = read_path(&f, &t).expect("its volume");
    assert_eq!(r.dev, id.dev);
    assert_eq!(read_path(&canonical, &t).map(|r| r.key), Ok(r.key));
}

/// A missing path has no identity.
#[cfg(windows)]
#[test]
fn a_missing_path_or_a_volume_without_file_ids_has_none() {
    let dir = scratch();
    assert_eq!(file_id(&dir.path().join("missing"), true), None);
    assert_eq!(file_id(&dir.path().join(r"missing\deeper"), false), None);
    let (names, _) = crate::win_paths::volume_names();
    let mut fat = Vec::new();
    for name in names {
        let root = PathBuf::from(&name);
        let Ok((_, fs)) = crate::win_paths::volume_info(&root) else {
            continue;
        };
        if fs.eq_ignore_ascii_case("FAT")
            || fs.eq_ignore_ascii_case("FAT32")
            || fs.eq_ignore_ascii_case("exFAT")
        {
            assert_eq!(file_id(&root, true), None, "{name} is {fs}");
            fat.push(format!("{name} ({fs})"));
        }
    }
    eprintln!("FAT and exFAT volumes checked: {} {fat:?}", fat.len());
}

/// A directory junction answers as itself unless followed.
#[cfg(windows)]
#[test]
fn a_junction_answers_as_itself_unless_followed() {
    let dir = scratch();
    let root = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    let target = root.join("target-dir");
    std::fs::create_dir(&target).expect("target");
    std::fs::write(target.join("inside.txt"), "x").expect("write");
    let link = root.join("link-dir");
    crate::win_paths::make_junction(&link, &target).expect("the junction");
    let to = file_id(&target, true).expect("the target");
    let followed = file_id(&link, true).expect("the link followed");
    let itself = file_id(&link, false).expect("the link itself");
    assert_eq!(followed, to, "following reaches the target");
    assert_ne!(itself, followed, "the link is not its target");
    assert_eq!(itself.dev, to.dev);
    let through = std::fs::canonicalize(link.join("inside.txt")).expect("canonical");
    assert_eq!(
        table().is_under(&through, &target),
        Some(true),
        "{through:?}"
    );
}

/// A real folder with a long name, read through its 8.3 short spelling, is spelled long.
#[cfg(windows)]
#[test]
fn a_short_spelling_of_a_real_folder_is_spelled_long() {
    let t = table();
    let dir = scratch();
    let root = std::fs::canonicalize(dir.path()).expect("canonical scratch");
    let folder = root.join("a folder with a long name");
    std::fs::create_dir(&folder).expect("the long-named folder");
    std::fs::write(folder.join("inside.txt"), "x").expect("a file inside");
    let short = crate::short_spelling(&folder).unwrap_or_else(|| {
        panic!(
            "PREMISE: the volume generates 8.3 short names; none for {}",
            folder.display()
        )
    });
    assert!(
        crate::paths::short_spelled(&short).is_some(),
        "{short:?} is short-spelled"
    );
    assert_eq!(
        t.is_under(&short, &folder),
        None,
        "the short spelling places nowhere"
    );
    let long = t.long_spelling(&short);
    assert!(matches!(long, std::borrow::Cow::Owned(_)), "{long:?}");
    assert_eq!(file_id(&long, true), file_id(&folder, true), "{long:?}");
    assert_eq!(t.is_under(&long, &folder), Some(true), "{long:?}");
    assert_eq!(t.is_under(&long, &root.join("elsewhere")), Some(false));
    assert!(mount_of(&long, &t).is_some_and(|m| m.local), "{long:?}");
    let file = short.join("inside.txt");
    assert_eq!(t.is_under(&t.long_spelling(&file), &folder), Some(true));
    let ghost = short.join("not-there-yet").join("deeper");
    assert_eq!(t.is_under(&ghost, &folder), None, "{ghost:?}");
    let ghost_long = t.long_spelling(&ghost);
    assert_eq!(
        t.is_under(&ghost_long, &folder),
        Some(true),
        "{ghost_long:?}"
    );

    let target = root.join("the junction's target");
    std::fs::create_dir_all(target.join("sub")).expect("the junction's target");
    let link = folder.join("a junction with a long name");
    crate::win_paths::make_junction(&link, &target).expect("the junction");
    let inside = crate::short_spelling(&link.join("sub")).expect("its short spelling");
    assert!(
        crate::paths::short_spelled(&inside).is_some(),
        "{inside:?} is short-spelled"
    );
    let inside_long = t.long_spelling(&inside);
    assert_eq!(
        t.is_under(&inside_long, &folder),
        t.is_under(&link.join("sub"), &folder),
        "{inside_long:?} places as its long spelling does"
    );
    assert_eq!(
        t.is_under(&inside_long, &folder),
        Some(true),
        "{inside_long:?}"
    );
    assert_eq!(
        t.is_under(&inside_long, &target),
        Some(false),
        "{inside_long:?}"
    );
    std::fs::remove_dir(&link).expect("the junction, as itself");
}

/// A quiet guard ([`crate::quiet_errors`]) holds this thread's critical-error and open-file boxes
/// off for its life and restores the mode it found.
#[cfg(windows)]
#[test]
fn a_quiet_guard_holds_the_boxes_off_for_its_life_alone() {
    std::thread::spawn(|| {
        assert!(!crate::errors_quiet(), "a fresh thread's boxes are on");
        {
            let _outer = crate::quiet_errors();
            assert!(crate::errors_quiet(), "the guard turned them off");
            {
                let _inner = crate::quiet_errors();
                assert!(crate::errors_quiet());
            }
            assert!(
                crate::errors_quiet(),
                "the inner guard restored the outer's mode"
            );
        }
        assert!(
            !crate::errors_quiet(),
            "the outer guard restored the thread's own"
        );
    })
    .join()
    .expect("the guarded thread");
}

/// The hand-declared `FILE_ID_INFO` has the SDK's layout.
#[cfg(windows)]
#[test]
fn file_id_info_has_the_sdk_layout() {
    use crate::win_paths::FileIdInfo;
    assert_eq!(std::mem::size_of::<FileIdInfo>(), 24);
    assert_eq!(std::mem::align_of::<FileIdInfo>(), 8);
    assert_eq!(std::mem::offset_of!(FileIdInfo, volume_serial_number), 0);
    assert_eq!(std::mem::offset_of!(FileIdInfo, file_id), 8);
}

/// AN OPEN SOMEONE ELSE NAMED RAISES NO BOX ([`crate::quiet_errors`]).
#[cfg(windows)]
#[test]
fn quiet_errors_keeps_the_threads_other_bits_and_restores_them() {
    use crate::win_paths::{QUIET_BITS, set_thread_error_mode, thread_error_mode};
    const KEPT: u32 = 0x0002;
    fn quietly<T>(f: impl FnOnce() -> T) -> T {
        let _quiet = crate::quiet_errors();
        f()
    }
    std::thread::spawn(|| {
        assert!(set_thread_error_mode(KEPT), "set the thread's mode");
        let before = thread_error_mode();
        assert_eq!(before & QUIET_BITS, 0, "the boxes are on: {before:#x}");
        assert_eq!(before & KEPT, KEPT, "{before:#x}");
        let inside = quietly(thread_error_mode);
        assert_eq!(inside & QUIET_BITS, QUIET_BITS, "off inside: {inside:#x}");
        assert_eq!(inside & KEPT, KEPT, "kept inside: {inside:#x}");
        assert_eq!(thread_error_mode(), before, "restored after");
        let unwound = std::panic::catch_unwind(|| quietly(|| -> u32 { panic!("inside") }));
        assert!(unwound.is_err());
        assert_eq!(thread_error_mode(), before, "restored after an unwind");
    })
    .join()
    .expect("the thread's checks");
}
