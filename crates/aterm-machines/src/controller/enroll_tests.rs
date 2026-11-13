// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;

const VM_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";
const VM_FP: &str = "SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q";

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn enroll_takes_one_address_and_defaults_the_admin_and_how_the_key_is_trusted() {
    let sys = Fake::root();
    sys.on("/usr/bin/id -urn", Ran::ok("owner\n"));
    let r = parse_args(
        &sys,
        &args(&[
            "lab-07.local",
            "--user",
            "labadmin",
            "--fingerprint",
            "0Tdy7Goos0XG",
        ]),
    )
    .unwrap();
    assert_eq!((r.user.as_str(), r.user_given), ("labadmin", true));
    assert_eq!(r.trust, Trust::Fingerprint("SHA256:0Tdy7Goos0XG".into()));
    assert!(sys.calls().is_empty(), "a --user is never looked up");
    let r = parse_args(&sys, &args(&["lab-07.local"])).unwrap();
    assert_eq!((r.user.as_str(), r.user_given), ("owner", false));
    assert_eq!(r.trust, Trust::FirstUse);
    assert_eq!(r.requests().len(), 1);
    for flag in ["--fingerprint", "--user", "--name", "--parallel"] {
        let err = parse_args(&sys, &args(&["lab-07.local", flag])).unwrap_err();
        assert!(err.contains("takes a value"), "{err}");
    }
    assert!(parse_args(&sys, &args(&["-oProxyCommand=x", "--user", "a"])).is_err());
    assert!(parse_args(&sys, &args(&["h", "--user", "a b"])).is_err());
    assert!(parse_args(&sys, &args(&["--found", "--user", "-x"])).is_err());
    let nameless = Fake::root();
    nameless.on("/usr/bin/id -urn", Ran::exit(1, "id: 501: no such user\n"));
    let err = parse_args(&nameless, &args(&["lab-07.local"])).unwrap_err();
    assert!(err.contains("--user <admin>"), "{err}");
}

#[test]
fn several_macs_share_the_admin_and_refuse_what_describes_one_mac() {
    let sys = Fake::root();
    sys.on("/usr/bin/id -urn", Ran::ok("owner\n"));
    let r = parse_args(
        &sys,
        &args(&["m1.local", "m2.local", "M1.LOCAL", "--parallel", "4"]),
    )
    .unwrap();
    assert_eq!(r.addresses, ["m1.local", "m2.local"], "each address once");
    assert_eq!(r.parallel, Some(4));
    assert!(r.requests().iter().all(|q| q.user == "owner"));
    let found = parse_args(&sys, &args(&["--found"])).unwrap();
    assert!(found.found && found.addresses.is_empty());
    for words in [
        &["m1.local", "m2.local", "--fingerprint", "SHA256:x"][..],
        &["--found", "--fingerprint", "SHA256:x"],
        &["m1.local", "m2.local", "--name", "lab-01"],
        &["--found", "--name", "lab-01"],
    ] {
        let err = parse_args(&sys, &args(words)).unwrap_err();
        assert!(err.contains("one Mac"), "{words:?}: {err}");
    }
    assert!(
        parse_args(&sys, &args(&[]))
            .unwrap_err()
            .contains("--found")
    );
    assert!(parse_args(&sys, &args(&["m1.local", "--parallel", "0"])).is_err());
}

#[test]
fn the_scanned_key_must_match_the_fingerprint_read_at_the_mac() {
    let out = format!("127.0.0.1 ssh-ed25519 {VM_KEY}\n");
    let (kind, blob) = parse_keyscan(&out).unwrap();
    assert_eq!((kind.as_str(), blob.as_str()), ("ssh-ed25519", VM_KEY));
    assert_eq!(parse_keyscan("# 127.0.0.1:22 SSH-2.0-OpenSSH_10.3\n"), None);
    assert_eq!(
        parse_fingerprint(
            "256 SHA256:0Tdy7Goos0XGPRcvSTdM+yrBQzHigTDPivQLBq34e5Q 127.0.0.1 (ED25519)\n"
        )
        .as_deref(),
        Some(VM_FP)
    );
    let key = HostKey {
        kind,
        blob,
        fingerprint: VM_FP.into(),
    };
    check_host_key(&key, &Trust::Fingerprint(VM_FP.into())).unwrap();
    check_host_key(&key, &Trust::FirstUse).unwrap();
    let err = check_host_key(&key, &Trust::Fingerprint("SHA256:other".into())).unwrap_err();
    assert!(err.contains("Nothing was sent"), "{err}");
}

/// How `ssh` ends a scan whose key exchange recorded the key: no method authenticates.
fn denied() -> Ran {
    Ran::exit(
        255,
        "hostkey@host: Permission denied (publickey,password).\n",
    )
}

/// `sys` answers enrollment's scan of `address` (under the controller's folder): `key`
/// recorded, or nothing, and `ssh` ending as `ran`.
fn scanned(sys: &Fake, address: &str, key: Option<&str>, ran: Ran) {
    super::super::lan_proxy::scanned(
        sys,
        &PathBuf::from(ROOT).join("scan"),
        address,
        22,
        super::super::lan_proxy::SCAN_WITHIN,
        key,
        ran,
    );
}

/// `sys` answers `found_macs`' scan of `address` (under the controller's folder).
fn found_scanned(sys: &Fake, address: &str, key: &str) {
    super::super::lan_proxy::scanned(
        sys,
        &PathBuf::from(ROOT).join("scan"),
        address,
        22,
        discover::OFFERED_WITHIN,
        Some(key),
        denied(),
    );
}

/// The scan reads the key `ssh` recorded, not what it printed, and leaves no record behind.
#[test]
fn scanning_reads_the_key_and_fingerprints_it_from_a_file() {
    let fake = Fake::root();
    super::super::lan_proxy::scanned(
        &fake,
        Path::new("/s"),
        "192.168.64.2",
        22,
        super::super::lan_proxy::SCAN_WITHIN,
        Some(VM_KEY),
        denied(),
    );
    fake.on(
        "/usr/bin/ssh-keygen -lf -",
        Ran::ok(&format!("256 {VM_FP} 192.168.64.2 (ED25519)\n")),
    );
    let key = scan_host_key(&fake, Path::new("/s"), "192.168.64.2", Path::new("/s")).unwrap();
    assert_eq!(key.fingerprint, VM_FP);
    assert!(fake.written("/s/hostkey.pub").is_none());
    let record = super::super::lan_proxy::scan_file(Path::new("/s"), "192.168.64.2", 22);
    assert!(fake.read_file(&record).is_err(), "the record is removed");
}

#[test]
fn what_the_scan_said_tells_no_route_nothing_there_and_no_remote_login_apart() {
    let said = |err: &str| NoHostKey::of(&Ran::exit(255, err));
    assert_eq!(
        said("ssh: connect to host 0.0.0.1 port 22: No route to host\n"),
        NoHostKey::NoRoute
    );
    assert_eq!(
        said("ssh: connect to host 192.168.64.252 port 22: Host is down\n"),
        NoHostKey::Down
    );
    assert_eq!(said(""), NoHostKey::Silent);
    assert_eq!(
        said("ssh: connect to host 192.168.64.17 port 22: Operation timed out\n"),
        NoHostKey::Silent
    );
    assert_eq!(
        said("kex_exchange_identification: Connection closed by remote host\n"),
        NoHostKey::Closed
    );
    assert_eq!(
        said("ssh: connect to host 192.168.64.17 port 22: Connection refused\n"),
        NoHostKey::Closed
    );
    assert!(matches!(
        said("kex_exchange_identification: read: Connection reset by peer\n"),
        NoHostKey::Answered(_)
    ));
    let broker = format!(
        "{}{}",
        super::super::lan_proxy::BROKER_SAID,
        aterm_uds::lan::Unavailable::NotInstalled
    );
    assert_eq!(
        said(&format!(
            "{broker}\nConnection closed by UNKNOWN port 65535\n"
        )),
        NoHostKey::Broker(broker.clone())
    );
    assert_eq!(
        NoHostKey::Broker(broker.clone()).why("192.168.64.2"),
        broker
    );
    for (class, remote_login) in [
        (NoHostKey::Down, false),
        (NoHostKey::Closed, true),
        (NoHostKey::Silent, true),
        (NoHostKey::Answered("exit 255".into()), true),
    ] {
        let why = class.why("192.168.64.3");
        assert_eq!(why.contains("Remote Login"), remote_login, "{why}");
    }
    for err in [
        "ssh: connect to host 192.168.64.3 port 22: No route to host\n",
        "ssh: connect to host 192.168.64.3 port 22: Host is down\n",
        "kex_exchange_identification: Connection closed by remote host\n",
        "",
        "kex_exchange_identification: read: Connection reset by peer\n",
        "ssh: connect to host 192.168.64.3 port 22: Operation timed out\n",
    ] {
        let why = said(err).why("192.168.64.3");
        assert!(!why.contains("192.168.64.3"), "{err:?}: {why}");
    }
    let fake = Fake::root();
    super::super::lan_proxy::scanned(
        &fake,
        Path::new("/s"),
        "192.168.64.4",
        22,
        super::super::lan_proxy::SCAN_WITHIN,
        None,
        Ran::exit(
            255,
            "ssh: connect to host 192.168.64.4 port 22: Host is down\n",
        ),
    );
    let err = scan_host_key(&fake, Path::new("/s"), "192.168.64.4", Path::new("/s")).unwrap_err();
    assert_eq!(err, NoHostKey::Down.why("192.168.64.4"));
}

