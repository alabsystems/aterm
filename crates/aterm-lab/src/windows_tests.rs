// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// `--no-wait` and `--admit` before the verb are the lane's, in either order; after it they
/// are the verb's own words. Negative control: `--admit` with another word than `wait` or
/// `now` is a usage error naming it.
#[test]
fn the_lane_waits_unless_no_wait_comes_before_the_verb() {
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let asked = words(&["--no-wait", "exec", "--", "cargo", "test"]);
    assert_eq!(
        lane_flags(&asked, Admit::Wait).expect("flags"),
        (&asked[1..], Wait::No, Admit::Wait),
        "--no-wait before the verb is the lane's"
    );
    let asked = words(&["exec", "--", "x", "--no-wait", "--admit", "now"]);
    assert_eq!(
        lane_flags(&asked, Admit::Wait).expect("flags"),
        (&asked[..], Wait::Block, Admit::Wait),
        "after the verb it is the verb's own word"
    );
    let asked = words(&["--admit", "now", "--no-wait", "smoke"]);
    assert_eq!(
        lane_flags(&asked, Admit::Wait).expect("flags"),
        (&asked[3..], Wait::No, Admit::Now)
    );
    let refused =
        lane_flags(&words(&["--admit", "later", "smoke"]), Admit::Wait).expect_err("usage");
    assert_eq!(refused.code, 2);
    assert!(
        refused
            .msg
            .as_deref()
            .is_some_and(|m| m.contains("'later'")),
        "{:?}",
        refused.msg
    );
}

/// The lab takes a run's `--admit` out of its words before the lane reads them
/// (`admission::take`), so the lane's boot is admitted as the run is: `lab windows --admit now
/// smoke` boots at once. Negative control: read without the run's mode, the same words would
/// wait for disk.
#[test]
fn the_runs_admit_is_the_lanes_boot_claim() {
    let mut asked: Vec<String> = ["--admit", "now", "--no-wait", "smoke"]
        .map(String::from)
        .to_vec();
    let run = crate::admission::strip(&mut asked).expect("the run's mode");
    assert_eq!(
        lane_flags(&asked, run).expect("flags"),
        (&asked[1..], Wait::No, Admit::Now)
    );
    assert_eq!(
        lane_flags(&asked, Admit::Wait).expect("flags").2,
        Admit::Wait,
        "the lane alone no longer sees the flag"
    );
}

#[cfg(unix)]
#[test]
fn native_tools_keep_qemu_image_and_firmware_in_the_selected_family() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let native = tmp.path().join("native prefix");
    let existing = tmp.path().join("existing");
    let candidates = tool_prefixes(Some(&native));
    assert_eq!(candidates[0], native);
    assert_eq!(candidates[1..], PREFIXES.map(PathBuf::from));
    assert_eq!(tool_prefixes(None), PREFIXES.map(PathBuf::from));
    assert_eq!(tool_prefixes(Some(Path::new(PREFIXES[0]))).len(), 2);
    let put = |root: &Path, name: &str, executable: bool| {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Resolution must never execute these deliberately invalid programs.
        std::fs::write(&path, b"not an executable image").unwrap();
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )
        .unwrap();
    };
    for root in [&native, &existing] {
        for name in ["bin/qemu-system-aarch64", "bin/qemu-img", "bin/swtpm"] {
            put(root, name, true);
        }
        for name in [
            "share/qemu/edk2-aarch64-code.fd",
            "share/qemu/edk2-arm-vars.fd",
        ] {
            put(root, name, false);
        }
    }
    let roots = [native.as_path(), existing.as_path()];
    let tools = Tools::locate_in(&roots).unwrap();
    assert_eq!(tools.qemu, native.join("bin/qemu-system-aarch64"));
    assert_eq!(tools.qemu_img, native.join("bin/qemu-img"));
    assert_eq!(tools.code, native.join("share/qemu/edk2-aarch64-code.fd"));
    assert_eq!(tools.vars, native.join("share/qemu/edk2-arm-vars.fd"));
    assert_eq!(tools.swtpm, native.join("bin/swtpm"));
    std::fs::remove_file(&tools.vars).unwrap();
    let fail = Tools::locate_in(&roots).unwrap_err();
    assert!(
        fail.msg
            .unwrap()
            .contains(&tools.vars.display().to_string())
    );
    put(&native, "share/qemu/edk2-arm-vars.fd", false);
    std::fs::remove_file(&tools.qemu_img).unwrap();
    let fail = Tools::locate_in(&roots).unwrap_err();
    assert!(
        fail.msg
            .unwrap()
            .contains(&tools.qemu_img.display().to_string())
    );
    // A root with no selected QEMU can yield to the complete existing family.
    std::fs::remove_file(&tools.qemu).unwrap();
    let tools = Tools::locate_in(&roots).unwrap();
    assert_eq!(tools.qemu, existing.join("bin/qemu-system-aarch64"));
    assert_eq!(tools.qemu_img, existing.join("bin/qemu-img"));
    assert_eq!(tools.vars, existing.join("share/qemu/edk2-arm-vars.fd"));
}

