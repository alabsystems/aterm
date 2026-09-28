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
        Ok(argv(&["--add-dir", "/a", "/b c", "--resume", ID])),
        "a launch --effort is dropped: the resumed session takes the user's default effort"
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

/// THE HEALED RELAUNCH LINE (2026-09-26): the shell takes its key from the file
/// FIRST, in its own dialect, then runs the relaunch line unchanged — still one
/// line, still led by the one space that keeps it out of history. The path is
/// quoted by the same rule as every other word (the real control dir has a
/// space in it); the key is not in the line at all. A path with a control
/// character, or a healed line past the kernel's bound, is refused — and the
/// caller then types the plain line, so the heal never costs the relaunch.
#[test]
fn the_relaunch_line_takes_the_key_first_in_the_shells_own_dialect() {
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let key = Path::new("/Users/_me/Library/Application Support/aterm/rekey/s-0a.1");
    let q = "'/Users/_me/Library/Application Support/aterm/rekey/s-0a.1'";
    let args = argv(&["--resume", ID]);
    let zsh = relaunch_line(Dialect::Zsh, Some(hook), Some("/w"), exe, &args).unwrap();
    // The UPGRADING form where it fits (2026-09-26): the shell's own text, the
    // quoted path its only path.
    let form = |shell, q: &str| {
        aterm_shell_integration::typed_rekey_with_loader(shell, q).expect("scripted")
    };
    assert_eq!(
        with_rekey(Dialect::Zsh, &zsh, key).unwrap(),
        format!(
            " {} {}",
            form(aterm_shell_integration::ShellType::Zsh, q),
            &zsh[1..]
        )
    );
    let bash = relaunch_line(Dialect::Bash, None, None, exe, &args).unwrap();
    let healed = with_rekey(Dialect::Bash, &bash, key).unwrap();
    assert_eq!(
        healed,
        format!(
            " {} {}",
            form(aterm_shell_integration::ShellType::Bash, q),
            &bash[1..]
        )
    );
    let fish = relaunch_line(Dialect::Fish, None, None, exe, &args).unwrap();
    let healed = with_rekey(Dialect::Fish, &fish, key).unwrap();
    assert_eq!(
        healed,
        format!(
            " {} {}",
            form(aterm_shell_integration::ShellType::Fish, q),
            &fish[1..]
        )
    );
    // The key-only form beneath it is the scripts' older text, unchanged.
    assert_eq!(
        aterm_shell_integration::typed_rekey(aterm_shell_integration::ShellType::Zsh, q)
            .expect("scripted"),
        format!(
            "{{ read -r __aterm_shell_nonce <{q}; }} 2>/dev/null; command rm -f -- {q}; \
             __aterm_id_suffix_str=\";id=$__aterm_shell_nonce\";"
        )
    );
    // Quoted like any word: a quote in the path cannot end the word early.
    let odd = with_rekey(Dialect::Zsh, &bash, Path::new("/r/it's")).unwrap();
    assert!(odd.contains("<'/r/it'\\''s'; }"), "{odd}");
    assert!(
        with_rekey(Dialect::Fish, &fish, Path::new("/r/it's"))
            .unwrap()
            .contains(r"command cat '/r/it\'s' 2>/dev/null"),
    );
    // Refused: a control character, and a line the tty could cut.
    assert!(with_rekey(Dialect::Zsh, &zsh, Path::new("/r/a\nb")).is_err());
    let line_of = |tty: LineDiscipline, len: usize| {
        let padded = |pad: &str| {
            relaunch_line_on(
                tty,
                Dialect::Zsh,
                None,
                None,
                exe,
                &argv(&["--append-system-prompt", pad, "--resume", ID]),
            )
        };
        let base = padded("").unwrap().len();
        padded(&"p".repeat(len - base)).unwrap()
    };
    let take = with_rekey(Dialect::Zsh, &zsh, key).unwrap().len() - zsh.len();
    let key_only = aterm_shell_integration::typed_rekey(aterm_shell_integration::ShellType::Zsh, q)
        .expect("scripted")
        .len()
        + 1;
    assert!(key_only < take, "the key-only form is the shorter");
    for tty in [LineDiscipline::Darwin, LineDiscipline::Linux] {
        // Exactly the bound, the upgrading heal included, is written in full;
        // one byte more falls back to the key-only heal, which still fits.
        let room = line_of(tty, tty.max_line() - take);
        let full = with_rekey_on(tty, Dialect::Zsh, &room, key).unwrap();
        assert_eq!(full.len(), tty.max_line(), "{tty:?}");
        assert!(full.contains("__atk"), "{tty:?}: the upgrading form");
        let over = line_of(tty, tty.max_line() - take + 1);
        let fallback = with_rekey_on(tty, Dialect::Zsh, &over, key).unwrap();
        assert!(
            fallback.starts_with(" { read -r __aterm_shell_nonce <")
                && fallback.ends_with(&over[1..]),
            "{tty:?}: past the upgrading form's bound, the key-only heal"
        );
        // Exactly the key-only bound is written; one byte more is refused.
        let room = line_of(tty, tty.max_line() - key_only);
        assert_eq!(
            with_rekey_on(tty, Dialect::Zsh, &room, key).map(|l| l.len()),
            Ok(tty.max_line()),
            "{tty:?}"
        );
        let over = line_of(tty, tty.max_line() - key_only + 1);
        assert!(
            with_rekey_on(tty, Dialect::Zsh, &over, key).is_err(),
            "{tty:?}: a plain line that fits, healed past both bounds: refused"
        );
    }
    // The everyday line, heal, `cd`, a flag or two and the key's path: it fits
    // with room to spare on the smaller bound.
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
    )
    .unwrap();
    let healed = with_rekey(Dialect::Zsh, &everyday, key).unwrap();
    assert!(healed.len() < MAX_LINE * 3 / 4, "{}", healed.len());
    assert!(healed.starts_with(" {") && !healed.starts_with("  "));
}

/// The body of `fn <name>(` (private or `pub(super)`) in `src`, up to its
/// closing brace at column 0.
fn body_of<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let start = src
        .find(&format!("\nfn {name}("))
        .or_else(|| src.find(&format!("\npub(super) fn {name}(")))?;
    let body = &src[start..];
    Some(&body[..body.find("\n}\n").unwrap_or(body.len())])
}

/// Whether `restart` in the driver's source plans the relaunch — the flags and
/// the line, both built in the relaunch primitive's `line_for` — and returns
/// on a refusal, BEFORE its one signal: the call to `terminate`, which holds
/// the driver's only `libc::SIGTERM` (its fallback for an aterm older than the
/// server's exact-pid `signal term pid=`); the relaunch primitive sends none.
fn refused_before_signalled(src: &str, relaunch: &str) -> bool {
    let (Some(restart), Some(line_for)) = (body_of(src, "restart"), body_of(relaunch, "line_for"))
    else {
        return false;
    };
    let at = |needle: &str| restart.find(needle);
    let (Some(planned), Some(kill)) = (at("plan(opts"), at("terminate(c")) else {
        return false;
    };
    planned < kill
        && restart[planned..kill].contains("return unplanned(")
        && body_of(relaunch, "plan").is_some_and(|p| p.contains("line_for("))
        && line_for.contains("upgrade::rewrite_argv(")
        && line_for.contains("upgrade::relaunch_line(")
        && src.matches("libc::SIGTERM").count() == 1
        && body_of(src, "terminate").is_some_and(|t| t.contains("libc::SIGTERM"))
        && src.matches("terminate(c,").count() == 1
        && !relaunch.contains("libc::SIGTERM")
}

#[test]
fn a_refused_line_is_refused_before_the_agent_is_signalled() {
    // The bound above refuses a session, never ends one: the driver plans the
    // line, and returns on a refusal, before its one signal. The source is
    // the evidence. (That the plan is asked before the ANNOUNCEMENT too is the
    // driver's own test, `upgrade_drive_tests.rs`.)
    let drive = include_str!("upgrade_drive.rs");
    let relaunch = include_str!("relaunch.rs");
    assert!(refused_before_signalled(drive, relaunch));
    // NEGATIVE CONTROL: the same driver with its one signal moved to the top of
    // `restart`, ahead of the plan.
    let signals_first = drive
        .replacen(
            "terminate(c, tab, pid, opts.human_grace_s)",
            "Terminated::Failed",
            1,
        )
        .replacen("\nfn restart(", "\nfn restart(/* terminate(c */", 1);
    assert!(!refused_before_signalled(&signals_first, relaunch));
}

/// THE NOTICE'S LIST OF WHAT RUNS (2026-09-26): each process by pid, name,
/// age and command, in one line that a paste cannot split (no control
/// characters), at most [`HELD_NAMED`] named and the rest counted. Nothing at
/// all when nothing runs, so a notice to an agent with no background work
/// reads exactly as before.
#[test]
fn the_notice_lists_what_runs_under_the_agent_in_one_line() {
    assert_eq!(running_clause(&[]), "");
    let held = |pid: u32, age_s: u64, command: &str| Held {
        pid,
        name: "zsh".to_string(),
        age_s,
        command: command.to_string(),
    };
    let two = running_clause(&[
        held(
            63492,
            5 * 86_400 + 4 * 3_600,
            "until [ $n -ge 6 ]; do sleep 15; done",
        ),
        held(64036, 90, "until [ $n -ge 6 ];\n do sleep 20;\tdone"),
    ]);
    assert_eq!(
        two,
        " Running under you now, as aterm sees it: pid 63492 (zsh, 5d4h): until [ $n -ge 6 ]; \
         do sleep 15; done; pid 64036 (zsh, 1m): until [ $n -ge 6 ]; do sleep 20; done."
    );
    assert!(!two.chars().any(char::is_control), "{two:?}");
    let long = "x".repeat(HELD_COMMAND_CHARS * 2);
    let many: Vec<Held> = (0..7).map(|i| held(100 + i, 5, &long)).collect();
    let clause = running_clause(&many);
    assert!(clause.ends_with("; and 2 more."), "{clause}");
    assert_eq!(clause.matches("pid ").count(), HELD_NAMED);
    assert!(
        clause.contains('…') && !clause.contains(&long),
        "each command is cut"
    );
    // A process the table names but `ps` could not describe is still named.
    assert_eq!(
        running_clause(&[held(7, 0, "")]),
        " Running under you now, as aterm sees it: pid 7 (zsh, 0s)."
    );
    // The notice itself stays one line with the clause appended.
    let notice = prepare_prompt(&v("2.1.278"), &v("2.1.283"), Source::Managed, "M") + &two;
    assert!(!notice.contains('\n') && notice.starts_with(ANNOUNCE_HEAD));
}

/// The incident's closer (measured 2026-09-27), as the transcript of the
/// Claude Code that ran it twice, at 03:09:00Z and 03:19:43Z on 2026-09-22,
/// records it: 253 characters, 119 of them the one path in `D=`.
const CLOSER: &str = "D=/Users//person/.claude/projects/-Users-person/\
    00000000-0000-4000-8000-000000000001/subagents/workflows/wf_examplerun-1; \
    tail -f -n +1 \"$D/journal.jsonl\" | /usr/bin/grep -m1 -F \
    '\"agentId\":\"a0000000000000001\",\"result\"' > /dev/null; echo \"closer finished\"";

