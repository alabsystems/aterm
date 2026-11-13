// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::items::defaults::tests::{ctx, user};
use crate::port::{Fake, Stat};
use aterm_types::machine_name::{Nickname, Number};

const STATUS: &str = "/usr/sbin/DevToolsSecurity -status";

fn console() -> Ctx {
    let mut ctx = ctx(&[]);
    let mut user = user("alice", 501);
    user.admin = false;
    ctx.console = Some(user);
    ctx
}

fn rule_stat(uid: u32, mode: u32) -> Stat {
    Stat {
        uid,
        gid: 0,
        mode,
        dir: false,
        symlink: false,
        len: 1,
    }
}

/// The account owes developer mode while its status says off, and naming while no
/// nonempty root-owned 0440 file stands. Unknown status or metadata admits no elevation.
/// NEGATIVE CONTROLS: empty, directory, symlink and account-writable rules are not in
/// place; an unread refusal record is not evidence that no earlier refusal exists.
#[test]
fn owed_reads_each_step_and_an_unreadable_one_owes_nothing() {
    let sys = Fake::root();
    sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"));
    assert_eq!(owed(&sys), vec![Owed::Debugger, Owed::NameRule]);

    let on = Fake::root();
    on.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
        .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
    assert_eq!(owed(&on), Vec::<Owed>::new());

    on.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Indexing enabled."),
    );
    on.on(
        "/usr/bin/mdutil -s /System/Volumes/Data",
        Ran::ok("Indexing disabled."),
    );
    assert_eq!(owed(&on), vec![Owed::Spotlight]);
    assert_eq!(owed(&on), Vec::<Owed>::new());

    let unread = Fake::root();
    unread
        .on(
            STATUS,
            Ran::ok("Developer mode rights are in an unknown state.\n"),
        )
        .stat_fails(ADMITTED_NAME_RULE, std::io::ErrorKind::PermissionDenied);
    assert_eq!(owed(&unread), Vec::<Owed>::new());

    for stat in [
        rule_stat(501, 0o440),
        rule_stat(0, 0o666),
        Stat {
            len: 0,
            ..rule_stat(0, 0o440)
        },
        Stat {
            dir: true,
            ..rule_stat(0, 0o440)
        },
        Stat {
            symlink: true,
            ..rule_stat(0, 0o440)
        },
    ] {
        let planted = Fake::root();
        planted
            .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .stat_of(ADMITTED_NAME_RULE, stat);
        assert_eq!(rule_in_place(&planted), Some(false));
        assert_eq!(owed(&planted), vec![Owed::NameRule]);
        assert!(
            name_by_rule(
                &planted,
                &GeneratedNames::number_only(Number::parse("m42").unwrap()),
                &|| Ok(())
            )
            .is_err()
        );
        assert!(!planted.calls().iter().any(|call| call.contains("scutil")));
    }
    // An unread refusal record is not evidence that no earlier refusal exists.
    for (kind, missing) in [
        (std::io::ErrorKind::NotFound, true),
        (std::io::ErrorKind::PermissionDenied, false),
        (std::io::ErrorKind::Other, false),
    ] {
        let sys = Fake::root();
        sys.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .stat_fails(REFUSED_RULE, kind);
        assert_eq!(owed(&sys).contains(&Owed::NameRule), missing, "{kind:?}");
    }
}

/// The raw process boundary requires complete UTF-8 streams and a successful definite
/// answer. Neither a valid prefix before corrupt bytes nor a failed tool can authorize
/// the root item, even if the other stream looks like a disabled-mode sentence.
#[test]
fn debugger_setup_refuses_incomplete_or_unsuccessful_tool_results() {
    let disabled = b"Developer mode is currently disabled.\n";
    let mut corrupt = disabled.to_vec();
    corrupt.push(0xff);
    for ran in [
        Ran {
            status: Some(0),
            stdout: corrupt,
            stderr: Vec::new(),
        },
        Ran {
            status: Some(0),
            stdout: disabled.to_vec(),
            stderr: vec![0xff],
        },
        Ran::exit(1, "Developer mode is currently disabled.\n"),
        Ran {
            status: None,
            stdout: disabled.to_vec(),
            stderr: Vec::new(),
        },
    ] {
        assert_eq!(developer_mode(&ran), None, "{ran:?}");
        let sys = Fake::root();
        sys.on(STATUS, ran.clone());
        assert!(matches!(
            DeveloperMode.probe(&sys, &ctx(&[])),
            Probe::Unusable { .. }
        ));
        assert!(!owed(&sys).contains(&Owed::Debugger));
        // A known-off probe can become unread before apply; the old plan grants no
        // authority to enable the switch from that new unknown reading.
        let stale = Fake::root();
        stale
            .on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
            .on(STATUS, ran)
            .on("/usr/sbin/DevToolsSecurity -enable", Ran::ok(""));
        assert!(matches!(
            DeveloperMode.probe(&stale, &ctx(&[])),
            Probe::Change { .. }
        ));
        assert!(DeveloperMode.apply(&stale, &ctx(&[])).is_err());
        assert!(
            !stale
                .calls()
                .iter()
                .any(|call| call.contains("-enable") || call.contains("-o edit"))
        );
    }
}