/// A missing QEMU or swtpm is named with where the lane looked, never with an install
/// command: aterm provides neither (init design §6.1 row 26). The lookup's own NOT RUN
/// says it, over scratch prefixes: no QEMU in either, then QEMU in the second and no
/// swtpm. NEGATIVE CONTROL: with both found, the lookup goes on to the firmware, which
/// the scratch prefix lacks, and runs nothing.
#[cfg(unix)]
#[test]
fn a_missing_qemu_or_swtpm_is_named_without_an_install_command() {
    use std::os::unix::fs::PermissionsExt as _;
    let default: Vec<String> = PREFIXES.iter().map(|p| format!("{p}/bin")).collect();
    for (bin, what) in [("qemu-system-aarch64", "QEMU"), ("swtpm", "swtpm")] {
        let why = missing(bin, what, &PREFIXES.map(Path::new));
        assert!(
            why.starts_with(&format!("no {bin} in {}: ", default.join(" or ")))
                && why.contains("aterm provides none"),
            "{why}"
        );
        assert!(
            !why.contains("install"),
            "no remedy installs anything: {why}"
        );
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
    let prefixes = [one.as_path(), two.as_path()];
    let put = |path: &Path| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\nexit 9\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let refusal = |prefixes: &[&Path]| match Tools::find_in(prefixes) {
        Err(fail) => (fail.code, fail.msg.unwrap_or_default()),
        Ok(tools) => panic!("{tools:?}"),
    };
    let looked = format!(
        "{} or {}",
        one.join("bin").display(),
        two.join("bin").display()
    );
    let (code, msg) = refusal(&prefixes);
    assert_eq!(code, NOT_RUN);
    assert_eq!(
        msg,
        format!(
            "NOT RUN: {}",
            missing("qemu-system-aarch64", "QEMU", &prefixes)
        )
    );
    assert!(msg.contains(&looked) && !msg.contains("brew"), "{msg}");
    put(&two.join("bin/qemu-system-aarch64"));
    let (_, msg) = refusal(&prefixes);
    assert_eq!(
        msg,
        format!("NOT RUN: {}", missing("swtpm", "swtpm", &prefixes))
    );
    assert!(!msg.contains("brew"), "{msg}");
    put(&one.join("bin/swtpm"));
    let (code, msg) = refusal(&prefixes);
    assert_eq!(code, NOT_RUN);
    assert!(
        msg.starts_with("NOT RUN: QEMU's firmware ")
            && msg.contains(&two.join("share/qemu").display().to_string()),
        "{msg}"
    );
}

#[test]
fn the_page_names_its_edition_and_each_isos_digest() {
    let page = "<select><option value=\"\" selected>Select</option>\
                <option value=\"3816\">Windows 11 (multi-edition ISO for Arm64)</option></select>\
                <table><tr><td>English International 64-bit</td><td>500BCB6F272B7C455156470EA06816F558F13B885723F84EE9EAB7534C7DEE61</td></tr>\
                <tr><td>English 64-bit</td>\n  <td>8992F2D2CFD2FA647E579E9EB31FC21F34DA8333C29B6170FDE17D2ED6623E9C</td></tr></table>";
    assert_eq!(edition_id(page).as_deref(), Some("3816"));
    assert_eq!(
        published_sha256(page, ISO_DIGEST_ROW).as_deref(),
        Some("8992f2d2cfd2fa647e579e9eb31fc21f34da8333c29b6170fde17d2ed6623e9c")
    );
    assert_eq!(
        published_sha256("<td>English 64-bit</td><td>soon</td>", ISO_DIGEST_ROW),
        None
    );
    assert_eq!(edition_id("<option value=\"12\">Other</option>"), None);
}

#[test]
fn the_fingerprint_request_is_read_from_mdt_js() {
    let js = "window.dfp={};i=\"https://ov-df.microsoft.com/?session_id=s&CustomerId=c&PageId=si&w=8DF2\";\
              i+=\"&mdt=\"+r;i+=\"&rticks=\"+1792536204805;false&&(i+=1)";
    assert_eq!(
        fingerprint_request(js),
        Some((
            "https://ov-df.microsoft.com/?session_id=s&CustomerId=c&PageId=si&w=8DF2".into(),
            "1792536204805".into()
        ))
    );
    assert_eq!(fingerprint_request("nothing here"), None);
}

#[test]
fn the_download_service_answers_a_sku_and_a_link_or_its_error() {
    let skus =
        r#"{"Skus":[{"Id":"27163","Language":"Arabic"},{"Id":"27162","Language":"English"}]}"#;
    assert_eq!(sku_id(skus, "English").as_deref(), Some("27162"));
    assert_eq!(sku_id(skus, "Klingon"), None);
    let link = r#"{"ProductDownloadOptions":[{"Uri":"https://software.download.prss.microsoft.com/dbazure/Win11_arm64.iso?t=x&P1=2"}]}"#;
    assert_eq!(
        download_link(link).as_deref(),
        Ok("https://software.download.prss.microsoft.com/dbazure/Win11_arm64.iso?t=x&P1=2")
    );
    let refused = r#"{"Errors":[{"Key":"ErrorSettings.SentinelReject","Value":"Sentinel marked this request as rejected.","Type":8}]}"#;
    assert_eq!(
        download_link(refused),
        Err("Sentinel marked this request as rejected.".into())
    );
    assert_eq!(
        iso_name("https://x/y/Win11_arm64.iso?t=1").as_deref(),
        Some("Win11_arm64.iso")
    );
    assert_eq!(iso_name("https://x/y/../evil?t=1"), None);
    assert_eq!(iso_name("https://x/y/a b.iso"), None);
}

fn layout() -> Layout {
    Layout::under(Path::new("/Users//someone"))
}

fn tools() -> Tools {
    Tools {
        qemu: "/opt/homebrew/bin/qemu-system-aarch64".into(),
        qemu_img: "/opt/homebrew/bin/qemu-img".into(),
        swtpm: "/opt/homebrew/bin/swtpm".into(),
        code: "/opt/homebrew/share/qemu/edk2-aarch64-code.fd".into(),
        vars: "/opt/homebrew/share/qemu/edk2-arm-vars.fd".into(),
        tpm_ppi: false,
    }
}

#[test]
fn a_download_makes_the_cache_folder_a_first_up_has_not_got() {
    let root = aterm_tempfile::tempdir().expect("scratch");
    let source = root.path().join("source.iso");
    std::fs::write(&source, b"the image").expect("source");
    let dest = root.path().join("Library/Caches/aterm/windows-lane/w.iso");
    let url = format!("file://{}", source.display());
    assert_eq!(curl_file(&url, &dest, false), Ok(()));
    assert_eq!(std::fs::read(&dest).expect("downloaded"), b"the image");
    assert!(!dest.with_extension("part").exists());
}

#[test]
fn the_vm_listens_on_loopback_alone_and_shows_nothing() {
    let l = layout();
    let a = qemu_args(
        &l,
        &tools(),
        &Boot {
            install: None,
            port: 52222,
            vnc: None,
        },
    );
    let joined = a.join(" ");
    assert!(joined.contains("hostfwd=tcp:127.0.0.1:52222-:22"));
    assert!(joined.contains("-display none"));
    assert!(!joined.contains("-vnc"));
    assert!(joined.contains("-machine virt,accel=hvf -cpu host -smp 6 -m 8G"));
    assert!(!joined.contains("id=install"));
    assert!(
        joined.contains("-daemonize -pidfile /Users//someone/.aterm/lab/windows-lane/run/qemu.pid")
    );
    assert!(joined.contains("-device tpm-tis-device,tpmdev=tpm0 "));
    let ppi = Tools {
        tpm_ppi: true,
        ..tools()
    };
    let boot = Boot {
        install: None,
        port: 1,
        vnc: None,
    };
    assert!(
        qemu_args(&l, &ppi, &boot)
            .join(" ")
            .contains("-device tpm-tis-device,tpmdev=tpm0,ppi=off "),
        "a QEMU with the switch turns the TPM's physical-presence RAM off"
    );
    let with = qemu_args(
        &l,
        &tools(),
        &Boot {
            install: Some(("/c/win.iso".into(), l.media())),
            port: 1,
            vnc: Some(5901),
        },
    )
    .join(" ");
    assert!(with.contains("-vnc 127.0.0.1:1"));
    assert!(with.contains("file=/c/win.iso -device usb-storage,drive=install,bootindex=0"));
    assert!(with.contains("usb-storage,drive=answers"));
    assert!(lossy(&l.qmp()).len() < 104);
    assert!(lossy(&l.tpm_sock()).len() < 104);
    assert!(swtpm_args(&l).join(" ").contains("--terminate"));
    let birth = aterm_uds::process::ProcessBirth {
        seconds: 12,
        microseconds: 34,
    };
    let later = aterm_uds::process::ProcessBirth {
        seconds: 12,
        microseconds: 35,
    };
    let argv = |bin: &Path, args: &[String]| {
        std::iter::once(bin.as_os_str().to_os_string())
            .chain(args.iter().map(OsString::from))
            .collect::<Vec<_>>()
    };
    let qa = argv(&tools().qemu, &a);
    let found = captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &qa, Some(birth));
    assert_eq!(found, Some(DaemonProcess { pid: 123, birth }));
    assert_eq!(
        captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &qa, Some(later)),
        None
    );
    assert_eq!(
        captured_daemon(&l, Daemon::Qemu, 123, None, &qa, None),
        None
    );
    assert_eq!(
        captured_daemon(&l, Daemon::Qemu, 1, Some(birth), &qa, Some(birth)),
        None
    );
    let foreign = Layout::under(Path::new("/Users//another"));
    assert_eq!(
        captured_daemon(&foreign, Daemon::Qemu, 123, Some(birth), &qa, Some(birth)),
        None
    );
    let mut wrong = qa.clone();
    let qmp = wrong
        .iter()
        .position(|w| w == OsStr::new("-qmp"))
        .expect("QMP argument");
    wrong[qmp + 1] = OsString::from("unix:/another/qmp.sock,server=on,wait=off");
    assert_eq!(
        captured_daemon(&l, Daemon::Qemu, 123, Some(birth), &wrong, Some(birth)),
        None
    );
    let ta = argv(&tools().swtpm, &swtpm_args(&l));
    assert!(captured_daemon(&l, Daemon::Tpm, 124, Some(birth), &ta, Some(birth)).is_some());
    assert_eq!(
        captured_daemon(&foreign, Daemon::Tpm, 124, Some(birth), &ta, Some(birth)),
        None
    );
    assert_eq!(
        captured_daemon(&l, Daemon::Tpm, 124, Some(birth), &ta, Some(later)),
        None
    );
}

