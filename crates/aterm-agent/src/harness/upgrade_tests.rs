// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn v(s: &str) -> Version {
    Version::parse(s).expect("a version")
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_string()).collect()
}

const ID: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";

/// The session file Claude Code 2.1.280 wrote for pid 9162, measured 2026-09-23.
const SESSION_9162: &str = r#"{"pid":9162,"sessionId":"03396a15-856e-4f1b-8174-ae9a3e4b369f","cwd":"/Users/_owner/aterm","startedAt":1790194436213,"procStart":"Wed Sep 23 20:13:55 2026","version":"2.1.280","peerProtocol":1,"peerFeatures":["notify_idle","reply_across_default_dirs","artifact_yield"],"kind":"interactive","entrypoint":"cli","pidDomain":"darwin","messagingSocketPath":"/tmp/cc-socks/9162.sock","name":"aterm-83","nameSource":"derived","nameSince":1790194436213,"updatedAt":1790212772957,"status":"busy","statusUpdatedAt":1790212772957}"#;

#[test]
fn versions_order_numerically_and_refuse_suffixes() {
    assert!(v("2.1.281") > v("2.1.280"));
    assert!(v("2.1.10") > v("2.1.9"), "numeric, not lexical");
    assert_eq!(v("2.1"), v("2.1.0"));
    assert_eq!(v("v2.1.280").to_string(), "2.1.280");
    for bad in ["", "2.1.x", "2..1", "2.1.280-beta", " . "] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_measured_session_file_parses_and_a_changed_shape_refuses() {
    let s = parse_session_file(SESSION_9162).expect("the measured file");
    assert_eq!(s.pid, 9162);
    assert_eq!(s.session_id, ID);
    assert_eq!(s.cwd, "/Users/_owner/aterm");
    assert_eq!(s.version, "2.1.280");
    assert_eq!(s.status, "busy");
    assert_eq!(s.status_updated_at_ms, 1_790_212_772_957);
    assert_eq!(s.proc_start, "Wed Sep 23 20:13:55 2026");
    assert_eq!(s.kind, "interactive");
    assert_eq!(s.entrypoint, "cli");
    assert!(is_session_id(&s.session_id));
    let without_status = SESSION_9162.replace(r#""status":"busy","#, "");
    assert!(
        parse_session_file(&without_status)
            .unwrap_err()
            .contains("status")
    );
    assert!(parse_session_file("[1,2]").is_err());
    assert!(parse_session_file("not json").is_err());
    assert!(!is_session_id("abc; rm -rf ~"));
}

fn cand(exe: &str, ver: &str, source: Source) -> Candidate {
    Candidate {
        exe: PathBuf::from(exe),
        version: v(ver),
        source,
    }
}

#[test]
fn the_target_is_the_newest_build_never_sideways_and_managed_on_a_tie() {
    let managed_280 = cand("/pkg/agents/claude", "2.1.280", Source::Managed);
    let native_281 = cand("/n/versions/2.1.281", "2.1.281", Source::Native);
    // Pid 9162 today (measured): native 2.1.280 running, managed 2.1.280, the
    // native updater put 2.1.281 on disk -> the native 2.1.281.
    let t = choose_target(&v("2.1.280"), &[managed_280.clone(), native_281.clone()]);
    assert_eq!(t, Some(native_281.clone()));
    // A tie at the newest version goes to the managed copy.
    let managed_281 = cand("/pkg/agents/claude", "2.1.281", Source::Managed);
    let t = choose_target(&v("2.1.280"), &[native_281.clone(), managed_281.clone()]);
    assert_eq!(t, Some(managed_281));
    // Nothing newer: never moved sideways or down.
    assert_eq!(
        choose_target(&v("2.1.281"), &[managed_280.clone(), native_281]),
        None
    );
    assert_eq!(choose_target(&v("2.1.280"), &[managed_280]), None);
    // The managed twin on an old build (pid 54125's shape) moves onto current.
    let t = choose_target(
        &v("2.1.278"),
        &[cand("/pkg/agents/claude", "2.1.280", Source::Managed)],
    );
    assert_eq!(
        t.map(|c| c.version.to_string()),
        Some("2.1.280".to_string())
    );
}

#[test]
fn the_rewrite_carries_the_flags_and_resumes_the_conversation() {
    // Pid 9162's argv, measured: a bare --resume (the picker) becomes the id.
    let got = rewrite_argv(
        &argv(&["claude", "--dangerously-skip-permissions", "--resume"]),
        ID,
    );
    assert_eq!(
        got,
        Ok(argv(&["--dangerously-skip-permissions", "--resume", ID]))
    );
    // The resume family goes, with its values, whatever its spelling.
    for form in [
        argv(&["claude", "-r", "0000aaaa-1111", "--model", "opus"]),
        argv(&["claude", "--resume=0000aaaa-1111", "--model", "opus"]),
        argv(&["claude", "--continue", "--model", "opus"]),
        argv(&["claude", "-c", "--model", "opus"]),
        argv(&["claude", "--session-id", "0000aaaa-1111", "--model", "opus"]),
        argv(&[
            "claude",
            "--fork-session",
            "--model",
            "opus",
            "--resume",
            "0000aaaa-1111",
        ]),
    ] {
        assert_eq!(
            rewrite_argv(&form, ID),
            Ok(argv(&["--model", "opus", "--resume", ID])),
            "{form:?}"
        );
    }
    // A variadic flag keeps all its values; the first prompt (a positional) is
    // dropped, never sent twice; `--` ends options.
    assert_eq!(
        rewrite_argv(
            &argv(&[
                "claude",
                "--add-dir",
                "/a",
                "/b c",
                "--effort",
                "high",
                "--",
                "fix it"
            ]),
            ID
        ),
        Ok(argv(&[
            "--add-dir",
            "/a",
            "/b c",
            "--effort",
            "high",
            "--resume",
            ID
        ]))
    );
    assert_eq!(
        rewrite_argv(&argv(&["/x/claude", "fix the bug", "--verbose"]), ID),
        Ok(argv(&["--verbose", "--resume", ID]))
    );
    // Fail closed.
    assert_eq!(
        rewrite_argv(&argv(&["claude", "--brand-new-flag", "x"]), ID),
        Err(ArgvRefusal::UnknownFlag("--brand-new-flag".to_string()))
    );
    for not in [
        "-p",
        "--print",
        "--bg",
        "--tmux",
        "-w",
        "--no-session-persistence",
    ] {
        assert_eq!(
            rewrite_argv(&argv(&["claude", not]), ID),
            Err(ArgvRefusal::NotResumable(not.to_string())),
            "{not}"
        );
    }
}

#[test]
fn a_value_is_consumed_exactly_as_claudes_parser_consumes_it() {
    // `<values...>` is REQUIRED: Claude's parser takes the first value whatever
    // it looks like. A rewrite that stopped there left the flag bare, and the
    // bare flag swallowed the appended `--resume <id>` as its own values
    // (measured with the bundled parser: add-dir = ["--resume", id], no resume).
    for first in ["-", "-c", "--continue", "-r", "--", "--print", "-x"] {
        assert_eq!(
            rewrite_argv(
                &argv(&["claude", "--add-dir", first, "--model", "opus"]),
                ID
            ),
            Ok(argv(&[
                "--add-dir",
                first,
                "--model",
                "opus",
                "--resume",
                ID
            ])),
            "--add-dir {first}"
        );
    }
    // ...then every further value up to the next option, a lone `-` included.
    assert_eq!(
        rewrite_argv(
            &argv(&["claude", "--allowedTools", "Bash", "-", "Read", "-c"]),
            ID
        ),
        Ok(argv(&[
            "--allowedTools",
            "Bash",
            "-",
            "Read",
            "--resume",
            ID
        ]))
    );
    // `[value]` takes a lone `-` too.
    assert_eq!(
        rewrite_argv(&argv(&["claude", "--debug", "-", "--model", "opus"]), ID),
        Ok(argv(&["--debug", "-", "--model", "opus", "--resume", ID]))
    );
    // Unchanged: after a complete list a flag is a flag, and fails closed.
    assert_eq!(
        rewrite_argv(
            &argv(&["claude", "--add-dir", "/a", "-c", "--model", "opus"]),
            ID
        ),
        Ok(argv(&[
            "--add-dir",
            "/a",
            "--model",
            "opus",
            "--resume",
            ID
        ]))
    );
    assert_eq!(
        rewrite_argv(&argv(&["claude", "--add-dir", "/a", "--print"]), ID),
        Err(ArgvRefusal::NotResumable("--print".to_string()))
    );
    assert_eq!(
        rewrite_argv(&argv(&["claude", "--add-dir", "/a", "-x"]), ID),
        Err(ArgvRefusal::UnknownFlag("-x".to_string()))
    );
}

/// What Claude's parser made of an argv: every option it met, in order, with the
/// value it took (a list's values one event each, as Commander emits them), and
/// the words it took as operands. An unknown option or a missing value ends
/// Claude before any session exists.
#[derive(Debug, Default)]
struct Parsed {
    events: Vec<(String, Option<String>)>,
    operands: Vec<String>,
    unknown: bool,
    missing: bool,
}

/// CLAUDE'S OWN PARSER, transcribed branch for branch: `parseOptions` of the
/// Commander bundled in Claude Code 2.1.281, read out of the shipped binary. Its
/// options are the [`FLAGS`] table: `One` and `Many` are required values, `Many`
/// the variadic one, `Optional` an optional value, `None` a boolean. The one branch
/// left out is the positional-options stop (the program calls
/// `enablePositionalOptions()`), which fires only on an operand that names a
/// subcommand (`claude mcp …`): never a live session, and no word used here.
fn claude_parses(args: &[String]) -> Parsed {
    // `function r(o){return o.length>1&&o[0]==="-"}`
    let r = |o: &str| o.len() > 1 && o.starts_with('-');
    let mut p = Parsed::default();
    let mut s: std::collections::VecDeque<String> = args.iter().skip(1).cloned().collect();
    let mut active: Option<String> = None;
    while let Some(o) = s.pop_front() {
        if o == "--" {
            p.operands.extend(s.drain(..));
            break;
        }
        if let Some(a) = &active
            && !r(&o)
        {
            p.events.push((a.clone(), Some(o)));
            continue;
        }
        active = None;
        if r(&o)
            && let Some((arity, _)) = flag(&o)
        {
            match arity {
                Arity::One | Arity::Many => {
                    let Some(v) = s.pop_front() else {
                        p.missing = true; // optionMissingArgument
                        return p;
                    };
                    p.events.push((o.clone(), Some(v)));
                }
                Arity::Optional => {
                    let v = if s.front().is_some_and(|n| !r(n)) {
                        s.pop_front()
                    } else {
                        None
                    };
                    p.events.push((o.clone(), v));
                }
                Arity::None => p.events.push((o.clone(), None)),
            }
            active = (arity == Arity::Many).then_some(o);
            continue;
        }
        // `-abc`: a known `-a`, then the rest as its value or as more flags.
        if let Some(c) = o.strip_prefix('-').and_then(|t| t.chars().next())
            && c != '-'
            && o.len() > 1 + c.len_utf8()
            && let Some((arity, _)) = flag(&format!("-{c}"))
        {
            let rest = &o[1 + c.len_utf8()..];
            if arity == Arity::None {
                p.events.push((format!("-{c}"), None));
                s.push_front(format!("-{rest}"));
            } else {
                p.events.push((format!("-{c}"), Some(rest.to_string())));
            }
            continue;
        }
        // `--name=value`, for an option that takes a value.
        if let Some(h) = o.find('=')
            && h > 2
            && o.starts_with("--")
            && let Some((arity, _)) = flag(&o[..h])
            && arity != Arity::None
        {
            p.events
                .push((o[..h].to_string(), Some(o[h + 1..].to_string())));
            continue;
        }
        if r(&o) {
            p.unknown = true; // unknownOption
            return p;
        }
        p.operands.push(o);
    }
    p
}

/// The events of `fate` in a parse.
fn of_fate(p: &Parsed, fate: Fate) -> Vec<(String, Option<String>)> {
    p.events
        .iter()
        .filter(|(n, _)| flag(n).is_some_and(|(_, f)| f == fate))
        .cloned()
        .collect()
}

#[test]
fn every_short_argv_resumes_with_exactly_the_values_claude_parsed() {
    // Every argv of up to four words over flags of each arity and fate, the
    // dash-led values that decide consumption, an inline value, `--` and an
    // unknown flag — each fed to the rewrite AND to Claude's own parser.
    let words = [
        "--add-dir",
        "--debug",
        "--model",
        "--verbose",
        "-c",
        "-r",
        "--session-id",
        "--print",
        "--file",
        "-",
        "--",
        "/a",
        "--add-dir=/b",
        "--resume=x",
        "-x",
    ];
    let mut carried = 0usize;
    let mut refused = 0usize;
    let mut todo: Vec<Vec<String>> = vec![argv(&["claude"])];
    while let Some(a) = todo.pop() {
        if a.len() < 5 {
            for w in words {
                let mut next = a.clone();
                next.push(w.to_string());
                todo.push(next);
            }
        }
        let meant = claude_parses(&a);
        let got = rewrite_argv(&a, ID);
        if meant.unknown {
            // Claude never ran with this argv, and the rewrite fails closed.
            assert!(got.is_err(), "{a:?} -> {got:?}");
            continue;
        }
        if meant.missing {
            // Claude never ran with this argv: nothing to carry.
            continue;
        }
        if !of_fate(&meant, Fate::Refuse).is_empty() {
            assert!(
                matches!(got, Err(ArgvRefusal::NotResumable(_))),
                "{a:?} -> {got:?}"
            );
            refused += 1;
            continue;
        }
        let Ok(flags) = got else {
            // Refusing is always safe: a refused relaunch ends nothing.
            continue;
        };
        let mut relaunch = argv(&["claude"]);
        relaunch.extend(flags.iter().cloned());
        let now = claude_parses(&relaunch);
        assert!(
            !now.unknown && !now.missing && now.operands.is_empty(),
            "{a:?} -> {flags:?}"
        );
        assert!(of_fate(&now, Fate::Refuse).is_empty(), "{a:?} -> {flags:?}");
        assert_eq!(
            of_fate(&now, Fate::Keep),
            of_fate(&meant, Fate::Keep),
            "{a:?} -> {flags:?}: every carried flag keeps the values Claude gave it"
        );
        assert_eq!(
            of_fate(&now, Fate::Drop),
            vec![("--resume".to_string(), Some(ID.to_string()))],
            "{a:?} -> {flags:?}: resumes this conversation and nothing else"
        );
        carried += 1;
    }
    // Counted, never assumed: the space is walked and both answers occur.
    assert!(carried > 20_000 && refused > 10_000, "{carried} {refused}");
}

#[test]
fn the_relaunch_line_heals_a_frozen_shell_and_quotes_every_word() {
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let args = argv(&["--dangerously-skip-permissions", "--resume", ID]);
    assert_eq!(
        relaunch_line(Dialect::Zsh, Some(hook), None, exe, &args).unwrap(),
        format!(
            " . '/Users/_me/.aterm/shell.d/00-atpkg.zsh'; rehash; \
             '/Users/_me/Library/Application Support/aterm/pkg/agents/claude' \
             '--dangerously-skip-permissions' '--resume' '{ID}'"
        )
    );
    let bash = relaunch_line(
        Dialect::Bash,
        Some(Path::new("/h/00-atpkg.bash")),
        None,
        exe,
        &args,
    )
    .unwrap();
    assert!(
        bash.starts_with(" . '/h/00-atpkg.bash'; hash -r; '"),
        "{bash}"
    );
    let fish = relaunch_line(
        Dialect::Fish,
        Some(Path::new("/h/00-atpkg.fish")),
        None,
        exe,
        &args,
    )
    .unwrap();
    assert!(fish.starts_with(" source '/h/00-atpkg.fish'; '"), "{fish}");
    // A healthy shell: no heal. A different launch directory: a subshell `cd`.
    let cd = relaunch_line(Dialect::Zsh, None, Some("/w/it's"), exe, &args).unwrap();
    assert!(cd.starts_with(" ( cd -- '/w/it'\\''s' && exec '"), "{cd}");
    assert!(cd.ends_with(&format!("'{ID}' )")), "{cd}");
    assert!(relaunch_line(Dialect::Fish, None, Some("/w"), exe, &args).is_err());
    // Refused, never truncated or smuggled.
    assert!(relaunch_line(Dialect::Zsh, None, None, exe, &argv(&["--name", "a\nb"])).is_err());
    let long = "x".repeat(MAX_LINE);
    assert!(relaunch_line(Dialect::Zsh, None, None, exe, &[long]).is_err());
    assert_eq!(quote(Dialect::Fish, r"a\b'c"), r"'a\\b\'c'");
    assert_eq!(Dialect::from_exe_name("-zsh"), Some(Dialect::Zsh));
    assert_eq!(Dialect::from_exe_name("/bin/bash"), Some(Dialect::Bash));
    assert_eq!(Dialect::from_exe_name("tcsh"), None);
}

#[test]
fn a_line_the_tty_could_cut_is_refused() {
    // macOS keeps at most 1024 bytes of a line typed before the shell's line
    // editor is back (measured: with the Enter in that window 1023 ran and 1024
    // did not); `--resume <id>` is the END of the line, so any cut loses it.
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let line = |tty: LineDiscipline, pad: &str| {
        relaunch_line_on(
            tty,
            Dialect::Zsh,
            Some(hook),
            None,
            exe,
            &argv(&["--append-system-prompt", pad, "--resume", ID]),
        )
    };
    let base = line(LineDiscipline::Darwin, "")
        .expect("the everyday line")
        .len();
    let at = |tty: LineDiscipline, len: usize| line(tty, &"p".repeat(len - base));
    for tty in [LineDiscipline::Darwin, LineDiscipline::Linux] {
        let cut = at(tty, tty.line_keeps());
        assert!(
            cut.is_err(),
            "{tty:?}: a {}-byte line would be cut by the tty",
            cut.as_ref().map_or(0, String::len)
        );
        // The bound itself: exactly the kernel's max is written, one byte more
        // is not.
        assert_eq!(at(tty, tty.max_line()).map(|l| l.len()), Ok(tty.max_line()));
        assert!(at(tty, tty.max_line() + 1).is_err(), "{tty:?}");
    }
    // Each kernel keeps its OWN bound: Linux keeps 4095 bytes of a line, so a
    // line the old 2048 carried there is still carried — one bound for both
    // would refuse it for macOS's sake.
    assert!(at(LineDiscipline::Linux, 2048).is_ok());
    // The measured macOS cut, pinned by its number: 1024 bytes did not run.
    assert!(at(LineDiscipline::Darwin, 1024).is_err());
    // What the driver gets is this kernel's.
    assert_eq!(MAX_LINE, LineDiscipline::HOST.max_line());
    assert_eq!(
        LineDiscipline::HOST,
        if cfg!(target_os = "linux") {
            LineDiscipline::Linux
        } else {
            LineDiscipline::Darwin
        }
    );
    // The everyday line still fits, heal and `cd` included.
    let everyday = relaunch_line(
        Dialect::Zsh,
        Some(hook),
        Some("/Users/_me/some/project dir"),
        exe,
        &argv(&[
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--resume",
            ID,
        ]),
    );
    assert!(everyday.is_ok_and(|l| l.len() < MAX_LINE / 2));
}

/// The body of `fn <name>(` in `src`, up to its closing brace at column 0.
fn body_of<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let start = src.find(&format!("\nfn {name}("))?;
    let body = &src[start..];
    Some(&body[..body.find("\n}\n").unwrap_or(body.len())])
}

/// Whether `restart` in the driver's source plans the relaunch — the flags and
/// the line, both built in `line_for` — and returns on a refusal, BEFORE its
/// one signal.
fn refused_before_signalled(src: &str) -> bool {
    let (Some(restart), Some(line_for)) = (body_of(src, "restart"), body_of(src, "line_for"))
    else {
        return false;
    };
    let at = |needle: &str| restart.find(needle);
    let (Some(planned), Some(kill)) = (at("plan(opts"), at("libc::SIGTERM")) else {
        return false;
    };
    planned < kill
        && restart[planned..kill].contains("return unplanned(")
        && body_of(src, "plan").is_some_and(|p| p.contains("line_for("))
        && line_for.contains("upgrade::rewrite_argv(")
        && line_for.contains("upgrade::relaunch_line(")
        && src.matches("libc::SIGTERM").count() == 1
}

#[test]
fn a_refused_line_is_refused_before_the_agent_is_signalled() {
    // The bound above refuses a session, never ends one: the driver plans the
    // line, and returns on a refusal, before its one SIGTERM. The source is
    // the evidence. (That the plan is asked before the ANNOUNCEMENT too is the
    // driver's own test, `upgrade_drive_tests.rs`.)
    let drive = include_str!("upgrade_drive.rs");
    assert!(refused_before_signalled(drive));
    // NEGATIVE CONTROL: the same driver with its one signal moved to the top of
    // `restart`, ahead of the plan.
    let signals_first = drive.replacen("libc::SIGTERM", "0", 1).replacen(
        "\nfn restart(",
        "\nfn restart(/* libc::SIGTERM */",
        1,
    );
    assert!(!refused_before_signalled(&signals_first));
}

#[test]
fn the_ready_answer_is_an_assistant_line_never_the_announcement() {
    let marker = ready_marker(ID, &v("2.1.281"), 7);
    assert!(marker.starts_with("ATERM-UPGRADE-READY-"));
    assert_ne!(
        marker,
        ready_marker(ID, &v("2.1.281"), 8),
        "a new salt, a new marker"
    );
    let prompt = prepare_prompt(&v("2.1.280"), &v("2.1.281"), Source::Native, &marker);
    assert!(prompt.contains(&marker) && prompt.contains("do not cancel them"));
    let user = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":{}}}]}}}}"#,
        aterm_json::to_string(&prompt).unwrap()
    );
    assert!(
        !transcript_has_ready(&user, &marker),
        "the announcement is not the answer"
    );
    let said_inline = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"I will reply {marker} soon"}}]}}}}"#
    );
    assert!(
        !transcript_has_ready(&said_inline, &marker),
        "only a line of its own counts"
    );
    let answered = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Everything is saved.\n{marker}"}}]}}}}"#
    );
    assert!(transcript_has_ready(
        &format!("{{cut\n{user}\n{answered}\n"),
        &marker
    ));
    let fenced = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"`{marker}`"}}]}}}}"#
    );
    assert!(transcript_has_ready(&fenced, &marker));
}