/// THE NOTICE KEEPS WHAT MAKES A WAIT DEAD (measured 2026-09-27: two shells
/// held a tab five days on [`CLOSER`], a `tail -f` its `grep -m1` had already
/// matched and left). Cut at its 160th character, the notice quoted each as
/// its 119-character path and then `; tail -f -n +1 "$D/journal.jsonl" |
/// /us…`: the `grep -m1` that made the `tail -f` a wait that can never end
/// was gone, and what was left read as live work. A
/// command over [`HELD_COMMAND_CHARS`] now has the middle of each long path
/// elided before that cut, which stays the backstop.
#[test]
fn the_notice_keeps_a_pipeline_a_long_path_would_push_out() {
    assert_eq!(CLOSER.chars().count(), 253);
    let quoted = |command: &str| {
        let clause = running_clause(&[Held {
            pid: 41234,
            name: "zsh".to_string(),
            age_s: 475_200,
            command: command.to_string(),
        }]);
        let (_, quoted) = clause.split_once("): ").expect("a command");
        let quoted = quoted.strip_suffix('.').expect("the clause's period");
        assert!(quoted.chars().count() <= HELD_COMMAND_CHARS, "{quoted}");
        assert!(
            quoted.chars().all(|c| c == '…' || command.contains(c)),
            "only what the agent ran, and the cut mark: {quoted}"
        );
        quoted.to_string()
    };
    let closer = quoted(CLOSER);
    assert!(
        closer.contains("grep -m1 -F") && closer.contains("echo \"closer finished\""),
        "{closer}"
    );
    assert_eq!(
        closer,
        "D=/Users/…/wf_examplerun-1; tail -f -n +1 \"$D/journal.jsonl\" | /usr/bin/grep -m1 -F \
         '\"agentId\":\"a0000000000000001\",\"result\"' > /dev/null; echo \"closer finished\""
    );

    // A long path passed as an argument is elided the same way, the words in
    // a quote are not: there a path can be the pattern the wait turns on.
    let dir = "/Users//person/.claude/projects/-Users-person/00000000-0000-4000-8000-000000000001";
    let pattern = format!("{dir}/subagents/done");
    let argument = quoted(&format!(
        "tail -f -n +1 {dir}/subagents/workflows/wf_examplerun-1/journal.jsonl | grep -m1 -F \
         \"wrote {pattern} ok\""
    ));
    assert_eq!(
        argument,
        format!("tail -f -n +1 /Users/…/journal.jsonl | grep -m1 -F \"wrote {pattern} ok\"")
    );

    // NEGATIVE CONTROLS. A command that fits is quoted whole, however long
    // its paths. A URL is never a path, its query included.
    let fits = format!("tail -f {dir}/journal.jsonl");
    assert_eq!(quoted(&fits), fits);
    let url = "https://api.github.com/repos/people/aterm/actions/runs/123456789/jobs?filter=latest";
    let fetch = quoted(&format!(
        "until curl -fsS -o {dir}/jobs.json {url}; do sleep 30; done"
    ));
    assert_eq!(
        fetch,
        format!("until curl -fsS -o /Users/…/jobs.json {url}; do sleep 30; done")
    );
    // Only a ROOTED path is elided: a long relative one is quoted whole, as
    // the agent wrote it. A redirection's path is elided after its operator.
    let relative =
        "crates/aterm-agent/src/harness/subagents/workflows/wf_examplerun-1/journal.jsonl";
    assert!(relative.chars().count() > HELD_PATH_CHARS);
    let padded = |word: &str| format!("tail -f {word} {}", "x".repeat(HELD_COMMAND_CHARS));
    let whole = quoted(&padded(relative));
    assert!(
        whole.starts_with(&format!("tail -f {relative} ")),
        "{whole}"
    );
    let redirect = quoted(&padded(&format!("2>>{dir}/subagents/workflows/log")));
    assert!(
        redirect.starts_with("tail -f 2>>/Users/…/log "),
        "{redirect}"
    );
    // A word with a quote, a glob or an escape in it is not a path to elide,
    // and a long path with nothing between its first name and its last is
    // left as it is.
    for word in [
        format!("{dir}/*/journal.jsonl"),
        format!("{dir}/sub\\ dir/journal.jsonl"),
        format!("'{dir}/subagents/journal.jsonl'"),
        format!("/{}/{}", "a".repeat(40), "b".repeat(40)),
    ] {
        let command = format!("{word} {}", "x".repeat(HELD_COMMAND_CHARS));
        assert!(quoted(&command).starts_with(&word), "{word}");
    }
    // The fold is unchanged: a line or paragraph separator in the command is
    // a space, never a break in the one-paste notice.
    let broken = quoted(&format!("{CLOSER}\u{2028}echo\u{2029}done"));
    assert!(
        !broken.contains(['\u{2028}', '\u{2029}']) && !broken.chars().any(char::is_control),
        "{broken:?}"
    );
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
    assert!(
        prompt.contains("can never happen") && prompt.contains("that wait is not work"),
        "a wait that can never end is not work to keep (2026-09-26): {prompt}"
    );
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
        hold_s: 0,
        owner_now: false,
        attended: false,
        background_point: false,
        taskless: false,
        limited: false,
        login: false,
        undelivered: false,
        queued: false,
        ready_s: 0,
        idle_looks: 0,
        failed_s: 0,
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

/// Only a box (a visible one, or Claude's own `waiting` status) and a typed draft
/// are a PERSON's to clear. The agent's own work, aterm's hold and the settle
/// window are not — the negative controls, each of which shuts the gate too.
#[test]
fn only_a_box_and_a_draft_are_a_persons_hold() {
    assert_eq!(person_hold(&idle_facts()), None);
    let persons: [Knock; 3] = [
        (|f| f.approval_box = true, "box"),
        (|f| f.status = "waiting".into(), "box"),
        (|f| f.composer_empty = false, "draft"),
    ];
    for (edit, word) in persons {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(person_hold(&f), Some(word));
    }
    let agents: [Knock; 6] = [
        (|f| f.held = true, "held"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.status = "shell".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.background = vec!["zsh".to_string()], "background"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for (edit, word) in agents {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(gate_restart(&f, true), Gate::Wait(word), "the gate is shut");
        assert_eq!(person_hold(&f), None, "{word} is not a person's");
    }
}

/// THE DRAIN IS BOUNDED AGAINST A PERSON. A READY answer held past [`DRAIN_S`] by
/// a box nobody answers or a draft nobody sends, standing [`HOLD_S`], is void.
/// Before the bound it is still a wait. A busy turn and a hold are waited for
/// past it and never voided (the negative controls: the owner's "never kill
/// background work", and a void is not a kill either). Background work under
/// the agent is never voided or ended either, but past [`REASK_S`] it is asked
/// about again, and past [`MAX_ASKS`] the upgrade gives up. Once void, the
/// answer is gone, so the reducer asks again at the next idle point, not later.
#[test]
fn a_ready_answer_a_person_holds_past_the_drain_bound_is_void() {
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    let bound = 100 + 30 * 60;
    let mut boxed = idle_facts();
    boxed.approval_box = true;
    boxed.hold_s = 5 * 60;
    assert_eq!(
        next_step(&asked, &boxed, true, bound - 1),
        Step::Wait("box")
    );
    assert_eq!(next_step(&asked, &boxed, true, bound), Step::Void("box"));
    let mut waiting = idle_facts();
    waiting.status = "waiting".into();
    waiting.hold_s = 5 * 60;
    assert_eq!(next_step(&asked, &waiting, true, bound), Step::Void("box"));
    let mut drafted = idle_facts();
    drafted.composer_empty = false;
    drafted.hold_s = 5 * 60;
    assert_eq!(
        next_step(&asked, &drafted, true, bound - 1),
        Step::Wait("draft")
    );
    assert_eq!(
        next_step(&asked, &drafted, true, bound),
        Step::Void("draft")
    );
    // A box raised under running background work is still a person's: the READY
    // is void, and nothing is ended — the background keeps running.
    let mut both = boxed.clone();
    both.background = vec!["caffeinate".to_string()];
    assert_eq!(next_step(&asked, &both, true, bound), Step::Void("box"));

    let agents: [Knock; 4] = [
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.held = true, "held"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for (edit, word) in agents {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(
            next_step(&asked, &f, true, bound + 7 * 24 * 3600),
            Step::Wait(word),
            "the agent's own {word} is waited for, however long"
        );
    }
    // Work running UNDER the agent is waited for and never voided or ended.
    // But a READY answer it outlives by a whole REASK_S can no longer be acted
    // on, so it is asked about again (2026-09-26: two poll loops that could
    // never end held a tab for four days, told once). Past MAX_ASKS the
    // upgrade gives up. It never ends the agent.
    let mut working = idle_facts();
    working.background = vec!["zsh".to_string()];
    // The answer came with the notice: its own clock has run as long.
    working.ready_s = 7 * 24 * 3600;
    assert_eq!(
        next_step(&asked, &working, true, bound - 1),
        Step::Wait("background")
    );
    assert_eq!(
        next_step(&asked, &working, true, bound + 7 * 24 * 3600),
        Step::Announce
    );
    let last = Phase::Announced {
        at_s: 100,
        asks: MAX_ASKS,
    };
    assert_eq!(
        next_step(&last, &working, true, bound + 7 * 24 * 3600),
        Step::GiveUp
    );
    assert_eq!(
        next_step(&asked, &idle_facts(), true, bound + 3600),
        Step::Terminate,
        "an open gate past the bound still restarts"
    );

    // After the void the answer no longer counts: the re-ask at the next idle point.
    assert_eq!(
        next_step(&asked, &idle_facts(), false, bound),
        Step::Announce
    );
    assert_eq!(next_step(&asked, &boxed, false, bound), Step::Wait("box"));
    let tired = Phase::Announced {
        at_s: 100,
        asks: MAX_ASKS,
    };
    assert_eq!(next_step(&tired, &boxed, true, bound), Step::Void("box"));
    assert_eq!(next_step(&tired, &boxed, false, bound), Step::GiveUp);
}

/// Audit F8 (2026-09-24): ONE SAMPLE OF A BOX IS NOT A PERSON'S HOLD. The
/// supervisor answers or declines a box it can read within seconds, and
/// Claude's status reads `waiting` while it does; the drain voided the READY
/// answer on the first sweep past [`DRAIN_S`] that happened to catch one, and
/// typed the announcement again. A hold voids only once it has stood for
/// [`HOLD_S`], sample after sample (the negative control: the same box, held
/// that long).
#[test]
fn a_box_one_sweep_catches_is_not_a_persons_hold() {
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    let late = 100 + 31 * 60;
    for edit in [
        (|f: &mut Facts| f.approval_box = true) as fn(&mut Facts),
        |f| f.status = "waiting".into(),
        |f| f.composer_empty = false,
    ] {
        let mut f = idle_facts();
        f.busy_footer = true;
        edit(&mut f);
        let word = person_hold(&f).expect("a hold");
        for held in [0, 60, 5 * 60 - 1] {
            f.hold_s = held;
            let step = next_step(&asked, &f, true, late);
            assert!(matches!(step, Step::Wait(_)), "{word} {held}: {step:?}");
        }
        f.hold_s = 5 * 60;
        assert_eq!(next_step(&asked, &f, true, late), Step::Void(word));
    }
    // The dwell the driver measures: from the first sample of a hold, every
    // sample since showing one; a sample without one starts it over.
    let mut boxed = idle_facts();
    boxed.approval_box = true;
    assert_eq!(hold_since((0, 0), &boxed, 500), (500, 500));
    assert_eq!(hold_since((500, 500), &boxed, 560), (500, 560));
    assert_eq!(hold_since((500, 560), &idle_facts(), 620), (0, 0));
    assert_eq!(hold_since((0, 0), &boxed, 680), (680, 680));
}

/// The review of 2026-09-25 (F8): A HOLD STANDS ONLY WHILE IT IS SAMPLED. The
/// dwell was wall time from the first sample, kept across any gap: two sweeps
/// two hours apart — either side of a system sleep, a driver restart — that
/// each caught a box (two boxes, each answered in seconds, for all anyone saw)
/// voided the READY answer and spent one of [`MAX_ASKS`]. A gap longer than
/// [`HOLD_GAP_S`] now begins the hold again, so those two samples are a wait;
/// NEGATIVE CONTROL: a box sampled once a minute for [`HOLD_S`] still voids
/// it, and a gap of exactly [`HOLD_GAP_S`] carries the hold on.
#[test]
fn a_hold_stands_only_while_it_is_sampled() {
    let asked = Phase::Announced { at_s: 0, asks: 1 };
    let mut boxed = idle_facts();
    boxed.approval_box = true;
    // The driver's sampling: `hold_since`, then `hold_s` from its start.
    let sample = |prior: (u64, u64), f: &Facts, now: u64| {
        let held = hold_since(prior, f, now);
        let mut f = f.clone();
        f.hold_s = if held.0 == 0 { 0 } else { now - held.0 };
        (held, next_step(&asked, &f, true, now))
    };
    // Two sweeps two hours apart, past the drain's bound.
    let (held, _) = sample((0, 0), &boxed, DRAIN_S);
    let (held, step) = sample(held, &boxed, DRAIN_S + 7200);
    assert_eq!(held, (DRAIN_S + 7200, DRAIN_S + 7200), "begun again");
    assert_eq!(
        step,
        Step::Wait("box"),
        "a box two samples caught is no hold"
    );
    // Sampled once a minute: it stands, and past HOLD_S it voids.
    let mut held = (0, 0);
    let mut step = Step::Wait("x");
    for k in 0..=HOLD_S / 60 {
        (held, step) = sample(held, &boxed, DRAIN_S + 60 * k);
    }
    assert_eq!(held.0, DRAIN_S, "{held:?}");
    assert_eq!(step, Step::Void("box"));
    // The gap's edge: exactly HOLD_GAP_S carries it on, one second more does not.
    assert_eq!(
        hold_since((500, 600), &boxed, 600 + HOLD_GAP_S),
        (500, 600 + HOLD_GAP_S)
    );
    assert_eq!(
        hold_since((500, 600), &boxed, 601 + HOLD_GAP_S),
        (601 + HOLD_GAP_S, 601 + HOLD_GAP_S)
    );
}

/// The drain model (`harness_upgrade_drain_bound_model`) folds the drain and
/// the re-ask clocks into ONE `Bound`, and the bind below samples only
/// [`DRAIN_S`]. Were the two to differ, the reducer would re-ask inside
/// `[REASK_S, DRAIN_S)` where the model allows only a wait, and no sample here
/// would see it. The equality is the model's assumption, pinned where the
/// build reads it.
const _: () = assert!(DRAIN_S == REASK_S);

/// TIER-1: on every state of the derived drain model
/// (`harness_upgrade_drain_bound_model`), the real reducer takes the step the
/// model's guards allow. The mapping:
/// - A person is a box that has stood [`HOLD_S`] (one a sweep has only just
///   caught is not yet a person's: `a_box_one_sweep_catches_is_not_a_persons_hold`).
/// - The agent's own work is a background shell.
/// - A break is a step taken at one ([`Facts::background_point`], Claude's
///   status `busy`).
/// - A lagging status is Claude's own `busy` standing [`QUIET_S`] over a
///   screen read idle [`IDLE_LOOKS`] looks in a row ([`status_lags`]).
/// - The model's `waited` looks are the notice's age, and its `aged` looks
///   are the READY answer's age ([`Facts::ready_s`]), each in `Bound`
///   steps of [`DRAIN_S`] (== [`REASK_S`]).
/// - Its `MaxAsks` notices are [`MAX_ASKS`], and a notice typed is `ReAsk`.
///
/// An answer is never older than its notice. Negative controls: at `Buggy=1`
/// (no void, a wait in silence on the agent's work, and the notice's clock
/// for a READY answer), at `LagEnds=1` (the restart on a lagging status, the
/// review of 2026-09-27) and at `LagHolds=1` (a READY behind the lag waited
/// on in silence) the model disagrees with the reducer.
#[test]
fn the_reducer_takes_the_step_the_drain_model_allows() {
    let model = aterm_spec::derive::harness_upgrade_drain_bound_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let lag_ends = aterm_spec::interp::with_consts(&model, &[("LagEnds", 1)]);
    let lag_holds = aterm_spec::interp::with_consts(&model, &[("LagHolds", 1)]);
    let (mut ends_caught, mut holds_caught) = (0, 0);
    let bound: i64 = 2;
    let max_asks: i64 = 2;
    let at = |looks: i64| DRAIN_S * u64::try_from(looks).unwrap() / u64::try_from(bound).unwrap();
    let mut disagreements = 0;
    let mut steps = std::collections::BTreeSet::new();
    for waited in 0..=bound {
        for asks in 1..=max_asks {
            for ready in [0, 1] {
                // The answer's age matters only while there is one.
                let ages = if ready == 1 { 0..=waited } else { 0..=0 };
                for aged in ages {
                    for person in [0, 1] {
                        for agent in [0, 1] {
                            for (brk, lag) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                                let mut state = model.init_state();
                                for (var, value) in [
                                    ("waited", waited),
                                    ("aged", aged),
                                    ("asks", asks),
                                    ("ready", ready),
                                    ("person", person),
                                    ("agent", agent),
                                    ("brk", brk),
                                    ("lag", lag),
                                ] {
                                    state.insert(var, value);
                                }
                                let mut f = if brk == 1 {
                                    Facts {
                                        status: "busy".to_string(),
                                        status_age_s: 0,
                                        quiet_s: 0,
                                        background_point: true,
                                        ..idle_facts()
                                    }
                                } else {
                                    idle_facts()
                                };
                                if lag == 1 {
                                    f.status = "busy".to_string();
                                    f.status_age_s = QUIET_S;
                                    f.idle_looks = IDLE_LOOKS;
                                }
                                f.approval_box = person == 1;
                                f.hold_s = if person == 1 { HOLD_S } else { 0 };
                                if agent == 1 {
                                    f.background = vec!["zsh".to_string()];
                                }
                                f.ready_s = if ready == 1 { at(aged) } else { 0 };
                                let real_asks = if asks == max_asks { MAX_ASKS } else { 1 };
                                let asked = Phase::Announced {
                                    at_s: 100,
                                    asks: real_asks,
                                };
                                let step = next_step(&asked, &f, ready == 1, 100 + at(waited));
                                steps.insert(match step {
                                    Step::Terminate => "terminate",
                                    Step::Void(_) => "void",
                                    Step::Announce => "re-ask",
                                    Step::GiveUp => "give-up",
                                    Step::Wait(_) => "wait",
                                    Step::Fresh => "fresh",
                                    Step::Rearm => "rearm",
                                });
                                let decided = |m: &aterm_spec::derive::Model| {
                                    [
                                        m.action_enabled("Terminate", &state),
                                        m.action_enabled("Void", &state),
                                        m.action_enabled("ReAsk", &state),
                                        m.action_enabled("GiveUp", &state),
                                        m.action_enabled("Wait", &state),
                                    ]
                                };
                                let real = [
                                    step == Step::Terminate,
                                    matches!(step, Step::Void(_)),
                                    step == Step::Announce,
                                    step == Step::GiveUp,
                                    matches!(step, Step::Wait(_)),
                                ];
                                assert_eq!(real, decided(&model), "{state:?} -> {step:?}");
                                disagreements += usize::from(real != decided(&buggy));
                                ends_caught += usize::from(real != decided(&lag_ends));
                                holds_caught += usize::from(real != decided(&lag_holds));
                            }
                        }
                    }
                }
            }
        }
    }
    // Every step the reducer can take was driven: the bind is not vacuous.
    assert_eq!(steps.len(), 5, "{steps:?}");
    assert!(disagreements > 0, "the unbounded drain is caught");
    assert!(ends_caught > 0, "a restart on a lagging status is caught");
    assert!(holds_caught > 0, "a silent wait behind the lag is caught");
}

/// A BREAK ENDS NOTHING ([`break_step`]): a notice, a void, a give-up and a
/// wait pass through it; the one step that ends the agent becomes a wait on
/// its work. Both drivers apply this rule. Before 2026-09-26 each carried its
/// own hand-written list, which let only a notice and a wait through: a
/// give-up and a void at a break were silently turned into waits.
#[test]
fn a_break_passes_every_step_but_the_end() {
    for step in [
        Step::Announce,
        Step::GiveUp,
        Step::Void("draft"),
        Step::Wait("awaiting-ready"),
    ] {
        assert_eq!(break_step(step), step);
    }
    assert_eq!(break_step(Step::Terminate), Step::Wait("background"));
}

/// A READY ANSWER GETS ITS OWN CLOCK (review of 2026-09-26). The last notice
/// goes out at T. The agent winds down for 35 minutes, as the notice asks,
/// and answers READY while a shell of its own is still exiting. The notice's
/// clock alone would give up at the first look, seconds after the answer.
/// The answer's own clock waits a whole [`REASK_S`] first, at a break and at
/// an idle point alike. If the shell ends inside it, the agent is restarted.
#[test]
fn a_late_ready_answer_gets_its_own_reask_interval() {
    let last = Phase::Announced {
        at_s: 0,
        asks: MAX_ASKS,
    };
    let late = 35 * 60;
    let mut idle = idle_facts();
    idle.background = vec!["zsh".to_string()];
    let mut brk = Facts {
        status: "shell".to_string(),
        status_age_s: 0,
        quiet_s: 0,
        background_point: true,
        ..idle.clone()
    };
    for f in [&mut idle, &mut brk] {
        f.ready_s = 10;
        assert_eq!(next_step(&last, f, true, late), Step::Wait("background"));
        f.ready_s = REASK_S;
        assert_eq!(next_step(&last, f, true, late + REASK_S), Step::GiveUp);
        // NEGATIVE CONTROL: an answer as old as its notice — the notice's
        // clock alone — gives up at once.
        f.ready_s = late;
        assert_eq!(next_step(&last, f, true, late), Step::GiveUp);
    }
    // The shell ended inside the answer's interval: the restart goes.
    let mut done = idle_facts();
    done.ready_s = 60;
    assert_eq!(next_step(&last, &done, true, late + 60), Step::Terminate);
}

/// A PERSON AT A BUSY TAB IS ASKED FIRST (review of 2026-09-25): the gate
/// asked the person only after Claude's status, so a busy attended tab waited
/// `not-idle` — a wait that names the turn, not the person holding the tab —
/// and the upgrade took the tab's turn ends for a notice its idle point then
/// refused (`attended`). A person at the tab now waits `attended` busy or
/// idle, before the notice and before the restart (a wait the session's
/// worker owns no turn end for). NEGATIVE CONTROLS: the same busy visit
/// unattended waits `not-idle`; under the owner's `--now` the person is
/// waived, and the busy tab waits on its turn alone.
#[test]
fn a_person_at_a_busy_tab_waits_attended() {
    let busy = |attended: bool| Facts {
        status: "busy".to_string(),
        status_age_s: 0,
        busy_footer: true,
        quiet_s: 0,
        attended,
        ..idle_facts()
    };
    let asked = Phase::Announced { at_s: 0, asks: 1 };
    for (phase, ready) in [
        (Phase::Pending, false),
        (asked.clone(), true),
        (asked, false),
    ] {
        let now = REASK_S + 10;
        let step = requested_step(&Request::None, &phase, &busy(true), ready, now, "9.9.9");
        assert_eq!(step, Step::Wait("attended"), "{phase:?} ready={ready}");
        // Negative control: nobody at it — the turn's wait.
        let step = requested_step(&Request::None, &phase, &busy(false), ready, now, "9.9.9");
        assert_eq!(step, Step::Wait("not-idle"), "{phase:?} ready={ready}");
        // The owner's word waives the person: the busy turn is all it waits on.
        let step = requested_step(&Request::Now, &phase, &busy(true), ready, now, "9.9.9");
        assert_eq!(step, Step::Wait("not-idle"), "--now {phase:?}");
    }
}

/// ONE PRESENCE FACT (review of 2026-09-25): a session is attended when a
/// person gave IT input within `[harness] human_grace_s`, by the server's own
/// per-session stamp — never by where the tab sits or the machine's input
/// clock. A host that sends no stamp proves no absence: attended.
#[test]
fn a_session_is_attended_by_its_own_person_stamp() {
    use crate::supervise::screen::HumanInput;
    let grace = crate::supervise::SupervisorConfig::default().human_grace_s;
    let limit = u64::from(grace) * 1000;
    assert!(attended_by(HumanInput::Ago(0), grace));
    assert!(attended_by(HumanInput::Ago(limit - 1), grace));
    assert!(
        !attended_by(HumanInput::Ago(limit), grace),
        "the grace gone"
    );
    assert!(
        attended_by(HumanInput::Ago(limit), grace + 1),
        "a longer grace holds it"
    );
    assert!(
        !attended_by(HumanInput::Never, grace),
        "nobody ever keyed it"
    );
    assert!(attended_by(HumanInput::Unknown, grace), "fails closed");
}

/// A PERSON AT THE TAB (the 2026-09-24 review; [`Facts::attended`]): nothing
/// the upgrade decides for itself is typed into it, and — READY or not — it
/// is never signalled there (review of 2026-09-25): the SIGTERM is the one act
/// that cannot be taken back. Only the owner's own `--now` waives it
/// ([`Facts::owner_now`]), at the notice and at the signal alike. NEGATIVE
/// CONTROLS: the same READY facts unattended are ended, and an attended tab
/// is still refused a draft, a box and a busy Claude under `--now`.
#[test]
fn an_attended_tab_is_neither_typed_into_nor_signalled_without_the_owners_now() {
    let attended = Facts {
        attended: true,
        ..idle_facts()
    };
    assert_eq!(
        gate_announce(&attended),
        Gate::Wait("attended"),
        "not typed"
    );
    assert_eq!(
        gate_restart(&attended, true),
        Gate::Wait("attended"),
        "never signalled"
    );
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    assert_eq!(
        requested_step(
            &Request::None,
            &Phase::Pending,
            &attended,
            false,
            200,
            "9.9.9"
        ),
        Step::Wait("attended")
    );
    assert_eq!(
        requested_step(&Request::None, &asked, &attended, true, 200, "9.9.9"),
        Step::Wait("attended")
    );
    assert_eq!(
        requested_step(&Request::Now, &asked, &attended, true, 200, "9.9.9"),
        Step::Terminate,
        "the owner's word"
    );
    assert_eq!(
        requested_step(
            &Request::Now,
            &Phase::Pending,
            &attended,
            false,
            200,
            "9.9.9"
        ),
        Step::Announce
    );
    // Negative controls.
    assert_eq!(gate_restart(&idle_facts(), true), Gate::Go, "unattended");
    let knocks: [Knock; 3] = [
        (|f| f.composer_empty = false, "draft"),
        (|f| f.approval_box = true, "box"),
        (|f| f.status = "busy".into(), "not-idle"),
    ];
    for (edit, word) in knocks {
        let mut f = attended.clone();
        edit(&mut f);
        assert_eq!(
            requested_step(&Request::Now, &Phase::Pending, &f, false, 200, "9.9.9"),
            Step::Wait(word),
            "--now, yet {word}"
        );
    }
}

/// THE READY ANSWER IS THE AGENT'S LAST WORD, or none (review of
/// 2026-09-25): a READY row the agent has since answered past — a `keep
/// going` it took, a person's message — is no consent to a restart, however
/// long it stays in the tail. NEGATIVE CONTROLS: a subagent's row and one
/// Claude Code writes itself (`<synthetic>`) after the answer supersede
/// nothing.
#[test]
fn a_ready_answer_counts_only_while_it_is_the_agents_last_word() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let said = |text: &str, extra: &str| {
        format!(
            r#"{{"type":"assistant"{extra},"message":{{"model":"claude-opus-5-5","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    };
    let ready = said(&format!("Saved.\\n{marker}"), "");
    let user = r#"{"type":"user","message":{"role":"user","content":"keep going"}}"#;
    assert!(transcript_has_ready(&ready, marker));
    let past = [
        ready.clone(),
        user.to_string(),
        said("Started the schema migration; step 2 of 5 next.", ""),
    ]
    .join("\n");
    assert!(!transcript_has_ready(&past, marker), "answered past");
    // A tool call after the answer is work after it too.
    let tool = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#;
    assert!(!transcript_has_ready(
        &[ready.clone(), tool.to_string()].join("\n"),
        marker
    ));
    // Negative controls.
    let sidechain = said("Subagent: done.", r#","isSidechain":true"#);
    let synthetic = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
    assert!(transcript_has_ready(
        &[ready.clone(), sidechain, synthetic.to_string()].join("\n"),
        marker
    ));
    // Answered again after the `keep going`: READY once more.
    assert!(transcript_has_ready(&[past, ready].join("\n"), marker));
}

/// A NOTICE ENDS EVERY READY BEFORE IT (review of 2026-09-25): a READY the
/// agent gave to an earlier notice — one the owner's hold ended — was still
/// its last assistant word after the notice typed once the hold lifted, until
/// it wrote a real assistant row; an API error Claude Code records as a
/// `<synthetic>` row, or a person's Esc (a USER row), left it standing, and
/// the next visit signalled an agent that never answered the new notice. A
/// main-chain user row that IS an announcement ends it. NEGATIVE CONTROLS: a
/// READY after the notice answers it; a user row that only mentions the head
/// mid-sentence is no notice (a person's direction), and a subagent's notice
/// ends nothing.
#[test]
fn a_ready_before_the_latest_notice_is_no_answer_to_it() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let ready = format!(
        r#"{{"type":"assistant","message":{{"model":"claude-opus-5-5","content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    );
    let prompt = prepare_prompt(
        &v("2.1.281"),
        &v("2.1.282"),
        Source::Native,
        "ATERM-UPGRADE-READY-feedface",
    );
    let as_text = aterm_json::to_string(&prompt).expect("json");
    let notice = format!(r#"{{"type":"user","message":{{"role":"user","content":{as_text}}}}}"#);
    let notice_parts = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":{as_text}}}]}}}}"#
    );
    let synthetic = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 Overloaded"}]}}"#;
    let esc = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
    for after in [&notice, &notice_parts] {
        assert!(
            !transcript_has_ready(
                &[ready.as_str(), after, synthetic].join(
                    "
"
                ),
                marker
            ),
            "an API error after the new notice"
        );
        assert!(
            !transcript_has_ready(
                &[ready.as_str(), after, esc].join(
                    "
"
                ),
                marker
            ),
            "a person's Esc after the new notice"
        );
        // Negative control: READY after the notice answers it.
        assert!(transcript_has_ready(
            &[after.as_str(), &ready].join(
                "
"
            ),
            marker
        ));
    }
    // Negative controls: a mention mid-sentence, and a subagent's notice.
    // The mention is a person's DIRECTION (it ends the READY as one, the
    // second review of 2026-09-26), never a notice: a notice ends a direction
    // the agent took up before it, the mention does not.
    let mention = r#"{"type":"user","message":{"role":"user","content":"why did [aterm harness] Claude Code ask that?"}}"#;
    assert!(!transcript_has_ready(
        &[ready.as_str(), mention].join("\n"),
        marker
    ));
    let taken = r#"{"type":"user","message":{"role":"user","content":"switch to the parser"}}"#;
    let answer = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Switching."}]}}"#;
    assert!(directed_since_ready(
        &[taken, answer, mention].join("\n"),
        &[]
    ));
    assert!(!directed_since_ready(
        &[taken, answer, notice.as_str()].join("\n"),
        &[]
    ));
    let side = notice.replacen(
        r#"{"type":"user","#,
        r#"{"type":"user","isSidechain":true,"#,
        1,
    );
    assert!(transcript_has_ready(
        &[ready.as_str(), &side].join(
            "
"
        ),
        marker
    ));
}

/// A DIRECTION AFTER THE READY ENDS IT (the second review of 2026-09-26): a
/// main-chain user row that directs the conversation — a person's Esc, a
/// person's message met only by a `<synthetic>` row (an API error, the usage
/// limit), a peer's message — ends the READY before it, whether or not the
/// agent has written a real row since; kept as the last assistant word, it
/// let an upgrade that had given up asking restart the session over the
/// person who had just spoken. NEGATIVE CONTROLS: the rows Claude Code
/// writes on its own after the answer (the limit's reset, a tool's result, a
/// task notification, a command's output, the harness's own line, a
/// subagent's prompt) end nothing, and a READY given again after the
/// direction answers.
#[test]
fn a_direction_after_the_ready_ends_it() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let ready = format!(
        r#"{{"type":"assistant","message":{{"model":"claude-opus-5-5","content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    );
    let user = |extra: &str, content: &str| {
        format!(r#"{{"type":"user"{extra},"message":{{"role":"user","content":{content}}}}}"#)
    };
    let synthetic = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 Overloaded"}]}}"#;
    let esc = user(
        "",
        r#"[{"type":"text","text":"[Request interrupted by user]"}]"#,
    );
    let asked = user("", r#""hold on, first fix the failing test""#);
    let peer = user("", r#""[from s-d3346b29] v0.91.0 is out""#);
    let rows = |after: &[&str]| {
        std::iter::once(ready.as_str())
            .chain(after.iter().copied())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(transcript_has_ready(&rows(&[]), marker));
    assert!(!transcript_has_ready(&rows(&[&esc]), marker), "an Esc");
    assert!(
        !transcript_has_ready(&rows(&[&asked, synthetic]), marker),
        "a message the agent met only with a <synthetic> row"
    );
    assert!(!transcript_has_ready(&rows(&[&peer]), marker), "a peer's");
    // Negative controls: the rows Claude Code writes itself, the harness's
    // own line and a subagent's prompt end nothing.
    let release = aterm_json::to_string(&Value::from(release_prompt(Agent::Claude))).expect("json");
    for quiet in [
        user(
            r#","isMeta":true"#,
            r#""Your claude.ai usage limit has reset. Continue the task you were working on""#,
        ),
        user(
            "",
            r#"[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]"#,
        ),
        user("", r#""<task-notification>\ndone""#),
        user("", r#""<local-command-stdout>continuing at 6am""#),
        user("", &release),
        user(r#","isSidechain":true"#, r#""do the thing""#),
    ] {
        assert!(
            transcript_has_ready(&rows(&[&quiet, synthetic]), marker),
            "{quiet}"
        );
    }
    // Answered again after the direction: READY once more.
    assert!(transcript_has_ready(&rows(&[&esc, &ready]), marker));
}

/// A READY ANSWER IS A LINE THAT IS THE MARKER ALONE ([`is_ready_line`],
/// review of 2026-09-25): words that only QUOTE a marker are no answer.
/// NEGATIVE CONTROLS: the marker on a line of its own, in backticks or
/// indented, is one; another marker, or one cut short, is not.
#[test]
fn a_ready_answer_is_the_marker_on_a_line_of_its_own() {
    let marker = ready_marker(ID, &v("2.1.282"), 3);
    assert!(is_ready_line(&marker, &marker));
    assert!(is_ready_line(&format!("  `{marker}`"), &marker));
    assert!(!is_ready_line(
        &format!("I'll reply with {marker} once the build is done."),
        &marker
    ));
    assert!(!is_ready_line(&ready_marker(ID, &v("2.1.282"), 4), &marker));
    assert!(!is_ready_line(&marker[..marker.len() - 1], &marker));
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
         left off. If you were waiting on the user, nobody is here to answer: decide for \
         yourself, prefer reversible steps, and keep going."
    );
    assert!(!said.starts_with(ANNOUNCE_HEAD), "never the announcement");
    // No model known: the clause is left out, not guessed.
    let bare = continue_prompt(&from, &to, None);
    assert_eq!(
        bare,
        "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.282 (from \
         2.1.281) and resumed. Continue where you left off. If you were waiting on the user, \
         nobody is here to answer: decide for yourself, prefer reversible steps, and keep going."
    );
    // Both continuations end on the same instruction, and it never invites a
    // "waiting on the user" report (owner, 2026-09-25).
    let with_model = continue_prompt_with_model(&from, &to, None, Some("claude-opus-5-5"));
    for text in [&said, &bare, &with_model] {
        assert!(text.ends_with(CARRY_ON), "{text}");
        assert!(!text.contains("say so in one line"), "{text}");
    }
    // No model before is known: the reason is not guessed either.
    assert!(
        with_model.contains(
            "and resumed; it now runs claude-opus-5-5, the model aterm's upgrade \
                             chose for it"
        ),
        "{with_model}"
    );
}

/// THE INCIDENT'S NOTICE, 2026-09-25 (2.1.282 -> 2.1.283): the words the
/// session got said only what it ran BEFORE — true, and silent on the fact
/// that it came back on the same old model. A restart that moves the model
/// says both, the model after being the one its relaunch line asked for.
#[test]
fn a_model_move_says_what_ran_before_and_what_runs_now() {
    let (from, to) = (v("2.1.282"), v("2.1.283"));
    // NEGATIVE CONTROL: the notice the incident typed, unchanged for a
    // restart that moves no model.
    assert_eq!(
        continue_prompt_with_model(&from, &to, Some("claude-opus-5"), None),
        format!(
            "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283 (from \
             2.1.282); it ran claude-opus-5 before the restart and was resumed. {CARRY_ON}"
        )
    );
    assert_eq!(
        continue_prompt_with_model(&from, &to, Some("claude-opus-5"), Some("claude-opus-5-5")),
        format!(
            "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283 (from \
             2.1.282) and resumed; it ran claude-opus-5 before the restart and now runs \
             claude-opus-5-5, the newest model of its family (for this session only: your \
             default model is unchanged). {CARRY_ON}"
        )
    );
    // The 1M window, named as it was asked for.
    assert!(
        continue_prompt_with_model(
            &from,
            &to,
            Some("claude-opus-5"),
            Some("claude-opus-5-5[1m]")
        )
        .contains("now runs claude-opus-5-5[1m], the newest model of its family")
    );
    // A move ACROSS families is the priority list's (owner, 2026-09-27: only
    // with nothing newer of its own family), and is said so.
    assert_eq!(
        continue_prompt_with_model(
            &from,
            &to,
            Some("claude-fable-5-1"),
            Some("claude-opus-5-5")
        ),
        format!(
            "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283 (from \
             2.1.282) and resumed; it ran claude-fable-5-1 before the restart and now runs \
             claude-opus-5-5, the best available model on aterm's priority list, nothing newer \
             of its own family being on offer (for this session only: your default model is \
             unchanged). {CARRY_ON}"
        )
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

// ---------------------------------------------------------------- the owner's word

const TARGET: &str = "2.1.282";

/// `--now` waives the settling window (and, the owner being the person at the
/// tab, the attended-tab guard) and NOTHING else: a draft, a box, the
/// busy footer, a non-idle status and a hold still wait, and a restart still
/// needs the READY answer and an empty process tree. The negative control is
/// the same reading with no request, which waits `settling`.
#[test]
fn a_now_request_waives_settling_and_never_the_draft_or_the_ready_answer() {
    let mut settling = idle_facts();
    settling.quiet_s = 3;
    settling.status_age_s = 3;
    let step = |r: &Request, p: &Phase, f: &Facts, ready: bool| {
        requested_step(r, p, f, ready, 100, TARGET)
    };
    assert_eq!(
        step(&Request::None, &Phase::Pending, &settling, false),
        Step::Wait("settling"),
        "no request: the settling window stands"
    );
    assert_eq!(
        step(&Request::Now, &Phase::Pending, &settling, false),
        Step::Announce
    );
    let guards: [Knock; 5] = [
        (|f| f.held = true, "held"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
    ];
    for (edit, word) in guards {
        let mut f = settling.clone();
        edit(&mut f);
        assert_eq!(
            step(&Request::Now, &Phase::Pending, &f, false),
            Step::Wait(word)
        );
    }
    // The owner's word is that person asking: an attended tab is moved too.
    let mut attended = settling.clone();
    attended.attended = true;
    assert_eq!(
        step(&Request::None, &Phase::Pending, &attended, false),
        Step::Wait("attended"),
        "no request: a person at the tab holds the upgrade's own judgment back"
    );
    assert_eq!(
        step(&Request::Now, &Phase::Pending, &attended, false),
        Step::Announce
    );
    // Announced, no READY: no restart.
    let asked = Phase::Announced { at_s: 90, asks: 1 };
    assert_eq!(
        step(&Request::Now, &asked, &settling, false),
        Step::Wait("awaiting-ready"),
        "no READY, no restart"
    );
    assert_eq!(
        step(&Request::Now, &asked, &settling, true),
        Step::Terminate
    );
    let mut working = settling.clone();
    working.background = vec!["zsh".to_string()];
    assert_eq!(
        step(&Request::Now, &asked, &working, true),
        Step::Wait("background")
    );
    assert_eq!(
        step(&Request::None, &asked, &settling, true),
        Step::Wait("settling"),
        "negative control at the restart gate"
    );
}

/// A deferral holds a session until it runs out, and a skip holds only the
/// version it names; neither ever holds a restart already in flight.
#[test]
fn a_deferral_holds_until_it_runs_out_and_a_skip_holds_only_its_version() {
    let f = idle_facts();
    let defer = Request::DeferUntil(1_000);
    let asked = Phase::Announced { at_s: 10, asks: 1 };
    assert_eq!(
        requested_step(&defer, &Phase::Pending, &f, false, 999, TARGET),
        Step::Wait("deferred")
    );
    assert_eq!(
        requested_step(&defer, &asked, &f, true, 999, TARGET),
        Step::Wait("deferred"),
        "a READY session is not ended while deferred"
    );
    assert_eq!(
        requested_step(&defer, &Phase::Pending, &f, false, 1_000, TARGET),
        Step::Announce,
        "run out: the gates alone decide"
    );
    let in_flight = Phase::Exiting { at_s: 1 };
    assert_eq!(
        requested_step(&defer, &in_flight, &f, true, 5, TARGET),
        Step::Wait("in-flight")
    );
    let skip = Request::Skip(TARGET.to_string());
    assert_eq!(
        requested_step(&skip, &Phase::Pending, &f, false, 5, TARGET),
        Step::Wait("skipped")
    );
    assert_eq!(
        requested_step(&skip, &Phase::Pending, &f, false, 5, "2.1.283"),
        Step::Announce,
        "a newer build is not the skipped one"
    );
    assert_eq!(
        requested_step(&skip, &Phase::Relaunched { at_s: 1 }, &f, true, 5, TARGET),
        Step::Wait("in-flight")
    );
}

#[test]
fn a_family_move_is_said_as_the_newest_of_its_family_and_a_miss_as_not_taken() {
    assert_eq!(
        restart_outcome_listed(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5-5"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (the newest of \
         its family; /model changes it)"
    );
    assert_eq!(
        restart_outcome_listed(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 (the relaunch asked for \
         claude-opus-5-5, the newest of its family; it was not taken)"
    );
    // The 1M window is the same model: asked with the tag, answered without.
    assert_eq!(
        restart_outcome_listed(
            "2.1.283",
            Some("claude-opus-5"),
            Some("claude-opus-5-5"),
            "claude-opus-5-5[1m]"
        ),
        "claude restarted on 2.1.283 · model claude-opus-5 -> claude-opus-5-5 (the newest of \
         its family; /model changes it)"
    );
    assert_eq!(
        restart_outcome_listed("2.1.282", None, None, "claude-opus-5-5"),
        "claude restarted on 2.1.282 · model unconfirmed (asked for claude-opus-5-5)"
    );
    // A move across families is the priority list's, taken or not.
    assert_eq!(
        restart_outcome_listed(
            "2.1.283",
            Some("claude-fable-5-1"),
            Some("claude-opus-5-5"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.283 · model claude-fable-5-1 -> claude-opus-5-5 (the priority \
         list chose it; /model changes it)"
    );
    assert_eq!(
        restart_outcome_listed(
            "2.1.283",
            Some("claude-fable-5-1"),
            Some("claude-fable-5-1"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.283 · model claude-fable-5-1 (the priority list asked for \
         claude-opus-5-5; it was not taken)"
    );
}

#[test]
fn the_request_word_round_trips_and_an_unknown_word_asks_for_nothing() {
    for r in [
        Request::None,
        Request::Now,
        Request::DeferUntil(1_790_311_076),
        Request::Skip("2.1.282".to_string()),
    ] {
        assert_eq!(Request::parse(&r.word()), Some(r.clone()), "{r:?}");
    }
    assert_eq!(Request::parse(""), Some(Request::None));
    for bad in ["later", "defer-until:soon", "skip:", "skip:2.1.x", "NOW"] {
        assert_eq!(Request::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn the_wait_word_names_claudes_status_only_for_not_idle() {
    assert_eq!(wait_word("not-idle", "busy"), "not-idle:busy");
    assert_eq!(wait_word("not-idle", "shell"), "not-idle:shell");
    assert_eq!(wait_word("not-idle", "waiting"), "not-idle:waiting");
    assert_eq!(wait_word("draft", "busy"), "draft", "another gate's word");
    assert_eq!(
        wait_word("not-idle", "Busy; rm\u{1b}[2J"),
        "not-idle:usyrm",
        "a third party's word is cut to [a-z-]"
    );
    assert_eq!(wait_word("not-idle", ""), "not-idle");
}

#[test]
fn a_span_reads_as_its_two_largest_units() {
    assert_eq!(span(0), "0s");
    assert_eq!(span(45), "45s");
    assert_eq!(span(60), "1m");
    assert_eq!(span(12 * 60 + 5), "12m");
    assert_eq!(span(3_600), "1h");
    assert_eq!(span(8 * 3_600 + 22 * 60 + 30), "8h22m");
    assert_eq!(span(86_400), "1d");
    assert_eq!(span(3 * 86_400 + 4 * 3_600 + 59), "3d4h");
}

#[test]
fn a_model_restart_says_the_model_and_keeps_the_announce_head() {
    let marker = ready_marker(ID, &v("2.1.282"), 1);
    let only = prepare_prompt_with_model(
        &v("2.1.282"),
        &v("2.1.282"),
        Source::Managed,
        &marker,
        Some("claude-opus-5-5"),
        Some("claude-opus-5"),
    );
    assert!(only.starts_with(ANNOUNCE_HEAD), "{only}");
    assert!(only.contains("--model claude-opus-5-5") && only.contains(&marker));
    assert!(only.contains("do not cancel them"));
    assert!(
        only.contains("can run claude-opus-5-5, the newest model of its family, which this"),
        "{only}"
    );
    let both = prepare_prompt_with_model(
        &v("2.1.280"),
        &v("2.1.282"),
        Source::Managed,
        &marker,
        Some("claude-opus-5-5"),
        Some("claude-fable-5-1"),
    );
    assert!(both.starts_with(ANNOUNCE_HEAD) && both.contains("2.1.282 (managed) is installed"));
    assert!(
        both.contains("onto claude-opus-5-5 (the best available model on aterm's priority list"),
        "{both}"
    );
    assert_eq!(
        prepare_prompt_with_model(
            &v("2.1.280"),
            &v("2.1.282"),
            Source::Managed,
            &marker,
            None,
            Some("claude-opus-5")
        ),
        prepare_prompt(&v("2.1.280"), &v("2.1.282"), Source::Managed, &marker)
    );
    let cont = continue_prompt_with_model(
        &v("2.1.282"),
        &v("2.1.282"),
        Some("claude-opus-5"),
        Some("claude-opus-5-5"),
    );
    assert!(
        cont.contains("it ran claude-opus-5 before the restart and now runs claude-opus-5-5"),
        "{cont}"
    );
    assert!(cont.starts_with(
        "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.282 and resumed;"
    ));
    assert_eq!(
        continue_prompt_with_model(&v("2.1.280"), &v("2.1.282"), None, None),
        continue_prompt(&v("2.1.280"), &v("2.1.282"), None)
    );
}

/// THE NOTICE AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK (the owner's
/// answer of 2026-09-26: "Busy agentic sessions get upgraded at their next
/// natural break. The notice interrupts the agent's orchestration once, and
/// the restart still never kills running work"). Claude's own `busy` (a
/// workflow it waits on) or `shell` status is no wait there, and the loop's
/// settle stands for the screen's (a workflow's progress line never holds
/// still). The break takes only NOTICES: the first one, and a re-ask once a
/// whole [`REASK_S`] has passed with the work still running. Past
/// [`MAX_ASKS`] the upgrade gives up. READY's restart and a void are an idle
/// point's (`background`), and the restart's gate still asks for nothing
/// running under the agent. NEGATIVE CONTROLS: a person, a hold, a box, a
/// draft, a live turn's busy footer and a dialog's `waiting` still wait at
/// the break. The same `busy` session at an idle point's step waits
/// `not-idle`, as before.
#[test]
fn a_break_of_background_work_takes_only_notices_and_ends_nothing() {
    let at_break = |status: &str| Facts {
        status: status.to_string(),
        status_age_s: 0,
        quiet_s: 0,
        background_point: true,
        ..idle_facts()
    };
    for status in ["busy", "shell", "idle"] {
        assert_eq!(
            next_step(&Phase::Pending, &at_break(status), false, 1_000),
            Step::Announce,
            "{status}"
        );
    }
    let knocks: [Knock; 6] = [
        (|f| f.attended = true, "attended"),
        (|f| f.held = true, "held"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.status = "waiting".into(), "not-idle"),
    ];
    for (edit, word) in knocks {
        let mut f = at_break("busy");
        edit(&mut f);
        assert_eq!(
            next_step(&Phase::Pending, &f, false, 1_000),
            Step::Wait(word)
        );
    }
    // Within a re-ask interval, nothing but the wait: READY's restart waits
    // for an idle point, whatever the facts say.
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&announced, &at_break("busy"), false, REASK_S - 1),
        Step::Wait("background")
    );
    assert_eq!(
        next_step(&announced, &at_break("idle"), true, 10),
        Step::Wait("background")
    );
    // THE FOUR-DAY TAB (2026-09-26): a notice out, the agent's poll loops
    // running on, a break every minute. Once a whole REASK_S has passed, the
    // break asks again, READY or not. That is never an end, and past MAX_ASKS
    // it is a give-up. Before this fix it answered `background` for good. A
    // READY answer given with the notice has stood the same whole REASK_S.
    for ready in [false, true] {
        let brk = Facts {
            ready_s: if ready { REASK_S } else { 0 },
            ..at_break("shell")
        };
        assert_eq!(
            next_step(&announced, &brk, ready, REASK_S),
            Step::Announce,
            "ready={ready}"
        );
        let last = Phase::Announced {
            at_s: 0,
            asks: MAX_ASKS,
        };
        assert_eq!(
            next_step(&last, &brk, ready, REASK_S),
            Step::GiveUp,
            "ready={ready}"
        );
        // AT ITS LIMIT, a break asks nothing and gives up on nothing: the
        // notice would be queued unread.
        let limited = Facts {
            limited: true,
            ..brk.clone()
        };
        assert_eq!(
            next_step(&last, &limited, ready, REASK_S),
            Step::Wait("limited"),
            "ready={ready}"
        );
    }
    // A person's hand still holds the re-ask at a break.
    let mut drafted = at_break("busy");
    drafted.composer_empty = false;
    assert_eq!(
        next_step(&announced, &drafted, false, REASK_S),
        Step::Wait("draft")
    );
    // The same answer at an idle point restarts only with nothing running
    // under the agent.
    let idle_ready = |background: Vec<String>| Facts {
        background,
        ..idle_facts()
    };
    assert_eq!(
        next_step(&announced, &idle_ready(vec!["zsh".into()]), true, 10),
        Step::Wait("background")
    );
    assert_eq!(
        next_step(&announced, &idle_ready(Vec::new()), true, 10),
        Step::Terminate
    );
    // A READY answer that work under the agent outlives by a whole REASK_S
    // can no longer be acted on, so it is asked again (a new marker), never
    // waited on for good.
    assert_eq!(
        next_step(
            &announced,
            &Facts {
                ready_s: REASK_S,
                ..idle_ready(vec!["zsh".into()])
            },
            true,
            REASK_S
        ),
        Step::Announce
    );
    // NEGATIVE CONTROL: no break — the busy session waits, as before.
    let busy_at_idle_point = Facts {
        status: "busy".to_string(),
        ..idle_facts()
    };
    assert_eq!(
        next_step(&Phase::Pending, &busy_at_idle_point, false, 1_000),
        Step::Wait("not-idle")
    );
}

/// CLAUDE'S OWN STATUS IS READ AGAINST ITS SCREEN AND THE WORK UNDER IT
/// (2026-09-27). The gates took the session file's `status` as it stood,
/// and a break only off the screen's words: a build whose idle screen draws
/// no shell count, over shells that run under the agent with its status at
/// `shell`, waited `not-idle` at every idle point — no notice, so no re-ask
/// and no give-up, for as long as the shells ran; and a status Claude left
/// stale over an idle screen and an empty process tree waited the same.
/// Now, once the session's own reader has read the screen AUTHORITATIVELY
/// IDLE for [`IDLE_LOOKS`] looks with the status standing [`QUIET_S`]:
/// with work under the agent, the turn is over and that work is its own —
/// the notice goes, as at a break, then the re-ask and the give-up, and the
/// restart waits `background`; with NOTHING under it, the status is stale,
/// and the NOTICE and the RELEASE go — each types one line and ends nothing.
/// THE RESTART STILL WAITS FOR CLAUDE'S OWN `idle` (the Drain step's rule:
/// work inside the agent's own process — a background agent, a workflow — is
/// no process under it, and only that status says it runs): the READY it
/// holds is asked again once [`REASK_S`] has passed, given up on after
/// [`MAX_ASKS`], and a gave-up upgrade's late READY is void past
/// [`DRAIN_S`], so the agent is released, never left stopped. A conversation
/// with no task is not restarted afresh on it either, and the owner's `--now`
/// waives none of it. NEGATIVE CONTROLS: a live turn (a foreground Bash call)
/// never reads authoritatively idle, so it still waits `not-idle`; one look
/// short, or a status younger than [`QUIET_S`], waits `status-stale`, which
/// names it; a person's `waiting`, a box, a draft, the busy footer and the
/// settle still hold.
#[test]
fn claudes_status_is_read_against_its_idle_screen_and_the_work_under_it() {
    let own_work = Facts {
        status: "shell".to_string(),
        status_age_s: QUIET_S,
        background: vec!["zsh".to_string(), "zsh".to_string()],
        idle_looks: IDLE_LOOKS,
        ..idle_facts()
    };
    assert_eq!(gate_announce(&own_work), Gate::Go);
    assert_eq!(
        next_step(&Phase::Pending, &own_work, false, 1_000),
        Step::Announce
    );
    assert_eq!(gate_restart(&own_work, true), Gate::Wait("background"));
    let asked = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&asked, &own_work, false, 10),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(next_step(&asked, &own_work, false, REASK_S), Step::Announce);
    let last = Phase::Announced {
        at_s: 0,
        asks: MAX_ASKS,
    };
    assert_eq!(next_step(&last, &own_work, false, REASK_S), Step::GiveUp);
    assert_eq!(
        next_step(&asked, &own_work, true, 10),
        Step::Wait("background"),
        "never an end"
    );
    let busy = Facts {
        status: "busy".to_string(),
        ..own_work.clone()
    };
    assert_eq!(
        next_step(&Phase::Pending, &busy, false, 1_000),
        Step::Announce
    );
    // THE STALE STATUS: nothing under the agent. One line goes; nothing ends.
    let stale = Facts {
        background: Vec::new(),
        ..busy.clone()
    };
    assert_eq!(gate_announce(&stale), Gate::Go);
    assert_eq!(gate_release(&stale, false), Gate::Go);
    for status in ["busy", "shell"] {
        let stale = Facts {
            status: status.to_string(),
            ..stale.clone()
        };
        assert_eq!(gate_restart(&stale, true), Gate::Wait("status-stale"));
        assert_eq!(
            next_step(&asked, &stale, true, 10),
            Step::Wait("status-stale"),
            "{status}: never an end on a status that is not idle"
        );
        assert_eq!(
            requested_step(&Request::Now, &asked, &stale, true, 10, "9.9.9"),
            Step::Wait("status-stale"),
            "{status}: the owner's --now waives no status"
        );
        let taskless = Facts {
            taskless: true,
            ..stale.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &taskless, false, 1_000),
            Step::Wait("status-stale"),
            "{status}: nor a fresh restart"
        );
        // The READY it holds: asked again past REASK_S, given up on after
        // MAX_ASKS, and — given up — void past DRAIN_S, the agent released.
        let held = Facts {
            ready_s: REASK_S,
            ..stale.clone()
        };
        assert_eq!(next_step(&asked, &held, true, REASK_S), Step::Announce);
        assert_eq!(next_step(&last, &held, true, REASK_S), Step::GiveUp);
        let gave_up = Phase::Failed(GAVE_UP.to_string());
        assert_eq!(
            next_step(&gave_up, &stale, true, 10),
            Step::Wait("status-stale")
        );
        let drained = Facts {
            ready_s: DRAIN_S,
            ..stale.clone()
        };
        assert_eq!(
            next_step(&gave_up, &drained, true, DRAIN_S),
            Step::Void("status-stale")
        );
    }
    // NEGATIVE CONTROL: Claude's own `idle` over the same screen restarts.
    let idle = Facts {
        status: "idle".to_string(),
        ..stale.clone()
    };
    assert_eq!(next_step(&asked, &idle, true, 10), Step::Terminate);
    // NEGATIVE CONTROLS.
    let knocks: [Knock; 8] = [
        (|f| f.idle_looks = 0, "not-idle"),
        (|f| f.idle_looks = IDLE_LOOKS - 1, "status-stale"),
        (|f| f.status_age_s = QUIET_S - 1, "status-stale"),
        (|f| f.status = "waiting".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for base in [&own_work, &stale] {
        for (edit, word) in knocks {
            let mut f = base.clone();
            edit(&mut f);
            assert_eq!(gate_announce(&f), Gate::Wait(word), "{word}: {f:?}");
            assert_eq!(
                next_step(&Phase::Pending, &f, false, 1_000),
                Step::Wait(word)
            );
        }
    }
    // The wait that names it carries Claude's word beside it.
    assert_eq!(wait_word("status-stale", "busy"), "status-stale:busy");
}

/// A STOPPED UPGRADE TAKES ITS OWN STEP AT A BREAK, NOT A BARE `background`
/// — save the restart, which waits for an idle point (2026-09-27: two
/// widowed `tail -f` shells under a Claude Code whose status read `shell`
/// made every screen a break, and no idle point ever came). Every break after
/// the give-up answered `background` before the reducer looked at the phase,
/// so the release the give-up owed was never typed, and a late READY that
/// work outlived was never voided: the agent the upgrade asked was left
/// stopped for as long as the shells lived. Now a gave-up upgrade with no
/// READY waits `failed` at a break (the release it owes is its next act), and
/// one whose late READY that work outlived past the drain is VOIDED there,
/// owing the release. The release line goes wherever a notice may go, a
/// settled break included: it types one line and ends nothing. NEGATIVE
/// CONTROLS: the restart a READY would take still waits at a break
/// ([`gate_restart`], [`break_step`]); an announced upgrade still waits
/// `background` within its window; and a person, a draft, a box, a hold, the
/// limit and a live turn's footer still hold the release at a break.
#[test]
fn a_stopped_upgrade_takes_its_idle_points_step_at_a_break() {
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let brk = Facts {
        status: "shell".to_string(),
        status_age_s: 0,
        quiet_s: 0,
        background_point: true,
        background: vec!["zsh".to_string(), "zsh".to_string()],
        ..idle_facts()
    };
    assert_eq!(next_step(&gave_up, &brk, false, 1), Step::Wait("failed"));
    assert!(release_is_next("failed"), "the release it owes is next");
    for other in ["signal-refused", "resumed-elsewhere"] {
        assert_eq!(
            next_step(&Phase::Failed(other.to_string()), &brk, false, 1),
            Step::Wait("failed"),
            "{other}"
        );
    }
    // A late READY the agent's own work outlived past the drain: void.
    let outlived = Facts {
        ready_s: DRAIN_S,
        ..brk.clone()
    };
    assert_eq!(
        next_step(&gave_up, &outlived, true, 1),
        Step::Void("background")
    );
    // Short of the drain, and with nothing left running, the restart it
    // would take waits for an idle point: nothing is ended at a break.
    let fresh = Facts {
        ready_s: DRAIN_S - 1,
        ..brk.clone()
    };
    assert_eq!(
        break_step(next_step(&gave_up, &fresh, true, 1)),
        Step::Wait("background")
    );
    let done = Facts {
        background: Vec::new(),
        ..brk.clone()
    };
    assert_eq!(
        break_step(next_step(&gave_up, &done, true, 1)),
        Step::Wait("background")
    );
    // WORK INSIDE THE AGENT'S OWN PROCESS (the review of 2026-09-27: a
    // workflow Claude's `busy` says it waits on, dead or alive, and nothing
    // under the agent): the break arm waits on it as on a shell under the
    // agent, before the restart's gate is ever asked — so the late READY it
    // outlives past the drain is voided the same way, whatever Claude's word
    // for it, the owner's `--now` included.
    for status in ["busy", "shell", "idle"] {
        let own = Facts {
            status: status.to_string(),
            background: Vec::new(),
            ..brk.clone()
        };
        assert_eq!(
            next_step(&gave_up, &own, true, 1),
            Step::Wait("background"),
            "{status}: short of the drain, waited for"
        );
        let outlived = Facts {
            ready_s: DRAIN_S,
            ..own.clone()
        };
        assert_eq!(
            next_step(&gave_up, &outlived, true, 1),
            Step::Void("background"),
            "{status}: past it, void"
        );
        assert_eq!(
            requested_step(&Request::Now, &gave_up, &outlived, true, 1, "9.9.9"),
            Step::Void("background"),
            "{status}: the owner's --now ends nothing at a break"
        );
    }
    // NEGATIVE CONTROL: off a break, Claude's own `idle` with nothing under
    // the agent is the restart.
    assert_eq!(gate_restart(&idle_facts(), true), Gate::Go);
    // THE RELEASE AT A BREAK: the notice's own gate there.
    assert_eq!(gate_release(&brk, false), Gate::Go);
    assert_eq!(gate_release(&brk, true), Gate::Wait("ready"));
    let knocks: [Knock; 6] = [
        (|f| f.attended = true, "attended"),
        (|f| f.held = true, "held"),
        (|f| f.limited = true, "limited"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
    ];
    for (edit, word) in knocks {
        let mut f = brk.clone();
        edit(&mut f);
        assert_eq!(gate_release(&f, false), Gate::Wait(word), "{word}");
    }
    // NEGATIVE CONTROL: an announced upgrade within its window still waits
    // on the agent's own work at a break.
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&announced, &brk, false, REASK_S - 1),
        Step::Wait("background")
    );
}

/// THE RESTART'S OWN GATE WAITS AT A BREAK (the merge review of 2026-09-27,
/// G1a). At a break of the agent's own work ([`Facts::background_point`])
/// that work runs by definition — a shell under the agent, or a workflow
/// inside its own process that [`Facts::background`] never names — and the
/// Drain never ends the agent's own work (the module doc: it "is waited for,
/// and never ended"). [`next_step`]'s break arm answers every phase there
/// before this gate is asked, and the drivers' [`break_step`] turns an end
/// into a wait; the merge with main's break arm had left the gate itself
/// answering `Go` there — on Claude's `idle`, on a `busy` or `shell` a break
/// admits, and on the owner's `--now` — while [`next_step`]'s doc said it
/// never does. Now the gate says what the break arm says. The break arm's
/// word for a phase the drivers carry instead of stepping (a restart in
/// flight, a move done) is `background` too. NEGATIVE CONTROLS: a gate that
/// is shut says its own word first, and off a break the same facts are the
/// restart.
#[test]
fn the_restarts_gate_waits_at_a_break_whatever_else_lets_it_go() {
    for status in ["idle", "busy", "shell"] {
        for owner_now in [false, true] {
            let brk = Facts {
                status: status.to_string(),
                background_point: true,
                owner_now,
                ..idle_facts()
            };
            assert_eq!(
                gate_restart(&brk, true),
                Gate::Wait("background"),
                "{status}, --now {owner_now}"
            );
        }
    }
    let brk = Facts {
        background_point: true,
        ..idle_facts()
    };
    assert_eq!(gate_restart(&brk, false), Gate::Wait("not-ready"));
    let held = Facts {
        held: true,
        ..brk.clone()
    };
    assert_eq!(gate_restart(&held, true), Gate::Wait("held"));
    for phase in [
        Phase::Exiting { at_s: 1 },
        Phase::Relaunched { at_s: 1 },
        Phase::Done,
    ] {
        assert_eq!(
            next_step(&phase, &brk, true, 2),
            Step::Wait("background"),
            "{phase:?}"
        );
    }
    assert_eq!(gate_restart(&idle_facts(), true), Gate::Go);
}

/// A main-chain user row saying `text`, as Claude Code 2.1.283 writes a typed
/// prompt — the harness's and a person's alike (measured on the live E2E of
/// 2026-09-26: `promptSource` and `origin` cannot tell them apart).
fn prompt_row(text: &str) -> String {
    format!(
        r#"{{"type":"user","isSidechain":false,"promptSource":"typed","origin":{{"kind":"human"}},"message":{{"role":"user","content":"{text}"}}}}"#
    )
}

/// A main-chain assistant row.
fn answer_row(model: &str) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"ok"}}]}}}}"#
    )
}

/// D1 OF THE LIVE E2E OF 2026-09-26: A CONVERSATION HAS A TASK from the first
/// prompt of someone else's — never for the harness's own turns. The E2E's
/// 0eada4a1 (the upgrade notice, READY), 1a5299ab (notice, READY, carry-on,
/// its answer) read TASKLESS; a person's or an orchestrator's prompt makes it
/// tasked the moment it is recorded — answered or not yet (the review of
/// 2026-09-26: an orchestrator's first prompt not answered yet read taskless,
/// and the fresh restart's last look let its signal through) — and for good,
/// whatever the harness types after — stopped with Esc before its answer too.
/// A command of theirs is a task once it
/// starts a turn and while it has not said it started none; `/model`, whose
/// local output says so, is none. Not prompts: a subagent's rows, Claude
/// Code's own expansions (`isMeta`), a tool's result, Esc's note. NEGATIVE
/// CONTROLS: the same transcripts with one person's prompt in them read
/// tasked; an unknown user row's shape reads as someone's.
#[test]
fn a_conversation_has_a_task_from_someone_elses_first_prompt() {
    let notice = prompt_row(
        "[aterm harness] Claude Code 2.1.283 (managed) is installed; this session runs 2.1.281.",
    );
    let carry = prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283.");
    let relaunched =
        prompt_row("[aterm harness] Relaunched: Claude Code exited without anyone asking.");
    let person = prompt_row("Staged harness test, please follow exactly.");
    let tool = r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#.to_string();
    let meta = r#"{"type":"user","isMeta":true,"isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"Please analyze this codebase and create a CLAUDE.md file"}]}}"#.to_string();
    let sub_prompt = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"a subagent's task"}}"#.to_string();
    let sub_answer = r#"{"type":"assistant","isSidechain":true,"message":{"model":"claude-haiku-4-5","content":[]}}"#.to_string();
    let model_cmd = prompt_row("<command-name>/model</command-name>");
    let model_out = prompt_row("<local-command-stdout>Set model to haiku</local-command-stdout>");
    // Claude Code 2.1.283's own shape for a prompt command (`/init`): the
    // command row, its expansion (`isMeta`), the turn.
    let init_cmd = prompt_row(
        "<command-message>init is analyzing your codebase…</command-message>\\n<command-name>/init</command-name>",
    );
    let esc = prompt_row("[Request interrupted by user]");
    let ready = answer_row("claude-haiku-4-5-20251001");
    let synthetic = answer_row("<synthetic>");
    let text = |rows: &[&String]| {
        rows.iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let taskless: [(&str, Vec<&String>); 7] = [
        ("nothing yet", vec![]),
        ("the notice, answered READY", vec![&notice, &ready]),
        (
            "notice, READY, carry-on, answered",
            vec![&notice, &ready, &carry, &ready],
        ),
        (
            "a relaunch's carry-on, answered",
            vec![&relaunched, &ready, &tool, &ready],
        ),
        (
            "a subagent's prompt, answered",
            vec![&notice, &sub_prompt, &sub_answer],
        ),
        ("an expansion and Esc", vec![&meta, &ready, &esc, &ready]),
        ("`/model`, no turn", vec![&model_cmd, &model_out]),
    ];
    for (what, rows) in &taskless {
        assert!(!transcript_tasked(&text(rows), &[]), "{what}");
    }
    let tasked: [(&str, Vec<&String>); 11] = [
        ("a person's prompt, answered", vec![&person, &ready]),
        ("a person's prompt, not answered yet", vec![&person]),
        // ND1 of the live re-test of 2026-09-26: a first prompt stopped with
        // Esc before any answer is still someone's ask.
        ("a person's prompt, stopped with Esc", vec![&person, &esc]),
        (
            "a first prompt after the notice, stopped with Esc",
            vec![&notice, &ready, &person, &esc],
        ),
        (
            "a first prompt after the notice, not answered yet",
            vec![&notice, &ready, &person],
        ),
        ("an API error is an answer", vec![&person, &synthetic]),
        ("a prompt command's turn", vec![&init_cmd, &meta, &ready]),
        (
            "a command that has not said yet",
            vec![&notice, &ready, &init_cmd],
        ),
        (
            "the harness's turns after it change nothing",
            vec![&person, &ready, &notice, &ready, &carry, &ready],
        ),
        (
            "a person's prompt after the notice",
            vec![&notice, &ready, &person, &tool, &ready],
        ),
        (
            "`/model`, then a real prompt",
            vec![&model_cmd, &model_out, &person, &ready],
        ),
    ];
    for (what, rows) in &tasked {
        assert!(transcript_tasked(&text(rows), &[]), "{what}");
    }
    // An unknown shape reads as someone's (the old behaviour), never as none.
    let odd = r#"{"type":"user","message":{"role":"user","content":{"weird":1}}}"#.to_string();
    assert!(transcript_tasked(&text(&[&odd]), &[]));
    // Folded a line at a time, the answer is the same, and stays: a prompt
    // for good at once, a command only while it has not said.
    let mut scan = TaskScan::default();
    assert!(!scan.line(&notice) && !scan.line(&ready));
    assert!(scan.line(&model_cmd) && !scan.settled());
    assert!(!scan.line(&model_out) && !scan.tasked());
    assert!(scan.line(&person) && scan.settled());
    assert!(scan.line(&notice) && scan.line(&model_out) && scan.tasked());
    // Every turn the harness types carries the mark the scan reads.
    let (from, to) = (v("2.1.281"), v("2.1.283"));
    for typed in [
        prepare_prompt(&from, &to, Source::Managed, "M"),
        prepare_prompt_with_model(&from, &to, Source::Managed, "M", Some("opus"), None),
        continue_prompt(&from, &to, Some("haiku")),
        continue_prompt_with_model(&from, &to, None, Some("opus")),
        super::super::relaunch::resumed_prompt("2.1.283", "exit"),
        super::super::relaunch::resumed_prompt("2.1.283", "memory"),
        super::super::upgrade_codex::continue_prompt(&from, &to),
    ] {
        assert!(typed.starts_with(HARNESS_MARK), "{typed}");
    }
    assert!(super::super::upgrade_codex::ANNOUNCE_HEAD.starts_with(HARNESS_MARK));
}

/// D1: THE SUPERVISOR'S OWN TURNS ARE NO TASK EITHER (the review of
/// 2026-09-26). `keep going` and `answer_text` carry no mark — their words
/// are the owner's — but the loop's ledger records every text it types
/// ([`TaskScan::supervisor_typed`]), and a row with the same words (however
/// wrapped) is the harness's: the E2E's chain — the notice, READY, the
/// carry-on, `keep going`, `answer_text` — holds no task. So is a command it
/// typed (`/compact`), read out of Claude Code's command row. NEGATIVE
/// CONTROLS: the same record read with no ledger is tasked by the first
/// unmarked turn; a person's prompt that is not the loop's words is theirs,
/// ledger or not; and a person's command is still a command.
#[test]
fn the_supervisors_own_turns_by_its_ledger_are_no_task() {
    let cfg = crate::supervise::SupervisorConfig::default();
    let ours = vec![
        cfg.continue_text.clone(),
        cfg.answer_text.clone(),
        "/compact".to_string(),
    ];
    let chain = [
        prompt_row("[aterm harness] Claude Code 2.1.283 (managed) is installed"),
        answer_row("claude-haiku-4-5"),
        prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283."),
        answer_row("claude-haiku-4-5"),
        prompt_row("keep going"),
        answer_row("claude-haiku-4-5"),
        prompt_row(&cfg.answer_text.replace(". ", ".  ")),
        answer_row("claude-haiku-4-5"),
        prompt_row("<command-name>/compact</command-name>\\n<command-args></command-args>"),
        answer_row("claude-haiku-4-5"),
    ]
    .join("\n");
    assert!(!transcript_tasked(&chain, &ours), "the harness's alone");
    assert!(transcript_tasked(&chain, &[]), "no ledger: someone's");
    let person = format!("{chain}\n{}", prompt_row("keep going, and fix the parser"));
    assert!(transcript_tasked(&person, &ours), "not the loop's words");
    let command = format!(
        "{chain}\n{}\n{}",
        prompt_row("<command-name>/init</command-name>"),
        answer_row("claude-haiku-4-5")
    );
    assert!(transcript_tasked(&command, &ours), "a person's command");
}

/// D1: A CONVERSATION WITH NO TASK IS NEVER ANNOUNCED TO — it is restarted
/// AFRESH ([`Step::Fresh`]) once idle the full [`QUIET_S`] (no answer to
/// read, but a first prompt may be on its way: an orchestrator's `await agent
/// idle` returns at the first idle verdict — ND1 of the live re-test of
/// 2026-09-26, where the restart fired 1.3 s after it), whether nothing was
/// typed yet or an older aterm's notice stands unanswered. A person at the
/// tab, a hold, a box, a draft, a busy agent and work running under it still
/// wait, and a break of its background work ends nothing. NEGATIVE
/// CONTROLS: the same facts with a task are announced to; a taskless agent
/// whose verdict moved under `QUIET_S` ago (a launch, a prompt's turn)
/// settles first — ten seconds, the settle before this, among them.
#[test]
fn a_conversation_with_no_task_is_restarted_afresh_never_announced_to() {
    let now = 10_000;
    let settled = Facts {
        status_age_s: QUIET_S,
        quiet_s: QUIET_S,
        taskless: true,
        ..idle_facts()
    };
    for phase in [
        Phase::Pending,
        Phase::Announced {
            at_s: now - 5,
            asks: 1,
        },
    ] {
        assert_eq!(
            next_step(&phase, &settled, false, now),
            Step::Fresh,
            "{phase:?}"
        );
    }
    // Just launched, or just out of a turn: the settle first.
    for (status_age_s, quiet_s) in [(0, 0), (10, 10), (QUIET_S - 1, QUIET_S), (QUIET_S, 1)] {
        let f = Facts {
            status_age_s,
            quiet_s,
            ..settled.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &f, false, now),
            Step::Wait("settling"),
            "{status_age_s}/{quiet_s}"
        );
    }
    let knocks: [Knock; 7] = [
        (|f| f.attended = true, "attended"),
        (|f| f.held = true, "held"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.status = "busy".to_string(), "not-idle"),
        (|f| f.background = vec!["zsh".to_string()], "background"),
        (|f| f.background_point = true, "background"),
    ];
    for (knock, why) in knocks {
        let mut f = settled.clone();
        knock(&mut f);
        assert_eq!(next_step(&Phase::Pending, &f, false, now), Step::Wait(why));
    }
    // A restart in flight or finished is its own: nothing afresh.
    for phase in [Phase::Exiting { at_s: now }, Phase::Done] {
        assert!(matches!(
            next_step(&phase, &settled, false, now),
            Step::Wait(_)
        ));
    }
    // NEGATIVE CONTROL: with a task, the notice.
    let tasked = Facts {
        taskless: false,
        ..settled.clone()
    };
    assert_eq!(
        next_step(&Phase::Pending, &tasked, false, now),
        Step::Announce
    );
}

// ------------------------------------------- the stall of 2026-09-25/26

/// The row Claude Code 2.1.280 parked the incident's session under, as the
/// window's supervisor journaled it (`EVENT limited seq=43484 message=…`,
/// tab `s-d3346b29dd236432b852`, 2026-09-25 16:00:26 PDT).
const AUTO_CONTINUE: &str =
    "⚠ Usage limit reached · continuing automatically at 6am · esc to cancel";

/// A Claude Code 2.1.280 screen: the last turn's words, then `live` (the
/// rows Claude Code parks between them and the composer), the composer
/// empty, the auto-mode footer.
fn claude_screen(live: &[&str]) -> Vec<String> {
    let rule = "─".repeat(80);
    let mut r = rows(&["⏺ Both workflows are relaunched; waiting on them.", ""]);
    r.extend(rows(live));
    r.extend([
        rule.clone(),
        "❯ ".to_string(),
        rule,
        "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string(),
    ]);
    r
}

/// THE LIMIT FACT IS THE SUPERVISOR'S OWN READING (F1): the incident's screen
/// — the auto-continue banner alone over an empty composer, which the gates
/// read as idle — is `limited` by the one recogniser the supervisor journaled
/// `EVENT limited` with, and so is the same wall over a break of the agent's
/// own background work, which the supervisor's phase reads BUSY. NEGATIVE
/// CONTROLS: the screen without the banner, the banner made history by the
/// words under it, and a copy of it in a tool's output are not.
#[test]
fn the_limit_fact_is_read_by_the_supervisors_own_recogniser() {
    let parked = claude_screen(&[AUTO_CONTINUE, ""]);
    assert!(limited(Agent::Claude, &parked));
    let reading = aterm_phase::read(Some("claude"), &parked, None);
    assert!(
        matches!(reading.phase, aterm_phase::Phase::Limited { .. }),
        "the supervisor's reader says limited too: {:?}",
        reading.phase
    );
    // The composer the gates read is empty on it: a limited session LOOKS
    // idle to every other fact.
    assert!(composer_is_empty(
        &parked,
        Some((parked.len() - 3, 2)),
        false
    ));

    let at_break = claude_screen(&[AUTO_CONTINUE, "✻ Waiting for 2 dynamic workflows to finish"]);
    assert!(
        limited(Agent::Claude, &at_break),
        "the wall stands over the agent's own background work"
    );

    assert!(!limited(Agent::Claude, &claude_screen(&[""])));
    let history = claude_screen(&[
        AUTO_CONTINUE,
        "",
        "⏺ The limit reset; picking the workflows back up.",
        "",
    ]);
    assert!(!limited(Agent::Claude, &history), "words under it end it");
    let quoted = claude_screen(&[
        "⏺ Bash(aterm ctl @s-9 text | grep limit)",
        &format!("  ⎿  {AUTO_CONTINUE}"),
        "",
    ]);
    assert!(
        !limited(Agent::Claude, &quoted),
        "a tool's output is not the wall"
    );
}

/// F1: A SESSION AT ITS LIMIT IS NEVER ASKED, AND NOTHING OF THE UPGRADE'S IS
/// SPENT THERE. Every gate waits `limited` — the owner's `--now` and a break
/// of the agent's background work included — no re-ask, no give-up, no void
/// and no restart is taken, and an announced upgrade's clock restarts at every
/// look that finds the limit ([`clock_held`]). NEGATIVE CONTROLS: the same
/// facts off the limit take each of those steps.
#[test]
fn a_limited_session_is_never_asked_and_its_clock_is_held() {
    let limited_facts = Facts {
        limited: true,
        ..idle_facts()
    };
    assert_eq!(gate_announce(&limited_facts), Gate::Wait("limited"));
    assert_eq!(gate_restart(&limited_facts, true), Gate::Wait("limited"));
    let now = Facts {
        owner_now: true,
        ..limited_facts.clone()
    };
    assert_eq!(
        gate_announce(&now),
        Gate::Wait("limited"),
        "--now waives no limit"
    );
    let at_break = Facts {
        status: "busy".to_string(),
        background_point: true,
        ..limited_facts.clone()
    };
    assert_eq!(gate_announce(&at_break), Gate::Wait("limited"));
    assert_eq!(
        requested_step(
            &Request::Now,
            &Phase::Pending,
            &limited_facts,
            false,
            1,
            "9.9.9"
        ),
        Step::Wait("limited")
    );

    // Four half-hours and the asks budget, as the incident spent them.
    let t0 = 1_790_377_339;
    let asked = Phase::Announced { at_s: t0, asks: 1 };
    let tired = Phase::Announced {
        at_s: t0,
        asks: MAX_ASKS,
    };
    let mut boxed = limited_facts.clone();
    boxed.approval_box = true;
    boxed.hold_s = HOLD_S;
    for k in 1..=4 {
        let later = t0 + k * REASK_S;
        assert_eq!(
            next_step(&Phase::Pending, &limited_facts, false, later),
            Step::Wait("limited")
        );
        assert_eq!(
            next_step(&asked, &limited_facts, false, later),
            Step::Wait("limited")
        );
        assert_eq!(
            next_step(&tired, &limited_facts, false, later),
            Step::Wait("limited")
        );
        assert_eq!(
            next_step(&asked, &limited_facts, true, later),
            Step::Wait("limited")
        );
        assert_eq!(
            next_step(&asked, &boxed, true, later),
            Step::Wait("limited")
        );
        // The clock is held: each look restarts it.
        assert_eq!(
            clock_held(&asked, &limited_facts, later),
            Phase::Announced {
                at_s: later,
                asks: 1
            }
        );
    }
    // NEGATIVE CONTROLS: off the limit, the same looks re-ask, give up, end
    // and void, and the clock runs.
    let late = t0 + REASK_S;
    let idle = idle_facts();
    assert_eq!(
        next_step(&Phase::Pending, &idle, false, late),
        Step::Announce
    );
    assert_eq!(next_step(&asked, &idle, false, late), Step::Announce);
    assert_eq!(next_step(&tired, &idle, false, late), Step::GiveUp);
    assert_eq!(next_step(&asked, &idle, true, late), Step::Terminate);
    let mut person = idle.clone();
    person.approval_box = true;
    person.hold_s = HOLD_S;
    assert_eq!(next_step(&asked, &person, true, late), Step::Void("box"));
    assert_eq!(clock_held(&asked, &idle, late), asked);
    assert_eq!(
        clock_held(&Phase::Pending, &limited_facts, late),
        Phase::Pending
    );
}

/// F2: AN UPGRADE THAT GAVE UP ASKING STILL HEARS A LATE READY — the one the
/// incident's agent gave at 06:02 — and takes exactly the step an announced
/// upgrade takes on it: every gate of the restart kept (the agent's own work,
/// a person at the tab, the limit), then the restart. NEGATIVE CONTROLS: no
/// READY waits `failed`, and so does a READY to an upgrade that stopped for
/// any other reason.
#[test]
fn an_upgrade_that_gave_up_asking_honours_a_late_ready() {
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let idle = idle_facts();
    assert_eq!(next_step(&gave_up, &idle, true, 1), Step::Terminate);
    let knocks: [Knock; 5] = [
        (|f| f.background = vec!["zsh".to_string()], "background"),
        (|f| f.attended = true, "attended"),
        (|f| f.limited = true, "limited"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for (edit, word) in knocks {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(next_step(&gave_up, &f, true, 1), Step::Wait(word), "{word}");
    }
    assert_eq!(next_step(&gave_up, &idle, false, 1), Step::Wait("failed"));
    for other in ["signal-refused", "not-a-shell-job", "resumed-elsewhere"] {
        assert_eq!(
            next_step(&Phase::Failed(other.to_string()), &idle, true, 1),
            Step::Wait("failed"),
            "{other}"
        );
    }
    // The owner's view still reads it as the upgrade that gave up.
    assert_eq!(gave_up.word(), "failed:unanswered");
}

/// A READY THE RESTART'S GATE HOLDS PAST THE DRAIN IS NEVER WAITED ON FOR
/// GOOD — an announced upgrade's and a gave-up one's late answer alike. A
/// person's box or draft that has stood [`HOLD_S`] voids it in either phase
/// ([`person_void`]). The agent's own background work still running
/// [`DRAIN_S`] after the answer is met by what each phase has left: an
/// announced upgrade ASKS AGAIN, naming what runs, and the new notice
/// supersedes the answer (the four-day tab of 2026-09-26); one that gave up
/// has no ask left and VOIDS it, owing the release ([`void_of`]; review of
/// 2026-09-26: the gave-up arm had no void at all, so a READY a person's
/// hold kept for hours ended the agent the moment it cleared, and one a
/// `run_in_background` server held left the agent stopped for good). A gate
/// about to open — the settle after that work ends — never voids, however
/// old the answer; at the limit, nothing does. NEGATIVE CONTROLS: short of
/// each bound, the gate's own wait.
#[test]
fn a_ready_the_gate_holds_past_the_drain_is_void_in_either_phase() {
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    let drained = DRAIN_S;
    let fresh = DRAIN_S - 1;
    let with = |edit: fn(&mut Facts), ready_s: u64, hold_s: u64| {
        let mut f = Facts {
            ready_s,
            hold_s,
            ..idle_facts()
        };
        edit(&mut f);
        f
    };
    let boxed: fn(&mut Facts) = |f| f.approval_box = true;
    let drafted: fn(&mut Facts) = |f| f.composer_empty = false;
    let working: fn(&mut Facts) = |f| f.background = vec!["zsh".to_string()];
    let settling: fn(&mut Facts) = |f| f.quiet_s = 3;
    for phase in [&announced, &gave_up] {
        let at = |f: &Facts| next_step(phase, f, true, drained);
        assert_eq!(at(&with(boxed, 0, HOLD_S)), Step::Void("box"), "{phase:?}");
        assert_eq!(at(&with(drafted, 0, HOLD_S)), Step::Void("draft"));
        let outlived = if *phase == announced {
            Step::Announce
        } else {
            Step::Void("background")
        };
        assert_eq!(at(&with(working, drained, 0)), outlived, "{phase:?}");
        // Short of the bounds, and a gate about to open: the gate's wait.
        assert_eq!(at(&with(boxed, 0, HOLD_S - 1)), Step::Wait("box"));
        assert_eq!(at(&with(working, fresh, 0)), Step::Wait("background"));
        assert_eq!(at(&with(settling, 10 * drained, 0)), Step::Wait("settling"));
        // At the limit nothing is voided (nor its clock run: `ready_since`).
        let limited = Facts {
            limited: true,
            ..with(working, drained, HOLD_S)
        };
        assert_eq!(at(&limited), Step::Wait("limited"), "{phase:?}");
    }
    // An announced upgrade's drain counts from its notice too: a box right
    // after it is only a wait.
    assert_eq!(
        next_step(&announced, &with(boxed, 0, HOLD_S), true, fresh),
        Step::Wait("box")
    );
    // The READY clock: begun when the answer is first heard, kept while it
    // stands, begun again at every limited look, gone with the answer.
    let idle = idle_facts();
    let limited = Facts {
        limited: true,
        ..idle_facts()
    };
    assert_eq!(ready_since(0, true, &idle, 50), 50);
    assert_eq!(ready_since(50, true, &idle, 90), 50);
    assert_eq!(ready_since(50, true, &limited, 90), 90);
    assert_eq!(ready_since(50, false, &idle, 90), 0);
}

/// F3: THE RELEASE IS TYPED UNDER THE NOTICE'S OWN GATE, and never over a
/// READY answer the restart will act on. Its words tell the agent nothing
/// will restart it and to go on — and are neither a notice nor a
/// continuation to any reader of the transcript.
#[test]
fn the_release_is_typed_where_a_notice_could_be_and_never_over_a_ready() {
    assert_eq!(gate_release(&idle_facts(), false), Gate::Go);
    assert_eq!(gate_release(&idle_facts(), true), Gate::Wait("ready"));
    let knocks: [Knock; 7] = [
        (|f| f.held = true, "held"),
        (|f| f.attended = true, "attended"),
        (|f| f.limited = true, "limited"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.quiet_s = 3, "settling"),
    ];
    for (edit, word) in knocks {
        let mut f = idle_facts();
        edit(&mut f);
        assert_eq!(gate_release(&f, false), Gate::Wait(word), "{word}");
    }
    // A break of the agent's own background work types it too, where a
    // notice may go (2026-09-27: a break that never ends reached no idle
    // point, and the release a give-up owed was never typed).
    let at_break = Facts {
        status: "busy".to_string(),
        background_point: true,
        ..idle_facts()
    };
    assert_eq!(gate_release(&at_break, false), Gate::Go);

    let text = release_prompt(Agent::Claude);
    assert_eq!(
        text,
        "[aterm harness] Upgrade off: The Claude Code upgrade is off for now: nothing will \
         restart this session without asking you again first, and nothing the notice asked of \
         you still applies. Carry on as you would have without it."
    );
    // It points at no work of its own (the review of 2026-09-26: "continue
    // the work you were doing before the notice" pulled an agent back over
    // direction it had been given since).
    assert!(!text.contains("before the notice"));
    assert!(!text.starts_with(ANNOUNCE_HEAD));
    assert!(!text.contains(READY_PREFIX));
    let row = format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#);
    let answer = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Carrying on."}]}}"#;
    assert!(
        !directed_since_ready(&[row.as_str(), answer].join("\n"), &[]),
        "the harness's own line directs nothing"
    );
    let row: Value = aterm_json::from_str(&row).expect("json");
    assert!(!is_announcement(row.get("message").expect("message")));
}

// ------------------------------------------------ no stop is for good (2026-09-27)

/// Every reason a round stops for: the give-up, the refusals, the stops of a
/// restart, the orphan pass's expirations and the Codex lane's.
const STOPS: [&str; 12] = [
    GAVE_UP,
    "not-a-shell-job",
    "argv:--print",
    "line:fish-cwd",
    "signal-refused",
    "relaunch-refused",
    "no-resume",
    "resumed-elsewhere",
    "exited-before-continuing",
    "stale-exit",
    "no-resume-hint",
    "shell-gone",
];

/// A break of the agent's own background work: a shell of its own under it.
fn break_with_work() -> Facts {
    Facts {
        status: "busy".to_string(),
        status_age_s: 0,
        quiet_s: 0,
        background_point: true,
        background: vec!["zsh".to_string()],
        ..idle_facts()
    }
}

/// ONE ROUND ON, ONE ROUND OFF: the rest is one round's worth of asking, so
/// the nagging is at most half of any stretch, and the silence after a first
/// stop that hears no late READY is the give-up's window plus the rest. That
/// is NOT the longest silence: a late READY the agent's own work outlives
/// adds a drain and a second rest ([`RETRY_S`]'s doc;
/// `upgrade_stall_tests::the_longest_silence_is_the_window_a_rest_a_drain_and_a_rest`).
#[test]
fn the_rest_is_one_rounds_worth_of_asking() {
    assert_eq!(RETRY_S, u64::from(MAX_ASKS) * REASK_S);
    assert_eq!(span(RETRY_S), "2h");
    assert_eq!(
        span(REASK_S + RETRY_S),
        "2h30m",
        "the silence after a first stop with no late READY"
    );
}

/// NO STOP IS FOR GOOD (the owner, 2026-09-27: "you should NEVER have
/// upgrades stalled"): EVERY reason a round stops for rests `RETRY_S` —
/// `failed` at an idle point, `background` at a break, as before — and then
/// the reducer starts a new round (`Step::Rearm`), at an idle point, at a
/// break (the break's own rule lets it through: it types nothing and ends
/// nothing), at a usage limit and at the login wall. NEGATIVE CONTROL: one
/// second short of the rest, never.
#[test]
fn every_stopped_round_rests_then_rearms() {
    for why in STOPS {
        let stopped = Phase::Failed(why.to_string());
        let aged = |f: Facts, secs: u64| Facts {
            failed_s: secs,
            ..f
        };
        assert_eq!(
            next_step(&stopped, &aged(idle_facts(), RETRY_S - 1), false, 1),
            Step::Wait("failed"),
            "{why}"
        );
        assert_eq!(
            next_step(&stopped, &aged(break_with_work(), RETRY_S - 1), false, 1),
            Step::Wait("failed"),
            "{why}: at a break, the rest says so"
        );
        for (place, f) in [
            ("idle", idle_facts()),
            ("break", break_with_work()),
            (
                "limited",
                Facts {
                    limited: true,
                    ..idle_facts()
                },
            ),
            (
                "login",
                Facts {
                    login: true,
                    ..idle_facts()
                },
            ),
            (
                "a person",
                Facts {
                    attended: true,
                    composer_empty: false,
                    ..idle_facts()
                },
            ),
        ] {
            let f = aged(f, RETRY_S);
            assert_eq!(
                next_step(&stopped, &f, false, 1),
                Step::Rearm,
                "{why}: {place}"
            );
            assert_eq!(
                break_step(next_step(&stopped, &f, false, 1)),
                Step::Rearm,
                "{why}: {place}"
            );
            assert!(retry_due(&stopped, RETRY_S, false), "{why}");
            assert!(!retry_due(&stopped, RETRY_S - 1, false), "{why}");
        }
    }
    // Only a stopped round rests: nothing else is re-armed, however old.
    for phase in [
        Phase::Pending,
        Phase::Announced { at_s: 0, asks: 1 },
        Phase::Exiting { at_s: 0 },
        Phase::Relaunched { at_s: 0 },
        Phase::Done,
    ] {
        assert!(!retry_due(&phase, u64::MAX, false), "{phase:?}");
    }
}

/// THE OWNER'S TAB OF 2026-09-27, through the reducer (`s-d3346b29dd236432b852`:
/// `phase=failed:unanswered pending_for=1d22h wait=background wait_for=2h56m
/// stalled=gave-up`). Its agent runs a background gate or monitor at every
/// look, so nearly every step is taken at a break. The round gave up; the
/// agent's late READY came; its own work outlived the answer — and the break
/// answered `background` before any bound was asked, for good, while at an
/// idle point the void sent it back to `failed`, for good. Now: at a break
/// the late READY is voided on the drain's bound as at an idle point, and
/// once the round has rested — the READY voided, a STALE READY in the
/// transcript no marker holds any more — a new round starts at the break and
/// its first notice is typed there. NEGATIVE CONTROL: a READY heard for less
/// than the drain's bound is still waited for — the late READY is honoured,
/// never cut short by the new round.
#[test]
fn the_owners_stall_never_waits_for_ever() {
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let heard = |ready_s: u64, failed_s: u64| Facts {
        ready_s,
        failed_s,
        ..break_with_work()
    };
    // The late READY at a break: waited for, then voided on the bound.
    assert_eq!(
        next_step(&gave_up, &heard(60, RETRY_S * 20), true, 1),
        Step::Wait("background"),
        "a READY heard a minute ago is honoured, even past the rest"
    );
    assert_eq!(
        next_step(&gave_up, &heard(DRAIN_S, 60), true, 1),
        Step::Void("background"),
        "outlived by the agent's own work past the drain"
    );
    // The same at an idle point, as it was.
    let idle_with_work = Facts {
        background: vec!["zsh".to_string()],
        ready_s: DRAIN_S,
        ..idle_facts()
    };
    assert_eq!(
        next_step(&gave_up, &idle_with_work, true, 1),
        Step::Void("background")
    );
    // Voided: a stale READY no marker holds (`ready` false). It rests —
    // never `failed` for ever — and past the rest a new round starts, at a
    // break and at an idle point alike.
    assert_eq!(
        next_step(&gave_up, &heard(0, 60), false, 1),
        Step::Wait("failed")
    );
    assert_eq!(
        next_step(&gave_up, &heard(0, RETRY_S), false, 1),
        Step::Rearm,
        "at a break"
    );
    assert_eq!(
        next_step(
            &gave_up,
            &Facts {
                failed_s: RETRY_S,
                ..idle_with_work.clone()
            },
            false,
            1
        ),
        Step::Rearm,
        "at an idle point"
    );
    // The new round asks at once — at the break its work makes — and every
    // gate of a first notice still holds.
    assert_eq!(
        next_step(&Phase::Pending, &break_with_work(), false, 1),
        Step::Announce
    );
    let drafted = Facts {
        composer_empty: false,
        ..break_with_work()
    };
    assert_eq!(
        next_step(&Phase::Pending, &drafted, false, 1),
        Step::Wait("draft")
    );
}

/// THE OWNER'S WORD HOLDS A NEW ROUND AS IT HOLDS ANY ([`rearm_held`]): a
/// stopped round the owner skipped (`--skip` of THIS target) stays skipped,
/// however long it rests; a deferral holds it until it runs out, then the
/// round re-arms. NEGATIVE CONTROLS: a skip of another build, no word, and
/// `--now` hold nothing.
#[test]
fn a_skip_stays_skipped_and_a_deferral_lets_a_stopped_round_go() {
    let rested = Facts {
        failed_s: RETRY_S * 10,
        ..idle_facts()
    };
    let now = 1_000_000;
    for why in STOPS {
        let stopped = Phase::Failed(why.to_string());
        let step =
            |request: Request| requested_step(&request, &stopped, &rested, false, now, "9.9.9");
        assert_eq!(
            step(Request::Skip("9.9.9".into())),
            Step::Wait("skipped"),
            "{why}"
        );
        assert_eq!(
            step(Request::DeferUntil(now + 60)),
            Step::Wait("deferred"),
            "{why}"
        );
        for free in [
            Request::Skip("9.9.8".into()),
            Request::DeferUntil(now),
            Request::None,
            Request::Now,
        ] {
            assert_eq!(step(free.clone()), Step::Rearm, "{why}: {free:?}");
        }
    }
}

/// A SESSION AT ITS LIMIT IS RE-ARMED BUT NEVER ASKED: the new round types
/// nothing, so it is taken at the limit; its first notice waits `limited`,
/// at an idle point and at a break alike, for as long as the limit stands.
#[test]
fn a_limited_session_is_rearmed_but_never_asked_while_limited() {
    for f in [idle_facts(), break_with_work()] {
        let limited = Facts {
            limited: true,
            failed_s: RETRY_S,
            ..f
        };
        assert_eq!(
            next_step(&Phase::Failed(GAVE_UP.to_string()), &limited, false, 1),
            Step::Rearm
        );
        assert_eq!(
            next_step(&Phase::Pending, &limited, false, 1),
            Step::Wait("limited")
        );
        assert_eq!(gate_announce(&limited), Gate::Wait("limited"));
    }
}

// ------------------------------------ the notices queued behind a limit (2026-09-27)

/// The owner's session of 2026-09-27 (`25e3b26e…`, tab
/// `s-c543f4e0edd3439e5791`): its fourth notice's READY marker, as 0.93.0
/// typed it.
const QUEUED_MARKER: &str = "ATERM-UPGRADE-READY-6a57ff7a";

fn json_text(text: &str) -> String {
    aterm_json::to_string(&Value::from(text)).expect("json")
}

/// A notice with `marker`, typed at `ts`.
fn notice_row(ts: &str, marker: &str) -> String {
    let text = prepare_prompt(&v("2.1.280"), &v("2.1.283"), Source::Managed, marker);
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"timestamp":"{ts}","version":"2.1.280"}}"#,
        json_text(&text)
    )
}

/// CLAUDE CODE'S OWN ANSWER AT THE WEEKLY LIMIT, as the owner's transcript
/// holds it (row 2427, 2026-09-27T15:06:40.380Z, the first notice's turn;
/// ids cut, every key the readers look at kept).
fn limit_row(ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"{ts}","message":{{"diagnostics":null,"id":"m","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)"}}],"context_management":null}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":1791046800,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.280"}}"#
    )
}

/// A row of the session's own model saying `text` at `ts`.
fn said_row(ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","timestamp":"{ts}","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}]}},"version":"2.1.280"}}"#,
        json_text(text)
    )
}