/// The stand-in QEMU ([`a_qemu_stand_in`]).
const QEMU_STAND_IN: &str = "windows::tests::a_qemu_stand_in";
/// Set for the stand-in QEMU: how its guest behaves ([`SHUTS_DOWN`], [`HUNG`], [`SPINNING`],
/// [`WORKING`]).
const QEMU_ENV: &str = "ATERM_LAB_TEST_QEMU";
/// A guest that shuts down at ACPI's power button, its drives and screen still.
const SHUTS_DOWN: &str = "shuts-down";
/// A guest hung in its firmware: it ignores the power button, its drives and screens still.
const HUNG: &str = "hung";
/// A guest hung with Windows' boot spinner turning: it ignores the power button and its
/// drives are still, but its screens change at every look.
const SPINNING: &str = "spinning";
/// A guest at work that ignores the power button: its drives have done more at every look,
/// and its screens change.
const WORKING: &str = "working";
/// The stand-in booter ([`a_booter_stand_in`]).
const BOOTER_STAND_IN: &str = "windows::tests::a_booter_stand_in";
/// The stand-in booter's layout home, verb and whether it records its boot, tabs between.
const BOOTER_ENV: &str = "ATERM_LAB_TEST_WINDOWS_BOOTER";
/// What the stand-in booter says once its boot is recorded, for its QEMU to start.
const BOOTING: &str = "the stand-in starts QEMU";
/// What the stand-in booter says, with its QEMU's pid, once the VM is up.
const BOOTED: &str = "the stand-in booted QEMU pid";