#[test]
fn membership_admission_requires_a_complete_answer_and_expected_exit() {
    let yes = b"yes alice is a member of _developer\n";
    let no = b"no alice is NOT a member of _developer\n";
    let reading = |status, stdout: &[u8], stderr: &[u8]| Ran {
        status,
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    };
    for (ran, expected) in [
        (reading(Some(0), yes, b""), Some(true)),
        (reading(Some(0), no, b""), Some(false)),
        (reading(Some(67), no, b""), Some(false)),
    ] {
        assert_eq!(member(&ran), expected);
    }
    let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    for ran in [
        reading(Some(1), yes, b""),
        reading(Some(1), no, b""),
        reading(Some(67), yes, b""),
        reading(None, yes, b""),
        reading(None, no, b""),
        reading(Some(0), b"yes alice is a member\xff", b""),
        reading(Some(0), b"no alice is NOT a member\xff", b""),
        reading(Some(0), yes, b"\xff"),
        reading(Some(0), yes, b"directory service warning\n"),
        reading(
            Some(0),
            b"yes alice is a member\nno alice is NOT a member\n",
            b"",
        ),
    ] {
        assert_eq!(member(&ran), None, "{ran:?}");
        for boundary in [admin, developer] {
            let ctx = console();
            let user = ctx.console.as_ref().unwrap();
            let mut sys = Fake::root();
            sys.euid = user.uid;
            sys.logged_in(user)
                .stat_of("/dev/console", rule_stat(user.uid, 0o600))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .on(
                    admin,
                    if boundary == admin {
                        ran.clone()
                    } else {
                        reading(Some(67), no, b"")
                    },
                )
                .on(
                    developer,
                    if boundary == developer {
                        ran.clone()
                    } else {
                        reading(Some(67), no, b"")
                    },
                );
            assert!(!owed(&sys).contains(&Owed::Debugger), "{boundary}: {ran:?}");
            assert!(matches!(
                DeveloperMode.probe(&sys, &ctx),
                Probe::Unusable { .. }
            ));
            DeveloperMode.apply(&sys, &ctx).unwrap();
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains("-o edit") || call.contains("-enable"))
            );
        }
    }
}

#[test]
fn incomplete_root_query_capture_admits_no_debugger_elevation() {
    let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    for query in [STATUS, admin, developer] {
        let ctx = console();
        let user = ctx.console.as_ref().unwrap();
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(user)
            .stat_of("/dev/console", rule_stat(user.uid, 0o600))
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
            .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
            .on(
                developer,
                Ran::ok("no alice is NOT a member of _developer\n"),
            )
            .refuse_complete_stdout(query, "stdout did not reach EOF");
        assert!(
            matches!(DeveloperMode.probe(&sys, &ctx), Probe::Unusable { .. }),
            "{query}"
        );
        assert!(!owed(&sys).contains(&Owed::Debugger), "{query}");
    }
    // A mode sentence on stdout cannot override unproven diagnostic capture.
    let sys = Fake::root();
    sys.on(
        STATUS,
        Ran {
            status: Some(0),
            stdout: b"Developer mode is currently disabled.\n".to_vec(),
            stderr: vec![b' '; aterm_uds::deadline::OUTPUT_KEEP + 1],
        },
    );
    assert!(matches!(
        DeveloperMode.probe(&sys, &ctx(&[])),
        Probe::Unusable { .. }
    ));
    assert!(!owed(&sys).contains(&Owed::Debugger));
}