#[test]
fn an_empty_composer_is_blank_or_a_placeholder_and_nothing_else() {
    let blank = rows(&["─────────────────", "❯ ", "─────────────────"]);
    assert!(composer_is_empty(&blank, Some((1, 2)), false));
    let suggestion = rows(&["─────────────────", "❯ try the tests", "─────────────────"]);
    assert!(
        composer_is_empty(&suggestion, Some((1, 2)), true),
        "the dim placeholder"
    );
    assert!(
        !composer_is_empty(&suggestion, Some((1, 17)), true),
        "typed: the cursor moved"
    );
    assert!(
        !composer_is_empty(&suggestion, None, true),
        "no cursor, no proof"
    );
    let two_rows = rows(&[
        "─────────────────",
        "❯ ",
        "second line",
        "─────────────────",
    ]);
    assert!(!composer_is_empty(&two_rows, Some((1, 2)), true));
    // The live screen of 2026-09-23 while the person typed `we'd want`.
    let typing = rows(&["─────────────────", "❯ we'd want", "─────────────────"]);
    assert!(!composer_is_empty(&typing, Some((1, 11)), false));
    assert!(
        !composer_is_empty(&rows(&["$ ls"]), Some((0, 4)), false),
        "no composer"
    );
}

fn idle_facts() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        approval_box: false,
        busy_footer: false,
        background: Vec::new(),
        held: false,
        quiet_s: 60,
    }
}