/// Not a test alone: run as [`QEMU_STAND_IN`] with `argv[0]` a `qemu-system-aarch64` and the
/// lane's own QEMU words ([`qemu_args`]) after libtest's `--`, it is the VM's QEMU as the
/// lane identifies one ([`daemon_words`]): its pid in `-pidfile`, QMP on `-qmp`'s socket, a
/// `screendump` written to the file it names (the same bytes each time, or new ones each
/// time for a [`SPINNING`] or [`WORKING`] guest), `query-blockstats` answered with its
/// drives' operations (one more at each ask for a [`WORKING`] guest), gone with its pid file
/// at `quit`, and at `system_powerdown` only for a guest that [`SHUTS_DOWN`]. Its one-minute
/// hang detector ends it when nobody stops it.
#[test]
fn a_qemu_stand_in() {
    let Ok(guest) = std::env::var(QEMU_ENV) else {
        return;
    };
    let words: Vec<String> = std::env::args().collect();
    let after = |key: &str| {
        words
            .windows(2)
            .find(|w| w[0] == key)
            .map(|w| w[1].clone())
            .unwrap_or_else(|| panic!("{key} in {words:?}"))
    };
    let pid_file = after("-pidfile");
    let qmp = after("-qmp");
    let qmp = qmp
        .strip_prefix("unix:")
        .and_then(|q| q.split(',').next())
        .expect("a unix QMP socket")
        .to_owned();
    let listener = std::os::unix::net::UnixListener::bind(&qmp).expect("QMP's socket");
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).expect("the pid file");
    let mut shots = 0u32;
    let mut operations = 12u64;
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let stream = match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(e) => panic!("QMP accept: {e}"),
        };
        let mut reply = stream.try_clone().expect("a writer");
        let _ = reply.write_all(b"{\"QMP\": {\"version\": {}, \"capabilities\": []}}\n");
        let mut stopped = false;
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if line.contains("\"screendump\"") {
                let asked: aterm_json::Value = aterm_json::from_str(&line).expect("QMP JSON");
                let file = asked
                    .get("arguments")
                    .and_then(|a| a.get("filename"))
                    .and_then(aterm_json::Value::as_str)
                    .expect("a screendump's file");
                shots += 1;
                let screen = if guest == SPINNING || guest == WORKING {
                    format!("Start boot option, the spinner at frame {shots}")
                } else {
                    "Start boot option".to_owned()
                };
                std::fs::write(file, screen).expect("the screen");
            }
            if line.contains("\"query-blockstats\"") {
                if guest == WORKING {
                    operations += 1;
                }
                let stats = format!(
                    "{{\"return\": [{{\"device\": \"disk\", \"stats\": {{\"rd_operations\": \
                     {operations}, \"wr_operations\": 3, \"flush_operations\": 1}}}}, \
                     {{\"device\": \"pflash1\", \"stats\": {{\"rd_operations\": 0, \
                     \"wr_operations\": 0, \"flush_operations\": 0}}}}]}}\n"
                );
                let _ = reply.write_all(stats.as_bytes());
                continue;
            }
            let _ = reply.write_all(b"{\"return\": {}}\n");
            if line.contains("\"quit\"")
                || (line.contains("system_powerdown") && guest == SHUTS_DOWN)
            {
                stopped = true;
                break;
            }
        }
        if stopped {
            break;
        }
    }
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(&qmp);
}

/// Not a test alone: run as [`BOOTER_STAND_IN`], it boots the VM of the layout under
/// [`BOOTER_ENV`]'s home as a lane run of its verb does: the boot recorded beside the VM
/// ([`crate::vm_admit::Booter`]), then the VM's QEMU started, and once it is up its pid
/// recorded as the boot's VM process ([`crate::vm_admit::record_vm`]). The QEMU is
/// [`boot_by`]'s, started when this booter says [`BOOTING`]: `-daemonize` leaves the real one
/// to launchd, never to the run. Told not to record, it boots as a lab that keeps no records
/// does. It says [`BOOTED`] with QEMU's pid; then a one-shot run dies by its own `SIGKILL`,
/// and `up` ends as `up` ends ([`Up::keep`]: the VM kept).
#[test]
fn a_booter_stand_in() {
    let Ok(spec) = std::env::var(BOOTER_ENV) else {
        return;
    };
    let mut words = spec.split('\t');
    let (home, verb) = words.next().zip(words.next()).expect("a home and a verb");
    let records = words.next() != Some("unrecorded");
    let l = Layout::under(Path::new(home));
    if records {
        crate::vm_admit::Booter::me(verb, Path::new("/checkouts/booter"))
            .record(&l.booter())
            .expect("the boot recorded");
    }
    std::fs::create_dir_all(l.run()).expect("the run directory");
    println!("{BOOTING}");
    let _ = std::io::stdout().flush();
    // The one-minute hang detector: the stand-in writes its pid file and opens QMP at once.
    let end = Instant::now() + Duration::from_secs(60);
    let pid = loop {
        if let Ok(Some(pid)) = running(&l)
            && l.qmp().exists()
        {
            break pid;
        }
        assert!(Instant::now() < end, "the stand-in QEMU did not come up");
        std::thread::sleep(Duration::from_millis(20));
    };
    if records {
        crate::vm_admit::record_vm(Some(&l.booter()), qemu(&l));
    }
    println!("{BOOTED} {pid}");
    let _ = std::io::stdout().flush();
    if verb.ends_with(" up") {
        if records {
            crate::vm_admit::settle(Some(&l.booter()), true, false);
        }
        return;
    }
    aterm_uds::process::kill_self_now()
}

/// [`a_qemu_stand_in`] as the QEMU of the VM in `l`, its guest one that shuts down
/// ([`SHUTS_DOWN`]).
fn qemu_stand_in(l: &Layout) -> aterm_uds::spawnfd::Child {
    qemu_stand_in_as(l, SHUTS_DOWN)
}