/// A person's message at `ts` (the owner's `first, continue`).
fn person_row(ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"timestamp":"{ts}","version":"2.1.280"}}"#,
        json_text(text)
    )
}

/// A NOTICE THE LIMIT ANSWERED IS QUEUED, NEVER READ AS ASKED (the owner's
/// report of 2026-09-27: 0.93.0 typed four notices into a session parked at
/// its weekly limit, each answered within two seconds by Claude Code's
/// `rate_limit` row, and when the session went on all four reached the agent
/// at once). Its fate is `Queued` until the model writes a row of its own,
/// and then `Taken` at that row's time — the moment its window opens. The
/// login wall's reading is unchanged by it: a limit is no wall. NEGATIVE
/// CONTROLS: a notice the model answered is `Answered`, one the wall
/// answered `Wall`, one nothing has answered yet `Open`; a limit hit BEFORE
/// the notice (the wind-down turn's) leaves the notice's own fate its own; an
/// API error that is no limit and a warning are passed over; a subagent's
/// limit is not the session's; a limit row Claude Code writes without the
/// `error` key is read by the wall table's words.
#[test]
fn a_notice_the_limit_answered_is_queued_until_the_model_takes_it() {
    let m = QUEUED_MARKER;
    let notice = notice_row("2026-09-27T15:06:39.624Z", m);
    let queued = [notice.clone(), limit_row("2026-09-27T15:06:40.380Z")].join("\n");
    assert_eq!(notice_fate_of(&queued, m), Some(NoticeFate::Queued));
    assert_eq!(notice_fate(&queued, m), Some(false), "a limit is no wall");
    assert!(!notice_undelivered(&queued, m));
    let taken = [
        queued.clone(),
        person_row("2026-09-27T19:57:55.873Z", "first, continue"),
        said_row(
            "2026-09-27T19:58:05.554Z",
            "I'm pushing first. After that I'll get to a clean stopping point for the upgrade.",
        ),
    ]
    .join("\n");
    assert_eq!(
        notice_fate_of(&taken, m),
        Some(NoticeFate::Taken(Some(1_790_539_085))),
        "taken when the model first wrote after it"
    );
    // HOW LONG IT WAITS ON ITS LIMIT (review of 2026-09-27): the reset its
    // row names; a row naming none, REASK_S from when it was written; a
    // `/login` finished since, over (0) — in both of Claude Code's shapes.
    assert_eq!(queued_until(&queued, m), Some(1_791_046_800));
    let unstamped = [
        notice.clone(),
        limit_row("2026-09-27T15:06:40.380Z").replace(r#""resetsAt":1791046800,"#, ""),
    ]
    .join("\n");
    assert_eq!(queued_until(&unstamped, m), Some(1_790_521_600 + REASK_S));
    let login_2_1_281 = person_row(
        "2026-09-27T19:57:47Z",
        "<local-command-stdout>Login successful</local-command-stdout>",
    );
    let login_2_1_283 = r#"{"isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>Login successful</local-command-stdout>","timestamp":"2026-09-27T19:57:47Z"}"#;
    for login in [login_2_1_281.as_str(), login_2_1_283] {
        let switched = [queued.as_str(), login].join("\n");
        assert_eq!(notice_fate_of(&switched, m), Some(NoticeFate::Queued));
        assert_eq!(queued_until(&switched, m), Some(0), "{login}");
        let hit_again = [switched.clone(), limit_row("2026-09-27T19:58:00Z")].join("\n");
        assert_eq!(
            queued_until(&hit_again, m),
            Some(1_791_046_800),
            "a limit again after the login holds it again"
        );
    }
    assert_eq!(queued_until(&taken, m), None, "taken: no longer queued");
    // NEGATIVE CONTROLS.
    let answered = [notice.clone(), said_row("2026-09-27T15:06:50Z", "Saved.")].join("\n");
    assert_eq!(notice_fate_of(&answered, m), Some(NoticeFate::Answered));
    assert_eq!(queued_until(&answered, m), None);
    let wound_down = [
        limit_row("2026-09-27T15:04:28.344Z"),
        notice.clone(),
        said_row("2026-09-27T15:07:00Z", "Saved."),
    ]
    .join("\n");
    assert_eq!(
        notice_fate_of(&wound_down, m),
        Some(NoticeFate::Answered),
        "a limit before the notice is not its answer"
    );
    assert_eq!(notice_fate_of(&notice, m), Some(NoticeFate::Open));
    assert_eq!(
        notice_fate_of(&queued, "ATERM-UPGRADE-READY-00000000"),
        None
    );
    let overloaded = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 Overloaded"}]},"isApiErrorMessage":true,"error":"unknown"}"#;
    assert_eq!(
        notice_fate_of(&[notice.as_str(), overloaded].join("\n"), m),
        Some(NoticeFate::Open),
        "an API error that is no limit is passed over, as before"
    );
    let warning = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"Approaching usage limit · resets 7:30pm"}]},"isApiErrorMessage":true}"#;
    assert_eq!(
        notice_fate_of(&[notice.as_str(), warning].join("\n"), m),
        Some(NoticeFate::Open),
        "a warning is no limit"
    );
    let subagent = limit_row("2026-09-27T15:06:40Z")
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        notice_fate_of(&[notice.clone(), subagent].join("\n"), m),
        Some(NoticeFate::Open),
        "a subagent's limit is not the session's"
    );
    let unkeyed = limit_row("2026-09-27T15:06:40Z")
        .replace(r#""error":"rate_limit","#, "")
        .replace("weekly limit", "session limit");
    assert_eq!(
        notice_fate_of(&[notice, unkeyed].join("\n"), m),
        Some(NoticeFate::Queued),
        "the wall table's words read it"
    );
}