#[test]
fn pinning_replaces_only_the_aliases_own_line() {
    let key = HostKey {
        kind: "ssh-ed25519".into(),
        blob: VM_KEY.into(),
        fingerprint: VM_FP.into(),
    };
    let uuid = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";
    let text = known_hosts_with(
        "OTHER ssh-ed25519 AAAA\nC3396674-FCD3-52E5-A1C1-BD5B9E8F5C43 ssh-ed25519 OLD\n",
        uuid,
        Some(&key),
    );
    assert_eq!(
        text,
        format!("OTHER ssh-ed25519 AAAA\n{uuid} ssh-ed25519 {VM_KEY}\n")
    );
    assert_eq!(
        known_hosts_with(&text, uuid, None),
        "OTHER ssh-ed25519 AAAA\n"
    );
}

/// A line OpenSSH reads as pinning the alias, whatever its case, in a comma list or as
/// `[alias]:port`, is replaced too, so a re-pin never leaves an old key beside the new one.
/// NEGATIVE CONTROL: a line naming another host, or the alias only as a prefix, is kept.
#[test]
fn pinning_replaces_every_line_openssh_reads_as_the_alias_s() {
    let key = HostKey {
        kind: "ssh-ed25519".into(),
        blob: VM_KEY.into(),
        fingerprint: VM_FP.into(),
    };
    let lower = "c3396674-fcd3-52e5-a1c1-bd5b9e8f5c43";
    let upper = lower.to_ascii_uppercase();
    let current = format!(
        "{upper} ssh-ed25519 OLD1\na,{upper} ssh-ed25519 OLD2\n[{upper}]:2222 ssh-ed25519 OLD3\n{lower}-x ssh-ed25519 KEEP1\nOTHER ssh-ed25519 KEEP2\n"
    );
    assert_eq!(
        known_hosts_with(&current, lower, Some(&key)),
        format!(
            "{lower}-x ssh-ed25519 KEEP1\nOTHER ssh-ed25519 KEEP2\n{lower} ssh-ed25519 {VM_KEY}\n"
        )
    );
}

fn vm_facts() -> String {
    [
        "+-o VMA2MACOSAP  <class IOPlatformExpertDevice>\n    {\n      \"IOPlatformSerialNumber\" = \"ZNYVNTRJN5\"\n      \"IOPlatformUUID\" = \"C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43\"\n    }\n",
        "VirtualMac2,1\n",
        "arm64\n",
        "27.0\n",
        "26A428\n",
        "Managed’s Virtual Machine\n",
        "Manageds-Virtual-Machine\n",
        "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T/\n",
        "Apple M5 (Virtual)\n",
        "+-o product  <class IOPlatformDevice>\n    {\n      \"product-name\" = <\"Apple Virtual Machine 1\">\n    }\n",
    ]
    .join("--aterm--\n")
}

#[test]
fn the_hosts_facts_are_read_section_by_section() {
    let f = parse_facts(&vm_facts()).unwrap();
    assert_eq!(f.uuid, "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43");
    assert_eq!(f.serial, "ZNYVNTRJN5");
    assert_eq!(f.model, "VirtualMac2,1");
    assert_eq!(f.computer_name, "Managed’s Virtual Machine");
    assert_eq!(
        f.tmpdir,
        "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T/"
    );
    assert!(parse_facts("nothing").is_err());
    assert!(parse_facts(&vm_facts().replace("IOPlatformUUID", "Other")).is_err());
    assert_eq!(
        f.described(),
        "\"Managed’s Virtual Machine\": Virtual Machine, Apple M5 (Virtual), macOS 27.0, \
         serial ZNYVNTRJN5"
    );
    let air = Facts {
        model: "Mac16,12".into(),
        product_name: "MacBook Air (15-inch, M4, 2025)".into(),
        chip: "Apple M4\n".into(),
        ..f.clone()
    };
    assert!(
        air.described()
            .contains(": MacBook Air, Apple M4, macOS 27.0"),
        "{}",
        air.described()
    );
    let intel = Facts {
        model: "MacBookPro14,3".into(),
        product_name: String::new(),
        chip: "Intel(R) Core(TM) i7-7920HQ CPU @ 3.10GHz\n".into(),
        ..f
    };
    assert!(
        intel.described().contains(": MacBook Pro, Intel Core i7,"),
        "{}",
        intel.described()
    );
}

#[test]
fn a_bundle_travels_whole_and_a_dev_binary_alone() {
    let (source, inner, bundle) =
        payload_of(Path::new("/Applications/aterm.app/Contents/MacOS/aterm"));
    assert_eq!(source, Path::new("/Applications/aterm.app"));
    assert_eq!(inner, Path::new("aterm.app/Contents/MacOS/aterm"));
    assert!(bundle);
    let (source, inner, bundle) = payload_of(Path::new("/w/target/debug/aterm-machines-dev"));
    assert_eq!(source, Path::new("/w/target/debug/aterm-machines-dev"));
    assert_eq!(inner, Path::new("aterm-machines-dev"));
    assert!(!bundle);
}

#[test]
fn only_a_bundle_is_archived_with_its_parent_kept() {
    let fake = Fake::root();
    fake.on(
        "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl /w/aterm-machines-dev /s/aterm-agent.zip",
        Ran::ok(""),
    );
    fake.file("/s/aterm-agent.zip", b"zip");
    let a = build_archive(&fake, Path::new("/w/aterm-machines-dev"), Path::new("/s")).unwrap();
    assert_eq!(a.exe, Path::new("aterm-machines-dev"));
    let fake = Fake::root();
    fake.on(
        "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl --keepParent /Applications/aterm.app /s/aterm-agent.zip",
        Ran::ok(""),
    );
    fake.file("/s/aterm-agent.zip", b"zip");
    let a = build_archive(
        &fake,
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        Path::new("/s"),
    )
    .unwrap();
    assert!(a.bundle);
}

fn archive(bundle: bool) -> Archive {
    Archive {
        path: "/s/aterm-agent.zip".into(),
        sha256: "ab".repeat(32),
        exe: if bundle {
            "aterm.app/Contents/MacOS/aterm".into()
        } else {
            "aterm-machines-dev".into()
        },
        bundle,
    }
}

fn install_of(a: &Archive) -> Install<'_> {
    Install {
        archive: a,
        remote_archive: "/var/folders/yt/x/T/aterm-agent-abababab.zip".into(),
        remote_signers: "/var/folders/yt/x/T/aterm-allowed-signers-abababab".into(),
        signers_sha: "cd".repeat(32),
        admin: "admin".into(),
        fleet_key: "ssh-ed25519 AAAA fleet@ctl".into(),
    }
}

#[test]
fn the_install_line_hashes_the_root_owned_copies_before_it_executes_anything() {
    let a = archive(true);
    let s = root_script(&install_of(&a));
    let at = |needle: &str| s.find(needle).unwrap_or_else(|| panic!("{needle}\n{s}"));
    let copy = at(
        "/bin/cp /var/folders/yt/x/T/aterm-agent-abababab.zip /Library/Management/aterm/.staging/payload.zip",
    );
    let hash = at("/usr/bin/shasum -a 256 /Library/Management/aterm/.staging/payload.zip");
    let expand = at("/usr/bin/ditto -x -k");
    let sign = at(
        "/usr/bin/codesign --verify --strict /Library/Management/aterm/.staging/payload/aterm.app",
    );
    let run = at(
        "/Library/Management/aterm/.staging/payload/aterm.app/Contents/MacOS/aterm machines agent install --admin admin --fleet-key 'ssh-ed25519 AAAA fleet@ctl'",
    );
    assert!(
        copy < hash && hash < expand && expand < sign && sign < run,
        "{s}"
    );
    let guard = at("if [ -e /Library/Management/aterm/.staging/exec ]");
    let clear = at("/bin/rm -rf /Library/Management/aterm/.staging\n");
    let stamp_line = "echo \"$sess $up $boot $woke\" > /Library/Management/aterm/.staging/exec\n";
    let stamp = s.rfind(stamp_line).unwrap();
    let read = s[..stamp].rfind("sess=$(/usr/sbin/sysctl -n kern.bootsessionuuid)\n");
    assert!(read.is_some_and(|read| sign < read), "{s}");
    assert!(guard < at(stamp_line) && at(stamp_line) < clear, "{s}");
    assert!(guard < clear && clear < copy, "{s}");
    assert!(
        sign < stamp && s[stamp..].lines().nth(1) == s[run..].lines().next(),
        "{s}"
    );
    assert!(
        !s.lines()
            .any(|l| l.starts_with("/var/folders") || l.starts_with("'/var")),
        "{s}"
    );
    let dev = root_script(&Install {
        remote_archive: "/t/a.zip".into(),
        remote_signers: "/t/s".into(),
        ..install_of(&archive(false))
    });
    assert!(!dev.contains("codesign"), "{dev}");
    assert!(dev.contains(
        "\n/Library/Management/aterm/.staging/payload/aterm-machines-dev machines agent install"
    ));
}