/// [`a_qemu_stand_in`] as the QEMU of the VM in `l`, its guest as `guest` says, with the
/// lane's own QEMU words: this test's child, which nothing but this test reaps.
fn qemu_stand_in_as(l: &Layout, guest: &str) -> aterm_uds::spawnfd::Child {
    let words = qemu_args(
        l,
        &tools(),
        &Boot {
            install: None,
            port: 1,
            vnc: None,
        },
    );
    aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
        .arg0(&tools().qemu)
        .args([
            "--exact",
            QEMU_STAND_IN,
            "--nocapture",
            "--test-threads=1",
            "--",
        ])
        .args(&words)
        .env(QEMU_ENV, guest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in QEMU starts")
}

/// A stopped QEMU of [`boot_by`]'s: held where a stop raced launchd's reap on a loaded Mac,
/// exited and not yet reaped, until this reaps it.
fn reaped(vm: &mut aterm_uds::spawnfd::Child) {
    use aterm_uds::process::{ProcessPresence, process_presence};
    let pid = i32::try_from(vm.id()).expect("a pid");
    assert_eq!(
        process_presence(pid),
        ProcessPresence::Exited,
        "QEMU pid {pid} has exited and is held unreaped"
    );
    let status = vm.wait().expect("QEMU is reaped");
    assert!(status.success(), "QEMU pid {pid}: {status}");
}

/// Boot the VM of the layout under `home` by [`a_booter_stand_in`] running `verb`, recording
/// the boot when `records`, its QEMU [`qemu_stand_in`] started once it says [`BOOTING`]: the
/// booter's pid, how it ended, its QEMU and that QEMU's pid. Nothing reaps the QEMU until the
/// test does ([`reaped`]), so a stop's wait for it always meets it exited and not yet reaped.
fn boot_by(
    home: &Path,
    verb: &str,
    records: bool,
) -> (
    u32,
    std::process::ExitStatus,
    aterm_uds::spawnfd::Child,
    i32,
) {
    let mut booter = aterm_uds::spawnfd::command(std::env::current_exe().expect("this test"))
        .args([
            "--exact",
            BOOTER_STAND_IN,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            BOOTER_ENV,
            format!(
                "{}\t{verb}\t{}",
                home.display(),
                if records { "recorded" } else { "unrecorded" }
            ),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the stand-in booter starts");
    let pid = booter.id();
    let mut vm = None;
    let mut said = Vec::new();
    // Read to its end, so the booter never writes into a closed pipe.
    for line in BufReader::new(booter.stdout.take().expect("its stdout"))
        .lines()
        .map_while(Result::ok)
    {
        if line.contains(BOOTING) && vm.is_none() {
            vm = Some(qemu_stand_in(&Layout::under(home)));
        }
        said.push(line);
    }
    let status = booter.wait().expect("the booter is reaped");
    let vm = vm.unwrap_or_else(|| panic!("the booter started no QEMU ({status}): {said:?}"));
    let qemu = i32::try_from(vm.id()).expect("a pid");
    let booted = said.iter().find_map(|l| {
        l.split_once(BOOTED)
            .and_then(|(_, p)| p.split_whitespace().next()?.parse().ok())
    });
    assert_eq!(booted, Some(qemu), "the booter's QEMU ({status}): {said:?}");
    (pid, status, vm, qemu)
}

/// A VM a one-shot run booted and left as it died (killed by its own `SIGKILL` after the
/// boot, QEMU still running) is stopped by the next run's bring-up through `down`'s own path
/// (QMP's power button here: no SSH port was recorded), which names the dead run; when it
/// ends, that QEMU is gone, and so is its record. Every stop here meets its QEMU exited and
/// not yet reaped ([`boot_by`] holds it there), as a loaded Mac once let one meet a QEMU
/// launchd had yet to reap and refuse it as foreign: such a QEMU is gone, and so is one a
/// pid file still names, as a QEMU ended by a signal leaves it. Negative controls: once such
/// a VM was stopped as a lab that keeps no records stops it (its record left behind) and
/// booted again by one, the record describes a boot that is over, and the VM is adopted;
/// and a VM `up` left is adopted with its QEMU running, until `down` stops it.
#[test]
fn a_vm_a_dead_one_shot_run_left_is_stopped_and_one_up_left_is_adopted() {
    use std::os::unix::process::ExitStatusExt as _;
    // Under /tmp: QMP's socket path must fit a sockaddr_un.
    let home = aterm_tempfile::Builder::new()
        .prefix("w")
        .tempdir_in("/tmp")
        .expect("a scratch home");
    let l = Layout::under(home.path());
    assert!(lossy(&l.qmp()).len() < 104, "{}", l.qmp().display());

    let (booter, status, mut vm, qemu) = boot_by(home.path(), "lab windows smoke", true);
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    assert_eq!(running(&l), Ok(Some(qemu)), "the dead run's QEMU runs on");
    let mut said = Vec::new();
    let adopted_pid =
        adopted(&l, &tools(), &mut |line| said.push(line.to_owned())).map_err(|f| f.msg);
    assert_eq!(adopted_pid, Ok(None), "{said:?}");
    assert!(
        said.first().is_some_and(|line| line.starts_with(&format!(
            "stopping aterm-windows left by dead run pid {booter} (lab windows smoke, \
             /checkouts/booter, since "
        )) && line.ends_with(&format!("(QEMU pid {qemu})"))),
        "{said:?}"
    );
    assert_eq!(running(&l), Ok(None));
    reaped(&mut vm);
    assert!(!l.booter().exists(), "the stopped VM's record went with it");

    let (_, status, mut vm, qemu) = boot_by(home.path(), "lab windows smoke", true);
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status}");
    stop(&l, None, Patience::STOP).expect("stopped as a lab that keeps no records stops it");
    std::fs::write(l.qemu_pid(), format!("{qemu}\n")).expect("the pid file");
    assert_eq!(
        running(&l),
        Ok(None),
        "a pid file names QEMU pid {qemu}, which has exited"
    );
    std::fs::remove_file(l.qemu_pid()).expect("the pid file goes");
    reaped(&mut vm);
    assert!(l.booter().exists(), "the dead run's record is left behind");
    let (_, status, mut vm, qemu) = boot_by(home.path(), "lab windows up", false);
    assert!(status.success(), "{status}");
    let mut said = Vec::new();
    let adopted_pid =
        adopted(&l, &tools(), &mut |line| said.push(line.to_owned())).map_err(|f| f.msg);
    assert_eq!(
        adopted_pid,
        Ok(Some(qemu)),
        "a VM booted since outside the records is not the dead run's: {said:?}"
    );
    assert!(said.is_empty(), "{said:?}");
    down(&l, &tools(), Patience::STOP).expect("down stops it");
    reaped(&mut vm);

    let (_, status, mut vm, qemu) = boot_by(home.path(), "lab windows up", true);
    assert!(status.success(), "{status}");
    let mut said = Vec::new();
    let adopted_pid =
        adopted(&l, &tools(), &mut |line| said.push(line.to_owned())).map_err(|f| f.msg);
    assert_eq!(adopted_pid, Ok(Some(qemu)), "{said:?}");
    assert!(said.is_empty(), "{said:?}");
    assert_eq!(vm.exited().ok(), Some(false), "the VM up left runs on");
    down(&l, &tools(), Patience::STOP).expect("down stops it");
    reaped(&mut vm);
}

#[test]
fn a_copy_sends_what_changed_and_removes_what_went() {
    let e = |p: &str, size: u64, t: &str| Entry {
        path: p.into(),
        size,
        mtime: t.into(),
    };
    let here = vec![
        e("a.rs", 1, "10.000000000"),
        e("b.rs", 2, "20.000000000"),
        e("d/c.rs", 3, "30.5"),
    ];
    let there = parse_manifest(&manifest(&[
        e("a.rs", 1, "10.000000000"),
        e("b.rs", 2, "19.000000000"),
        e("gone.rs", 9, "1.0"),
    ]));
    let (send, remove) = plan_copy(&here, &there);
    assert_eq!(send, vec!["b.rs".to_string(), "d/c.rs".to_string()]);
    assert_eq!(remove, vec!["gone.rs".to_string()]);
    assert_eq!(plan_copy(&here, &BTreeMap::new()).0.len(), 3);
    assert_eq!(
        parse_manifest("1 10.0 a b.rs\r\n"),
        BTreeMap::from([("a b.rs".to_string(), (1, "10.0".to_string()))])
    );
}

#[test]
fn a_copy_prunes_the_folders_its_removals_empty_deepest_first() {
    let e = |p: &str| Entry {
        path: p.into(),
        size: 1,
        mtime: "1.0".into(),
    };
    let here = vec![e("Cargo.toml"), e("crates/kept/src/lib.rs"), e("docs/a.md")];
    let remove: Vec<String> = [
        "crates/gone/Cargo.toml",
        "crates/gone/src/lib.rs",
        "crates/gone/src/deep/mod.rs",
        "crates/kept/src/old.rs",
        "docs/b.md",
        "top.rs",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        emptied_dirs(&here, &remove),
        ["crates/gone/src/deep", "crates/gone/src", "crates/gone"]
    );
    assert!(emptied_dirs(&here, &[]).is_empty());
    assert_eq!(
        emptied_dirs(&[], &["a/b/c.rs".to_string()]),
        ["a/b", "a"],
        "with nothing kept, every folder goes, the root's own excepted"
    );
}

#[test]
fn an_inf_installs_what_its_source_disks_files_name() {
    let inf = "[Version]\r\nSignature = \"$WINDOWS NT$\"\r\n\r\n[SourceDisksFiles]\r\n\
               netkvm.sys  = 1,,\r\nnetkvmp.exe = 1,, ; the config tool\r\n\r\n\
               [SourceDisksFiles.arm64]\r\nother.sys = 1\r\n[Strings]\r\nx = y\r\n";
    assert_eq!(
        inf_sources(inf),
        ["netkvm.sys", "netkvmp.exe", "other.sys"].map(String::from)
    );
    assert!(inf_sources("[Strings]\nnetkvm.sys = 1\n").is_empty());
}

#[test]
fn a_hard_linked_iso_member_is_read_through_its_target() {
    assert_eq!(
        link_target(
            "-r-xr-xr-x  2 0  0  0 Jul 22 17:49 NetKVM/w11/ARM64/netkvm.inf link to NetKVM/2k25/ARM64/netkvm.inf\n"
        ),
        Some("NetKVM/2k25/ARM64/netkvm.inf")
    );
    assert_eq!(
        link_target("-r-xr-xr-x  1 0  0  18403 Jul 22 17:48 NetKVM/w10/ARM64/netkvm.inf\n"),
        None
    );
}

#[test]
fn exec_takes_its_arch_and_then_the_command() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let a = parse_exec(&s(&["--arch", "x64", "--", "cargo", "--arch"]))
        .unwrap_or_else(|_| unreachable!());
    assert_eq!(a.arch, Arch::X86_64);
    assert!(!a.desktop);
    assert_eq!(a.argv, s(&["cargo", "--arch"]));
    assert_eq!(
        parse_exec(&s(&["cargo"])).map(|a| a.arch).ok(),
        Some(Arch::Aarch64)
    );
    let d = parse_exec(&s(&["--desktop", "--arch", "x86_64", "cargo", "--desktop"]))
        .unwrap_or_else(|_| unreachable!());
    assert!(d.desktop);
    assert_eq!(d.arch, Arch::X86_64);
    assert_eq!(d.argv, s(&["cargo", "--desktop"]));
    assert!(parse_exec(&s(&["--arch", "mips", "x"])).is_err());
    assert!(parse_exec(&s(&["--"])).is_err());
    assert_eq!(released(0, Ok(())).ok(), Some(0));
    assert_eq!(released(7, Ok(())).ok(), Some(7));
    let failed = released(0, Err("snapshot restore failed".into()))
        .expect_err("cleanup is part of the command result");
    assert_eq!(failed.code, 1);
    assert_eq!(failed.msg.as_deref(), Some("snapshot restore failed"));
}

/// The VM's drives read as no progress only while their operations stay the same, counted
/// from the wait's start, and from their last operation once they did one; a look QMP did
/// not answer changes nothing.
#[test]
fn drives_that_do_nothing_are_no_progress_from_their_last_operation() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut progress = Progress::from(t0);
    assert_eq!(progress.saw(None, at(5)), Duration::from_secs(5));
    assert_eq!(progress.saw(Some(40), at(10)), Duration::from_secs(10));
    assert_eq!(progress.saw(Some(40), at(70)), Duration::from_secs(70));
    assert_eq!(progress.saw(Some(41), at(80)), Duration::ZERO);
    assert_eq!(progress.saw(None, at(85)), Duration::from_secs(5));
    assert_eq!(progress.saw(Some(41), at(90)), Duration::from_secs(10));
    assert_eq!(progress.saw(Some(900), at(100)), Duration::ZERO);
    assert_eq!(progress.done(), Some(900));
}