/// THE REDUCER ON A QUEUED NOTICE: one queued behind the limit (read
/// [`Facts::limited`] by the driver) is never asked again or given up on
/// however long it waits, and its window waits with it. NEGATIVE CONTROL:
/// the same notice read and left with no READY is given up on after the
/// last ask, as before — a round that then rests and asks again
/// ([`RETRY_S`]).
#[test]
fn a_queued_notice_is_never_given_up_on() {
    let tired = Phase::Announced {
        at_s: 1_000,
        asks: MAX_ASKS,
    };
    let late = 1_000 + REASK_S;
    let queued = Facts {
        limited: true,
        ..idle_facts()
    };
    for now in [late, late + 10 * REASK_S] {
        assert_eq!(
            next_step(&tired, &queued, false, now),
            Step::Wait("limited")
        );
    }
    assert_eq!(
        clock_held(&tired, &queued, late),
        Phase::Announced {
            at_s: late,
            asks: MAX_ASKS
        },
        "its window waits with it"
    );
    // NEGATIVE CONTROL.
    assert_eq!(next_step(&tired, &idle_facts(), false, late), Step::GiveUp);
    assert_eq!(
        next_step(
            &tired,
            &Facts {
                background_point: true,
                status: "busy".to_string(),
                ..idle_facts()
            },
            false,
            late
        ),
        Step::GiveUp
    );
}

