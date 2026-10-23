// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A tape's header.

use std::sync::Arc;

use super::*;
use crate::supervise::{ApprovalEnv, Ctl, CtlReply, HostStep, IdleHost, Session, WorkerSource};

/// A transport no header test may use.
struct NoCtl;

impl Ctl for NoCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        panic!("a header test sent {args:?}");
    }
}

/// A host that is only there: the header writes its presence.
#[derive(Debug)]
struct Present;

impl IdleHost for Present {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

/// `posix` (`/st/drive`) as an absolute path of this host: itself on unix, on
/// drive `C:` on Windows (where a `/`-rooted path names no drive, and a header
/// takes it for a relative one).
fn abs(posix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{posix}"))
    } else {
        PathBuf::from(posix)
    }
}

/// Where a remapped decode puts the recorded absolute path `recorded` beneath
/// `base`: its names, on Windows under its drive's own namespace first
/// (`disk-43/path` for `C:`, [`Roots::remapped`]).
fn remapped_at(base: &Path, recorded: &Path) -> PathBuf {
    let mut at = base.to_path_buf();
    for c in recorded.components() {
        match c {
            std::path::Component::Prefix(p) => {
                if let std::path::Prefix::Disk(drive) = p.kind() {
                    at.push(format!("disk-{drive:02x}"));
                    at.push("path");
                }
            }
            std::path::Component::Normal(name) => at.push(name),
            _ => {}
        }
    }
    at
}

/// A loop's setup for `@s-1`, its ledger under `/st/drive`, the approval
/// policy's home `/h` and `$TMPDIR` `/tmpd`.
fn watched() -> SessionSetup {
    let mut ctl = NoCtl;
    let mut s = Session::new(&mut ctl, Some("@s-1".to_string()));
    s.set_approval_env(ApprovalEnv {
        home: Some(abs("/h")),
        uid: 501,
        tmpdir: Some(PathBuf::from("/tmpd")),
        worker: WorkerSource::Session,
    });
    s.set_approval_ledger(Some(abs("/st/drive/s-1.jsonl")));
    s.setup().clone()
}

/// Options with every field away from its default, a host behind them.
fn every_option() -> SuperviseOpts {
    let mut policy = SupervisorConfig::default();
    policy.set("approve", "safe").expect("approve");
    policy
        .set(
            "rules_file",
            &abs("/st/rules/standing.md").display().to_string(),
        )
        .expect("rules");
    policy.set("trust_roots", "~/src/app*,~/w").expect("roots");
    policy.set("codex.enabled", "false").expect("codex");
    SuperviseOpts {
        max: Duration::from_secs(3600),
        python_allow: vec!["scripts/*.py".to_string(), "a\nb\\c\t\u{7}é".to_string()],
        context_warn: 25,
        journal: Some(abs("/j/journal.jsonl")),
        ledger: Some(abs("/st/drive/s-1.jsonl")),
        policy,
        yield_when_held: true,
        idle_host: Some(Arc::new(Present)),
        tasks: Some(abs("/st/tasks")),
    }
}

/// Every field of the options, the policy aside: `SuperviseOpts` has no
/// equality of its own (its host is a trait object).
fn assert_same_opts(a: &SuperviseOpts, b: &SuperviseOpts) {
    let SuperviseOpts {
        max,
        python_allow,
        context_warn,
        journal,
        ledger,
        policy,
        yield_when_held,
        idle_host: _,
        tasks,
    } = a;
    assert_eq!(*max, b.max);
    assert_eq!(*python_allow, b.python_allow);
    assert_eq!(*context_warn, b.context_warn);
    assert_eq!(*journal, b.journal);
    assert_eq!(*ledger, b.ledger);
    assert_eq!(*policy, b.policy);
    assert_eq!(*yield_when_held, b.yield_when_held);
    assert_eq!(*tasks, b.tasks);
}

