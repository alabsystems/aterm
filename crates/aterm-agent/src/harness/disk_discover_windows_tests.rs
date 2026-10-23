// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for finding the build directories on Windows, and for the rules that say who uses one there.

use super::*;
use crate::harness::disk::tests::{Tmp, lay_target};

fn mkdirs(p: &Path) -> PathBuf {
    std::fs::create_dir_all(p).expect("mkdir");
    p.to_path_buf()
}

/// A junction at `link` to the folder `target`.
fn junction(target: &Path, link: &Path) {
    atpkg::platform::atomic_symlink(target, link).expect("a junction");
    assert!(
        std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink()),
        "std reads a junction as a link"
    );
}

/// The roots of a search of `home` alone, on its volume (its serial and its
fn home_roots(home: &Path) -> Roots {
    Roots {
        home: Some(home.to_path_buf()),
        device: device_of(home),
        volume: crate::harness::disk::target::VolumeGuid::of_path_now(home),
        mounts: mount_points(),
        ..Roots::default()
    }
}

/// What `roots` finds, as the one path key spells each (any spelling of one
fn found_keys(roots: &Roots) -> Vec<String> {
    let found = discover(roots, Budget::SHIPPED);
    assert_eq!(found.cut, None, "{:?}", found.notes);
    let mut keys: Vec<String> = found
        .found
        .iter()
        .map(|f| aterm_sysprobe::paths::windows_key(&f.path).expect("a keyed path"))
        .collect();
    keys.sort();
    keys
}

/// `path`'s key, for comparing with [`found_keys`].
fn key(path: &Path) -> String {
    aterm_sysprobe::paths::windows_key(&std::fs::canonicalize(path).expect("canonical"))
        .expect("a keyed path")
}

/// Make `top`, under the sync root `root`, and every file and folder under
fn placeholders(root: &aterm_dirfd::seam::SyncRoot, top: &Path) {
    let mut made = vec![top.to_path_buf()];
    let mut i = 0;
    while let Some(dir) = made.get(i).cloned() {
        i += 1;
        if dir.is_dir() {
            let mut children: Vec<PathBuf> = std::fs::read_dir(&dir)
                .expect("listed")
                .map(|e| e.expect("entry").path())
                .collect();
            children.sort();
            made.extend(children);
        }
    }
    for path in &made {
        root.convert(path).expect("a placeholder");
    }
}

/// Set the attributes `add` on the folder at `path` (as a cloud client or an
fn add_folder_attributes(path: &Path, add: u32) {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES`.
    const ATTRIBUTES: u32 = 0x0080 | 0x0100;
    /// `FILE_FLAG_BACKUP_SEMANTICS`: what opens a folder.
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let folder = std::fs::OpenOptions::new()
        .access_mode(ATTRIBUTES)
        .custom_flags(BACKUP_SEMANTICS)
        .open(path)
        .expect("the folder opened");
    super::super::win::add_attributes(&folder, add).expect("attributes set");
}

/// A SCRATCH HOME'S CHECKOUTS ARE FOUND, ITS PROFILE'S OWN TREES NEVER LOOKED
#[test]
fn a_scratch_homes_checkouts_are_found_and_its_cloud_and_appdata_folders_never_entered() {
    let tmp = Tmp::new("discover-win-home");
    let home = mkdirs(&tmp.path().join("home"));
    lay_target(&home.join("proj").join("target"));
    lay_target(&home.join("ws").join("crates").join("target"));
    for skipped in [
        "AppData",
        "OneDrive",
        "OneDrive - Contoso",
        "onedrive-backup",
    ] {
        lay_target(&home.join(skipped).join("repo").join("target"));
    }
    lay_target(&home.join("cold").join("target"));
    add_folder_attributes(&home.join("cold"), 0x0000_1000);
    lay_target(&home.join("MyOneDrive").join("repo").join("target"));

    assert_eq!(
        found_keys(&home_roots(&home)),
        [
            key(&home.join("MyOneDrive").join("repo").join("target")),
            key(&home.join("proj").join("target")),
            key(&home.join("ws").join("crates").join("target")),
        ]
    );
}

/// Make the empty folder at `path` a reparse point that is no link (a tag of no
fn foreign_reparse_point(path: &Path) {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_FLAG_BACKUP_SEMANTICS`: what opens a folder.
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let folder = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(super::super::win::FILE_FLAG_OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
        .open(path)
        .expect("opened as itself");
    super::super::win::set_foreign_reparse_point(&folder).expect("a reparse point set");
}

/// A FOLDER THAT IS A REPARSE POINT BUT NO LINK (a cloud placeholder, to std
#[test]
fn a_reparse_point_that_is_no_link_is_never_listed() {
    let tmp = Tmp::new("discover-win-reparse");
    let dir = mkdirs(&tmp.path().join("dir"));
    foreign_reparse_point(&mkdirs(&dir.join("placeholder")));
    let offline = mkdirs(&dir.join("offline-placeholder"));
    add_folder_attributes(&offline, 0x0000_1000);
    foreign_reparse_point(&offline);
    mkdirs(&dir.join("plain"));
    junction(&dir.join("plain"), &dir.join("link"));

    let listed: BTreeMap<String, Option<PassedOver>> = std::fs::read_dir(&dir)
        .expect("listed")
        .map(|e| {
            let e = e.expect("an entry");
            let ty = e.file_type().expect("its type");
            (
                e.file_name().to_string_lossy().into_owned(),
                fetches_or_reparses(&e, ty),
            )
        })
        .collect();
    assert_eq!(
        listed,
        BTreeMap::from([
            ("link".to_owned(), None),
            ("offline-placeholder".to_owned(), Some(PassedOver::Fetches)),
            ("placeholder".to_owned(), Some(PassedOver::Reparse)),
            ("plain".to_owned(), None),
        ])
    );
}

/// A CHECKOUT IN A REAL CLOUD SYNC ROOT IS NEVER OFFERED. A scratch folder
#[test]
fn a_checkout_in_a_cloud_sync_root_is_never_offered() {
    let tmp = Tmp::new("discover-win-sync-root");
    let home = mkdirs(&tmp.path().join("home"));
    let synced = mkdirs(&home.join("Synced"));
    let proj = synced.join("proj");
    lay_target(&proj.join("target"));
    lay_target(&home.join("plain").join("target"));
    let root = aterm_dirfd::seam::SyncRoot::register(&synced).expect("a sync root");
    placeholders(&root, &proj);
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);
    let roots = Roots {
        cwds: vec![proj.clone()],
        agent_target_dirs: vec![proj.join("target")],
        ..home_roots(&home)
    };
    let found = discover(&roots, Budget::SHIPPED);
    let keys: Vec<String> = found
        .found
        .iter()
        .map(|f| aterm_sysprobe::paths::windows_key(&f.path).expect("keyed"))
        .collect();
    assert_eq!(keys, [key(&home.join("plain").join("target"))], "{found:?}");
    assert!(
        found.notes.iter().any(|n| n.contains("cloud sync root")),
        "{:?}",
        found.notes
    );
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 0);

    drop(root);
    let mut keys = found_keys(&roots);
    keys.sort();
    let mut all = vec![
        key(&home.join("plain").join("target")),
        key(&proj.join("target")),
    ];
    all.sort();
    assert_eq!(keys, all, "a plain folder again");
}