/// THE LIMIT'S RESET, FROM THE TRANSCRIPT ([`transcript_limit_until`]): while
/// the session's last word is Claude Code's limit row, the instant it names
/// (`quotaLimits.resetsAt`) is how long the session stands at its limit —
/// what keeps even a first notice out of it once the banner has left the
/// screen. Claude Code's other rows of its own are passed over. A limit row
/// naming no reset holds [`REASK_S`] from when it was written, as a notice
/// queued behind one waits (review of 2026-09-27: read as no limit, a
/// pending upgrade typed a fresh notice minutes after one). NEGATIVE
/// CONTROLS: the model answering after it ends it; a row naming neither a
/// reset nor a time, a subagent's and none at all say nothing.
#[test]
fn the_limit_stands_until_the_reset_its_row_names() {
    let limit = limit_row("2026-09-27T15:04:28.344Z");
    let answered = said_row("2026-09-27T14:00:00Z", "Working.");
    let no_response = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
    assert_eq!(
        transcript_limit_until(&[answered.clone(), limit.clone()].join("\n")),
        Some(1_791_046_800)
    );
    assert_eq!(
        transcript_limit_until(&[limit.as_str(), no_response].join("\n")),
        Some(1_791_046_800),
        "passed over"
    );
    // NEGATIVE CONTROLS.
    let later = said_row("2026-09-27T19:58:05.554Z", "Pushing first.");
    assert_eq!(
        transcript_limit_until(&[limit.clone(), later].join("\n")),
        None
    );
    let unstamped = limit.replace(r#""resetsAt":1791046800,"#, "");
    assert_eq!(
        transcript_limit_until(&unstamped),
        Some(1_790_521_468 + REASK_S),
        "no reset named: REASK_S from its row"
    );
    let untimed = unstamped.replace(r#""timestamp":"2026-09-27T15:04:28.344Z","#, "");
    assert!(!untimed.contains("timestamp"), "{untimed}");
    assert_eq!(transcript_limit_until(&untimed), None);
    let subagent = limit.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        transcript_limit_until(&[answered.clone(), subagent].join("\n")),
        None
    );
    assert_eq!(transcript_limit_until(&answered), None);
    assert_eq!(transcript_limit_until(""), None);
    // A `/login` that finished after it ends it (review of 2026-09-27: the
    // owner's `Login successful` at 19:57:47Z, an account switched, the
    // reset days off) — and a limit hit again after it stands again.
    let login = person_row(
        "2026-09-27T19:57:47Z",
        "<local-command-stdout>Login successful</local-command-stdout>",
    );
    assert_eq!(
        transcript_limit_until(&[limit.clone(), login.clone()].join("\n")),
        None
    );
    assert_eq!(
        transcript_limit_until(&[login, limit].join("\n")),
        Some(1_791_046_800),
        "a login before the limit ends nothing"
    );
}

/// THE COPIES OF A QUEUED NOTICE, COUNTED UNTIL THE MODEL TAKES ONE
/// ([`REQUEUE_MAX`]): every upgrade notice typed since the model's last row
/// of its own is one more in the conversation, untaken — this notice's copies
/// and another's (an earlier round's: review of 2026-09-27), whatever
/// answered each (a limit, the login wall: the same review — a wall's answer
/// restarted the count, and limit and wall answers in turn could stack more
/// than the bound); a `/login` between them takes none (the copies stay); a
/// row of the model's own starts the count again. The FULL queue's rest
/// ([`Scan::rests_until`]) counts from the latest copy's limit row: its
/// reset, or [`REASK_S`] after a row naming none, then [`RETRY_S`]. NEGATIVE
/// CONTROLS: a notice not queued — answered, taken, open behind a queued
/// copy — has none counted, and no rest; a marker nothing carries, none.
#[test]
fn the_copies_of_a_queued_notice_are_counted_until_the_model_takes_one() {
    let m = QUEUED_MARKER;
    let at = |h: u32| format!("2026-09-27T{h:02}:06:39Z");
    let copy = |h: u32, marker: &str| [notice_row(&at(h), marker), limit_row(&at(h))];
    let rows = |parts: &[&[String]]| parts.concat().join("\n");
    assert_eq!(queued_copies(&rows(&[&copy(15, m)]), m), 1);
    let three = rows(&[&copy(15, m), &copy(16, m), &copy(17, m)]);
    assert_eq!(queued_copies(&three, m), 3);
    assert_eq!(queued_until(&three, m), Some(1_791_046_800));
    let login = person_row(
        "2026-09-27T15:30:00Z",
        "<local-command-stdout>Login successful</local-command-stdout>",
    );
    assert_eq!(
        queued_copies(&rows(&[&copy(15, m), &[login], &copy(16, m)]), m),
        2,
        "a /login takes no copy"
    );
    let took = [
        person_row("2026-09-27T15:40:00Z", "first, continue"),
        said_row("2026-09-27T15:40:10Z", "Pushing first."),
    ];
    assert_eq!(
        queued_copies(&rows(&[&copy(15, m), &copy(16, m), &took, &copy(17, m)]), m),
        1,
        "the model's row starts the count again"
    );
    // NEGATIVE CONTROLS.
    let taken = rows(&[&copy(15, m), &copy(16, m), &took]);
    assert!(matches!(
        notice_fate_of(&taken, m),
        Some(NoticeFate::Taken(_))
    ));
    assert_eq!(queued_copies(&taken, m), 0, "taken");
    let answered = rows(&[
        &copy(15, m),
        &[
            notice_row(&at(16), m),
            said_row("2026-09-27T16:06:50Z", "Saved."),
        ],
    ]);
    assert_eq!(queued_copies(&answered, m), 0, "answered");
    let open = rows(&[&copy(15, m), &[notice_row(&at(16), m)]]);
    assert_eq!(notice_fate_of(&open, m), Some(NoticeFate::Open));
    assert_eq!(queued_copies(&open, m), 0, "open");
    let other = "ATERM-UPGRADE-READY-504f8ea5";
    assert_eq!(
        queued_copies(
            &rows(&[&copy(15, other), &copy(16, other), &copy(17, m)]),
            m
        ),
        3,
        "another notice's copies wait untaken in the same conversation"
    );
    let walled = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-27T16:06:40Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"Login expired · Please run /login"}]},"isApiErrorMessage":true,"error":"authentication_failed"}"#;
    assert_eq!(
        queued_copies(
            &rows(&[
                &copy(15, m),
                &[notice_row(&at(16), m), walled.to_string()],
                &copy(17, m)
            ]),
            m
        ),
        3,
        "a copy the login wall answered is untaken too"
    );
    assert_eq!(queued_copies(&three, "ATERM-UPGRADE-READY-00000000"), 0);

    // THE REST of a full queue, from the latest copy's limit row.
    let scan = notice_scan(&three, m).expect("queued");
    assert_eq!(
        scan.rests_until(),
        Some(1_791_046_800 + RETRY_S),
        "a named reset, then the rest"
    );
    let unnamed = three.replace(r#""resetsAt":1791046800,"#, "");
    let last_row = notice_scan(&unnamed, m)
        .and_then(|scan| scan.answered_at)
        .expect("the last limit row's time");
    assert_eq!(
        notice_scan(&unnamed, m).and_then(|scan| scan.rests_until()),
        Some(last_row + REASK_S + RETRY_S),
        "no reset named: REASK_S, then the rest"
    );
    assert_eq!(notice_scan(&taken, m).and_then(|s| s.rests_until()), None);
    assert_eq!(notice_scan(&open, m).and_then(|s| s.rests_until()), None);

    // A queue past its room rests longer for every copy unread: five
    // copies, the named reset, then eight hours.
    let five = rows(&[
        &copy(13, m),
        &copy(14, m),
        &copy(15, m),
        &copy(16, m),
        &copy(17, m),
    ]);
    assert_eq!(
        notice_scan(&five, m).and_then(|scan| scan.rests_until()),
        Some(1_791_046_800 + 4 * RETRY_S),
        "five unread: the rest doubled twice"
    );
}