/// A scratch VM home under /tmp (QMP's socket path must fit a sockaddr_un), its layout, and
/// tools whose `qemu-img` does nothing (`/usr/bin/true`): a revert then puts back the
/// firmware variables and the TPM state, which these tests read, and no disk.
fn scratch_vm() -> (aterm_tempfile::TempDir, Layout, Tools) {
    let home = aterm_tempfile::Builder::new()
        .prefix("w")
        .tempdir_in("/tmp")
        .expect("a scratch home");
    let l = Layout::under(home.path());
    assert!(lossy(&l.qmp()).len() < 104, "{}", l.qmp().display());
    std::fs::create_dir_all(l.run()).expect("the run directory");
    let t = Tools {
        qemu_img: "/usr/bin/true".into(),
        ..tools()
    };
    (home, l, t)
}

/// Give the VM in `l` a snapshot whose firmware variables and TPM state read `clean`, and
/// live ones that read `dirty`, as a boot of the snapshot leaves them
/// ([`Layout::unreverted`]).
fn snapshotted_and_dirtied(l: &Layout) {
    std::fs::write(l.provisioned(), "0123456789abcdef\n").expect("the snapshot's plan");
    std::fs::write(l.vars_saved(), "clean").expect("the saved variables");
    std::fs::write(l.vars(), "dirty").expect("the variables");
    for (dir, text) in [(l.tpm_saved(), "clean"), (l.tpm(), "dirty")] {
        std::fs::create_dir_all(&dir).expect("a TPM state");
        std::fs::write(dir.join("tpm2-00.permall"), text).expect("its file");
    }
    std::fs::write(l.unreverted(), "a boot of the snapshot\n").expect("the mark");
}