/// A boot id in `kern.bootsessionuuid`'s form.
const BOOT_ID: &str = "55C317A3-F793-4F0D-BE04-262D2C6ED332";

/// The guard of `stage`, its clocks reading `boot`, `woke` and `now` (Unix seconds) and
/// the boot id `session`, run before the stage's removal; the stage is gone when it passed.
fn guarded(stage: &Path, session: &str, boot: u64, woke: u64, now: u64) -> Ran {
    let at = stage.to_str().unwrap();
    assert!(!at.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)));
    let sysctl = |sec: u64| format!("echo '{{ sec = {sec}, usec = 0 }} Thu Oct 22 09:00:00 2026'");
    let (session, boot, wake, now) = (
        format!("echo {session}"),
        sysctl(boot),
        sysctl(woke),
        format!("echo {now}"),
    );
    let guard = guard_reading(
        at,
        &Clocks {
            session: &session,
            boot: &boot,
            wake: &wake,
            now: &now,
        },
    );
    sh(&format!("set -e\n{guard}/bin/rm -rf {at}\n"))
}

/// `script`, run by `/bin/sh`.
fn sh(script: &str) -> Ran {
    crate::port::Live::new()
        .run(&Cmd::new("/bin/sh").arg("-c").arg(script))
        .unwrap()
}

/// A stage with a staged agent, its `exec` stamp `stamp` when it is `Some`, and its `ran`
/// marker when `ran`.
fn staged(stage: &Path, stamp: Option<&str>, ran: bool) {
    std::fs::create_dir_all(stage.join("payload")).unwrap();
    std::fs::write(stage.join("payload/aterm"), b"agent").unwrap();
    if let Some(stamp) = stamp {
        std::fs::write(stage.join("exec"), stamp).unwrap();
    }
    if ran {
        std::fs::write(stage.join("ran"), b"").unwrap();
    }
}

/// The minutes a refusal asks for, `None` when the guard passed.
fn held_for(stage: &Path, ran: &Ran) -> Option<String> {
    if ran.success() {
        assert!(!stage.exists(), "a pass clears the stage");
        return None;
    }
    assert_eq!(ran.status, Some(1), "{}", ran.err());
    assert!(stage.join("payload/aterm").is_file(), "the stage stays");
    let err = ran.err();
    assert!(err.contains("has not shown it ran"), "{err}");
    let minutes = err
        .split(" again in ")
        .nth(1)
        .and_then(|t| t.split(' ').next());
    Some(minutes.unwrap_or_else(|| panic!("{err}")).to_owned())
}

#[test]
fn the_stage_stays_while_the_helper_an_earlier_enrollment_started_has_not_shown_it_ran() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    let stamp = format!("{BOOT_ID} {UP} {BOOT} 0\n");
    let at = |up: u64| guarded(&stage, BOOT_ID, BOOT, 0, BOOT + up);

    staged(&stage, Some(&stamp), false);
    assert_eq!(held_for(&stage, &at(UP)).as_deref(), Some("10"));
    assert_eq!(held_for(&stage, &at(UP + 9 * 60)).as_deref(), Some("1"));
    assert_eq!(
        held_for(&stage, &at(UP + 10 * 60)),
        None,
        "its hang has passed"
    );

    staged(&stage, Some(&stamp), true);
    assert_eq!(held_for(&stage, &at(UP)), None, "the helper showed it ran");
    staged(&stage, None, false);
    assert_eq!(
        held_for(&stage, &at(UP)),
        None,
        "no helper was started from it"
    );

    staged(&stage, Some(&stamp), false);
    let other = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
    let restarted = guarded(&stage, other, BOOT + 9_000, 0, BOOT + 9_030);
    assert_eq!(
        held_for(&stage, &restarted),
        None,
        "a restart since the stamp"
    );

    staged(&stage, Some(&stamp), false);
    let clocks = guarded(&stage, "", BOOT, 0, BOOT + UP);
    assert_eq!(clocks.status, Some(1));
    assert!(
        clocks.err().contains("boot id and clocks read"),
        "{}",
        clocks.err()
    );
    assert!(stage.join("payload/aterm").is_file());

    // The root script's own stamp and guard, on this Mac's clocks.
    #[cfg(target_os = "macos")]
    {
        let at = stage.to_str().unwrap();
        std::fs::remove_file(stage.join("exec")).unwrap();
        let stamped = sh(&format!(
            "set -e\n{}{}",
            clock_reading(&MAC_CLOCKS),
            stamp_line(at)
        ));
        assert!(stamped.success(), "{}", stamped.err());
        let held = sh(&format!("set -e\n{}/bin/rm -rf {at}\n", stage_guard(at)));
        assert!(
            held_for(&stage, &held).is_some(),
            "a stamp just written holds"
        );
    }
}

#[test]
fn the_stage_guard_counts_awake_time_that_no_step_of_the_wall_clock_moves() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    const STEP: u64 = 20 * 60;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    let stamp = format!("{BOOT_ID} {UP} {BOOT} 0\n");

    // Two minutes after the stamp the wall clock steps 20 minutes forward, and XNU moves
    // the boot time with it.
    staged(&stage, Some(&stamp), false);
    let stepped = guarded(&stage, BOOT_ID, BOOT + STEP, 0, BOOT + STEP + UP + 120);
    assert_eq!(
        held_for(&stage, &stepped).as_deref(),
        Some("8"),
        "the negative control"
    );
    let stepped = guarded(&stage, BOOT_ID, BOOT + STEP, 0, BOOT + STEP + UP + 600);
    assert_eq!(held_for(&stage, &stepped), None);

    // An hour asleep a minute after the stamp, then a minute awake: only awake time counts.
    staged(&stage, Some(&stamp), false);
    let woke = BOOT + UP + 60 + 3_600;
    let after = |awake: u64| guarded(&stage, BOOT_ID, BOOT, woke, woke + awake);
    assert_eq!(held_for(&stage, &after(60)).as_deref(), Some("9"));
    assert_eq!(held_for(&stage, &after(600)), None);
}

#[test]
fn the_agent_ages_the_root_scripts_stamp_as_the_stage_guard_does() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    let stamp = format!("{BOOT_ID} {UP} {BOOT} 0\n");
    let other = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
    let (stepped, woke) = (BOOT + 1_200, BOOT + UP + 3_660);
    for (session, boot, woke, now) in [
        (BOOT_ID, BOOT, 0, BOOT + UP),
        (BOOT_ID, BOOT, 0, BOOT + UP + 9 * 60),
        (BOOT_ID, BOOT, 0, BOOT + UP + 10 * 60),
        (BOOT_ID, BOOT, 0, BOOT + UP - 60),
        (other, BOOT + 9_000, 0, BOOT + 9_030),
        (BOOT_ID, stepped, 0, stepped + UP + 120),
        (BOOT_ID, BOOT, woke, woke + 60),
        (BOOT_ID, stepped, woke, woke + 3_600),
    ] {
        staged(&stage, Some(&stamp), false);
        let shell = held_for(&stage, &guarded(&stage, session, boot, woke, now));
        let clocks = crate::first_exec::BootClocks {
            session: session.to_owned(),
            boot,
            woke,
            up: now - boot,
        };
        let agent = crate::first_exec::root_stamp_left(&stamp, Some(&clocks))
            .expect("the root script's stamp");
        assert_eq!(
            agent.map(|left| crate::first_exec::minutes(left).to_string()),
            shell,
            "{clocks:?}"
        );
    }
}

#[test]
fn a_wall_clock_step_between_the_boot_and_wall_clock_reads_stops_the_script() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    const STEP: u64 = 20 * 60;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    let at = stage.to_str().unwrap();
    let first = tmp.path().join("boot-read");
    let first = first.to_str().unwrap();
    let sysctl = |sec: u64| format!("echo '{{ sec = {sec}, usec = 0 }} Thu Oct 22 09:00:00 2026'");
    // The first read of the boot time comes before a 20-minute forward step; the wall
    // clock and the second read come after it.
    let boot = format!(
        "if [ -e {first} ]; then {}; else : > {first}; {}; fi",
        sysctl(BOOT + STEP),
        sysctl(BOOT)
    );
    let (session, wake, now) = (
        format!("echo {BOOT_ID}"),
        sysctl(0),
        format!("echo {}", BOOT + STEP + UP + 120),
    );
    let clocks = Clocks {
        session: &session,
        boot: &boot,
        wake: &wake,
        now: &now,
    };

    staged(&stage, Some(&format!("{BOOT_ID} {UP} {BOOT} 0\n")), false);
    let guard = guard_reading(at, &clocks);
    let ran = sh(&format!("set -e\n{guard}/bin/rm -rf {at}\n"));
    assert_eq!(ran.status, Some(1), "the negative control: {}", ran.err());
    assert!(
        ran.err().contains("boot id and clocks read"),
        "{}",
        ran.err()
    );
    assert!(stage.join("payload/aterm").is_file(), "the stage stays");

    // At the stamp, the script stops before the staged agent's exec.
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(stage.join("exec")).unwrap();
    let ran = sh(&format!(
        "set -e\n{}{}: > {at}/started\n",
        clock_reading(&clocks),
        stamp_line(at)
    ));
    assert_eq!(ran.status, Some(1), "{}", ran.err());
    assert!(!stage.join("exec").exists() && !stage.join("started").exists());
}