/// THE FULL QUEUE'S REST GROWS WITH EVERY COPY UNREAD, UP TO A DAY (the
/// owner, 2026-09-27: a limit naming no reset still added a copy every two
/// and a half hours for as long as it stood): two hours for the queue at its
/// room's end (`1 + REQUEUE_MAX` unread), then four, eight and sixteen, then
/// a day for every count past that — `QUEUE_REST_DOUBLINGS` rests shorter
/// than a day. NEGATIVE CONTROLS: the rest never exceeds a day, however many
/// wait (a count the scan saturates at), and a queue with room — which types
/// again straight away, never after a rest — reads the first rest.
#[test]
fn a_full_queues_rest_doubles_per_copy_unread_up_to_a_day() {
    let full = REQUEUE_MAX + 1;
    assert_eq!(RETRY_S, 2 * 3_600);
    assert_eq!(QUEUE_REST_MAX_S, 86_400);
    assert_eq!(QUEUE_REST_DOUBLINGS, 4);
    let hours: Vec<u64> = (full..full + 7).map(|n| queue_rest(n) / 3_600).collect();
    assert_eq!(hours, [2, 4, 8, 16, 24, 24, 24]);
    let short = (full..full + 20)
        .filter(|n| queue_rest(*n) < QUEUE_REST_MAX_S)
        .count();
    assert_eq!(short, usize::try_from(QUEUE_REST_DOUBLINGS).expect("small"));
    // NEGATIVE CONTROLS.
    for n in [full + 30, 1_000, u32::MAX] {
        assert_eq!(queue_rest(n), QUEUE_REST_MAX_S, "{n}");
    }
    for n in 0..full {
        assert_eq!(queue_rest(n), RETRY_S, "{n}");
    }
}