/// THE HEADER ROUND TRIP: the run's facts, the setup, every option and the
/// policy come back; a host comes back as its presence, never the host; the
/// text written again from what was read is the text read.
#[test]
fn a_header_gives_back_what_the_run_was_given() {
    let setup = watched();
    let opts = every_option();
    let h = Header::capture("t-7", Entry::Hosted, Some(&abs("/st")), &setup, &opts);
    assert!(h.idle_host && h.opts.idle_host.is_none());
    assert!(
        h.policy_exact,
        "a policy `set` built is one its texts give back"
    );
    let text = h.encode();
    assert!(text.starts_with("atape 1\n"), "{text}");
    let back = Header::decode(&text).expect("a header this build wrote");
    assert_eq!(back.tape, "t-7");
    assert_eq!(back.build, aterm_types::version::build_id());
    assert_eq!(back.entry, Entry::Hosted);
    assert_eq!(back.roots, h.roots);
    assert_eq!(back.setup, setup);
    assert_same_opts(&back.opts, &opts);
    assert!(back.idle_host && back.opts.idle_host.is_none());
    assert!(back.policy_exact);
    assert_eq!(back.encode(), text, "the text read is the text written");

    let plain = SuperviseOpts::default();
    let h = Header::capture("t-8", Entry::Watch, None, &setup, &plain);
    let back = Header::decode(&h.encode()).expect("decodes");
    assert_same_opts(&back.opts, &plain);
    assert!(!back.idle_host);
}

/// The policy comes back through the HEADER, by the owner's own reader, for
/// any one thing moved off the default — each key to a value of its own
/// ([`SupervisorConfig::moved_off_default`], so every key is written at a
/// value that is not its default's and none merely round-trips the default
/// again), each agent switched off, each withheld rule — with nothing
/// supplied. The standing rules' file is the one value the
/// header holds as the loop opens it: a relative one by its absolute path.
#[test]
fn the_policy_comes_back_through_the_header_for_any_one_key_moved() {
    let setup = watched();
    let full = SupervisorConfig::default();
    let mut policies = Vec::new();
    for key in crate::supervise::config::KEYS {
        policies.push(SupervisorConfig::moved_off_default(key));
    }
    for (agent, _) in full.agent_texts() {
        let mut c = full.clone();
        c.set(&agent, "false").expect("an agent's own switch");
        policies.push(c);
    }
    let mut rm = full.clone();
    rm.withheld.rm_breaker = true;
    policies.push(rm);
    let mut read = full.clone();
    read.withheld.read_outside_cwd = true;
    policies.push(read);
    for policy in policies {
        assert_ne!(policy, full);
        let opts = SuperviseOpts {
            policy: policy.clone(),
            ..SuperviseOpts::default()
        };
        let h = Header::capture("t", Entry::Watch, None, &setup, &opts);
        let want = SupervisorConfig {
            rules_file: policy.rules_file.as_deref().map(opened),
            ..policy.clone()
        };
        assert_eq!(h.opts.policy, want);
        assert!(h.policy_exact, "{policy:?}");
        let back = Header::decode(&h.encode()).expect("decodes");
        assert_eq!(back.opts.policy, want);
        assert!(back.supplied.is_empty(), "{:?}", back.supplied);
    }
}

/// A policy no file can spell — a trust root with a comma in it, which
/// `set` reads as two — is said to be one the rebuild does not give back
/// (`exact=false`), not passed off as the run's.
#[test]
fn a_policy_its_texts_cannot_give_back_is_marked_inexact() {
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            trust_roots: vec!["~/a,b".to_string()],
            ..SupervisorConfig::default()
        },
        ..SuperviseOpts::default()
    };
    let h = Header::capture("t", Entry::Watch, None, &watched(), &opts);
    assert!(!h.policy_exact);
    let text = h.encode();
    assert!(text.lines().any(|l| l == "exact=false"), "{text}");
    let back = Header::decode(&text).expect("decodes");
    assert!(!back.policy_exact);
    assert_eq!(back.opts.policy.trust_roots, ["~/a", "b"]);
    assert_eq!(
        back.supplied,
        [format!("[policy] {INEXACT}")],
        "said inexact"
    );
}