#[test]
fn a_stamp_the_stage_guard_cannot_place_holds_the_whole_hang_from_when_it_finds_it() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stage = tmp.path().join("stage");
    let found = BOOT + UP + 7_200;
    for stamp in [
        String::new(),
        "5000000\n".to_owned(),
        format!("{BOOT_ID} {UP} {BOOT} 0 more\n"),
        format!("not-a-boot {UP} {BOOT} 0\n"),
        format!("------------------------------------ {UP} {BOOT} 0\n"),
    ] {
        staged(&stage, Some(&stamp), false);
        let first = guarded(&stage, BOOT_ID, BOOT, 0, found);
        assert_eq!(held_for(&stage, &first).as_deref(), Some("10"), "{stamp:?}");
        assert_eq!(
            std::fs::read_to_string(stage.join("exec")).unwrap(),
            format!("{BOOT_ID} {} {BOOT} 0\n", UP + 7_200),
            "{stamp:?} is stamped again"
        );
        let later = guarded(&stage, BOOT_ID, BOOT, 0, found + 9 * 60);
        assert_eq!(held_for(&stage, &later).as_deref(), Some("1"), "{stamp:?}");
        let done = guarded(&stage, BOOT_ID, BOOT, 0, found + 10 * 60);
        assert_eq!(held_for(&stage, &done), None, "{stamp:?}: the hold ends");
    }

    // A wake since the stamp after the wall clock stepped: the wake is not on `up`.
    staged(&stage, Some(&format!("{BOOT_ID} {UP} {BOOT} 0\n")), false);
    let (boot, woke) = (BOOT + 1_200, BOOT + UP + 7_200);
    let first = guarded(&stage, BOOT_ID, boot, woke, woke + 3_600);
    assert_eq!(held_for(&stage, &first).as_deref(), Some("10"));
    let later = guarded(&stage, BOOT_ID, boot, woke, woke + 3_600 + 10 * 60);
    assert_eq!(held_for(&stage, &later), None);
}

#[test]
fn the_root_shell_drops_the_channel_before_it_reads_anything() {
    let a = archive(true);
    let s = root_script(&install_of(&a));
    assert!(s.starts_with("exec </dev/null\nset -e\n"), "{s}");
    let secret = Secret::new(b"x").unwrap();
    let fed = install_command(&install_of(&a), Sudo::Password(&secret));
    assert!(
        fed.starts_with("/usr/bin/sudo -S -p '' -- /bin/sh -c 'exec </dev/null"),
        "{fed}"
    );
    assert_eq!(fed.matches("/usr/bin/sudo").count(), 1, "{fed}");
    assert!(fed.ends_with(
        "status=$?\n/bin/rm -f /var/folders/yt/x/T/aterm-agent-abababab.zip \
         /var/folders/yt/x/T/aterm-allowed-signers-abababab\nexit $status\n"
    ));
    let tty = install_command(&install_of(&a), Sudo::Terminal);
    assert!(tty.starts_with("/usr/bin/sudo -- /bin/sh -c "), "{tty}");
}

#[test]
fn the_master_trusts_only_the_vouched_key_and_takes_one_password_by_askpass() {
    let m = Master {
        ctl: "/c/ctl/0123abcd".into(),
        destination: "admin@192.168.64.2".into(),
    };
    let cmd = master_cmd(
        &m,
        Path::new("/s/known_hosts"),
        "aterm-machines-enroll",
        Path::new("/b/aterm-machines-askpass"),
        PASSWORD_METHOD,
    );
    let line = cmd.display();
    for want in [
        "SSH_ASKPASS=/b/aterm-machines-askpass SSH_ASKPASS_REQUIRE=force /usr/bin/ssh",
        "-F /dev/null",
        "-M -S /c/ctl/0123abcd",
        "ControlPersist=120",
        "StrictHostKeyChecking=yes",
        "UserKnownHostsFile=/s/known_hosts",
        "HostKeyAlias=aterm-machines-enroll",
        "PubkeyAuthentication=no",
        "PreferredAuthentications=password -o NumberOfPasswordPrompts=1",
        "-- admin@192.168.64.2 true",
    ] {
        assert!(line.contains(want), "{want}: {line}");
    }
    assert_eq!(cmd.env.len(), 2);
    let remote = remote_sh(&m, "echo ===", false).display();
    assert!(
        remote.ends_with(r"-- admin@192.168.64.2 '/bin/sh -c '\''echo ==='\'''"),
        "{remote}"
    );
    assert!(remote.contains("ControlMaster=no") && remote.contains("BatchMode=yes"));
    let copy = copy_cmd(&m, &[Path::new("/s/a.zip")], "/t/").display();
    for line in [&remote, &copy] {
        for want in [
            "-o UserKnownHostsFile=/dev/null -o GlobalKnownHostsFile=/dev/null -o \
             StrictHostKeyChecking=yes",
            "PubkeyAuthentication=no",
        ] {
            assert!(line.contains(want), "{want}: {line}");
        }
    }
}

#[test]
fn a_refusal_names_the_methods_the_mac_offers() {
    assert_eq!(
        denied_methods(DENIED),
        Some("publickey,password,keyboard-interactive")
    );
    assert_eq!(
        denied_methods("ssh: connect to host x port 22: No route to host"),
        None
    );
}

const ROOT: &str = "/ctl/machines";
const VM_UUID: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";

fn ctl() -> Paths {
    Paths {
        root: PathBuf::from(ROOT),
    }
}

/// The forced command's first hello answers for the Mac being enrolled when its UUID
/// differs only in case (the UUID is hex). NEGATIVE CONTROL: another UUID is refused.
#[test]
fn the_first_hello_matches_the_mac_s_uuid_in_either_case() {
    let host = Host {
        name: "lab-01".into(),
        uuid: VM_UUID.to_ascii_lowercase(),
        address: "192.168.64.4".into(),
        user: "admin".into(),
        baseline: None,
        users: Vec::new(),
    };
    let key = ssh::request_cmd(&ctl(), &Target::of(&host), "hello")
        .unwrap()
        .display();
    for (answers, ok) in [
        (VM_UUID, true),
        ("C3396674-FCD3-52E5-A1C1-BD5B9E8F5C44", false),
    ] {
        let sys = Fake::root();
        let hello = Hello {
            uuid: answers.into(),
            ..Hello::default()
        };
        sys.on(&key, Ran::ok(&aterm_json::to_string(&hello).unwrap()));
        let got = first_hello(&sys, &ctl(), &host);
        if ok {
            assert_eq!(got.unwrap().uuid, VM_UUID);
        } else {
            let why = got.unwrap_err();
            assert!(why.contains("the forced command answers as"), "{why}");
        }
    }
}

fn master() -> Master {
    Master {
        ctl: "/c/ctl/0123abcd".into(),
        destination: "admin@192.168.64.4".into(),
    }
}

fn request(name: Option<&str>) -> Request {
    Request {
        address: "192.168.64.4".into(),
        user: "admin".into(),
        name: name.map(str::to_owned),
        trust: Trust::FirstUse,
    }
}

fn vm_key() -> HostKey {
    HostKey {
        kind: "ssh-ed25519".into(),
        blob: VM_KEY.into(),
        fingerprint: VM_FP.into(),
    }
}

fn enrolling(holder: &str, uuid: &str) -> Fake {
    let sys = Fake::root();
    sys.file(
        &format!("{ROOT}/machines.toml"),
        format!(
            "schema = 1\n[[host]]\nname = \"{holder}\"\nuuid = \"{uuid}\"\naddress = \
             \"192.168.64.3\"\nuser = \"admin\"\n"
        )
        .as_bytes(),
    );
    sys.on(
        &remote_sh(&master(), FACTS_SCRIPT, false).display(),
        Ran::ok(&vm_facts()),
    );
    sys
}

fn installed_anything(sys: &Fake) -> bool {
    sys.calls().iter().any(|c| {
        c.contains("ditto")
            || c.contains("scp")
            || c.starts_with("tty ")
            || (c.starts_with("fed ") && c.contains("agent install"))
    })
}

const DENIED: &str =
    "admin@192.168.64.5: Permission denied (publickey,password,keyboard-interactive).\n";
const SUDO_WRONG: &str =
    "Sorry, try again.\n\nsudo: no password was provided\nsudo: 1 incorrect password attempt\n";
const RIGHT: &str = "hunter2-right";
const TYPO: &str = "hunter2-typo";
const EXE: &str = "/w/aterm-machines-dev";
const FLEET: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJpFPtSPTOuLIj6oqQTj8ndNDVNHFp4renP47+wxFYao aterm-machines-fleet@ctl";
const TMP: &str = "/var/folders/yt/jkgjjlb55fvbjsx8t5xw72280000gn/T";
const UUID_A: &str = "0A1B2C3D-0000-4000-8000-00000000000A";
const UUID_B: &str = "0A1B2C3D-0000-4000-8000-00000000000B";