/// THE WALK LISTS WITH CLOUD PLACEHOLDERS EXPOSED WHILE IT LIVES, whatever
#[test]
fn the_walk_lists_with_cloud_placeholders_exposed_while_it_lives() {
    use aterm_dirfd::seam::thread_placeholder_mode;
    let tmp = Tmp::new("discover-win-walk-exposed");
    let synced = mkdirs(&tmp.path().join("Synced"));
    let proj = mkdirs(&synced.join("proj"));
    mkdirs(&proj.join("src"));
    lay_target(&proj.join("target"));
    let root = aterm_dirfd::seam::SyncRoot::register(&synced).expect("a sync root");
    placeholders(&root, &proj);
    mkdirs(&proj.join("new"));
    let roots = Roots {
        device: device_of(tmp.path()),
        volume: crate::harness::disk::target::VolumeGuid::of_path_now(tmp.path()),
        ..Roots::default()
    };
    let names = |listed: Vec<(OsString, PathBuf, std::fs::FileType)>| -> Vec<String> {
        listed
            .into_iter()
            .map(|(name, _, _)| name.to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(thread_placeholder_mode(), 0, "the process's default");

    let mut walk = Walk::new(&roots, Budget::SHIPPED).expect("a walk");
    let living = thread_placeholder_mode();
    let exposed = names(walk.children(&proj));
    drop(walk);
    assert_eq!(exposed, ["new"]);
    assert_eq!(living, 2, "exposed while the walk lives");
    assert_eq!(thread_placeholder_mode(), 0, "the thread's own mode back");

    aterm_dirfd::seam::leave_placeholders_disguised(true);
    let mut walk = Walk::new(&roots, Budget::SHIPPED).expect("a walk");
    let disguised = names(walk.children(&proj));
    drop(walk);
    aterm_dirfd::seam::leave_placeholders_disguised(false);
    assert_eq!(disguised, ["new", "src", "target"]);
    drop(root);
}

/// NOTHING IN A CLOUD SYNC ROOT IS READ INTO, FROM ANY ROOT. A real sync
#[test]
fn nothing_in_a_cloud_sync_root_is_read_into_from_any_root() {
    use aterm_sysprobe::paths::is_under;
    let tmp = Tmp::new("discover-win-sync-reads");
    let synced = mkdirs(&tmp.path().join("Synced"));
    let proj = synced.join("proj");
    lay_target(&proj.join("target"));
    let home = synced.join("home");
    lay_target(&home.join("repo").join("target"));
    let cache = atpkg::build_cache::cache_root(&home);
    lay_target(&cache.join("ws").join(atpkg::build_cache::LEAF));
    let claude = synced.join("tmp");
    let scratch = claude
        .join("C--proj")
        .join("0dead000-5e55-1000")
        .join("scratchpad");
    lay_target(&scratch.join("target"));
    let linked = synced.join("linked");
    lay_target(&linked.join("target"));
    let beside = tmp.path().join("beside");
    lay_target(&beside.join("crates").join("target"));
    junction(&linked.join("target"), &beside.join("target"));
    let root = aterm_dirfd::seam::SyncRoot::register(&synced).expect("a sync root");
    for top in ["proj", "home", "tmp", "linked"] {
        placeholders(&root, &synced.join(top));
    }
    let roots = Roots {
        home: Some(home.clone()),
        cwds: vec![proj.clone(), beside.clone()],
        agent_target_dirs: vec![proj.join("target")],
        claude_tmp: Some(claude.clone()),
        device: device_of(tmp.path()),
        volume: crate::harness::disk::target::VolumeGuid::of_path_now(tmp.path()),
        mounts: mount_points(),
        ..Roots::default()
    };

    record_reads(true);
    let found = discover(&roots, Budget::SHIPPED);
    let reads = record_reads(false);
    let inside: Vec<&PathBuf> = reads
        .iter()
        .filter(|r| is_under(r, &synced) != Some(false))
        .collect();
    assert_eq!(inside, Vec::<&PathBuf>::new(), "read into the sync root");
    assert!(
        reads.iter().any(|r| is_under(r, &beside) == Some(true)),
        "the plain checkout was read: {reads:?}"
    );
    let keys: Vec<String> = found
        .found
        .iter()
        .map(|f| aterm_sysprobe::paths::windows_key(&f.path).expect("keyed"))
        .collect();
    assert_eq!(
        keys,
        [key(&beside.join("crates").join("target"))],
        "{found:?}"
    );
    assert!(
        found.notes.iter().any(|n| n.contains("cloud sync root")),
        "{:?}",
        found.notes
    );

    drop(root);
    record_reads(true);
    let mut keys = found_keys(&roots);
    let reads = record_reads(false);
    let resolved = |p: &Path| {
        std::fs::canonicalize(p)
            .ok()
            .and_then(|c| aterm_sysprobe::paths::windows_key(&c))
    };
    for read in [
        proj.join("target"),
        home.clone(),
        cache.clone(),
        claude.clone(),
        linked.join("target"),
    ] {
        assert!(
            reads.iter().any(|r| resolved(r) == Some(key(&read))),
            "{} read: {reads:?}",
            read.display()
        );
    }
    let mut all = vec![
        key(&beside.join("crates").join("target")),
        key(&proj.join("target")),
        key(&home.join("repo").join("target")),
        key(&cache.join("ws").join(atpkg::build_cache::LEAF)),
        key(&scratch.join("target")),
        key(&linked.join("target")),
    ];
    keys.sort();
    all.sort();
    assert_eq!(keys, all, "a plain folder again");
}

/// THE SEARCH STAYS ON THE MEASURED VOLUME BY ITS GUID TOO: with the
#[test]
fn the_search_stays_on_the_volume_of_the_measured_guid() {
    use crate::harness::disk::target::VolumeGuid;
    let tmp = Tmp::new("discover-win-guid");
    let home = mkdirs(&tmp.path().join("home"));
    lay_target(&home.join("proj").join("target"));
    let roots = Roots {
        cwds: vec![home.join("cwd")],
        ..home_roots(&home)
    };
    mkdirs(&home.join("cwd"));
    lay_target(&home.join("cwd").join("target"));
    assert_eq!(found_keys(&roots).len(), 2, "the measured GUID");
    let clone = VolumeGuid::parse("{00000000-0000-0000-0000-00000000c10e}");
    assert_ne!(clone, roots.volume);
    for volume in [clone, None] {
        let elsewhere = Roots {
            volume,
            ..roots.clone()
        };
        assert_eq!(found_keys(&elsewhere), Vec::<String>::new(), "{volume:?}");
    }
}

/// ONE FOLDER, ONE ROW, WHATEVER SPELLS IT: a working directory given plain,
#[test]
fn every_spelling_of_one_build_directory_is_one_row() {
    let tmp = Tmp::new("discover-win-spellings");
    let home = mkdirs(&tmp.path().join("home"));
    let proj = home.join("proj");
    lay_target(&proj.join("target"));
    lay_target(&home.join("other").join("target"));
    let canonical = std::fs::canonicalize(&proj).expect("canonical");
    let shouted = PathBuf::from(proj.to_string_lossy().to_uppercase());
    let roots = Roots {
        cwds: vec![
            proj.clone(),
            shouted.clone(),
            canonical.clone(),
            PathBuf::from(proj.to_string_lossy().to_lowercase()),
        ],
        agent_target_dirs: vec![shouted.join("TARGET")],
        ..home_roots(&home)
    };
    let found = discover(&roots, Budget::SHIPPED);
    let keys: Vec<String> = found
        .found
        .iter()
        .map(|f| aterm_sysprobe::paths::windows_key(&f.path).expect("keyed"))
        .collect();
    assert_eq!(
        keys,
        [
            key(&proj.join("target")),
            key(&home.join("other").join("target"))
        ],
        "{:?}",
        found.found
    );
    let id = identity(&canonical.join("target")).expect("an identity");
    assert_eq!(identity(&shouted.join("TARGET")), Some(id));
    assert_ne!(identity(&home.join("other").join("target")), Some(id));
}

/// A JUNCTION IS NEVER A SECOND ROW AND NEVER LEADS OUT OF THE FENCE: a
#[test]
fn a_junction_is_never_a_second_row_and_never_followed_into_another_mount() {
    let tmp = Tmp::new("discover-win-junction");
    let home = mkdirs(&tmp.path().join("home"));
    let proj = home.join("proj");
    lay_target(&proj.join("target"));
    junction(&proj, &home.join("alias"));
    mkdirs(&home.join("proj2"));
    junction(&proj.join("target"), &home.join("proj2").join("target"));

    let fence = mkdirs(&tmp.path().join("fence"));
    lay_target(&fence.join("t"));
    mkdirs(&home.join("proj3"));
    junction(&fence.join("t"), &home.join("proj3").join("target"));

    lay_target(&tmp.path().join("beside").join("t"));
    mkdirs(&home.join("proj4"));
    junction(
        &tmp.path().join("beside").join("t"),
        &home.join("proj4").join("target"),
    );

    let mut roots = home_roots(&home);
    roots.mounts.push(fence.clone());
    assert_eq!(
        found_keys(&roots),
        [
            key(&tmp.path().join("beside").join("t")),
            key(&proj.join("target")),
        ]
    );
    let roots = home_roots(&home);
    assert_eq!(found_keys(&roots).len(), 3);
}

/// ONLY THE THREE MEASURED IMAGES ARE PLATFORM SERVICES, each exactly where
#[test]
fn only_the_measured_images_where_the_system_keeps_them_are_platform_services() {
    let system = Path::new(r"C:\Windows\System32");
    let data = Path::new(r"C:\ProgramData");
    let service = |p: &str| windows_service(Path::new(p), Some(system), Some(data));
    for p in [
        r"C:\Windows\System32\ctfmon.exe",
        r"\\?\c:\windows\system32\CTFMON.EXE",
        r"C:\Windows\System32\taskhostw.exe",
        r"C:\ProgramData\Microsoft\Windows Defender\Platform\4.18.26090.9-0\DefenderSessionHelper.exe",
        r"\\?\C:\programdata\microsoft\windows defender\platform\4.18.26090.9-0\defendersessionhelper.exe",
    ] {
        assert!(service(p), "{p}");
    }
    for p in [
        r"C:\Windows\System32\cmd.exe",
        r"C:\Windows\System32\sub\ctfmon.exe",
        r"C:\Windows\ctfmon.exe",
        r"D:\Windows\System32\ctfmon.exe",
        r"C:\Windows\System32\CTFMON~1.EXE",
        r"C:\Users\x\ctfmon.exe",
        r"C:\ProgramData\Microsoft\Windows Defender\Platform\DefenderSessionHelper.exe",
        r"C:\ProgramData\Microsoft\Windows Defender\Platform\4.18\x\DefenderSessionHelper.exe",
        r"C:\ProgramData\Microsoft\Windows Defender\Platform\evil\DefenderSessionHelper.exe",
        r"C:\ProgramData\Microsoft\Windows Defender\Platform\4.18\MsMpEng.exe",
        r"C:\ProgramData\DefenderSessionHelper.exe",
        "ctfmon.exe",
    ] {
        assert!(!service(p), "{p}");
    }
    assert!(!windows_service(
        Path::new(r"C:\Windows\System32\ctfmon.exe"),
        None,
        Some(data)
    ));
    assert!(!windows_service(
        Path::new(
            r"C:\ProgramData\Microsoft\Windows Defender\Platform\4.18.1-0\DefenderSessionHelper.exe"
        ),
        Some(system),
        None
    ));

    let real = super::super::win::system_directory().expect("System32");
    assert!(platform_service(Some(&real.join("ctfmon.exe"))));
    assert!(platform_service(Some(&real.join("taskhostw.exe"))));
    assert!(!platform_service(Some(&real.join("cmd.exe"))));
    assert!(!platform_service(None));
}

/// AN UNREADABLE PROCESS KEEPS A DIRECTORY UNLESS IT IS ONE OF THE MEASURED
#[test]
fn an_unreadable_process_keeps_a_directory_unless_it_is_a_measured_service() {
    use aterm_uds::holders::PathScan;
    let system = super::super::win::system_directory().expect("System32");
    let data = super::super::win::program_data().expect("ProgramData");
    let helper = data
        .join("Microsoft")
        .join("Windows Defender")
        .join("Platform")
        .join("4.18.26090.9-0")
        .join("DefenderSessionHelper.exe");
    let measured = vec![
        (11, Some(system.join("ctfmon.exe"))),
        (12, Some(system.join("taskhostw.exe"))),
        (13, Some(helper)),
    ];
    let scan = |unreadable: Vec<(u32, Option<PathBuf>)>| -> std::io::Result<PathScan> {
        Ok(PathScan {
            unreadable,
            ..PathScan::default()
        })
    };
    assert_eq!(scan_says_unused(&scan(measured.clone()), |_| true), Ok(()));
    for extra in [None, Some(system.join("cmd.exe"))] {
        let mut more = measured.clone();
        more.push((14, extra));
        let kept = scan_says_unused(&scan(more), |_| true);
        assert!(
            kept.as_ref()
                .is_err_and(|why| why.starts_with("process 14")),
            "{kept:?}"
        );
    }
}

/// THE CLAUDE CENSUS TELLS THE SAME MEASURED SERVICES APART, AND NO OTHER:
#[test]
fn the_claude_census_passes_over_only_the_measured_services() {
    let system = super::super::win::system_directory().expect("System32");
    let data = super::super::win::program_data().expect("ProgramData");
    let helper = data
        .join("Microsoft")
        .join("Windows Defender")
        .join("Platform")
        .join("4.18.26090.9-0")
        .join("DefenderSessionHelper.exe");
    let unread = |pid: u32, exe: Option<Exe>| Process {
        pid,
        exe,
        exec: None,
        cwd: None,
        started: None,
    };
    let services = || {
        vec![
            unread(11, Some(Exe::At(system.join("ctfmon.exe")))),
            unread(12, Some(Exe::At(system.join("taskhostw.exe")))),
            unread(13, Some(Exe::At(helper.clone()))),
        ]
    };
    let census = claude_census(services(), None).expect("the services are told");
    assert!(census.claudes.is_empty() && census.dirs.is_empty());
    for (pid, exe) in [
        (
            14,
            Some(Exe::At(PathBuf::from(r"C:\Users\x\.local\bin\claude.exe"))),
        ),
        (15, Some(Exe::At(data.join("ctfmon.exe")))),
        (16, Some(Exe::Unnamed)),
        (17, None),
    ] {
        let mut all = services();
        all.push(unread(pid, exe));
        let refused = claude_census(all, None);
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.starts_with(&format!("process {pid} "))),
            "{pid}: {refused:?}"
        );
    }
}