/// A policy marked inexact because the owner's reader REFUSES one of its
/// values — an `autocompact_offer` off Claude Code's ladder, which a test
/// builds as it likes — still decodes: the value is read as the owner's file
/// reads it (its default kept) and named in what the build supplied, beside
/// the inexactness, so the tape replays and says what it could not take.
/// The same text claiming `exact=true` is refused (the negative control):
/// there a value no reader takes is damage.
#[test]
fn a_policy_marked_inexact_decodes_where_the_reader_refuses_a_value() {
    let mut policy = SupervisorConfig::default();
    policy.set("approve", "safe").expect("approve");
    policy.autocompact_offer = Some(450);
    let opts = SuperviseOpts {
        policy: policy.clone(),
        ..SuperviseOpts::default()
    };
    let h = Header::capture("t", Entry::Watch, None, &watched(), &opts);
    assert!(!h.policy_exact);
    let text = h.encode();
    assert!(
        text.lines().any(|l| l == "autocompact_offer=450k"),
        "{text}"
    );
    let back = Header::decode(&text).expect("an inexact policy's tape reads");
    assert!(!back.policy_exact);
    assert_eq!(
        back.opts.policy,
        SupervisorConfig {
            autocompact_offer: Some(crate::supervise::config::AUTOCOMPACT_DEFAULT_K),
            ..policy
        }
    );
    assert_eq!(back.supplied.len(), 2, "{:?}", back.supplied);
    assert!(
        back.supplied[0].starts_with("[policy] harness.autocompact_offer: ")
            && back.supplied[0].contains("450k")
            && back.supplied[0].ends_with("; this build read autocompact_offer=400k"),
        "what this build supplied in its place: {:?}",
        back.supplied
    );
    assert_eq!(back.supplied[1], format!("[policy] {INEXACT}"));
    assert_eq!(
        back.encode(),
        text.replacen("autocompact_offer=450k", "autocompact_offer=400k", 1),
        "written again, the nearest policy, still marked inexact"
    );
    let e = Header::decode(&replace_header_field(&text, "[policy]", "exact", "true"))
        .expect_err("a value no reader takes, in a policy said to rebuild exactly");
    assert!(e.contains("autocompact_offer"), "{e}");
}

/// THE ROOTS: each file the loop opens is written under the most specific
/// root that holds it, the same directory twice is one root, a path under
/// none is written as it is (one starting with `@` escaped), and a replay's
/// remapped decode moves every one of them under its base — while the
/// approval policy's home and `$TMPDIR`, values it reads the screen with,
/// stay put (`replay_loop_tests.rs` replays a run that looked under the
/// home, and moves the home as the negative control).
#[test]
fn paths_are_written_under_their_roots_and_move_with_them() {
    let setup = watched();
    let opts = every_option();
    let h = Header::capture("t", Entry::Watch, Some(&abs("/st")), &setup, &opts);
    let ids: Vec<&str> = h.roots.iter().map(|(id, _)| id).collect();
    assert_eq!(ids, ["ledger", "journal", "rules", "state", "home"]);
    let text = h.encode();
    for line in [
        "ledger=@ledger/s-1.jsonl",
        "journal=@journal/journal.jsonl",
        "rules_file=@rules/standing.md",
        "tasks=@state/tasks",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in\n{text}");
    }
    let base = Path::new("/nonexistent/aterm-replay-t/absent");
    let back = Header::decode_remapped(&text, base).expect("decodes");
    let under = |posix: &str| remapped_at(base, &abs(posix));
    assert_eq!(
        back.setup.ledger_path(),
        Some(under("/st/drive/s-1.jsonl").as_path())
    );
    assert_eq!(back.opts.ledger, Some(under("/st/drive/s-1.jsonl")));
    assert_eq!(back.opts.journal, Some(under("/j/journal.jsonl")));
    assert_eq!(
        back.opts.policy.rules_file,
        Some(under("/st/rules/standing.md"))
    );
    assert_eq!(back.opts.tasks, Some(under("/st/tasks")));
    assert_eq!(back.roots, h.roots.remapped(base));
    assert_eq!(back.setup.approval_env(), setup.approval_env());
    assert_eq!(setup_and_paths(&back.encode()), setup_and_paths(&text));
}

/// Every line but the `[roots]` section's: what a remap leaves unchanged.
fn setup_and_paths(text: &str) -> Vec<&str> {
    let mut in_roots = false;
    text.lines()
        .filter(|l| {
            if l.starts_with('[') {
                in_roots = *l == "[roots]";
            }
            !in_roots
        })
        .collect()
}