fn set_up() -> Fake {
    let sys = Fake::root();
    sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
    sys.file(
        &format!("{ROOT}/allowed_signers"),
        format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {VM_KEY}\n").as_bytes(),
    );
    sys.file(
        &format!("{ROOT}/keys/ssh_ed25519.pub"),
        format!("{FLEET}\n").as_bytes(),
    );
    let zip = Fake::scratch_path(None, "enroll", 0).join("aterm-agent.zip");
    let ditto = format!(
        "/usr/bin/ditto -c -k --norsrc --noextattr --noqtn --noacl {EXE} {}",
        zip.display()
    );
    sys.on(&ditto, Ran::ok(""));
    sys.on_write(&ditto, &zip, b"the agent");
    sys
}

fn payload() -> Payload {
    let sys = set_up();
    build_payload(
        &sys,
        &ctl(),
        Path::new(EXE),
        &Fake::scratch_path(None, "enroll", 0),
    )
    .unwrap()
}

fn master_of(address: &str) -> Master {
    master_as("admin", address)
}

fn master_as(user: &str, address: &str) -> Master {
    Master {
        ctl: ctl().control_path(address),
        destination: format!("{user}@{address}"),
    }
}

fn login_key(address: &str, seq: u64, method: &str) -> String {
    login_key_as("admin", address, seq, method)
}

fn login_key_as(user: &str, address: &str, seq: u64, method: &str) -> String {
    let known = Fake::scratch_path(None, "enroll", seq).join("known_hosts");
    let askpass = Fake::scratch_path(None, "enroll", 0).join(crate::askpass::ALIAS);
    format!(
        "fed {}",
        master_cmd(
            &master_as(user, address),
            &known,
            "aterm-machines-enroll",
            &askpass,
            method
        )
        .display()
    )
}

fn install_key(address: &str) -> String {
    let p = payload();
    let tag = p.tag().to_owned();
    let install = Install {
        archive: &p.archive,
        remote_archive: format!("{TMP}/aterm-agent-{tag}.zip"),
        remote_signers: format!("{TMP}/aterm-allowed-signers-{tag}"),
        signers_sha: p.signers_sha.clone(),
        admin: "admin".into(),
        fleet_key: p.fleet_key.clone(),
    };
    let secret = Secret::new(b"x").unwrap();
    format!(
        "fed {}",
        remote_sh(
            &master_of(address),
            &install_command(&install, Sudo::Password(&secret)),
            false
        )
        .display()
    )
}

fn a_mac(sys: &Fake, address: &str, uuid: &str, local: &str, install: Ran) {
    scanned(sys, address, Some(VM_KEY), denied());
    sys.on(
        "/usr/bin/ssh-keygen -lf -",
        Ran::ok(&format!("256 {VM_FP} {address} (ED25519)\n")),
    );
    let master = master_of(address);
    sys.on(
        &remote_sh(&master, FACTS_SCRIPT, false).display(),
        Ran::ok(
            &vm_facts()
                .replace(VM_UUID, uuid)
                .replace("Manageds-Virtual-Machine\n", &format!("{local}\n")),
        ),
    );
    let p = payload();
    sys.on(
        &copy_cmd(&master, &[&p.archive.path, &p.signers], &format!("{TMP}/")).display(),
        Ran::ok(""),
    );
    sys.on(&install_key(address), install);
}

fn batch(addresses: &[&str], user_given: bool) -> Args {
    Args {
        addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
        found: false,
        user: "admin".into(),
        user_given,
        user_recorded: false,
        name: None,
        trust: Trust::FirstUse,
        parallel: Some(1),
    }
}

fn run_batch(sys: &Fake, args: &Args) -> (Result<Summary, String>, String) {
    let mut out: Vec<u8> = Vec::new();
    let done = {
        let sink: Sink<'_> = Mutex::new(&mut out as &mut (dyn Write + Send));
        enroll_macs(
            sys,
            &ctl(),
            args,
            &args.requests(),
            Path::new(EXE),
            1,
            &sink,
        )
    };
    (done, String::from_utf8(out).unwrap())
}

fn never_shown(sys: &Fake, out: &str, password: &str) {
    for call in sys.calls() {
        assert!(!call.contains(password), "{call}");
    }
    assert!(!out.contains(password), "{out}");
    for path in [
        format!("{ROOT}/machines.toml"),
        format!("{ROOT}/known_hosts"),
        format!("{ROOT}/state/{UUID_A}.json"),
    ] {
        let bytes = sys.written(&path).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(password),
            "{path}"
        );
    }
}

#[test]
fn one_password_typed_once_logs_in_and_sudoes_on_every_mac() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(
        &sys,
        "lab-02.local",
        UUID_A,
        "lab-02",
        Ran::ok("installed\n"),
    );
    a_mac(
        &sys,
        "lab-04.local",
        UUID_B,
        "lab-04",
        Ran::ok("installed\n"),
    );
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
    let summary = done.unwrap();
    let names: Vec<&str> = summary
        .enrolled
        .iter()
        .map(|(_, h)| h.name.as_str())
        .collect();
    assert_eq!(names, ["lab-02", "lab-04"], "{out}");
    assert!(summary.skipped.is_empty());
    assert!(
        summary
            .text()
            .starts_with("enrolled 2 (lab-02, lab-04)\nNext:"),
        "{}",
        summary.text()
    );
    let calls = sys.calls();
    let asks: Vec<&String> = calls.iter().filter(|c| c.starts_with("ask")).collect();
    assert_eq!(
        asks,
        [
            "ask secret Password for admin on the 2 Macs (typed once, used for each, never \
             saved): "
        ]
    );
    let asked = calls.iter().position(|c| c.starts_with("ask")).unwrap();
    let last_scan = calls.iter().rposition(|c| c.contains(" hostkey@")).unwrap();
    let first_fed = calls.iter().position(|c| c.starts_with("fed ")).unwrap();
    assert!(last_scan < asked && asked < first_fed, "{calls:?}");
    let fed = sys.fed();
    assert_eq!(fed.len(), 4, "{fed:?}");
    assert!(fed.iter().all(|(_, bytes)| bytes == RIGHT.as_bytes()));
    never_shown(&sys, &out, RIGHT);
    assert!(
        out.contains("lab-02: first connection to this Mac (its ID: SHA256:"),
        "{out}"
    );
    assert!(out.contains("lab-04: enrolled as lab-04"), "{out}");
    assert!(calls.contains(&format!(
        "symlink {} -> {EXE}",
        Fake::scratch_path(None, "enroll", 0)
            .join(crate::askpass::ALIAS)
            .display()
    )));
    let inv = inventory::load(&sys, &ctl()).unwrap();
    assert_eq!(inv.hosts.len(), 2);
}

#[test]
fn a_mac_that_refuses_the_password_is_asked_for_once_more_by_name() {
    let sys = set_up();
    sys.answer(Some(TYPO)).answer(None).answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    sys.on(
        &login_key("lab-04.local", 2, PASSWORD_METHOD),
        Ran::exit(255, DENIED),
    );
    sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], false));
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 2, "{out}");
    let asks: Vec<String> = sys
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("ask"))
        .collect();
    assert_eq!(
        asks,
        [
            "ask secret Admin password on the 2 Macs (typed once, used for each, never \
             saved): "
                .to_owned(),
            "ask What is the admin account called on the Macs? (Enter keeps admin): ".to_owned(),
            "ask secret lab-04 did not take the password for admin. Type it again (Enter \
             skips it): "
                .to_owned(),
        ]
    );
    assert!(
        out.contains(
            "lab-04: no login as admin with that password (a wrong password, or no account \
             called admin there)"
        ),
        "{out}"
    );
    let fed: Vec<(String, Vec<u8>)> = sys.fed();
    let lab04: Vec<&[u8]> = fed
        .iter()
        .filter(|(k, _)| k.contains("lab-04.local"))
        .map(|(_, b)| b.as_slice())
        .collect();
    assert_eq!(
        lab04,
        [TYPO.as_bytes(), RIGHT.as_bytes(), RIGHT.as_bytes()],
        "the typo, then the retyped password to its login and its sudo"
    );
    never_shown(&sys, &out, TYPO);
    never_shown(&sys, &out, RIGHT);
}

#[test]
fn a_retyped_password_that_works_is_tried_on_the_others_before_each_is_asked() {
    let sys = set_up();
    sys.answer(Some(TYPO)).answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    for (address, seq) in [("lab-02.local", 1), ("lab-04.local", 2)] {
        sys.on(
            &login_key(address, seq, PASSWORD_METHOD),
            Ran::exit(255, DENIED),
        );
        sys.on(&login_key(address, seq, PASSWORD_METHOD), Ran::ok(""));
    }
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
    assert_eq!(done.unwrap().enrolled.len(), 2, "{out}");
    let asks = sys.calls().iter().filter(|c| c.starts_with("ask")).count();
    assert_eq!(asks, 2, "a typo costs one more prompt, not one per Mac");
}