/// A Mac whose mode is already on still needs the root batch when its console account
/// cannot use it. The admission and the root item's own probe agree; once membership is
/// present neither owes a change. Both readings are prompt-free and mutate nothing.
#[test]
fn enabled_mode_still_owes_the_console_accounts_missing_membership() {
    let check = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    let admin_check = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    for (admin, admin_answer, answer, missing) in [
        (
            false,
            "no alice is NOT a member of admin\n",
            "no alice is NOT a member of _developer\n",
            true,
        ),
        (
            false,
            "no alice is NOT a member of admin\n",
            "yes alice is a member of _developer\n",
            false,
        ),
        (
            false,
            "yes alice is a member of admin\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
        (
            true,
            "membership could not be read\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
    ] {
        let mut ctx = console();
        let user = ctx.console.as_mut().unwrap();
        user.admin = admin;
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(user)
            .stat_of("/dev/console", rule_stat(user.uid, 0o600))
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
            .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .on(admin_check, Ran::ok(admin_answer))
            .on(check, Ran::ok(answer));
        assert_eq!(
            owed(&sys),
            if missing {
                vec![Owed::Debugger]
            } else {
                Vec::new()
            }
        );
        assert_eq!(
            matches!(DeveloperMode.probe(&sys, &ctx), Probe::Change { .. }),
            missing
        );
        if admin || admin_answer.starts_with("yes ") {
            DeveloperMode.apply(&sys, &ctx).unwrap();
            assert!(
                !sys.calls().iter().any(|call| call == check),
                "admins already have authorization: {:?}",
                sys.calls()
            );
        }
        if admin {
            assert!(
                !sys.calls().iter().any(|call| call == admin_check),
                "a known admin needs no further membership check"
            );
        }
        assert!(
            sys.calls().iter().all(|call| {
                !call.contains("-enable")
                    && !call.contains("-o edit")
                    && !call.starts_with("write ")
            }),
            "{:?}",
            sys.calls()
        );
    }
}

/// Membership that cannot be confirmed, or belongs to another console account, is no
/// reason to ask this login for a password. An unreadable mode keeps the same policy even
/// if a membership query could prove this account absent.
#[test]
fn uncertain_or_other_accounts_membership_admits_no_elevation() {
    let mut user = user("alice", 501);
    user.admin = false;
    let check = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    let admin_check = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    for (console_uid, mode, admin_answer, answer, queries_member) in [
        (
            Some(501),
            "Developer mode is currently enabled.\n",
            "no alice is NOT a member of admin\n",
            "membership could not be read\n",
            true,
        ),
        (
            Some(501),
            "Developer mode is currently enabled.\n",
            "admin membership could not be read\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
        (
            Some(502),
            "Developer mode is currently enabled.\n",
            "no alice is NOT a member of admin\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
        (
            None,
            "Developer mode is currently enabled.\n",
            "no alice is NOT a member of admin\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
        (
            Some(501),
            "Developer mode rights are in an unknown state.\n",
            "no alice is NOT a member of admin\n",
            "no alice is NOT a member of _developer\n",
            false,
        ),
    ] {
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(&user)
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
            .on(STATUS, Ran::ok(mode))
            .on(admin_check, Ran::ok(admin_answer))
            .on(check, Ran::ok(answer));
        if let Some(uid) = console_uid {
            sys.stat_of("/dev/console", rule_stat(uid, 0o600));
        }
        assert_eq!(owed(&sys), Vec::<Owed>::new(), "{console_uid:?} {mode}");
        assert_eq!(
            sys.calls().iter().any(|call| call == check),
            queries_member,
            "{console_uid:?} {mode} {:?}",
            sys.calls()
        );
    }
}

/// A root batch already admitted for another step can enable mode without assigning a
/// role Directory Services could not read. Readback retains the unread authorization;
/// neither unknown admin membership nor unknown developer membership grants a new role.
#[test]
fn an_authorized_root_apply_never_grants_uncertain_membership() {
    let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    let developer = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    let enable = "/usr/sbin/DevToolsSecurity -enable";
    for (admin_answer, developer_answer) in [
        (
            "admin membership could not be read\n",
            "no alice is NOT a member of _developer\n",
        ),
        (
            "no alice is NOT a member of admin\n",
            "developer membership could not be read\n",
        ),
    ] {
        let sys = Fake::root();
        sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
            .on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
            .on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .on(admin, Ran::ok(admin_answer))
            .on(developer, Ran::ok(developer_answer))
            .on(enable, Ran::ok(""));
        let ctx = console();
        assert!(
            matches!(DeveloperMode.probe(&sys, &ctx), Probe::Change { .. }),
            "a definite mode-enable step does not wait for membership evidence"
        );
        DeveloperMode.apply(&sys, &ctx).unwrap();
        assert!(sys.calls().iter().any(|call| call == enable));
        assert!(
            !sys.calls().iter().any(|call| call.contains("-o edit")),
            "{:?}",
            sys.calls()
        );
        assert!(
            matches!(DeveloperMode.probe(&sys, &ctx), Probe::Unusable { .. }),
            "an enabled mode does not prove the account has authorization"
        );
    }
}

/// The broker is owed, as the account reads it, while its clone is missing or another
/// build's, or its plist is missing or another source's; nothing is owed once both are this
/// build's and its launchd job is running. NEGATIVE CONTROLS: a clone and a plist the account
/// cannot read owe nothing, and neither does a development build outside a bundle, nor any of
/// it off macOS.
#[test]
fn the_broker_is_owed_by_its_image_and_plist_and_an_unreadable_one_owes_nothing() {
    use aterm_uds::lan::{IMAGE_EXECUTABLE, PLIST_PATH, install};
    const SOURCE: &str = "/Applications/aterm.app";
    let ours = install::test_image(0x0100_000c, &[b"this build"]);
    let plist = install::plist_text(Path::new(SOURCE));
    let reading = |image: Option<&[u8]>, daemon: Option<&str>, denied: bool, linux: bool| {
        let mut sys = Fake::root();
        sys.running_app = Some(SOURCE.into());
        sys.linux = linux;
        sys.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
            .on(
                "/bin/launchctl print system/systems.alab.aterm-lan",
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
            )
            .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
            .file("/Applications/aterm.app/Contents/MacOS/aterm", &ours);
        if let Some(image) = image {
            sys.file(IMAGE_EXECUTABLE, image);
        }
        if let Some(daemon) = daemon {
            sys.file(PLIST_PATH, daemon.as_bytes());
        }
        if denied {
            sys.stat_fails(IMAGE_EXECUTABLE, std::io::ErrorKind::PermissionDenied)
                .stat_fails(PLIST_PATH, std::io::ErrorKind::PermissionDenied);
        }
        owed(&sys)
    };
    let theirs = install::test_image(0x0100_000c, &[b"another build"]);
    let elsewhere = install::plist_text(Path::new("/Users//a/Applications/aterm.app"));
    let broker = vec![Owed::LanBroker];
    assert_eq!(
        reading(None, Some(&plist), false, false),
        broker,
        "no image"
    );
    assert_eq!(
        reading(Some(&theirs), Some(&plist), false, false),
        broker,
        "cdhash differs"
    );
    assert_eq!(reading(Some(&ours), None, false, false), broker, "no plist");
    assert_eq!(
        reading(Some(&ours), Some(&elsewhere), false, false),
        broker,
        "plist differs"
    );
    assert_eq!(
        reading(Some(&ours), Some(&plist), false, false),
        Vec::<Owed>::new()
    );
    assert_eq!(
        reading(Some(&ours), Some(&plist), true, false),
        Vec::<Owed>::new(),
        "unread"
    );
    assert_eq!(
        reading(None, None, false, true),
        Vec::<Owed>::new(),
        "linux"
    );

    let dev = Fake::root();
    dev.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
        .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
    assert_eq!(owed(&dev), Vec::<Owed>::new(), "a development build");
}

/// Neither current files nor a signed image awaiting adoption make discovery available when
/// launchd has no running broker. An unreadable job does not authorize an elevation; a running
/// job needs none either.
#[test]
fn init_owes_a_stopped_broker_even_while_its_signed_update_is_adoptable() {
    use aterm_uds::lan::{IMAGE_EXECUTABLE, PLIST_PATH, install};
    let release_image = |page: u8| {
        let identifier = install::BUNDLE_ID;
        let ident_at = 52_u32;
        let team_at = ident_at + identifier.len() as u32 + 1;
        let mut body = Vec::new();
        for word in [0x20400_u32, 0, 0, ident_at, 0, 0, 0, 0, 0, 0, team_at] {
            body.extend_from_slice(&word.to_be_bytes());
        }
        body.extend_from_slice(identifier.as_bytes());
        body.extend_from_slice(b"\0ABCDE12345\0");
        body.push(page);
        install::test_image(0x0100_000c, &[&body])
    };
    let source_image = release_image(1);
    for installed_image in [source_image.clone(), release_image(2)] {
        for (job, expected) in [
            (Ran::exit(113, "Could not find service"), true),
            (
                Ran::ok("system/systems.alab.aterm-lan = {\n\tstate = waiting\n}\n"),
                true,
            ),
            (
                Ran::ok("system/systems.alab.aterm-lan = {\n\tpid = 77\n}\n"),
                false,
            ),
            (Ran::exit(1, "Operation not permitted"), false),
        ] {
            let mut sys = Fake::root();
            let source = Path::new("/Applications/aterm.app");
            sys.running_app = Some(source.into());
            sys.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
                .stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
                .file(
                    "/Applications/aterm.app/Contents/MacOS/aterm",
                    &source_image,
                )
                .file(IMAGE_EXECUTABLE, &installed_image)
                .file(PLIST_PATH, install::plist_text(source).as_bytes())
                .on("/bin/launchctl print system/systems.alab.aterm-lan", job);
            assert_eq!(owed(&sys).contains(&Owed::LanBroker), expected);
        }
    }
}

/// Account spelling cannot select the built-in ALL alias or an administrator's user alias.
#[test]
fn a_naming_account_cannot_be_a_sudoers_user_alias() {
    for account in ["ALL", "ADMINS", "BUILD_42"] {
        let rule = name_rule(account).unwrap();
        for line in rule.lines().filter(|line| !line.starts_with('#')) {
            assert!(
                line.starts_with(&format!("\"{account}\" ALL = (root) NOPASSWD:")),
                "{line}"
            );
        }
    }
}

/// The rule names Apple's `scutil` alone, anchored on both ends, one line per name, with no
/// unbounded wildcard. POSIX regex character classes and finite bounds remain within 1024
/// bytes. NEGATIVE CONTROL: an account name that could
/// carry sudoers syntax or an option, or root's, gets no rule.
#[test]
fn the_rule_lets_one_account_set_the_three_names_to_a_label_and_nothing_else() {
    let admits = |pattern: &str, cases: &[(String, bool)]| {
        // Evaluate the emitted POSIX expression with the OS's real grep, without a
        // shell, files, elevated identity or a second implementation of its grammar.
        // Each candidate remains one NUL-free grep record; parse only the added line
        // number, preserving the complete matched bytes, including CR and DEL.
        let mut input = Vec::new();
        for (args, _) in cases {
            assert!(!args.contains(['\n', '\0']), "{args:?}");
            input.extend_from_slice(args.as_bytes());
            input.push(b'\n');
        }
        let read = crate::port::Live::new()
            .run_complete_stdout_input(
                &Cmd::new("/usr/bin/grep")
                    .args(["-E", "-n", "--", pattern])
                    .env("LC_ALL", "C"),
                &input,
            )
            .unwrap();
        assert!(matches!(read.status, Some(0 | 1)), "{read:?}");
        assert!(read.stderr.is_empty(), "{read:?}");
        assert_eq!(
            read.status,
            Some(i32::from(read.stdout.is_empty())),
            "{read:?}"
        );
        let mut matched = vec![false; cases.len()];
        for line in read.stdout.split_inclusive(|byte| *byte == b'\n') {
            let line = line
                .strip_suffix(b"\n")
                .expect("complete grep output record");
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .expect("grep line number");
            let number = std::str::from_utf8(&line[..colon])
                .unwrap()
                .parse::<usize>()
                .unwrap();
            let index = number
                .checked_sub(1)
                .expect("grep line numbers start at one");
            let (args, _) = cases.get(index).expect("grep selected an input record");
            assert!(!matched[index], "duplicate grep row {number}: {args:?}");
            assert_eq!(
                &line[colon + 1..],
                args.as_bytes(),
                "grep row {number}: {args:?}"
            );
            matched[index] = true;
        }
        for ((args, expected), matched) in cases.iter().zip(matched) {
            assert_eq!(matched, *expected, "{args:?}");
        }
    };
    let text = name_rule("alice").unwrap();
    let rules: Vec<&str> = text.lines().filter(|line| !line.starts_with('#')).collect();
    assert_eq!(rules.len(), 3, "{text}");
    for (rule, pref) in rules.iter().zip(NAME_PREFS) {
        assert_eq!(
            *rule,
            format!(
                "\"alice\" ALL = (root) NOPASSWD: /usr/sbin/scutil ^--set {pref} {}$",
                name_pattern(pref)
            )
        );
        let args = rule
            .strip_prefix("\"alice\" ALL = (root) NOPASSWD: /usr/sbin/scutil ")
            .expect("one account and Apple's scutil");
        // Sudoers treats these anchored arguments as an ERE: commas inside
        // finite repetition bounds are regex syntax, not command separators.
        assert!(args.starts_with('^') && args.ends_with('$'), "{rule}");
        assert!(!args.contains(['=', '\\', '*']), "{rule}");
        assert!(args.len() <= 1024 && !args.contains(".*"), "{rule}");
        let named = |name: &str| format!("--set {pref} {name}");
        let mut cases = Vec::new();
        let mut case = |name: &str, expected| cases.push((named(name), expected));
        for name in ["m0", "m1", "m8", "m999999"] {
            case(name, true);
        }
        for name in [
            "",
            "m00",
            "m01",
            "m08",
            "m000000",
            "m1000000",
            "M0",
            "0",
            "m0;id",
            "unnumbered",
        ] {
            case(name, false);
        }
        case("m0 extra", pref == "ComputerName");
        if pref == "ComputerName" {
            for nickname in ["é", "研究室"] {
                let names = GeneratedNames::new(
                    Number::new(85).unwrap(),
                    &Nickname::parse(nickname).unwrap(),
                );
                case(names.computer_name(), true);
            }
            let longest = Nickname::parse(&format!("{}abcdef", "研".repeat(18))).unwrap();
            for number in [0, 1, 12, 123, 1234, 12345, 999999] {
                let names = GeneratedNames::new(Number::new(number).unwrap(), &longest);
                assert_eq!(names.computer_name().len(), 63);
                case(names.computer_name(), true);
                case(&format!("{}a", names.computer_name()), false);
            }
            // These remain one grep record. A newline requires a different record
            // protocol and is not an argv-control oracle in this line-based evaluator.
            for name in [
                "m0 \tDog",
                "m0 Dog\r",
                "m0 Dog\u{7f}",
                "m0 /etc/sudoers",
                "m0 ../../tmp",
                "m0 Dog;id",
                "m0 Dog --get HostName",
            ] {
                case(name, false);
            }
            case("m0 Dog", true);
            case("m00 Dog", false);
            case("m0 -Dog", false);
        } else {
            case("m0-dog", true);
            case("m00-dog", false);
            case(&format!("m0-{}", "a".repeat(60)), true);
            case(&format!("m0-{}", "a".repeat(61)), false);
        }
        admits(args, &cases);
    }
    for bad in [
        "",
        "root",
        "-alice",
        "al ice",
        "alice,ALL",
        "a:b",
        "x=y",
        "a\\b",
    ] {
        assert_eq!(name_rule(bad), None, "{bad:?}");
    }
}

#[test]
fn overlapping_rule_checks_publish_their_own_checked_files() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    let entered = Arc::new(AtomicBool::new(false));
    let checks = Arc::new(Mutex::new(Vec::new()));
    let observed = checks.clone();
    let sys = Fake::root();
    sys.simulate(move |sys, command| {
        if command.program == Path::new("/bin/chmod") {
            return Some(Ran::ok(""));
        }
        if command.program != Path::new(VISUDO) {
            return None;
        }
        let staged = std::path::PathBuf::from(command.args.last().unwrap());
        let bytes = sys.read_file(&staged).unwrap();
        observed.lock().unwrap().push((staged, bytes));
        if !entered.swap(true, Ordering::SeqCst) {
            let mut other = console();
            other.console.as_mut().unwrap().name = "bob".into();
            NameRule::Current.apply(sys, &other).unwrap();
        }
        Some(Ran::ok(""))
    });
    NameRule::Current.apply(&sys, &console()).unwrap();
    let checked = checks.lock().unwrap();
    assert_eq!(checked.len(), 2);
    assert_eq!(checked[0].1, name_rule("alice").unwrap().as_bytes());
    assert_eq!(checked[1].1, name_rule("bob").unwrap().as_bytes());
    assert_eq!(
        sys.read_file(Path::new(ADMITTED_NAME_RULE)).unwrap(),
        checked[0].1
    );
    assert_ne!(checked[0].0, checked[1].0);
    for (staged, _) in checked.iter() {
        assert!(sys.stat(staged).is_err());
        assert!(sys.stat(staged.parent().unwrap()).is_err());
    }
}

/// As root, the rule is staged where sudo reads nothing, checked by `visudo -c`, and only
/// then renamed into place. NEGATIVE CONTROL: a rule `visudo` refuses never goes live: it is
/// kept where sudo reads nothing, and the rule is no longer owed, so no later launch asks for
/// the password again for it.
#[test]
fn the_rule_goes_live_only_once_visudo_passes_it() {
    let watch = |sys: &Fake, reply: Ran| {
        let path = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed = path.clone();
        sys.simulate(move |sys, command| {
            if command.program == Path::new("/bin/chmod") {
                assert_eq!(command.args[0].to_str(), Some("+a"));
                assert_eq!(command.args[1].to_str(), Some("user:alice allow read"));
                let staged = Path::new(command.args.last().unwrap());
                assert_eq!(sys.stat(staged).unwrap().mode, 0o440);
                return Some(Ran::ok(""));
            }
            if command.program != Path::new(VISUDO) {
                return None;
            }
            let staged = std::path::PathBuf::from(command.args.last().unwrap());
            let parent = sys.stat(staged.parent().unwrap()).unwrap();
            assert!(parent.dir && !parent.symlink);
            assert_eq!(parent.uid, 0);
            assert_eq!(parent.mode & 0o777, 0o700);
            *observed.lock().unwrap() = Some(staged);
            Some(reply.clone())
        });
        path
    };
    let mut sys = Fake::root();
    let checked_path = watch(&sys, Ran::ok(""));
    let ctx = console();
    assert!(matches!(
        NameRule::Current.probe(&sys, &ctx),
        Probe::Change { .. }
    ));
    NameRule::Current.apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    let staged = calls.iter().position(|c| c.starts_with("write ")).unwrap();
    let readable = calls
        .iter()
        .position(|c| c.starts_with("/bin/chmod +a "))
        .unwrap();
    let checked = calls.iter().position(|c| c.starts_with(VISUDO)).unwrap();
    let live = calls.iter().position(|c| c.starts_with("rename ")).unwrap();
    assert!(
        staged < readable && readable < checked && checked < live,
        "{calls:?}"
    );
    assert_eq!(
        sys.written(ADMITTED_NAME_RULE).as_deref(),
        name_rule("alice").as_deref().map(str::as_bytes)
    );
    assert_eq!(sys.mode_of(ADMITTED_NAME_RULE), Some(0o440));
    let path = checked_path.lock().unwrap().clone().unwrap();
    assert!(sys.stat(&path).is_err());
    assert!(sys.stat(path.parent().unwrap()).is_err());
    assert!(matches!(
        NameRule::Current.probe(&sys, &ctx),
        Probe::Ok { .. }
    ));

    // Exact old bytes do not prove the ordinary account has the new read ACL.
    // Explicit repair must run once anyway, then its paired readback settles.
    let refresh = NameRule::refresh();
    assert!(matches!(refresh.probe(&sys, &ctx), Probe::Change { .. }));
    refresh.apply(&sys, &ctx).unwrap();
    assert!(matches!(refresh.probe(&sys, &ctx), Probe::Ok { .. }));
    assert_eq!(sys.mode_of(ADMITTED_NAME_RULE), Some(0o440));
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| call.starts_with("/bin/chmod +a "))
            .count(),
        2
    );
    sys.logged_in(ctx.console.as_ref().unwrap());
    sys.euid = ctx.console.as_ref().unwrap().uid;
    assert_eq!(name_rule_current(&sys), Ok(true));
    let mut unsafe_rule = sys.stat(Path::new(ADMITTED_NAME_RULE)).unwrap();
    unsafe_rule.uid = sys.euid;
    sys.stat_of(ADMITTED_NAME_RULE, unsafe_rule);
    assert_eq!(name_rule_current(&sys), Ok(false));
    sys.stat_fails(ADMITTED_NAME_RULE, std::io::ErrorKind::PermissionDenied);
    assert!(name_rule_current(&sys).is_err());

    let no_acl = Fake::root();
    let previous = name_rule("bob").unwrap();
    no_acl.file(ADMITTED_NAME_RULE, previous.as_bytes());
    no_acl.simulate(|_, command| {
        (command.program == Path::new("/bin/chmod")).then(|| Ran::exit(1, "ACL refused"))
    });
    assert!(NameRule::refresh().apply(&no_acl, &ctx).is_err());
    assert_eq!(
        no_acl.read_file(Path::new(ADMITTED_NAME_RULE)).unwrap(),
        previous.as_bytes()
    );
    assert!(
        !no_acl
            .calls()
            .iter()
            .any(|call| call.starts_with(VISUDO) || call.starts_with("rename "))
    );

    let refused = Fake::root();
    let refused_path = watch(&refused, Ran::exit(1, "syntax error"));
    refused.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"));
    assert_eq!(owed(&refused), vec![Owed::NameRule]);
    assert!(NameRule::Current.apply(&refused, &ctx).is_err());
    assert_eq!(refused.written(ADMITTED_NAME_RULE), None);
    let path = refused_path.lock().unwrap().clone().unwrap();
    assert!(refused.stat(&path).is_err());
    assert!(refused.stat(path.parent().unwrap()).is_err());
    assert!(refused.written(REFUSED_RULE).is_some());
    assert_eq!(owed(&refused), Vec::<Owed>::new());

    // Neither a failed publication nor a failed refusal-marker move may replace the
    // live rule or discard an older refusal record. Only this attempt's stage is removed.
    for (destination, reply) in [
        (ADMITTED_NAME_RULE, Ran::ok("")),
        (REFUSED_RULE, Ran::exit(1, "syntax error")),
    ] {
        let failed = Fake::root();
        let previous = name_rule("bob").unwrap();
        failed
            .file(ADMITTED_NAME_RULE, previous.as_bytes())
            .file(REFUSED_RULE, b"previous refusal")
            .rename_fails(destination, std::io::ErrorKind::PermissionDenied);
        let checked_path = watch(&failed, reply);
        assert!(NameRule::Current.apply(&failed, &ctx).is_err());
        assert_eq!(
            failed.read_file(Path::new(ADMITTED_NAME_RULE)).unwrap(),
            previous.as_bytes()
        );
        assert_eq!(
            failed.read_file(Path::new(REFUSED_RULE)).unwrap(),
            b"previous refusal"
        );
        let path = checked_path.lock().unwrap().clone().unwrap();
        assert!(failed.stat(&path).is_err());
        assert!(failed.stat(path.parent().unwrap()).is_err());
    }
}

/// Developer mode turns on with a non-admin console account added to `_developer`, each only where
/// it is not so already. NEGATIVE CONTROL: with both in place the probe reads in place and
/// the apply runs neither change.
#[test]
fn developer_mode_turns_on_with_the_account_in_the_developer_group() {
    let admin = "/usr/sbin/dseditgroup -o checkmember -m alice admin";
    let checkmember = "/usr/sbin/dseditgroup -o checkmember -m alice _developer";
    let enable = "/usr/sbin/DevToolsSecurity -enable";
    let add = "/usr/sbin/dseditgroup -o edit -a alice -t user _developer";
    let sys = Fake::root();
    sys.on(STATUS, Ran::ok("Developer mode is currently disabled.\n"))
        .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
        .on(
            checkmember,
            Ran {
                status: Some(67),
                stdout: b"no alice is NOT a member of _developer\n".to_vec(),
                stderr: Vec::new(),
            },
        )
        .on(enable, Ran::ok(""))
        .on(add, Ran::ok(""));
    let ctx = console();
    assert_eq!(
        DeveloperMode.probe(&sys, &ctx),
        Probe::Change {
            from: "off, alice not in _developer".to_owned(),
            to: "on, alice in _developer".to_owned()
        }
    );
    DeveloperMode.apply(&sys, &ctx).unwrap();
    let calls = sys.calls();
    assert!(calls.iter().any(|c| c == enable) && calls.iter().any(|c| c == add));

    let done = Fake::root();
    done.on(STATUS, Ran::ok("Developer mode is currently enabled.\n"))
        .on(admin, Ran::ok("no alice is NOT a member of admin\n"))
        .on(
            checkmember,
            Ran::ok("yes alice is a member of _developer\n"),
        );
    assert_eq!(
        DeveloperMode.probe(&done, &ctx),
        Probe::Ok {
            now: "on".to_owned()
        }
    );
    DeveloperMode.apply(&done, &ctx).unwrap();
    let calls = done.calls();
    assert!(!calls.iter().any(|c| c == enable || c == add), "{calls:?}");
}

/// After an admission that came past the elevation, the person's account sets each name not
/// yet the label with `sudo -n` (never a prompt). NEGATIVE CONTROLS: with no rule in place
/// nothing runs, and free text is no label.
#[test]
fn an_admission_after_the_elevation_names_the_mac_through_the_rule_alone() {
    let sys = Fake::root();
    sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440))
        .on(
            "/usr/sbin/scutil --get ComputerName",
            Ran::ok("Alice's Mac\n"),
        )
        .on("/usr/sbin/scutil --get LocalHostName", Ran::ok("m42\n"))
        .on(
            "/usr/sbin/scutil --get HostName",
            Ran::exit(1, "HostName: not set\n"),
        )
        .on(
            "/usr/bin/sudo -n /usr/sbin/scutil --set ComputerName m42",
            Ran::ok(""),
        )
        .on(
            "/usr/bin/sudo -n /usr/sbin/scutil --set HostName m42",
            Ran::ok(""),
        );
    assert_eq!(
        name_by_rule(
            &sys,
            &GeneratedNames::number_only(Number::parse("m42").unwrap()),
            &|| Ok(())
        ),
        Ok(vec!["ComputerName", "HostName"])
    );

    let none = Fake::root();
    assert!(
        name_by_rule(
            &none,
            &GeneratedNames::number_only(Number::parse("m42").unwrap()),
            &|| Ok(())
        )
        .is_err()
    );
    assert!(
        none.calls().iter().all(|c| !c.contains("sudo")),
        "{:?}",
        none.calls()
    );
    assert!(Number::parse("Mac-mini").is_err());

    for (number, expected) in [
        ("m183", vec!["ComputerName"]),
        ("m184", vec!["ComputerName", "LocalHostName"]),
    ] {
        let sys = Fake::root();
        sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
        for (field, value) in [
            ("ComputerName", "drift"),
            ("LocalHostName", "m183-2"),
            ("HostName", number),
        ] {
            sys.on(&format!("/usr/sbin/scutil --get {field}"), Ran::ok(value));
            sys.on(
                &format!("/usr/bin/sudo -n /usr/sbin/scutil --set {field} {number}"),
                Ran::ok(""),
            );
        }
        assert_eq!(
            name_by_rule(
                &sys,
                &GeneratedNames::number_only(Number::parse(number).unwrap()),
                &|| Ok(())
            )
            .unwrap(),
            expected
        );
        let writes: Vec<_> = sys
            .calls()
            .into_iter()
            .filter(|call| call.contains(" --set "))
            .collect();
        assert_eq!(
            writes,
            expected
                .iter()
                .map(|field| format!("/usr/bin/sudo -n /usr/sbin/scutil --set {field} {number}"))
                .collect::<Vec<_>>(),
            "a repair of another slot must preserve an accepted Bonjour alias"
        );
    }
    let names = GeneratedNames::new(
        Number::parse("m85").unwrap(),
        &Nickname::parse("Dog").unwrap(),
    );
    let sys = Fake::root();
    sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
    for (pref, now) in [
        ("ComputerName", "m85"),
        ("LocalHostName", "m85-dog-2"),
        ("HostName", "m85"),
    ] {
        let slot = OsSlot::from_macos_preference(pref).unwrap();
        sys.on(&format!("/usr/sbin/scutil --get {pref}"), Ran::ok(now));
        sys.on(
            &Cmd::new(SUDO)
                .args([
                    "-n",
                    crate::items::names::SCUTIL,
                    "--set",
                    pref,
                    names.for_slot(slot).unwrap(),
                ])
                .display(),
            Ran::ok(""),
        );
    }
    assert_eq!(
        name_by_rule(&sys, &names, &|| Ok(())).unwrap(),
        ["ComputerName", "HostName"]
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.contains("--set LocalHostName"))
    );
    for (computer, host, expected) in [
        ("m85 Dog (2)", "m85-dog", vec![]),
        ("m85 Dog (27)", "m84", vec!["HostName"]),
        ("m85 Dog (02)", "m85-dog", vec!["ComputerName"]),
    ] {
        let sys = Fake::root();
        sys.stat_of(ADMITTED_NAME_RULE, rule_stat(0, 0o440));
        for (pref, value) in [
            ("ComputerName", computer),
            ("LocalHostName", "m85-dog-2"),
            ("HostName", host),
        ] {
            sys.on(&format!("/usr/sbin/scutil --get {pref}"), Ran::ok(value));
            let slot = OsSlot::from_macos_preference(pref).unwrap();
            sys.on(
                &Cmd::new(SUDO)
                    .args([
                        "-n",
                        crate::items::names::SCUTIL,
                        "--set",
                        pref,
                        names.for_slot(slot).unwrap(),
                    ])
                    .display(),
                Ran::ok(""),
            );
        }
        assert_eq!(name_by_rule(&sys, &names, &|| Ok(())).unwrap(), expected);
        let writes: Vec<_> = sys
            .calls()
            .into_iter()
            .filter(|call| call.contains(" --set "))
            .collect();
        assert_eq!(writes.len(), expected.len(), "{computer}: {writes:?}");
    }
}