/// What the VM in `l` holds: its firmware variables and TPM state, and whether it is marked
/// unreverted.
fn state_of(l: &Layout) -> (String, String, bool) {
    (
        std::fs::read_to_string(l.vars()).expect("the variables"),
        std::fs::read_to_string(l.tpm().join("tpm2-00.permall")).expect("the TPM state"),
        l.unreverted().exists(),
    )
}

/// [`qemu_stand_in_as`], waited for until it holds its pid file and QMP's socket (a
/// one-minute hang detector).
fn up_as(l: &Layout, guest: &str) -> aterm_uds::spawnfd::Child {
    let vm = qemu_stand_in_as(l, guest);
    let end = Instant::now() + Duration::from_secs(60);
    while !(matches!(running(l), Ok(Some(_))) && l.qmp().exists()) {
        assert!(Instant::now() < end, "the stand-in QEMU did not come up");
        std::thread::sleep(Duration::from_millis(20));
    }
    vm
}

/// Patience measured in seconds, for the stand-ins.
fn quick(within: u64, stall: Option<u64>) -> Patience {
    Patience {
        within: Duration::from_secs(within),
        stall: stall.map(Duration::from_secs),
        look: Duration::from_millis(100),
    }
}

/// A boot whose guest shows no progress (no answer, its drives still: a firmware hang, as
/// two restarts hung at "Start boot option") is started afresh once, as a step of the boot,
/// and answers then; the hung QEMU is ended and its replacement runs. So is one whose screens
/// change while its drives are still (Windows' boot spinner turning over a hang). Negative
/// controls: a guest that hangs again after its relaunch is given up at its second stall,
/// not at the wait's limit (here a one-minute hang detector), and its QEMU ended; and one
/// whose drives work is no stall, so it is waited for to the limit and never relaunched.
#[test]
fn a_boot_that_shows_no_progress_is_started_afresh_once_and_given_up_at_a_second_stall() {
    use std::cell::{Cell, RefCell};
    let boot = |first: &str, then: &str, p: Patience| {
        let (_home, l, _t) = scratch_vm();
        let vms = RefCell::new(vec![up_as(&l, first)]);
        let relaunched = Cell::new(0u32);
        let t0 = Instant::now();
        let waited = wait_answer(
            &l,
            p,
            "booting",
            &mut || then == "answers" && relaunched.get() > 0,
            &mut || {
                stop(&l, None, Patience::NOW)?;
                let next = if then == "answers" { HUNG } else { then };
                vms.borrow_mut().push(up_as(&l, next));
                relaunched.set(relaunched.get() + 1);
                Ok(())
            },
        );
        let took = t0.elapsed();
        let still = running(&l);
        let _ = stop(&l, None, Patience::NOW);
        for vm in vms.borrow_mut().iter_mut() {
            reaped(vm);
        }
        (waited, relaunched.get(), took, still)
    };

    for hung in [HUNG, SPINNING] {
        let (waited, relaunched, _, still) = boot(hung, "answers", quick(60, Some(1)));
        assert!(waited.is_ok(), "{hung}: {waited:?}");
        assert_eq!(relaunched, 1, "{hung}");
        assert!(
            matches!(still, Ok(Some(_))),
            "{hung}: the relaunched QEMU runs: {still:?}"
        );
    }

    let (waited, relaunched, _, still) = boot(HUNG, HUNG, quick(60, Some(1)));
    let why = waited.expect_err("a second stall gives up");
    assert!(
        why.starts_with("no answer on SSH and no operation on the VM's drives for ")
            && why.contains(" s while booting (its screens: ")
            && why.contains("again after it was started afresh")
            && why.ends_with("the VM stopped by force (ended at once)"),
        "{why}"
    );
    assert_eq!(relaunched, 1);
    assert_eq!(still, Ok(None), "the hung QEMU was ended");

    let (waited, relaunched, took, _) = boot(WORKING, WORKING, quick(3, Some(1)));
    let why = waited.expect_err("no answer within the limit");
    assert!(
        why.starts_with("the guest did not answer on SSH within 3 s while booting"),
        "{why}"
    );
    assert_eq!(relaunched, 0, "drives that work are progress");
    assert!(took >= Duration::from_secs(3), "{took:?}");
}