/// Roots: the most specific wins, the encoding is exact under `Path`'s
/// equality, and a path under a root the header does not name is refused.
#[test]
fn roots_encode_and_decode_paths() {
    let roots = Roots(vec![
        ("ledger", PathBuf::from("/s/drive")),
        ("state", PathBuf::from("/s")),
    ]);
    for (path, written) in [
        ("/s/drive/a.jsonl", "@ledger/a.jsonl"),
        ("/s/drive", "@ledger"),
        ("/s/other/b", "@state/other/b"),
        ("/s/drivex/c", "@state/drivex/c"),
        ("/elsewhere/d", "/elsewhere/d"),
        ("rel/e", "rel/e"),
        ("@f", "@@f"),
    ] {
        let raw = roots.encode(Path::new(path));
        assert_eq!(String::from_utf8_lossy(&raw), written, "{path}");
        assert_eq!(roots.decode(&raw), Ok(PathBuf::from(path)), "{path}");
    }
    let e = roots.decode(b"@home/x").expect_err("no home root");
    assert!(e.contains("@home"), "{e}");
}

/// A value comes back byte for byte: every control character, a backslash,
/// non-ASCII text — and, on unix, a path's bytes that are not UTF-8.
#[test]
fn values_come_back_byte_for_byte() {
    let all: Vec<u8> = (0u8..=255).collect();
    let text = escape(&all);
    assert!(!text.contains('\n') && !text.contains('\r'), "{text}");
    assert_eq!(unescape(&text), Ok(all));
    for s in [
        "",
        "plain",
        "a=b",
        "tab\there",
        "nl\nnl",
        "\\x41",
        "é\u{85}\u{2028}✓",
    ] {
        assert_eq!(
            unescape(&escape(s.as_bytes())),
            Ok(s.as_bytes().to_vec()),
            "{s:?}"
        );
    }
    for bad in ["\\", "\\q", "\\x4", "\\xzz"] {
        assert!(unescape(bad).is_err(), "{bad:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let odd = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe-not-utf8"));
        let raw = VERBATIM.encode(odd);
        assert_eq!(unescape(&escape(&raw)), Ok(raw.clone()));
        assert_eq!(VERBATIM.decode(&raw), Ok(odd.to_path_buf()));
    }
}

/// A duration is exact to the nanosecond in its largest unit; `max` is the
/// unbounded budget; anything else is no duration.
#[test]
fn durations_are_exact() {
    for (d, text) in [
        (Duration::ZERO, "0s"),
        (Duration::from_secs(20), "20s"),
        (Duration::from_millis(250), "250ms"),
        (Duration::from_micros(1_500), "1500us"),
        (Duration::new(3, 7), "3000000007ns"),
        (Duration::MAX, "max"),
        (Duration::from_secs(u64::MAX), "18446744073709551615s"),
    ] {
        assert_eq!(duration_text(d), text);
        assert_eq!(parse_duration(text), Some(d), "{text}");
    }
    for bad in [
        "",
        "s",
        "20",
        "20m",
        "-1s",
        "1.5s",
        "+3s",
        "99999999999999999999999s",
    ] {
        assert_eq!(parse_duration(bad), None, "{bad:?}");
    }
}