/// A NATIVE WINDOWS CLAUDE CODE IS READ AS ONE, its exec record read: the
#[test]
fn a_native_windows_claude_code_is_read_as_one() {
    let read = |pid: u32, exe: Option<&str>, argv: &[&str]| Process {
        pid,
        exe: exe.map(|e| Exe::At(PathBuf::from(e))),
        exec: Some(Exec {
            path: argv.first().map_or_else(String::new, |a| (*a).to_owned()),
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            config: None,
            home: None,
            stored_login: true,
        }),
        cwd: Some(PathBuf::from(r"C:\Users\x\proj")),
        started: None,
    };
    let native = r"C:\Users\x\.local\bin\claude.exe";
    let processes = vec![
        read(1, Some(native), &[native]),
        read(2, None, &[native]),
        read(
            3,
            Some(r"C:\Windows\System32\cmd.exe"),
            &[r"C:\Users\x\AppData\Roaming\npm\CLAUDE.CMD"],
        ),
        read(
            4,
            Some(r"C:\Program Files\nodejs\node.exe"),
            &[
                "node",
                r"C:\Users\x\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js",
            ],
        ),
        read(10, Some(r"C:\Windows\System32\notepad.exe"), &["notepad"]),
        read(
            11,
            Some(r"C:\tools\claudette.exe"),
            &[r"C:\tools\claudette.exe"],
        ),
        read(
            12,
            Some(r"C:\Windows\System32\notepad.exe"),
            &["notepad", r"C:\notes\claude.txt"],
        ),
        read(13, Some(native), &["ugrep"]),
    ];
    let census = claude_census(processes, None).expect("every exec record was read");
    let mut pids: Vec<u32> = census.claudes.iter().map(|c| c.pid).collect();
    pids.sort_unstable();
    assert_eq!(pids, [1, 2, 3, 4]);
}