/// One fact knocked off an otherwise-go reading, and the wait it must cause.
type Knock = (fn(&mut Facts), &'static str);

#[test]
fn the_gates_wait_for_every_fact_and_never_for_a_kill() {
    assert_eq!(gate_announce(&idle_facts()), Gate::Go);
    let cases: [Knock; 7] = [
        (|f| f.held = true, "held"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.status = "shell".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for (edit, word) in cases {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(gate_announce(&f), Gate::Wait(word));
        assert_eq!(gate_restart(&f, true), Gate::Wait(word));
    }
    assert_eq!(gate_restart(&idle_facts(), false), Gate::Wait("not-ready"));
    let mut working = idle_facts();
    working.background = vec!["zsh".to_string()];
    assert_eq!(gate_restart(&working, true), Gate::Wait("background"));
    assert_eq!(gate_restart(&idle_facts(), true), Gate::Go);
}

#[test]
fn the_reducer_announces_waits_for_ready_reasks_and_gives_up_without_forcing() {
    let f = idle_facts();
    assert_eq!(next_step(&Phase::Pending, &f, false, 100), Step::Announce);
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    assert_eq!(
        next_step(&asked, &f, false, 200),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(next_step(&asked, &f, true, 200), Step::Terminate);
    let mut busy = f.clone();
    busy.background = vec!["caffeinate".to_string()];
    assert_eq!(
        next_step(&asked, &busy, true, 200),
        Step::Wait("background")
    );
    assert_eq!(next_step(&asked, &f, false, 100 + REASK_S), Step::Announce);
    let tired = Phase::Announced {
        at_s: 100,
        asks: MAX_ASKS,
    };
    assert_eq!(next_step(&tired, &f, false, 100 + REASK_S), Step::GiveUp);
    assert_eq!(
        next_step(&Phase::Exiting { at_s: 1 }, &f, true, 5),
        Step::Wait("in-flight")
    );
    assert_eq!(Phase::Failed("x".into()).word(), "failed:x");
}

// ---------------------------------------------------------------- the model

/// An assistant row as Claude Code 2.1.282 writes one (measured 2026-09-24 in
/// the owner's transcript: `isSidechain`, `type`, `message.model`, the content
/// blocks), naming `model`.
fn said_by(model: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"model":"{model}","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"version":"2.1.282"}}"#
    )
}

/// The rows a turn interleaves with its assistant rows: the person's prompt,
/// a tool's result (a USER row), an attachment and a system row.
fn between() -> [String; 4] {
    [
        r#"{"type":"user","message":{"role":"user","content":"run the tests"}}"#.to_string(),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#.to_string(),
        r#"{"type":"attachment","attachment":{"type":"edited_text_file"}}"#.to_string(),
        r#"{"type":"system","subtype":"turn_duration","version":"2.1.282"}"#.to_string(),
    ]
}

/// Claude Code's own assistant rows name no model: `<synthetic>`, measured
/// 2026-09-24 (22 in the owner's transcripts — a limit notice, and `No
/// response requested.` written just after a restart).
const SYNTHETIC: &str = r#"{"type":"assistant","isApiErrorMessage":false,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"No response requested."}]}}"#;

fn jsonl(rows: &[&str]) -> String {
    rows.join("\n") + "\n"
}

#[test]
fn the_model_is_the_last_assistant_turns_and_the_first_is_the_resumed_ones() {
    let [prompt, tool, attached, system] = between();
    let (a, b, c) = (
        said_by("claude-opus-5"),
        said_by("claude-fable-5-1"),
        said_by("claude-opus-5-5"),
    );
    // Several turns on different models, the other rows interleaved.
    let tail = jsonl(&[&prompt, &a, &tool, &b, &attached, &prompt, &c, &system]);
    assert_eq!(transcript_model(&tail).as_deref(), Some("claude-opus-5-5"));
    assert_eq!(
        transcript_first_model(&tail, "2.1.282").as_deref(),
        Some("claude-opus-5")
    );
    // No assistant turn at all.
    let quiet = jsonl(&[&prompt, &tool, &attached, &system]);
    assert_eq!(transcript_model(&quiet), None);
    assert_eq!(transcript_first_model(&quiet, "2.1.282"), None);
    assert_eq!(transcript_model(""), None);
    // The tail's cut first line is skipped, not read: alone it is no turn, and
    // before a whole one the whole one answers from both ends.
    let cut = &b[b.len() / 3..];
    assert!(cut.contains("claude-fable-5-1"), "the cut keeps the model");
    assert_eq!(transcript_model(&jsonl(&[cut, &prompt])), None);
    assert_eq!(
        transcript_first_model(&jsonl(&[cut, &prompt, &c]), "2.1.282").as_deref(),
        Some("claude-opus-5-5")
    );
    // A row half-written at the end (Claude appending) is not a turn yet.
    let half = &c[..c.len() / 2];
    assert_eq!(
        transcript_model(&format!("{a}\n{half}")).as_deref(),
        Some("claude-opus-5")
    );
    // Claude's own rows name no model, and a row that names something no model
    // id looks like is not taken either: a model is typed into the tab.
    let odd = said_by("claude opus\\u001b[2J");
    for filler in [SYNTHETIC, odd.as_str()] {
        assert_eq!(
            transcript_model(&jsonl(&[&a, filler])).as_deref(),
            Some("claude-opus-5"),
            "{filler}"
        );
        assert_eq!(
            transcript_first_model(&jsonl(&[filler, &c]), "2.1.282").as_deref(),
            Some("claude-opus-5-5"),
            "{filler}"
        );
        assert_eq!(transcript_model(&jsonl(&[&prompt, filler])), None);
    }
    // A subagent's turn is not the session's model.
    let sidechain =
        said_by("claude-haiku-5").replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        transcript_model(&jsonl(&[&c, &sidechain])).as_deref(),
        Some("claude-opus-5-5")
    );
    // The ids other providers and the long-context variant carry are ids.
    for id in [
        "claude-opus-5-5[1m]",
        "us.anthropic.claude-opus-5-5-v1:0",
        "claude-opus-5-5@20260901",
    ] {
        assert_eq!(transcript_model(&said_by(id)).as_deref(), Some(id), "{id}");
    }
}

#[test]
fn the_continuation_says_what_the_session_ran_before_the_restart() {
    let (from, to) = (v("2.1.281"), v("2.1.282"));
    let said = continue_prompt(&from, &to, Some("claude-opus-5"));
    assert_eq!(
        said,
        "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.282 (from \
         2.1.281); it ran claude-opus-5 before the restart and was resumed. Continue where you \
         left off; if you were waiting on the user, say so in one line."
    );
    assert!(!said.starts_with(ANNOUNCE_HEAD), "never the announcement");
    // No model known: the clause is left out, not guessed.
    let bare = continue_prompt(&from, &to, None);
    assert_eq!(
        bare,
        "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.282 (from \
         2.1.281) and resumed. Continue where you left off; if you were waiting on the user, say \
         so in one line."
    );
}

#[test]
fn the_outcome_names_the_model_after_and_a_change_from_before() {
    assert_eq!(
        restart_outcome(
            "2.1.282",
            Some("claude-opus-5-5"),
            Some("claude-opus-5-5"),
            None
        ),
        "claude restarted on 2.1.282 · model claude-opus-5-5"
    );
    assert_eq!(
        restart_outcome("2.1.282", None, Some("claude-opus-5-5"), None),
        "claude restarted on 2.1.282 · model claude-opus-5-5",
        "nothing before to compare with"
    );
    assert_eq!(
        restart_outcome(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5-5"),
            None
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (a session \
         launched without --model takes the current default; /model changes it)"
    );
    assert_eq!(
        restart_outcome("2.1.282", Some("claude-opus-5"), None, None),
        "claude restarted on 2.1.282 · model unconfirmed (it ran claude-opus-5 before the \
         restart)"
    );
    assert_eq!(
        restart_outcome("2.1.282", None, None, None),
        "claude restarted on 2.1.282 · model unconfirmed"
    );
}

/// A cloud provider's model can be named by an ARN, longer than any first-party
/// id: it is an id all the same, read and said, never "unconfirmed".
#[test]
fn a_provider_arn_is_a_model_id() {
    let arn = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/\
               us.anthropic.claude-opus-5-5-20260901-v1:0";
    assert!(arn.len() > 64, "{}", arn.len());
    assert_eq!(transcript_model(&said_by(arn)).as_deref(), Some(arn));
    // The bound still holds: an id is never an unbounded run of bytes.
    let endless = "a".repeat(4096);
    assert_eq!(transcript_model(&said_by(&endless)), None);
}

/// THE REASON FOR A CHANGE is what decided the model after: a launch that
/// named `--model` keeps it on the relaunch — which undoes a `/model` choice
/// made since, and resolves an alias against the new build — so the default
/// had nothing to do with it and is not blamed.
#[test]
fn the_outcome_blames_a_kept_model_flag_not_the_default() {
    assert_eq!(
        restart_outcome(
            "2.1.282",
            Some("claude-opus-5-5"),
            Some("claude-sonnet-5"),
            Some("claude-sonnet-5")
        ),
        "claude restarted on 2.1.282 · model claude-opus-5-5 -> claude-sonnet-5 (the relaunch \
         kept the launch's --model claude-sonnet-5; /model changes it)"
    );
    assert_eq!(
        restart_outcome(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5-5"),
            Some("opus")
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (the relaunch \
         kept the launch's --model opus; /model changes it)",
        "an alias the new build resolves elsewhere"
    );
    // No change: the flag is not mentioned.
    assert_eq!(
        restart_outcome(
            "2.1.282",
            Some("claude-sonnet-5"),
            Some("claude-sonnet-5"),
            Some("claude-sonnet-5")
        ),
        "claude restarted on 2.1.282 · model claude-sonnet-5"
    );
}

/// THE KEPT `--model` is read by the rewrite's own parse: the last one wins,
/// as in Claude's parser; `--model=<v>` is one; a `--model` that is another
/// flag's value, or comes after `--`, is not one.
#[test]
fn the_launch_model_is_the_model_flag_the_rewrite_keeps() {
    let got = |w: &[&str]| launch_model(&argv(w));
    assert_eq!(
        got(&["claude", "--model", "claude-sonnet-5"]).as_deref(),
        Some("claude-sonnet-5")
    );
    assert_eq!(got(&["claude", "--model=opus"]).as_deref(), Some("opus"));
    assert_eq!(
        got(&[
            "claude",
            "--model",
            "opus",
            "--verbose",
            "--model",
            "sonnet"
        ])
        .as_deref(),
        Some("sonnet"),
        "the last one wins"
    );
    assert_eq!(got(&["claude", "--verbose"]), None);
    assert_eq!(got(&["claude", "--fallback-model", "sonnet"]), None);
    assert_eq!(
        got(&["claude", "--append-system-prompt", "--model", "x"]),
        None,
        "a value, not the flag (and `x` is a positional)"
    );
    assert_eq!(got(&["claude", "--", "--model", "x"]), None);
    assert_eq!(got(&["claude", "--model"]), None, "no value");
    // A launch the rewrite refuses relaunches nothing.
    assert_eq!(got(&["claude", "--print", "--model", "x"]), None);
}

/// THE RESUMED MODEL is the new process's: every transcript row carries the
/// `version` that wrote it (measured 2026-09-24: all 25,018 assistant rows in
/// the owner's transcripts), so a row the old process wrote past the mark —
/// alive past its SIGTERM for one more turn — is never taken as the answer.
#[test]
fn the_resumed_model_is_read_only_from_the_new_builds_rows() {
    let old = said_by("claude-opus-5").replace("2.1.282", "2.1.281");
    let new = said_by("claude-opus-5-5");
    let past = jsonl(&[&old, &new]);
    assert_eq!(
        transcript_first_model(&past, "2.1.282").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(transcript_first_model(&jsonl(&[&old]), "2.1.282"), None);
    // A row that names no version is no build's.
    let unversioned = new.replace(r#","version":"2.1.282""#, "");
    assert_eq!(
        transcript_first_model(&jsonl(&[&unversioned]), "2.1.282"),
        None
    );
}