/// `down` on a hung guest (it ignores ACPI's power button, its drives still, Windows' boot
/// spinner turning on its screen) ends its QEMU once it has shown no progress for the stall,
/// not at the shutdown's limit (here a one-minute hang detector), and reverts the VM to its
/// snapshot, saying so: a stop that had to end QEMU no longer leaves its state unreverted.
/// Negative controls: a guest whose drives work is waited for to the limit before it is
/// ended (and reverted); one that shuts down is reverted with no force; and a pid file that
/// names a process this VM's words do not describe is refused, its state preserved and still
/// marked unreverted.
#[test]
fn down_ends_a_hung_guest_and_reverts_it_to_its_snapshot() {
    let down_on = |guest: &str, p: Patience| {
        let (_home, l, t) = scratch_vm();
        snapshotted_and_dirtied(&l);
        let mut vm = up_as(&l, guest);
        let t0 = Instant::now();
        let shut = down(&l, &t, p);
        let took = t0.elapsed();
        assert_eq!(running(&l), Ok(None), "{guest}: QEMU is gone");
        reaped(&mut vm);
        (shut, took, state_of(&l))
    };
    let reverted = ("clean".to_owned(), "clean".to_owned(), false);

    let (shut, _, state) = down_on(SPINNING, quick(60, Some(1)));
    let shut = shut.expect("down");
    assert!(
        matches!(&shut.stopped, Stopped::Forced(why)
            if why.starts_with("Windows showed no sign of shutting down for ")
                && why.contains(" s, no operation on its drives; its screens: ")),
        "{shut}"
    );
    assert!(shut.reverted && shut.to_string().ends_with(") and reverted to its snapshot"));
    assert_eq!(state, reverted);

    let (shut, took, state) = down_on(WORKING, quick(3, Some(1)));
    let shut = shut.expect("down");
    assert!(
        matches!(&shut.stopped, Stopped::Forced(why)
            if why.starts_with("Windows did not shut down within 3 s")),
        "{shut}"
    );
    assert!(took >= Duration::from_secs(3), "{took:?}");
    assert_eq!(state, reverted);

    let (shut, _, state) = down_on(SHUTS_DOWN, quick(30, Some(10)));
    assert_eq!(
        shut.map(|s| s.to_string()).as_deref(),
        Ok("stopped (Windows shut down) and reverted to its snapshot")
    );
    assert_eq!(state, reverted);

    let (_home, l, t) = scratch_vm();
    snapshotted_and_dirtied(&l);
    std::fs::write(l.qemu_pid(), format!("{}\n", std::process::id())).expect("the pid file");
    let refused = down(&l, &t, quick(30, Some(1))).expect_err("not this VM's QEMU");
    assert!(refused.ends_with("; preserved"), "{refused}");
    assert_eq!(state_of(&l), ("dirty".to_owned(), "dirty".to_owned(), true));
}

/// A stopped VM still marked unreverted (a revert that failed after its stop, or a Mac that
/// went down under it) is reverted before anything boots it, once; a VM with no snapshot is
/// left as it is, the mark kept.
#[test]
fn a_stopped_vm_left_unreverted_is_reverted_before_it_boots() {
    let (_home, l, t) = scratch_vm();
    snapshotted_and_dirtied(&l);
    assert_eq!(revert_unreverted(&l, &t), Ok(true));
    assert_eq!(
        state_of(&l),
        ("clean".to_owned(), "clean".to_owned(), false)
    );
    std::fs::write(l.vars(), "dirty").expect("the variables");
    std::fs::write(l.tpm().join("tpm2-00.permall"), "dirty").expect("the TPM state");
    assert_eq!(revert_unreverted(&l, &t), Ok(false), "no mark, no revert");
    assert_eq!(
        state_of(&l),
        ("dirty".to_owned(), "dirty".to_owned(), false)
    );

    std::fs::remove_file(l.provisioned()).expect("no snapshot");
    std::fs::write(l.unreverted(), "a boot\n").expect("the mark");
    assert_eq!(revert_unreverted(&l, &t), Ok(false));
    assert_eq!(state_of(&l), ("dirty".to_owned(), "dirty".to_owned(), true));
}