/// A header this build did not write is refused by name: a newer format, a
/// key it does not have, one missing, one said twice, a value that does not
/// read, a root it does not name, a loop it does not run.
#[test]
fn a_header_this_build_did_not_write_is_refused_by_name() {
    let text = Header::capture("t", Entry::Watch, None, &watched(), &every_option()).encode();
    let refused = |edited: String, names: &str| {
        let e = Header::decode(&edited).expect_err(names);
        assert!(e.contains(names), "{names}: {e}");
    };
    refused(text.replacen("atape 1", "atape 2", 1), "format 2");
    refused(text.replacen("atape 1", "tape 1", 1), "not a tape header");
    refused(
        text.replacen("[opts]\n", "[opts]\nfuture_knob=1\n", 1),
        "future_knob",
    );
    refused(
        text.replacen("[policy]\n", "[policy]\nfuture_key=1\n", 1),
        "future_key: no key this build has — a policy a newer build wrote",
    );
    refused(
        text.replacen("[policy]\n", "[policy]\nnewbot.enabled=true\n", 1),
        "newbot.enabled: no agent this build has",
    );
    refused(
        text.lines()
            .filter(|l| !l.starts_with("exact="))
            .map(|l| format!("{l}\n"))
            .collect(),
        "names no exact",
    );
    refused(
        text.lines()
            .filter(|l| !l.starts_with("reconnect="))
            .map(|l| format!("{l}\n"))
            .collect(),
        "names no reconnect",
    );
    refused(
        text.replacen("[opts]\n", "[opts]\ncontext_warn=1\n", 1),
        "said twice",
    );
    refused(
        text.lines()
            .map(|l| match l.strip_prefix("reconnect=") {
                Some(_) => "reconnect=soon\n".to_string(),
                None => format!("{l}\n"),
            })
            .collect(),
        "no duration",
    );
    refused(
        text.replacen("[roots]\n", "[roots]\nattic=/a\n", 1),
        "attic",
    );
    refused(text.replacen("entry=watch", "entry=dance", 1), "entry");
    refused(text.replacen("approve=safe", "approve=maybe", 1), "approve");
    refused(
        text.replacen("[setup]\n", "[setup]\nNot A Key\n", 1),
        "not a key",
    );
}

/// A DAMAGED OR HOSTILE HEADER is refused as fast as it can be read: half a
/// million distinct keys in one section — a header frame holds up to 64 MiB,
/// and a tape may come from elsewhere — are read in one pass and the first
/// key this build does not have is refused by name. Checked against every
/// earlier line, as each key once was, the same header cost ~10^11
/// comparisons and decoding it hung instead of refusing it: the wait for the
/// refusal is a one-minute hang detector, not a latency budget.
#[test]
fn a_header_of_half_a_million_keys_is_refused_in_one_pass() {
    use std::fmt::Write as _;
    const HANG_DETECTOR: Duration = Duration::from_secs(60);
    let text = Header::capture("t", Entry::Watch, None, &watched(), &every_option()).encode();
    let mut many = String::from("[setup]\n");
    for i in 0..500_000 {
        let _ = writeln!(many, "k{i}=1");
    }
    let hostile = text.replacen("[setup]\n", &many, 1);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(Header::decode(&hostile).map(|_| ()));
    });
    let e = rx
        .recv_timeout(HANG_DETECTOR)
        .expect("the decode returns")
        .expect_err("keys this build does not have");
    assert!(
        e.contains("[setup] k0: a key this build does not have"),
        "{e}"
    );
}