/// A process the census reads as running the image `exe`, exec'd as `argv` (its exec path
fn running(pid: u32, exe: &Path, argv: &[&str]) -> Process {
    Process {
        pid,
        exe: Some(Exe::At(exe.to_path_buf())),
        exec: Some(Exec {
            path: argv.first().map_or_else(String::new, |a| (*a).to_owned()),
            argv: argv.iter().map(|a| (*a).to_owned()).collect(),
            config: None,
            home: None,
            stored_login: true,
        }),
        cwd: Some(PathBuf::from(r"C:\Users\x\proj")),
        started: None,
    }
}

/// The pids among `processes` [`census_of`] calls Claude Codes, Claude Code's temp root
fn claude_pids(processes: Vec<Process>, scratch: Option<&Scratch>) -> Vec<u32> {
    let census = census_of(processes, None, scratch).expect("every exec record was read");
    let mut pids: Vec<u32> = census.claudes.iter().map(|c| c.pid).collect();
    pids.sort_unstable();
    pids
}

/// Claude Code's temp root `root` given by hand ([`Scratch`]), on a mount table of one local
fn scratch_at(root: &str) -> Scratch {
    use aterm_sysprobe::volumes::{Mount, Table};
    fn spelled(through: &Path) -> Option<PathBuf> {
        through
            .to_str()?
            .eq_ignore_ascii_case(r"\\?\C:\Users\ANDREW~1")
            .then(|| PathBuf::from(r"\\?\C:\Users\Andrew Yates"))
    }
    let c = Mount {
        on: PathBuf::from(r"C:\"),
        from: r"\Device\HarddiskVolume3".to_owned(),
        fstype: "NTFS".to_owned(),
        local: true,
        automounted: false,
        removable: false,
        read_only: false,
        container: Some("Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned()),
    };
    Scratch {
        root: PathBuf::from(root),
        table: std::cell::OnceCell::from(Table {
            mounts: vec![c],
            complete: true,
            firmlinks: Vec::new(),
        }),
        expand: spelled,
    }
}

/// A PROGRAM RUN FROM A CLAUDE CODE SCRATCHPAD IS NO CLAUDE CODE. Windows' Claude Code temp
#[test]
fn a_program_run_from_a_claude_scratchpad_is_no_claude_code() {
    let text = |p: &Path| p.to_str().expect("UTF-8").to_owned();
    let root = r"C:\Users\Andrew Yates\AppData\Local\Temp\claude";
    let scratch = Path::new(root)
        .join("C--Users-x-proj")
        .join("0dead000-5e55-1000")
        .join("scratchpad");
    let aterm = scratch.join("wd").join("bin").join("aterm.exe");
    let verbatim = PathBuf::from(format!(r"\\?\{}", aterm.display()));
    let upper = PathBuf::from(text(&aterm).to_uppercase());
    let nested = scratch.join("claude").join("claude.exe");
    let tool = scratch.join("tool.exe");
    let short = r"C:\Users\ANDREW~1\AppData\Local\Temp\claude\C--Users-x-proj\0dead000-5e55-1000\scratchpad\tool.exe";
    let bash = r"C:/Users//andrew~1/AppData/Local/Temp/claude/C--Users-x-proj/0dead000-5e55-1000/scratchpad/tool.exe";
    let unknown = r"C:\Users\OTHER~1\AppData\Local\Temp\claude\p\s\scratchpad\tool.exe";
    let beside = r"C:\Users\Andrew Yates\AppData\Local\Temp\claude-x\claude.exe";
    let elsewhere = PathBuf::from(r"C:\tools\claude\aterm.exe");
    let native = PathBuf::from(r"C:\Users\x\.local\bin\claude.exe");
    let version = PathBuf::from(r"C:\Users\x\.local\share\claude\versions\2.1.287");
    let node = PathBuf::from(r"C:\Program Files\nodejs\node.exe");
    let cli = r"C:\Users\x\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js";
    let given = scratch_at(root);
    let processes = vec![
        running(1, &aterm, &[&text(&aterm), "harness"]),
        running(2, &verbatim, &[&text(&verbatim)]),
        running(3, &upper, &[&text(&upper)]),
        running(4, &nested, &[&text(&nested)]),
        running(5, &tool, &[short]),
        running(6, &tool, &[bash]),
        running(10, &elsewhere, &[&text(&elsewhere)]),
        running(11, &native, &[&text(&native)]),
        running(12, &version, &["claude"]),
        running(13, &node, &["node", cli]),
        running(14, Path::new(unknown), &[unknown]),
        running(15, Path::new(beside), &["claude"]),
    ];
    assert_eq!(
        claude_pids(processes, Some(&given)),
        [10, 11, 12, 13, 14, 15]
    );
    let alone = running(1, &aterm, &[&text(&aterm)]);
    let exec = alone.exec.clone().expect("its exec record");
    assert!(claude_code(alone.exe.as_ref(), &exec, None));

    let nested_root = scratch_at(r"C:\x\claude\tmp\claude");
    let deep = r"C:\x\claude\tmp\claude\C--Users-x-proj\0dead000-5e55-1000\scratchpad\aterm.exe";
    let above = r"C:\x\claude\aterm.exe";
    let in_temp = r"C:\x\claude\tmp\aterm.exe";
    let processes = vec![
        running(20, Path::new(deep), &[deep, "harness"]),
        running(30, Path::new(above), &[above]),
        running(31, Path::new(in_temp), &[in_temp]),
    ];
    assert_eq!(claude_pids(processes, Some(&nested_root)), [30, 31]);
}

/// THE CENSUS PLACES A SCRATCHPAD BY THIS ENVIRONMENT'S TEMP ROOT ([`claude_census`]'s own,
#[test]
fn the_census_places_a_scratchpad_by_this_environments_temp_root() {
    let scratch = Scratch::read().expect("Windows has a temp root");
    let pad = |root: &Path| {
        root.join("C--Users-x-proj")
            .join("0dead000-5e55-1000")
            .join("scratchpad")
            .join("aterm.exe")
    };
    let long = pad(&scratch.root);
    let as_set = pad(&claude_tmp_root(own_uid()));
    let elsewhere = PathBuf::from(r"C:\tools\claude\aterm.exe");
    let text = |p: &Path| p.to_str().expect("UTF-8").to_owned();
    let processes = vec![
        running(1, &long, &[&text(&long)]),
        running(2, &long, &[&text(&as_set)]),
        running(10, &elsewhere, &[&text(&elsewhere)]),
    ];
    let census = claude_census(processes, None).expect("every exec record was read");
    let pids: Vec<u32> = census.claudes.iter().map(|c| c.pid).collect();
    assert_eq!(
        pids,
        [10],
        "{} / {}",
        scratch.root.display(),
        as_set.display()
    );
}

/// A CLOUD SYNC ROOT THE `$HOME` SEARCH MEETS IS SAID, ONCE EACH. `$HOME`'s child `Synced`, a
#[test]
fn a_cloud_sync_root_the_home_search_meets_is_said_once_each() {
    let tmp = Tmp::new("discover-win-sync-note");
    let home = mkdirs(&tmp.path().join("home"));
    let synced = mkdirs(&home.join("Synced"));
    lay_target(&synced.join("proj-s").join("target"));
    lay_target(&synced.join("proj-t").join("target"));
    let docs = mkdirs(&home.join("work").join("docs"));
    lay_target(&docs.join("target"));
    let onedrive = mkdirs(&home.join("OneDrive"));
    lay_target(&onedrive.join("repo").join("target"));
    lay_target(&home.join("cold").join("target"));
    add_folder_attributes(&home.join("cold"), 0x0000_1000);
    let cold_placeholder = mkdirs(&home.join("cold-placeholder"));
    add_folder_attributes(&cold_placeholder, 0x0000_1000);
    foreign_reparse_point(&cold_placeholder);
    lay_target(&home.join("plain").join("target"));
    let roots: Vec<aterm_dirfd::seam::SyncRoot> = [&synced, &docs, &onedrive]
        .into_iter()
        .map(|root| aterm_dirfd::seam::SyncRoot::register(root).expect("a sync root"))
        .collect();

    let found = discover(&home_roots(&home), Budget::SHIPPED);
    let keys: Vec<String> = found
        .found
        .iter()
        .map(|f| aterm_sysprobe::paths::windows_key(&f.path).expect("keyed"))
        .collect();
    assert_eq!(keys, [key(&home.join("plain").join("target"))], "{found:?}");
    let said: Vec<Option<String>> = found
        .notes
        .iter()
        .filter_map(|n| n.strip_prefix("cargo-targets: "))
        .filter_map(|n| n.strip_suffix(&format!(" {}", super::super::target::IN_SYNC_ROOT)))
        .map(|p| aterm_sysprobe::paths::windows_key(Path::new(p)))
        .collect();
    assert_eq!(
        said,
        [Some(key(&synced)), Some(key(&docs))],
        "{:?}",
        found.notes
    );

    drop(roots);
    let found = discover(&home_roots(&home), Budget::SHIPPED);
    assert!(
        !found.notes.iter().any(|n| n.contains("cloud sync root")),
        "{:?}",
        found.notes
    );
    assert_eq!(found.found.len(), 4, "{found:?}");
}

/// A RUNNING CLAUDE CODE HOLDS ITS PROJECT AS WINDOWS SPELLS ITS FOLDER: the
#[test]
fn a_running_claude_holds_its_project_in_any_windows_spelling() {
    let at = |cwd: &str| RunningClaude {
        pid: 1,
        started: None,
        cwd: Some(PathBuf::from(cwd)),
        profile: None,
        stored_login: true,
    };
    let project = std::ffi::OsStr::new("C--Users-m6-an-aterm");
    for cwd in [
        r"C:\Users\m6-an\aterm",
        r"C:\Users\m6-an\aterm\",
        r"c:\users\M6-AN\Aterm",
        r"\\?\C:\Users\m6-an\aterm",
        r"C:\Users\m6-an\aterm\.claude\worktrees\wt-1\",
        r"C:\Users\m6-an\aterm\.CLAUDE\Worktrees\wt-1\crates",
    ] {
        assert!(at(cwd).may_hold(project), "{cwd}");
    }
    let wt = at(r"C:\Users\m6-an\aterm\.claude\worktrees\wt-1");
    assert!(wt.may_hold(std::ffi::OsStr::new("C--Users-m6-an-aterm-crates")));
    for cwd in [r"C:\Users\m6-an\other", r"C:\Users\m6-an", r"C:\"] {
        assert!(!at(cwd).may_hold(project), "{cwd}");
    }
    assert!(at(r"C:\").may_hold(std::ffi::OsStr::new("C--")));
    let share = std::ffi::OsStr::new("--srv-share-repo");
    for cwd in [r"\\srv\share\repo", r"\\?\UNC\srv\share\repo\"] {
        assert!(at(cwd).may_hold(share), "{cwd}");
    }
    assert!(!at(r"\\?\UNC\srv\share\other").may_hold(share));
}

/// AN OWNER OWNS ONLY WHAT LIES INSIDE ITS FOLDER, IN ANY SPELLING: a build
#[test]
fn an_owner_owns_what_lies_inside_its_folder_in_any_spelling() {
    let tmp = Tmp::new("discover-win-owns");
    let session = mkdirs(&tmp.path().join("proj").join("0dead000-session"));
    let build = lay_target(&session.join("scratchpad").join("target"));
    mkdirs(&tmp.path().join("beside"));
    let upper = PathBuf::from(session.to_string_lossy().to_uppercase());
    let owner = Owner::ClaudeSession {
        session: "0dead000-session".to_owned(),
        session_dir: upper.clone(),
        claude_dirs: Vec::new(),
        home: None,
    };
    let canonical = std::fs::canonicalize(&build).expect("canonical");
    assert!(strictly_inside(&canonical, &upper));
    let itself = std::fs::canonicalize(&session).expect("canonical");
    assert!(!strictly_inside(&itself, &upper));
    let sibling = PathBuf::from(format!("{}-two", upper.display()));
    assert!(!strictly_inside(&sibling.join("target"), &upper));
    assert!(!strictly_inside(&upper.join("TARGET~1"), &upper));
    assert_eq!(owns(&owner, &canonical), Ok(()));
    assert!(owns(&owner, &std::fs::canonicalize(&session).expect("canonical")).is_err());
    assert!(
        owns(
            &owner,
            &std::fs::canonicalize(tmp.path().join("beside")).expect("c")
        )
        .is_err()
    );

    let tool = build.join("debug").join("tool.run");
    std::fs::write(&tool, b"a program").expect("program");
    let own = [("exe", Ok(tool.clone()))];
    let lower = PathBuf::from(build.to_string_lossy().to_lowercase());
    assert_eq!(
        own_use(&own, std::slice::from_ref(&lower))
            .expect("read")
            .map(|(how, _)| how),
        Some("exe")
    );
    let own = [("exe", Ok(tool))];
    assert_eq!(
        own_use(&own, &[tmp.path().join("beside")]).expect("read"),
        None
    );
}

/// A directory's device is its volume's 64-bit serial, the one a reading
#[test]
fn a_windows_directory_is_on_its_volumes_serial() {
    use aterm_sysprobe::volumes;
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let id = volumes::file_id(dir.path(), false).expect("an identity");
    assert_ne!(id.dev, 0);
    assert_eq!(dev_nofollow(dir.path()), Some(id.dev));
    assert_eq!(device_of(dir.path()), Some(id.dev));
    assert_eq!(dev_nofollow(&dir.path().join("missing")), None);
    let table = volumes::table();
    assert!(table.complete, "{table:?}");
    assert_eq!(
        volumes::read_path(dir.path(), &table).map(|r| r.dev),
        Ok(id.dev)
    );
    let placing = placing_of(dir.path(), &table).expect("placed");
    assert_eq!(placing.home_dev, id.dev);
    assert!(placing_of(&dir.path().join("missing"), &table).is_none());
    let cut = volumes::Table {
        complete: false,
        ..table
    };
    assert!(placing_of(dir.path(), &cut).is_none());
}

/// A RUNNING WINDOWS PROCESS IS READ AS THE CENSUS READS IT, its exec record
#[test]
fn a_running_claude_code_is_read_whole_and_homed_by_its_userprofile() {
    use std::os::windows::process::CommandExt as _;
    let tmp = Tmp::new("discover-win-census");
    let system = super::super::win::system_directory().expect("System32");
    let program = mkdirs(&tmp.path().join("claude")).join("cmd.exe");
    std::fs::copy(system.join("cmd.exe"), &program).expect("a copy of cmd.exe");
    let project = mkdirs(&tmp.path().join("proj"));
    let profile = tmp.path().join("up");
    let mut child = aterm_uds::spawnfd::command(&program)
        .args(["/d", "/q", "/k"])
        .raw_arg("rem")
        .current_dir(&project)
        .env("USERPROFILE", &profile)
        .env("HOME", tmp.path().join("home"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in");
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let read = loop {
        let read = read_process(child.id(), Some(tmp.path()), Scratch::read().as_ref());
        let whole = read
            .as_ref()
            .is_some_and(|p| p.exec.is_some() && p.cwd.is_some());
        if whole || std::time::Instant::now() >= deadline {
            break read;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let pid = child.id();
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    let process = read.expect("it ran");
    let exec = process.exec.clone().expect("its exec record");
    assert_eq!(exec.home.as_deref(), profile.to_str(), "{exec:?}");
    assert_eq!(exec.config, None);
    assert_eq!(
        std::fs::canonicalize(&exec.path).ok(),
        std::fs::canonicalize(&program).ok(),
        "{exec:?}"
    );
    assert_eq!(exec.argv.len(), 2, "argv[0] and argv[1] alone: {exec:?}");
    let cwd = process.cwd.clone().expect("its working directory");
    assert_eq!(
        std::fs::canonicalize(&cwd).ok(),
        std::fs::canonicalize(&project).ok(),
        "{}",
        cwd.display()
    );
    assert!(process.started.is_some(), "its start");
    let census = claude_census([process], Some(tmp.path())).expect("read whole");
    let claudes: Vec<u32> = census.claudes.iter().map(|c| c.pid).collect();
    assert_eq!(claudes, [pid]);
    let name = project_name(project.to_str().expect("UTF-8"));
    assert!(census.claudes[0].may_hold(std::ffi::OsStr::new(&name)));
    assert_eq!(census.dirs, [profile.join(".claude")]);
}

/// A MEASUREMENT, run by hand (`-- --ignored --nocapture
#[test]
#[ignore = "a measurement, run by hand"]
fn the_claude_census_of_this_account() {
    let home = aterm_types::dirs::home_dir();
    let pids = aterm_uds::holders::uid_pids().expect("this account's processes");
    let scratch = Scratch::read();
    let (mut read, mut gone, mut unread) = (0, 0, Vec::new());
    for &pid in &pids {
        match read_process(pid, home.as_deref(), scratch.as_ref()) {
            None => gone += 1,
            Some(Process {
                exec: Some(exec),
                exe,
                cwd,
                ..
            }) => {
                read += 1;
                if claude_code(exe.as_ref(), &exec, scratch.as_ref()) {
                    let dir = own_dir(&exec, cwd.as_deref(), home.as_deref());
                    eprintln!("  claude {pid} {exe:?} in {cwd:?}: {dir:?}");
                }
            }
            Some(Process { exe, .. }) => unread.push((pid, exe)),
        }
    }
    eprintln!(
        "{} processes: {read} read, {gone} gone, {} not read",
        pids.len(),
        unread.len()
    );
    for (pid, exe) in &unread {
        let told = exe.as_ref().is_some_and(|e| told(None, e));
        eprintln!("  not read: {pid} {exe:?} (told: {told})");
    }
    let started = std::time::Instant::now();
    let census = running_claude_census(home.as_deref());
    let took = started.elapsed();
    match census {
        Ok(running) => eprintln!(
            "census Ok in {took:?}: {} Claude Codes {:?}, dirs {:?}",
            running.claudes.len(),
            running
                .claudes
                .iter()
                .map(|c| (c.pid, c.cwd.clone()))
                .collect::<Vec<_>>(),
            running.dirs
        ),
        Err(why) => eprintln!("census refused in {took:?}: {why}"),
    }
}