#[test]
fn a_mac_that_refuses_every_password_typed_is_skipped_with_one_line_and_the_rest_go_on() {
    let sys = set_up();
    sys.answer(Some(RIGHT)).answer(Some(TYPO));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    sys.on(
        &login_key("lab-02.local", 1, PASSWORD_METHOD),
        Ran::exit(255, DENIED),
    );
    sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 1, "{out}");
    assert_eq!(summary.skipped.len(), 1);
    let text = summary.text();
    assert!(
        text.starts_with(
            "enrolled 1 (lab-04); not enrolled: 1\n  lab-02: no login as admin with the \
             password typed"
        ) && text.contains("; run the same enroll command again to retry it\nNext:"),
        "{text}"
    );
    assert!(
        out.contains("lab-02: did not take that password either"),
        "{out}"
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.contains("scp") && c.contains("lab-02.local"))
    );
}

fn logins(sys: &Fake, user: &str, address: &str, replies: &[bool]) {
    let seq = if address == "lab-02.local" { 1 } else { 2 };
    for &ok in replies {
        let reply = if ok {
            Ran::ok("")
        } else {
            Ran::exit(255, DENIED)
        };
        sys.on(&login_key_as(user, address, seq, PASSWORD_METHOD), reply);
    }
}

fn asks(sys: &Fake) -> Vec<String> {
    sys.calls()
        .into_iter()
        .filter(|c| c.starts_with("ask"))
        .collect()
}

#[test]
fn a_lab_admin_who_is_not_this_macs_login_types_the_password_once_and_the_name_once() {
    let sys = set_up();
    sys.answer(Some(RIGHT)).answer(Some("admin"));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    for address in ["lab-02.local", "lab-04.local"] {
        logins(&sys, "owner", address, &[false]);
        logins(&sys, "admin", address, &[true]);
    }
    let mut args = batch(&["lab-02.local", "lab-04.local"], false);
    args.user = "owner".into();
    let (done, out) = run_batch(&sys, &args);
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 2, "{out}");
    assert!(summary.enrolled.iter().all(|(_, h)| h.user == "admin"));
    assert_eq!(
        asks(&sys),
        [
            "ask secret Admin password on the 2 Macs (typed once, used for each, never \
             saved): ",
            "ask What is the admin account called on the Macs? (Enter keeps owner): ",
        ],
        "{out}"
    );
    assert!(
        out.contains("2 Macs (lab-02, lab-04): no login as owner with that password"),
        "{out}"
    );
    assert!(sys.fed().iter().all(|(_, bytes)| bytes == RIGHT.as_bytes()));
    never_shown(&sys, &out, RIGHT);
}

#[test]
fn a_typo_at_one_macs_prompt_costs_it_nothing_when_the_next_password_typed_is_right() {
    let sys = set_up();
    sys.answer(Some(TYPO))
        .answer(Some(TYPO))
        .answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    logins(&sys, "admin", "lab-02.local", &[false, false, true]);
    logins(&sys, "admin", "lab-04.local", &[false, true]);
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 2, "{out}\n{summary:?}");
    assert_eq!(asks(&sys).len(), 3, "{:?}", asks(&sys));
    assert!(
        out.contains("lab-02: did not take that password either"),
        "{out}"
    );
    let lab02: Vec<Vec<u8>> = sys
        .fed()
        .into_iter()
        .filter(|(k, _)| k.contains("lab-02.local"))
        .map(|(_, b)| b)
        .collect();
    assert_eq!(
        lab02,
        [
            TYPO.as_bytes(),
            TYPO.as_bytes(),
            RIGHT.as_bytes(),
            RIGHT.as_bytes()
        ],
        "its two typos, then lab-04's password to its login and its sudo"
    );
}

#[test]
fn a_mac_that_refuses_under_the_new_name_too_is_told_by_that_name() {
    let sys = set_up();
    sys.answer(Some(RIGHT)).answer(Some("admin"));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    for address in ["lab-02.local", "lab-04.local"] {
        logins(&sys, "owner", address, &[false]);
    }
    logins(&sys, "admin", "lab-02.local", &[true]);
    logins(&sys, "admin", "lab-04.local", &[false]);
    let mut args = batch(&["lab-02.local", "lab-04.local"], false);
    args.user = "owner".into();
    let (done, out) = run_batch(&sys, &args);
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 1, "{out}");
    let (label, why) = &summary.skipped[0];
    assert_eq!(label, "lab-04");
    assert!(
        why.starts_with("no login as admin with the password typed")
            && why.ends_with("not asked again: the prompt was stopped"),
        "{why}"
    );
    assert!(
        asks(&sys)
            .last()
            .is_some_and(|a| a.contains("lab-04 did not take the password for admin.")),
        "{:?}",
        asks(&sys)
    );
}

#[test]
fn of_several_macs_one_with_remote_login_off_says_so_before_the_password_is_asked() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    scanned(
        &sys,
        "192.168.64.9",
        None,
        Ran::exit(
            255,
            "kex_exchange_identification: Connection closed by remote host\n",
        ),
    );
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "192.168.64.9"], true));
    let summary = done.unwrap();
    assert!(
        out.lines()
            .next()
            .is_some_and(|l| l.starts_with("lab-02: first connection")),
        "{out}"
    );
    assert!(
        out.contains(
            "192.168.64.9: not enrolled: Remote Login is off: turn it on at that Mac \
             (System Settings ▸ General ▸ Sharing ▸ Remote Login), then run the same \
             enroll command again\n"
        ),
        "{out}"
    );
    assert_eq!(
        asks(&sys),
        ["ask secret Password for admin on lab-02.local (never saved): "]
    );
    assert!(
        summary.text().starts_with(
            "enrolled 1 (lab-02); not enrolled: 1\n  192.168.64.9: Remote Login is off"
        ),
        "{}",
        summary.text()
    );
}

#[test]
fn a_second_enroll_takes_the_recorded_admin_and_an_enrolled_mac_is_nothing_to_do() {
    let inv: Inventory = aterm_toml::from_str(&format!(
        "schema = 1\n[[host]]\nname = \"m201\"\nuuid = \"{UUID_A}\"\naddress = \
         \"192.168.64.20\"\nuser = \"admin\"\n[[host]]\nname = \"m203\"\nuuid = \
         \"{UUID_B}\"\naddress = \"192.168.64.23\"\nuser = \"labadmin\"\n[[host]]\nname = \
         \"m204\"\nuuid = \"{VM_UUID}\"\naddress = \"192.168.64.24\"\nuser = \"admin\"\n"
    ))
    .unwrap();
    assert_eq!(recorded_admin(&inv).as_deref(), Some("admin"));
    assert_eq!(recorded_admin(&Inventory::default()), None);
    let mut args = batch(&["192.168.64.21"], false);
    args.user = "owner".into();
    let args = with_recorded_admin(args, &inv);
    assert_eq!((args.user.as_str(), args.user_recorded), ("admin", true));
    let mut given = batch(&["192.168.64.21"], true);
    given.user = "root2".into();
    assert_eq!(with_recorded_admin(given, &inv).user, "root2");
    let summary = Summary {
        already: vec![("192.168.64.21".into(), "m202".into())],
        ..Summary::default()
    };
    assert!(summary.skipped.is_empty());
    assert_eq!(
        summary.text(),
        "enrolled 0; already enrolled, nothing to do: m202\n"
    );
}

#[test]
fn the_retry_prompt_says_why_it_asks_again_and_that_others_get_the_password_too() {
    let sys = set_up();
    sys.answer(Some(TYPO)).answer(None).answer(None);
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    logins(&sys, "admin", "lab-02.local", &[false]);
    logins(&sys, "admin", "lab-04.local", &[false]);
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "lab-04.local"], true));
    assert_eq!(
        asks(&sys)[1],
        "ask secret lab-02 did not take the password for admin. Type it again (it is also \
         tried on the other Macs that refused; Enter skips this Mac): "
    );
    let summary = done.unwrap();
    assert_eq!(summary.skipped.len(), 2, "{out}");
    assert!(
        summary.skipped[0].1.ends_with(
            "skipped at its password prompt: run the same enroll command again to enroll it"
        ),
        "{:?}",
        summary.skipped
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|c| c.contains("scp") && c.contains("lab-02.local"))
    );
}

#[test]
fn a_mac_whose_sudo_refuses_is_skipped_naming_sudo() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(
        &sys,
        "lab-02.local",
        UUID_A,
        "lab-02",
        Ran::exit(1, SUDO_WRONG),
    );
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
    let summary = done.unwrap();
    let (_, why) = &summary.skipped[0];
    assert!(
        why.contains("sudo refused admin's password"),
        "{why}\n{out}"
    );
    assert!(inventory::load(&sys, &ctl()).unwrap().hosts.is_empty());
}

#[test]
fn a_mac_another_controller_enrolled_is_skipped_with_what_its_agent_said() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    let refused = Ran {
        status: Some(2),
        stdout: b"refused: install: this Mac is already enrolled \
                  (/Library/Management/aterm/allowed_signers exists) and only a signed rekey \
                  changes who it trusts\n"
            .to_vec(),
        stderr: Vec::new(),
    };
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", refused);
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
    let (_, why) = &done.unwrap().skipped[0];
    assert!(
        why.starts_with("the install command line failed (exit 2: refused: install: this Mac is already enrolled"),
        "{why}"
    );
    assert!(out.contains("refused: install:"), "{out}");
}

