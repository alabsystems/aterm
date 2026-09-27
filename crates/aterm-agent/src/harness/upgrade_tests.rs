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
        hold_s: 0,
        owner_now: false,
        attended: false,
        background_point: false,
        limited: false,
        ready_s: 0,
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
/// a box nobody answers or a draft nobody sends, standing [`HOLD_S`], is void —
/// before the bound it is still a wait, and the agent's own work (background
/// tasks, a busy turn) and a hold are waited for past it, never voided (the
/// negative controls: the owner's "never kill background work", and a void is not
/// a kill either). Once void the answer is gone, so the reducer asks again at the
/// next idle point, not later.
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

    let agents: [Knock; 5] = [
        (|f| f.background = vec!["zsh".to_string()], "background"),
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

/// TIER-1: the real reducer takes, on every state of the derived drain model
/// (`harness_upgrade_drain_bound_model`), the step the model's guards allow — a
/// person is a box that has stood [`HOLD_S`] (one a sweep has only just caught is
/// not yet a person's: `a_box_one_sweep_catches_is_not_a_persons_hold`), the
/// agent's own work a background shell, and the model's `Bound` sweeps are
/// [`DRAIN_S`]. Negative control: the model at `Buggy=1` (no void, the drain
/// before the bound) disagrees with the reducer at the bound.
#[test]
fn the_reducer_takes_the_step_the_drain_model_allows() {
    let model = aterm_spec::derive::harness_upgrade_drain_bound_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let bound: i64 = 2;
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    let mut disagreements = 0;
    for waited in 0..=bound {
        for ready in [0, 1] {
            for person in [0, 1] {
                for agent in [0, 1] {
                    let mut state = model.init_state();
                    for (var, value) in [
                        ("waited", waited),
                        ("ready", ready),
                        ("person", person),
                        ("agent", agent),
                    ] {
                        state.insert(var, value);
                    }
                    let mut f = idle_facts();
                    f.approval_box = person == 1;
                    f.hold_s = if person == 1 { HOLD_S } else { 0 };
                    if agent == 1 {
                        f.background = vec!["zsh".to_string()];
                    }
                    let now = 100
                        + DRAIN_S * u64::try_from(waited).unwrap() / u64::try_from(bound).unwrap();
                    let step = next_step(&asked, &f, ready == 1, now);
                    let decided = |m: &aterm_spec::derive::Model| {
                        [
                            m.action_enabled("Terminate", &state),
                            m.action_enabled("Void", &state),
                            m.action_enabled("Wait", &state),
                        ]
                    };
                    let real = [
                        step == Step::Terminate,
                        matches!(step, Step::Void(_)),
                        !matches!(step, Step::Terminate | Step::Void(_)),
                    ];
                    assert_eq!(real, decided(&model), "{state:?} -> {step:?}");
                    disagreements += usize::from(real != decided(&buggy));
                }
            }
        }
    }
    assert!(disagreements > 0, "the unbounded drain is caught");
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
fn a_listed_model_is_said_as_the_lists_choice_and_a_miss_as_not_taken() {
    assert_eq!(
        restart_outcome_listed(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5-5"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (the priority \
         list chose it; /model changes it)"
    );
    assert_eq!(
        restart_outcome_listed(
            "2.1.282",
            Some("claude-opus-5"),
            Some("claude-opus-5"),
            "claude-opus-5-5"
        ),
        "claude restarted on 2.1.282 · model claude-opus-5 (the priority list asked for \
         claude-opus-5-5; it was not taken)"
    );
    assert_eq!(
        restart_outcome_listed("2.1.282", None, None, "claude-opus-5-5"),
        "claude restarted on 2.1.282 · model unconfirmed (asked for claude-opus-5-5)"
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
    );
    assert!(only.starts_with(ANNOUNCE_HEAD), "{only}");
    assert!(only.contains("--model claude-opus-5-5") && only.contains(&marker));
    assert!(only.contains("do not cancel them"));
    let both = prepare_prompt_with_model(
        &v("2.1.280"),
        &v("2.1.282"),
        Source::Managed,
        &marker,
        Some("claude-opus-5-5"),
    );
    assert!(both.starts_with(ANNOUNCE_HEAD) && both.contains("2.1.282 (managed) is installed"));
    assert_eq!(
        prepare_prompt_with_model(&v("2.1.280"), &v("2.1.282"), Source::Managed, &marker, None),
        prepare_prompt(&v("2.1.280"), &v("2.1.282"), Source::Managed, &marker)
    );
    let cont = continue_prompt_with_model(
        &v("2.1.282"),
        &v("2.1.282"),
        Some("claude-opus-5"),
        Some("claude-opus-5-5"),
    );
    assert!(cont.contains("with claude-opus-5-5") && cont.contains("it ran claude-opus-5 before"));
    assert_eq!(
        continue_prompt_with_model(&v("2.1.280"), &v("2.1.282"), None, None),
        continue_prompt(&v("2.1.280"), &v("2.1.282"), None)
    );
}

/// THE FIRST NOTICE AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK (the
/// owner's answer of 2026-09-26: "Busy agentic sessions get upgraded at their
/// next natural break. The notice interrupts the agent's orchestration once,
/// and the restart still never kills running work"): Claude's own `busy` (a
/// workflow it waits on) or `shell` status is no wait there, and the loop's
/// settle stands for the screen's (a workflow's progress line never holds
/// still) — but only for the FIRST notice: a re-ask, READY's restart and a
/// drain are an idle point's (`background`), and the restart's gate still
/// asks for nothing running under the agent. NEGATIVE CONTROLS: a person, a
/// hold, a box, a draft, a live turn's busy footer and a dialog's `waiting`
/// still wait at the break; and the same `busy` session at an idle point's
/// step waits `not-idle`, as before.
#[test]
fn a_break_of_background_work_takes_the_first_notice_and_nothing_else() {
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
    // Only the first notice: a re-ask and READY's restart wait for an idle
    // point, whatever the facts say.
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&announced, &at_break("busy"), false, REASK_S + 1),
        Step::Wait("background")
    );
    assert_eq!(
        next_step(&announced, &at_break("idle"), true, 10),
        Step::Wait("background")
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

/// A READY THE RESTART'S GATE HOLDS PAST THE DRAIN IS VOID — an announced
/// upgrade's and a gave-up one's late answer alike ([`void_of`]): a person's
/// box or draft that has stood [`HOLD_S`], or the agent's own background work
/// still running [`DRAIN_S`] after the answer (review of 2026-09-26: the
/// gave-up arm had no void at all, so a READY a person's hold kept for
/// hours ended the agent the moment it cleared, and one a
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
        assert_eq!(at(&with(working, drained, 0)), Step::Void("background"));
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
    // Only an idle point types it: a break of the agent's own background
    // work is the notice's alone.
    let at_break = Facts {
        status: "busy".to_string(),
        background_point: true,
        ..idle_facts()
    };
    assert_eq!(gate_release(&at_break, false), Gate::Wait("not-idle"));

    let text = release_prompt(Agent::Claude);
    assert_eq!(
        text,
        "[aterm harness] Upgrade off: The Claude Code upgrade is off for now: nothing will \
         restart this session, and nothing the notice asked of you still applies. Carry on as \
         you would have without it."
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