/// Edit one real encoded field, keeping every other required header field intact.
fn replace_header_field(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut here = "";
    let mut changed = 0;
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with('[') {
            here = line;
        }
        if here == section && line.split_once('=').is_some_and(|(k, _)| k == key) {
            out.push_str(&format!("{key}={value}\n"));
            changed += 1;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert_eq!(changed, 1, "exact encoded field {section} {key}");
    out
}

/// Remapping is a file-path boundary, not just string substitution. Each file slot
/// refuses a root-resetting suffix, parent traversal and an unrooted path.
#[test]
fn remapped_file_paths_cannot_escape_the_named_roots() {
    let setup = watched();
    let opts = every_option();
    let text = Header::capture("t", Entry::Watch, Some(&abs("/st")), &setup, &opts).encode();
    let base = Path::new("/nonexistent/private-replay");
    let good = Header::decode_remapped(&text, base).expect("ordinary rooted files");
    assert_eq!(
        good.setup.ledger_path(),
        Some(remapped_at(base, &abs("/st/drive/s-1.jsonl")).as_path())
    );
    assert_eq!(
        good.setup.approval_env(),
        setup.approval_env(),
        "environment values remain verbatim"
    );
    let mut admitted = Vec::new();
    for (section, key) in [
        ("[setup]", "ledger"),
        ("[opts]", "journal"),
        ("[opts]", "ledger"),
        ("[policy]", "rules_file"),
    ] {
        for raw in [
            "@ledger//outside/review",
            "@ledger/../../outside/review",
            "/outside/review",
            "relative/review",
            "@@literal/review",
        ] {
            let edited = replace_header_field(&text, section, key, raw);
            if Header::decode_remapped(&edited, base).is_ok() {
                admitted.push((section, key, raw));
            }
        }
    }
    assert!(
        admitted.is_empty(),
        "remapped file paths escaped their roots: {admitted:?}"
    );
}

/// A FILE GIVEN RELATIVE (a journal of `run.jsonl`, a `rules_file` of
/// `standing.md`, a ledger) is one the loop opens from its own directory: the
/// header names it from there, under a root, so a replay moves it under its
/// base like every other — never hands the relative path back, which a
/// replay started in the recording's directory would open as the live
/// journal. An absolute path under no root still decodes as it was, but no
/// remapped decode admits it.
#[test]
fn relative_file_paths_are_rooted_and_move_with_a_remap() {
    let setup = watched().with_ledger_path(Some(PathBuf::from("drive/s-1.jsonl")));
    let opts = SuperviseOpts {
        journal: Some(PathBuf::from("run.jsonl")),
        policy: {
            let mut p = SupervisorConfig::default();
            p.set("rules_file", "standing.md")
                .expect("a relative rules file");
            p
        },
        ..SuperviseOpts::default()
    };
    let h = Header::capture("t", Entry::Watch, Some(&abs("/st")), &setup, &opts);
    let here = std::env::current_dir().expect("this test's directory");
    let text = h.encode();
    for line in [
        "ledger=@ledger/s-1.jsonl",
        "journal=@journal/run.jsonl",
        "rules_file=@journal/standing.md",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in\n{text}");
    }
    let back = Header::decode(&text).expect("decodes");
    assert_eq!(back.opts.journal, Some(here.join("run.jsonl")));
    assert_eq!(
        back.setup.ledger_path(),
        Some(here.join("drive/s-1.jsonl").as_path())
    );
    let base = Path::new("/nonexistent/private-replay");
    let moved = Header::decode_remapped(&text, base).expect("every file rooted");
    let at = remapped_at(base, &here);
    assert_eq!(
        moved.setup.ledger_path(),
        Some(at.join("drive/s-1.jsonl").as_path())
    );
    assert_eq!(moved.opts.journal, Some(at.join("run.jsonl")));
    assert_eq!(moved.opts.policy.rules_file, Some(at.join("standing.md")));

    let mut elsewhere = every_option();
    elsewhere.ledger = Some(abs("/elsewhere/other-ledger"));
    let text =
        Header::capture("t", Entry::Watch, Some(&abs("/st")), &watched(), &elsewhere).encode();
    let line = format!("ledger={}", abs("/elsewhere/other-ledger").display());
    assert!(text.lines().any(|l| l == line), "{text}");
    let ordinary = Header::decode(&text).expect("the ordinary codec reads it");
    assert_eq!(ordinary.opts.ledger, elsewhere.ledger);
    Header::decode_remapped(&text, base).expect_err("no root holds it");
}

/// A TAPE AN OLDER BUILD WROTE: it lacks every key that build did not have
/// yet — here a `[harness]` key (`land_at` came after the first tapes), an
/// agent's switch, the two setup knobs a shipped build did not write
/// before every build carried them, the three timings that came after the
/// format (the host's wait step, the box call's lag step and the refusal's
/// back-off rungs), the hand the loop types under, the tasks directory and
/// the recording build's host doors. It
/// decodes: each policy key at its full-power default, each knob at this
/// build's shipped value, everything the tape said as it said it, and every
/// key supplied named.
#[test]
fn a_tape_an_older_build_wrote_decodes_with_what_it_lacked_named() {
    let mut opts = every_option();
    opts.policy.set("land_at", "18:00").expect("land_at");
    let mut ctl = NoCtl;
    let mut s = Session::from_setup(&mut ctl, watched());
    s.set_person_hold_wait(Duration::from_millis(5));
    let setup = s.setup().clone();
    let text = Header::capture("t", Entry::Watch, Some(&abs("/st")), &setup, &opts).encode();
    for line in [
        "person_hold_wait=5ms",
        "beside_idle_min=20s",
        "land_at=18:00",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} in\n{text}");
    }
    let older: String = text
        .lines()
        .filter(|l| {
            ![
                "land_at=",
                "codex.enabled=",
                "person_hold_wait",
                "beside_idle_min=",
                "wait_step=",
                "box_call_step=",
                "turn_end.refused_backoff=",
                "types_under_hand=",
                "tasks=",
                "host_doors=",
            ]
            .iter()
            .any(|k| l.starts_with(k))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let back = Header::decode(&older).expect("an older build's tape");
    assert_eq!(
        back.supplied,
        [
            "[setup] types_under_hand",
            "[setup] wait_step",
            "[setup] box_call_step",
            "[setup] turn_end.refused_backoff",
            "[setup] person_hold_wait",
            "[setup] beside_idle_min",
            "[opts] tasks",
            "[opts] host_doors",
            "[policy] land_at",
            "[policy] codex.enabled",
        ]
    );
    assert_eq!(back.setup, watched(), "each knob at its shipped value");
    assert_eq!(
        back.opts.policy,
        SupervisorConfig {
            land_at: None,
            off: Vec::new(),
            ..opts.policy.clone()
        }
    );
    assert_eq!(back.opts.policy.approve, opts.policy.approve);
    assert!(back.policy_exact);
}

/// THE HEADER NAMES THE RECORDING BUILD'S HOST DOORS (the occupancy red-team's
/// F8): every method of the window's host, `host.occupancy` among them, and
/// they read back as written; an older build's header, which named none,
/// reads back as naming none — what a replay keys a door newer than the tape
/// on (`ReplayCore::lacks_door`).
#[test]
fn the_header_names_the_recording_builds_host_doors() {
    let h = Header::capture("t", Entry::Hosted, None, &watched(), &every_option());
    let doors = h.host_doors.clone().expect("a capture names its doors");
    assert_eq!(doors, crate::supervise::run::host_door::HOST_DOORS);
    assert!(doors.iter().any(|d| d == "host.occupancy"), "{doors:?}");
    let text = h.encode();
    let back = Header::decode(&text).expect("decodes");
    assert_eq!(back.host_doors, Some(doors));
    assert!(back.supplied.is_empty(), "{:?}", back.supplied);
    assert_eq!(back.encode(), text, "written back as read");

    let older: String = text
        .lines()
        .filter(|l| !l.starts_with("host_doors="))
        .map(|l| format!("{l}\n"))
        .collect();
    let back = Header::decode(&older).expect("an older build's tape");
    assert_eq!(back.host_doors, None);
    assert_eq!(back.supplied, ["[opts] host_doors"]);
    assert_eq!(
        back.encode(),
        older,
        "an older header is written back as it was"
    );
}

/// Real Windows path prefixes stay distinct, while a ledger keeps its place
/// under the state root. These paths are values: no directory is opened.
#[cfg(windows)]
#[test]
fn remapped_roots_keep_their_windows_namespace_and_nesting() {
    fn check(left: &str, right: &str) -> Vec<String> {
        let left = Path::new(left);
        let right = Path::new(right);
        assert_ne!(left, right, "distinct recorded roots");
        let roots = Roots::of(vec![
            ("ledger", left.join("drive")),
            ("journal", right.join("drive")),
            ("state", left.to_path_buf()),
        ]);
        for raw in [
            b"@ledger/item".as_slice(),
            b"@journal/item",
            b"@state/other",
        ] {
            let path = roots.decode_file(raw).expect("an admitted rooted file");
            assert_eq!(roots.encode(&path), raw, "recorded path identity");
        }
        let base = Path::new(r"R:\private-replay");
        let moved = roots.remapped(base);
        let state = moved.decode_file(b"@state").expect("state root");
        let ledger = moved.decode_file(b"@ledger").expect("ledger root");
        let journal = moved.decode_file(b"@journal").expect("journal root");
        assert_eq!(ledger, state.join("drive"), "recorded nesting stays intact");
        for path in [&state, &ledger, &journal] {
            assert!(
                path.starts_with(base),
                "all remapped roots stay under the base"
            );
        }
        for raw in [b"@ledger/..\\outside".as_slice(), b"@ledger/D:\\outside"] {
            moved
                .decode_file(raw)
                .expect_err("file admission stays below its root");
        }
        let mut lost = Vec::new();
        for raw in [
            b"@ledger/item".as_slice(),
            b"@journal/item",
            b"@state/other",
        ] {
            let path = moved.decode_file(raw).expect("an admitted remapped file");
            if moved.encode(&path) != raw {
                lost.push(String::from_utf8_lossy(raw).into_owned());
            }
        }
        if ledger == journal {
            lost.push("distinct recorded roots share one directory".to_string());
        }
        lost
    }

    for (left, right) in [
        (r"C:\data", r"C:\other"),
        (r"\\server\share\data", r"\\server\share\other"),
    ] {
        assert!(check(left, right).is_empty(), "ordinary paths round-trip");
    }
    let mut lost = Vec::new();
    for (left, right) in [
        (r"C:\data", r"D:\data"),
        (r"\\server-a\share\data", r"\\server-b\share\data"),
        (r"\\server\share-a\data", r"\\server\share-b\data"),
        (r"\\?\C:\data", r"\\?\D:\data"),
        (
            r"\\?\UNC\server-a\share\data",
            r"\\?\UNC\server-b\share\data",
        ),
        (r"\\.\device-a\data", r"\\.\device-b\data"),
        (r"\\?\volume-a\data", r"\\?\volume-b\data"),
        (r"C:\data", r"\\?\C:\data"),
    ] {
        for why in check(left, right) {
            lost.push(format!("{left:?} / {right:?}: {why}"));
        }
    }
    assert!(
        lost.is_empty(),
        "TAPE_REMAP_PREFIX_NAMESPACE_MUST_STAY_DISTINCT: {lost:?}"
    );
}

/// Format-one Claude headers also wrote the retired Codex setup durations.
/// They remain typed input, but cannot restore the retired behavior. A new
/// header still needs none of them; other unknown and malformed input is refused.
#[test]
fn a_claude_header_keeps_reading_its_retired_setup_durations() {
    let setup = watched();
    let opts = SuperviseOpts::default();
    let header = Header::capture(
        "legacy-setup",
        Entry::Watch,
        Some(Path::new("/st")),
        &setup,
        &opts,
    );
    let text = header.encode();
    assert!(text.starts_with("atape 1\n"));
    let current = Header::decode(&text).expect("ordinary current header");
    assert_eq!(current.setup, setup);
    assert_same_opts(&current.opts, &opts);
    let unknown = text.replacen("[setup]\n", "[setup]\nfuture_setup=1s\n", 1);
    assert!(
        Header::decode(&unknown)
            .expect_err("unknown setup key")
            .contains("future_setup")
    );
    let malformed = replace_header_field(&text, "[setup]", "reconnect", "soon");
    assert!(
        Header::decode(&malformed)
            .expect_err("malformed active duration")
            .contains("no duration")
    );

    let retired = [
        ("switch_break_note", "1ns"),
        ("turn_end.unknown_hold", "2ms"),
        ("turn_end.limits_every", "3s"),
        ("turn_end.restore_pick", "max"),
    ];
    let mut problems = Vec::new();
    let mut all = String::new();
    for (key, value) in retired {
        let line = format!("{key}={value}\n");
        all.push_str(&line);
        let legacy = text.replacen("[setup]\n", &format!("[setup]\n{line}"), 1);
        match Header::decode(&legacy) {
            Ok(back) => {
                assert_eq!(back.setup, setup, "{key}: inactive value");
                assert_same_opts(&back.opts, &opts);
                assert!(back.supplied.is_empty(), "no active value was supplied");
                assert_eq!(back.encode(), text, "retired key is not written again");
            }
            Err(why) => problems.push(format!("{key}: {why}")),
        }
        let bad = text.replacen("[setup]\n", &format!("[setup]\n{key}=soon\n"), 1);
        match Header::decode(&bad) {
            Err(why) if why.contains(key) && why.contains("no duration") => {}
            other => problems.push(format!("{key}: malformed value was not typed: {other:?}")),
        }
    }
    let legacy = text.replacen("[setup]\n", &format!("[setup]\n{all}"), 1);
    match Header::decode(&legacy) {
        Ok(back) => {
            assert_eq!(back.setup, setup);
            assert_same_opts(&back.opts, &opts);
            assert_eq!(back.encode(), text);
        }
        Err(why) => problems.push(format!("all retired fields: {why}")),
    }
    assert!(
        problems.is_empty(),
        "RETIRED_SETUP_HEADER_MUST_STILL_DECODE: {problems:#?}"
    );
}