#[test]
fn a_mac_offering_no_password_method_gets_keyboard_interactive() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    sys.on(
        &login_key("lab-02.local", 1, PASSWORD_METHOD),
        Ran::exit(
            255,
            "admin@lab-02.local: Permission denied (publickey,keyboard-interactive).\n",
        ),
    );
    sys.on(&login_key("lab-02.local", 1, KBD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
    assert_eq!(done.unwrap().enrolled.len(), 1, "{out}");
    assert_eq!(
        sys.calls().iter().filter(|c| c.starts_with("ask")).count(),
        1,
        "the same password, no second prompt"
    );
}

#[test]
fn a_controller_that_is_not_set_up_refuses_before_the_password_is_asked() {
    let sys = Fake::root();
    sys.file(&format!("{ROOT}/machines.toml"), b"schema = 1\n");
    let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
    assert!(done.unwrap_err().contains("aterm machines init"));
    assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
    let sys = set_up();
    sys.answer(None);
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
    assert!(done.unwrap_err().contains("no password typed"));
    assert!(sys.fed().is_empty(), "{:?}", sys.calls());
    let sys = set_up();
    scanned(&sys, "lab-02.local", None, Ran::exit(255, ""));
    let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
    let summary = done.unwrap();
    assert!(
        summary.skipped[0].1.contains("Remote Login"),
        "{:?}",
        summary.skipped
    );
    assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
}

#[test]
fn a_mac_with_remote_login_off_is_skipped_with_one_line_and_nothing_asked() {
    for (said, class) in [
        ("", NoHostKey::Silent),
        (
            "kex_exchange_identification: Connection closed by remote host\n",
            NoHostKey::Closed,
        ),
    ] {
        let sys = set_up();
        scanned(&sys, "192.168.64.9", None, Ran::exit(255, said));
        let inventory = format!("{ROOT}/machines.toml");
        let before = sys.written(&inventory);
        let (done, out) = run_batch(&sys, &batch(&["192.168.64.9"], true));
        let summary = done.unwrap();
        assert!(summary.enrolled.is_empty(), "{summary:?}");
        assert_eq!(
            summary.skipped,
            [("192.168.64.9".to_owned(), class.why("192.168.64.9"))]
        );
        assert!(out.is_empty(), "{out}");
        assert_eq!(
            summary.text().matches("192.168.64.9").count(),
            1,
            "{}",
            summary.text()
        );
        assert!(
            !sys.calls().iter().any(|c| c.starts_with("ask")),
            "{:?}",
            sys.calls()
        );
        assert_eq!(sys.written(&inventory), before);
    }
}

#[test]
fn a_host_at_an_enrolled_macs_address_is_never_trusted_on_first_use() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    sys.file(
        &format!("{ROOT}/machines.toml"),
        format!(
            "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
             \"lab-02.local\"\nuser = \"admin\"\n"
        )
        .as_bytes(),
    );
    sys.file(
        &format!("{ROOT}/known_hosts"),
        format!("{VM_UUID} ssh-ed25519 {OWN_KEY}\n").as_bytes(),
    );
    a_mac(&sys, "LAB-02.local.", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "lab-04.local", UUID_B, "lab-04", Ran::ok(""));
    sys.on(&login_key("lab-04.local", 2, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["LAB-02.local.", "lab-04.local"], true));
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 1, "{out}");
    let (label, why) = &summary.skipped[0];
    assert_eq!(label, "LAB-02.local.");
    assert!(
        why.contains("is where lab-02 is enrolled")
            && why.contains(VM_FP)
            && why.contains("unenroll lab-02 --unreachable")
            && why.contains("Nothing was sent"),
        "{why}"
    );
    assert!(
        sys.fed().iter().all(|(k, _)| !k.contains("LAB-02")),
        "{:?}",
        sys.fed()
    );
    let sys = set_up();
    sys.file(
        &format!("{ROOT}/machines.toml"),
        format!(
            "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
             \"lab-02.local\"\nuser = \"admin\"\n"
        )
        .as_bytes(),
    );
    sys.file(
        &format!("{ROOT}/known_hosts"),
        format!("{VM_UUID} ssh-ed25519 {VM_KEY}\n").as_bytes(),
    );
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    let (done, _) = run_batch(&sys, &batch(&["lab-02.local"], true));
    let summary = done.unwrap();
    assert_eq!(
        summary.already,
        [("lab-02".to_owned(), "lab-02".to_owned())]
    );
    assert!(summary.skipped.is_empty(), "{summary:?}");
    assert!(!sys.calls().iter().any(|c| c.starts_with("ask")));
}

#[test]
fn with_no_fingerprint_the_first_key_is_the_one_trusted_and_its_fingerprint_printed() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    let kept = Fake::scratch_path(None, "enroll", 1);
    sys.scratch_close_fails(&kept, Some(std::io::ErrorKind::PermissionDenied));
    let (_, out) = run_batch(&sys, &batch(&["lab-02.local"], true));
    let line = out.lines().find(|l| l.contains(VM_FP)).unwrap_or_default();
    assert!(line.contains("first connection to this Mac"), "{out}");
    assert_eq!(
        sys.written(&kept.join("known_hosts").to_string_lossy())
            .unwrap(),
        format!("aterm-machines-enroll ssh-ed25519 {VM_KEY}\n").as_bytes()
    );
}

#[test]
fn a_fingerprint_that_does_not_match_is_refused_before_any_password_is_sent() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    let mut args = batch(&["lab-02.local"], true);
    args.trust = Trust::Fingerprint("SHA256:other".into());
    let (done, _) = run_batch(&sys, &args);
    let (_, why) = &done.unwrap().skipped[0];
    assert!(why.contains("Nothing was sent"), "{why}");
    assert!(sys.fed().is_empty(), "{:?}", sys.calls());
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("ask")),
        "{:?}",
        sys.calls()
    );
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    args.trust = Trust::Fingerprint(VM_FP.into());
    let (done, out) = run_batch(&sys, &args);
    assert_eq!(done.unwrap().enrolled.len(), 1);
    assert!(out.contains(&format!("its ID is the one --fingerprint names ({VM_FP})")));
}

#[test]
fn two_addresses_of_one_mac_enroll_it_once() {
    let sys = set_up();
    sys.answer(Some(RIGHT));
    a_mac(&sys, "lab-02.local", UUID_A, "lab-02", Ran::ok(""));
    a_mac(&sys, "192.168.64.5", UUID_A, "lab-02", Ran::ok(""));
    sys.on(&login_key("lab-02.local", 1, PASSWORD_METHOD), Ran::ok(""));
    sys.on(&login_key("192.168.64.5", 2, PASSWORD_METHOD), Ran::ok(""));
    let (done, out) = run_batch(&sys, &batch(&["lab-02.local", "192.168.64.5"], true));
    let summary = done.unwrap();
    assert_eq!(summary.enrolled.len(), 1, "{out}");
    assert!(
        summary.skipped[0].1.contains("already enrolled as lab-02"),
        "{out}"
    );
}

#[test]
fn macs_in_flight_hold_their_uuid_and_name_against_each_other() {
    let sys = set_up();
    let paths = ctl();
    let scratch = Fake::scratch_path(None, "enroll", 0);
    let shared = Shared::new(&sys, &paths, Path::new(EXE), &scratch);
    let host = |name: &str, uuid: &str, address: &str| Host {
        name: name.into(),
        uuid: uuid.into(),
        address: address.into(),
        user: "admin".into(),
        baseline: None,
        users: Vec::new(),
    };
    shared
        .claim(&host("lab-02", UUID_A, "lab-02.local"))
        .unwrap();
    let err = shared
        .claim(&host("lab-02-again", UUID_A, "192.168.64.5"))
        .unwrap_err();
    assert!(err.contains("enrolling too"), "{err}");
    let err = shared
        .claim(&host("lab-02", UUID_B, "lab-04.local"))
        .unwrap_err();
    assert!(err.contains("the name lab-02 is taken"), "{err}");
    shared.release(UUID_A);
    shared
        .claim(&host("lab-02", UUID_B, "lab-04.local"))
        .unwrap();
}

#[test]
fn a_mac_is_labelled_by_its_tag_and_a_shared_label_by_its_address() {
    let reqs = |addresses: &[&str]| -> Vec<Request> { batch(addresses, true).requests() };
    assert_eq!(
        labels(&reqs(&[
            "m10-MacBook-M4-Max-36GB-1TB.local",
            "lab-02.local",
            "192.168.64.5"
        ])),
        ["m10", "lab-02", "192.168.64.5"]
    );
    assert_eq!(
        labels(&reqs(&["m1-Air.local", "m1-Pro.local"])),
        ["m1-Air.local", "m1-Pro.local"]
    );
}

const OWN_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIPfo6cCmlK4W9QpUZ7Jvt5f9a0r2n8eXaAZ0aqNjSm2r";

#[test]
fn found_takes_the_macs_it_sees_and_leaves_linux_boxes_this_mac_and_enrolled_ones() {
    let sys = discover::tests::lab_session();
    let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
    sys.file(
        OWN_HOST_KEY,
        format!("ssh-ed25519 {OWN_KEY} root@m14\n").as_bytes(),
    );
    sys.on(
        "/usr/sbin/scutil --get LocalHostName",
        Ran::ok("m14-Air-M5-32GB-4TB-prime\n"),
    );
    found_scanned(&sys, "lab-02.local", VM_KEY);
    let inv = Inventory::default();
    let (take, notes) = found_macs(&sys, &ctl(), &inv, &browse);
    assert_eq!(take, ["lab-02.local"]);
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("spark-1.local: left: not a Mac")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n == "m14-Air-M5-32GB-4TB-prime.local: left: this Mac, the controller"),
        "{notes:?}"
    );
    assert!(!sys.calls().iter().any(|c| c.contains("hostkey@spark-1")));
    sys.file(
        &format!("{ROOT}/known_hosts"),
        format!("{VM_UUID} ssh-ed25519 {VM_KEY}\n").as_bytes(),
    );
    let inv: Inventory = aterm_toml::from_str(&format!(
        "schema = 1\n[[host]]\nname = \"lab-02\"\nuuid = \"{VM_UUID}\"\naddress = \
         \"lab-02.local\"\nuser = \"admin\"\n"
    ))
    .unwrap();
    let (take, notes) = found_macs(&sys, &ctl(), &inv, &browse);
    assert!(take.is_empty(), "{take:?}");
    assert!(notes.contains(&"lab-02.local: left: enrolled already, as lab-02".to_owned()));
}

#[test]
fn found_says_so_when_the_sftp_browse_failed() {
    let sys = discover::tests::lab_session_with(Err("Bonjour error -65537".to_owned()));
    let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
    sys.on("/usr/sbin/scutil --get LocalHostName", Ran::exit(1, ""));
    found_scanned(&sys, "lab-02.local", VM_KEY);
    let (take, notes) = found_macs(&sys, &ctl(), &Inventory::default(), &browse);
    assert_eq!(take, ["lab-02.local"]);
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("spark-1.local: left: not known to be a Mac")),
        "{notes:?}"
    );
}

#[test]
fn found_knows_this_mac_by_its_host_key_when_its_name_says_nothing() {
    let sys = discover::tests::lab_session();
    let browse = discover::browse(&sys, Duration::from_secs(3)).unwrap();
    sys.file(
        OWN_HOST_KEY,
        format!("ssh-ed25519 {OWN_KEY} root@m14\n").as_bytes(),
    );
    sys.on("/usr/sbin/scutil --get LocalHostName", Ran::exit(1, ""));
    found_scanned(&sys, "lab-02.local", VM_KEY);
    found_scanned(&sys, "m14-Air-M5-32GB-4TB-prime.local", OWN_KEY);
    let (take, _) = found_macs(&sys, &ctl(), &Inventory::default(), &browse);
    assert_eq!(take, ["lab-02.local"]);
}

#[test]
fn a_name_another_mac_holds_is_refused_before_anything_is_installed() {
    let sys = enrolling(
        "Manageds-Virtual-Machine",
        "0A1B2C3D-0000-4000-8000-000000000003",
    );
    let mut out = Vec::new();
    let err = enroll_through(
        &sys,
        &ctl(),
        &request(None),
        Path::new("/w/aterm-machines-dev"),
        &master(),
        &vm_key(),
        Path::new("/s"),
        &mut out,
    )
    .unwrap_err();
    assert!(err.contains("taken") && err.contains("--name"), "{err}");
    assert!(!installed_anything(&sys), "{:?}", sys.calls());
}

#[test]
fn a_mac_is_named_by_the_tag_its_local_hostname_starts_with() {
    let sys = Fake::root();
    sys.file(
        &format!("{ROOT}/machines.toml"),
        b"schema = 1\n[[host]]\nname = \"m10\"\nuuid = \"0A1B2C3D-0000-4000-8000-000000000003\"\n\
          address = \"192.168.64.3\"\nuser = \"admin\"\n",
    );
    sys.on(
        &remote_sh(&master(), FACTS_SCRIPT, false).display(),
        Ran::ok(&vm_facts().replace(
            "Manageds-Virtual-Machine\n",
            "m10-MacBook-M4-Max-36GB-1TB\n",
        )),
    );
    let err = enroll_through(
        &sys,
        &ctl(),
        &request(None),
        Path::new("/w/aterm-machines-dev"),
        &master(),
        &vm_key(),
        Path::new("/s"),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(err.contains("the name m10 is taken"), "{err}");
    assert!(!installed_anything(&sys));
}

#[test]
fn a_name_whose_local_hostname_another_mac_would_get_is_refused_before_anything_is_installed() {
    let sys = enrolling("m1", "0A1B2C3D-0000-4000-8000-000000000003");
    sys.file(
        &format!("{ROOT}/baselines/default.toml"),
        crate::controller::templates::DEFAULT_BASELINE.as_bytes(),
    );
    let err = enroll_through(
        &sys,
        &ctl(),
        &request(Some("m1-spare")),
        Path::new("/w/aterm-machines-dev"),
        &master(),
        &vm_key(),
        Path::new("/s"),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(
        err.contains("m1-spare and m1 would both get the Local hostname m1-<hardware>")
            && err.contains("--name"),
        "{err}"
    );
    assert!(!installed_anything(&sys));
}

#[test]
fn a_known_hardware_uuid_says_how_to_retire_an_erased_mac_and_not_to_unenroll_a_copy() {
    let sys = enrolling("lab-03", VM_UUID);
    let mut out = Vec::new();
    let err = enroll_through(
        &sys,
        &ctl(),
        &request(Some("lab-09")),
        Path::new("/w/aterm-machines-dev"),
        &master(),
        &vm_key(),
        Path::new("/s"),
        &mut out,
    )
    .unwrap_err();
    assert!(
        err.contains("`aterm machines unenroll lab-03 --unreachable`"),
        "{err}"
    );
    assert!(err.contains("copy, leave lab-03 enrolled"), "{err}");
    assert!(!installed_anything(&sys));
}

#[test]
fn unenroll_refuses_a_host_whose_state_cannot_be_read_and_leaves_it_enrolled() {
    let sys = Fake::root();
    let inventory = format!(
        "schema = 1\n[github]\naccount = \"fleetbot\"\n[[host]]\nname = \"lab-03\"\nuuid = \
         \"{VM_UUID}\"\naddress = \"192.168.64.4\"\nuser = \"admin\"\n"
    );
    sys.file(&format!("{ROOT}/machines.toml"), inventory.as_bytes());
    sys.file(
        &format!("{ROOT}/state/{VM_UUID}.json"),
        b"{\"github_keys\":[[\"alice\",5",
    );
    let host = inventory::load(&sys, &ctl()).unwrap().hosts[0].clone();
    let mut out = Vec::new();
    let err = unenroll(&sys, &ctl(), &[], &host, true, &mut out).unwrap_err();
    assert!(
        err.contains("not unenrolled") && err.contains("aterm-machines:lab-03:"),
        "{err}"
    );
    assert_eq!(
        sys.written(&format!("{ROOT}/machines.toml")).unwrap(),
        inventory.as_bytes(),
        "still enrolled"
    );
    assert!(
        sys.calls().is_empty(),
        "nothing asked of GitHub or the host: {:?}",
        sys.calls()
    );
}

#[test]
fn unenroll_deletes_the_keys_over_agh_when_the_row_says() {
    let fake = agh_fake::FakeGithub::start();
    let login = super::super::github_keys_tests::LOGIN;
    let (sys, _token) = super::super::github_keys_tests::inline_on(Fake::root(), &ctl(), &fake);
    let inventory = format!(
        "schema = 1\n[github]\naccount = \"{login}\"\nkeys = \"agh-inline\"\n[[host]]\nname \
         = \"lab-03\"\nuuid = \"{VM_UUID}\"\naddress = \"192.168.64.4\"\nuser = \"admin\"\n"
    );
    sys.file(&format!("{ROOT}/machines.toml"), inventory.as_bytes());
    let id = fake.add_key(
        login,
        "aterm-machines:lab-03:alice",
        &format!("ssh-ed25519 {VM_KEY}"),
    );
    let state = inventory::HostState {
        github_keys: vec![("alice".into(), id)],
        ..inventory::HostState::default()
    };
    inventory::save_state(&sys, &ctl(), VM_UUID, &state).unwrap();
    let host = inventory::load(&sys, &ctl()).unwrap().hosts[0].clone();
    let mut out = Vec::new();
    unenroll(&sys, &ctl(), &[], &host, true, &mut out).unwrap();
    let saw = super::super::github_keys_tests::requests(&fake.seen());
    assert!(saw.contains(&format!("DELETE /user/keys/{id}")), "{saw:?}");
    assert!(fake.keys(login).is_empty());
    assert!(
        !sys.calls().iter().any(|c| c.starts_with("github ")),
        "no REST call: {:?}",
        sys.calls()
    );
}

#[test]
fn an_uninstall_stream_ends_in_its_report_or_its_refusal() {
    let refused = "{\"refused\":{\"why\":\"bad-signature: No principal matched.\"}}\n";
    assert_eq!(
        report_of(refused).unwrap_err(),
        "refused: bad-signature: No principal matched."
    );
    assert!(report_of("").is_err());
}
