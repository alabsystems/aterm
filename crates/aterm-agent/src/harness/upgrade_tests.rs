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

/// The session file Claude Code 2.1.280 writes for a session.
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
    // Native 2.1.280 running, managed 2.1.280, and the native updater put
    // 2.1.281 on disk -> the native 2.1.281.
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
    // A bare --resume (the picker) becomes the id.
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
    // `<values...>` requires a value: Claude's parser takes the first value
    // whatever it looks like, so a flag left bare would swallow the appended
    // `--resume <id>` as its own values.
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

/// Claude's own parser, transcribed branch for branch: `parseOptions` of the
/// Commander bundled in Claude Code 2.1.281. Its options are the [`FLAGS`]
/// table: `One` and `Many` are required values, `Many` the variadic one,
/// `Optional` an optional value, `None` a boolean. The positional-options stop
/// is left out: it fires only on an operand naming a subcommand (`claude mcp
/// …`), never a live session.
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
    // unknown flag, each fed to the rewrite and to Claude's own parser.
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
    // macOS keeps at most 1023 bytes of a line typed before the shell's line
    // editor is back; `--resume <id>` ends the line, so any cut loses it.
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let line = |tty: LineDiscipline, pad: &str| {
        relaunch_line_on(
            tty,
            Dialect::Zsh,
            Some(hook),
            None,
            &[],
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
    // Each kernel keeps its own bound: Linux keeps 4095 bytes of a line, so a
    // 2048-byte line is carried there.
    assert!(at(LineDiscipline::Linux, 2048).is_ok());
    // macOS drops a 1024-byte line.
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

/// The healed relaunch line: the shell takes its key from the file first, in
/// its own dialect, then runs the relaunch line unchanged, still one line led
/// by the space that keeps it out of history. The path is quoted like every
/// other word; the key is not in the line. A path with a control character,
/// or a healed line past the kernel's bound, is refused and the caller types
/// the plain line.
#[test]
fn the_relaunch_line_takes_the_key_first_in_the_shells_own_dialect() {
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let key = Path::new("/Users/_me/Library/Application Support/aterm/rekey/s-0a.1");
    let q = "'/Users/_me/Library/Application Support/aterm/rekey/s-0a.1'";
    let args = argv(&["--resume", ID]);
    let zsh = relaunch_line(Dialect::Zsh, Some(hook), Some("/w"), exe, &args).unwrap();
    // The upgrading form where it fits: the shell's own text, the quoted path
    // its only path.
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
    // The key-only form beneath it: the key read, and the suffix built, inside
    // the group whose stderr (a `set -x` trace too) is silenced.
    assert_eq!(
        aterm_shell_integration::typed_rekey(aterm_shell_integration::ShellType::Zsh, q)
            .expect("scripted"),
        format!(
            "{{ read -r __aterm_shell_nonce <{q}; \
             __aterm_id_suffix_str=\";id=$__aterm_shell_nonce\"; }} 2>/dev/null; \
             command rm -f -- {q};"
        )
    );
    // Quoted like any word: a quote in the path cannot end the word early.
    let odd = with_rekey(Dialect::Zsh, &bash, Path::new("/r/it's")).unwrap();
    assert!(odd.contains("<'/r/it'\\''s'; "), "{odd}");
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
                &[],
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

/// The notice's list of what runs: each process by pid, name, age and command
/// on one line a paste cannot split (no control characters), at most
/// [`HELD_NAMED`] named and the rest counted. Nothing when nothing runs.
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
    // The notice stays one line with the clause before its closing sentence,
    // so its last word is that sentence's, the word the guarded Enter waits on.
    let notice = with_running(
        &prepare_prompt(&v("2.1.278"), &v("2.1.283"), Source::Managed, "M"),
        &two,
    );
    assert!(!notice.contains('\n') && notice.starts_with(ANNOUNCE_HEAD));
    assert!(
        notice.ends_with(&format!("{} {NOTICE_CLOSE}", two.trim_end())),
        "{notice}"
    );
}

/// The oldest process is named first, ties in the order given, so a
/// long-running watch loop is never left among the counted ones past
/// [`HELD_NAMED`]. Negative control: the process table's order names none of
/// the watch.
#[test]
fn the_notice_names_the_oldest_processes_first() {
    let held = |pid: u32, age_s: u64| Held {
        pid,
        name: "zsh".to_string(),
        age_s,
        command: String::new(),
    };
    // Fourteen young shells the table lists first, the four-day watch last.
    let mut table: Vec<Held> = (0..14)
        .map(|i| held(60_000 + i, 60 + u64::from(i)))
        .collect();
    table.push(held(51_435, 4 * 86_400 + 22 * 3_600 + 26 * 60));
    let list = held_list(&table);
    assert!(
        list.starts_with("pid 51435 (zsh, 4d22h); pid 60013 (zsh, 1m); pid 60012 (zsh, 1m)"),
        "{list}"
    );
    assert!(list.ends_with("; and 10 more"), "{list}");
    assert_eq!(list.matches("pid ").count(), HELD_NAMED);
    // Ties keep the order given.
    let tied = held_list(&[held(3, 5), held(1, 5), held(2, 9)]);
    assert_eq!(tied, "pid 2 (zsh, 9s); pid 3 (zsh, 5s); pid 1 (zsh, 5s)");
    // Negative control: the table's order leaves the watch unnamed.
    let old_named: Vec<u32> = table.iter().take(HELD_NAMED).map(|h| h.pid).collect();
    assert!(!old_named.contains(&51_435), "{old_named:?}");
}

/// A real closer command: 253 characters, 119 of them the one path in `D=`.
const CLOSER: &str = "D=/Users//person/.claude/projects/-Users-person/\
    00000000-0000-4000-8000-000000000001/subagents/workflows/wf_examplerun-1; \
    tail -f -n +1 \"$D/journal.jsonl\" | /usr/bin/grep -m1 -F \
    '\"agentId\":\"a0000000000000001\",\"result\"' > /dev/null; echo \"closer finished\"";

/// The notice keeps what makes a wait dead: a command over
/// [`HELD_COMMAND_CHARS`] has the middle of each long path elided before the
/// cut, so the `grep -m1` that bounds a `tail -f` survives. The cut stays the
/// backstop.
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

    // Negative controls: a command that fits is quoted whole, however long its
    // paths. A URL is never a path, its query included.
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
    // Only a rooted path is elided: a long relative one is quoted whole, as
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
    // The cursor can only PROVE typed text (on the caret row, off column 2),
    // never placeholder: elsewhere the dim cell decides — a grid displaced by
    // a net-zero re-grid leaves the cursor a row below the composer.
    assert!(
        composer_is_empty(&suggestion, None, true),
        "no cursor: the dim cell decides"
    );
    assert!(
        !composer_is_empty(&suggestion, None, false),
        "no cursor, not dim: typed"
    );
    assert!(
        composer_is_empty(&suggestion, Some((2, 2)), true),
        "displaced: the cursor one row below the composer"
    );
    assert!(
        composer_is_empty(&suggestion, Some((0, 2)), true),
        "displaced the other way"
    );
    assert!(
        !composer_is_empty(&suggestion, Some((2, 2)), false),
        "displaced, typed text is still a draft"
    );
    assert_eq!(composer_dim_row(&suggestion, Some((1, 2))), Some(1));
    assert_eq!(composer_dim_row(&suggestion, Some((2, 2))), Some(1));
    assert_eq!(composer_dim_row(&suggestion, None), Some(1));
    assert_eq!(
        composer_dim_row(&suggestion, Some((1, 17))),
        None,
        "inside the text: nothing to ask"
    );
    assert_eq!(
        composer_dim_row(&blank, Some((1, 2))),
        Some(1),
        "blank: read, as it always was"
    );
    let two_rows = rows(&[
        "─────────────────",
        "❯ ",
        "second line",
        "─────────────────",
    ]);
    assert!(!composer_is_empty(&two_rows, Some((1, 2)), true));
    // A live screen while the person types `we'd want`.
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
        rung: Rung::Prefer,
        typing: false,
        still_s: 0,
        daemon_turn: DaemonTurn::None,
        work_ended: false,
        ready_unrecorded: false,
        ready_superseded: false,
        quiet: false,
        unnoticed: false,
    }
}

/// One fact knocked off an otherwise-go reading, and the wait it must cause.
type Knock = (fn(&mut Facts), &'static str);

/// THE QUIET UPGRADE, THE DEFAULT (the owner: "never
/// interrupt an agent that is working"; [`Facts::quiet`], [`quiet_step`]):
/// at a genuinely idle point — Claude's own `idle`, nothing under the agent,
/// no box, no draft, nobody at the tab, settled — a pending round RESTARTS
/// the session with nothing typed first (`Terminate`, never `Announce`), and
/// needs no READY; anywhere else it WAITS with the gate's own word, and
/// never types a notice: a turn still running, work under the agent, a break
/// of its own background work (a workflow, a background agent), a box, a
/// draft, a person at the tab, a hold, the settle, a usage limit, the login
/// wall. A round an older aterm announced is restarted at the same point,
/// READY or not. The owner is told first: a move the waiting record has not
/// stood a look for waits `notice-shown` ([`Facts::unnoticed`]). THE KNOB
/// (`upgrade_ask = true`, `quiet` false) is the ask-and-drain flow:
/// `Announce` at the same idle point, `awaiting-ready` after it. A restart
/// in flight, a finished move and a stopped round are the old reducer's.
#[test]
fn a_quiet_upgrade_restarts_only_a_genuinely_idle_session_and_types_nothing() {
    let quiet = Facts {
        quiet: true,
        ..idle_facts()
    };
    let announced = Phase::Announced { at_s: 100, asks: 1 };
    assert_eq!(
        next_step(&Phase::Pending, &quiet, false, 200),
        Step::Terminate
    );
    assert_eq!(next_step(&announced, &quiet, false, 200), Step::Terminate);
    assert_eq!(next_step(&announced, &quiet, true, 200), Step::Terminate);
    let knocks: [Knock; 13] = [
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.status = "shell".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.background = vec!["zsh".into()], "background"),
        (|f| f.background_point = true, "background"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
        (|f| f.attended = true, "attended"),
        (|f| f.held = true, "held"),
        (|f| f.quiet_s = 3, "settling"),
        (|f| f.limited = true, "limited"),
        (|f| f.login = true, "login"),
        (|f| f.unnoticed = true, NOTICE_SHOWN),
    ];
    for (edit, word) in knocks {
        let mut f = quiet.clone();
        edit(&mut f);
        for phase in [&Phase::Pending, &announced] {
            let step = next_step(phase, &f, false, 200);
            assert_eq!(step, Step::Wait(word), "{phase:?}");
            assert_ne!(step, Step::Announce, "nothing is ever typed first");
        }
    }
    // The old reducer's phases are its own.
    assert_eq!(
        next_step(&Phase::Exiting { at_s: 1 }, &quiet, false, 5),
        Step::Wait("in-flight")
    );
    assert_eq!(
        next_step(&Phase::Done, &quiet, false, 5),
        Step::Wait("done")
    );
    assert_eq!(
        next_step(&Phase::Failed("x".into()), &quiet, false, 5),
        Step::Wait("failed")
    );
    // The knob: the ask-and-drain flow at the very same point.
    let ask = idle_facts();
    assert_eq!(next_step(&Phase::Pending, &ask, false, 200), Step::Announce);
    assert_eq!(
        next_step(&announced, &ask, false, 200),
        Step::Wait("awaiting-ready")
    );
}

/// A ROUND AN OLDER BUILD ANNOUNCED STILL ENDS UNDER THE QUIET UPGRADE
/// ([`quiet_step`]): the agent was told to wind down, and the quiet
/// flow asks it nothing again. Once the agent's own work — a break of it,
/// a shell under it, a status its idle screen does not bear out — has held
/// the restart a whole [`REASK_S`] past the notice (or past a READY), the
/// round GIVES UP at any ask, so the driver owes the agent its release; a
/// READY a person's box held past [`DRAIN_S`] is void. The incident's tab:
/// behind for days, `wait=background` at every look, `Announced{asks:3}`
/// written by 0.99 — answered `Wait("background")` for ever, no release.
/// CONTROL: the ask flow gives up there at its last ask. NEGATIVE CONTROLS:
/// inside the window the restart's wait stands; a pending round (nothing
/// asked) is never given up on; a genuinely idle point restarts; a person at
/// the tab, or the settle, is no work of the agent's and never gives up.
#[test]
fn a_quiet_round_an_older_build_announced_still_ends_and_owes_its_release() {
    let brk = Facts {
        quiet: true,
        background_point: true,
        ..idle_facts()
    };
    let last = Phase::Announced {
        at_s: 0,
        asks: MAX_ASKS,
    };
    let first = Phase::Announced { at_s: 0, asks: 1 };
    let later = 10 * REASK_S;
    // CONTROL: the ask flow gives up at its last ask.
    let asking = Facts {
        quiet: false,
        ..brk.clone()
    };
    assert_eq!(next_step(&last, &asking, false, later), Step::GiveUp);
    // The quiet flow gives up at any ask: it never asks again.
    for phase in [&last, &first] {
        assert_eq!(
            next_step(phase, &brk, false, later),
            Step::GiveUp,
            "{phase:?}"
        );
    }
    // READY given, the agent's own shell under it at an idle point: given
    // up on once the READY has stood the window, never waited on for good.
    let held = Facts {
        quiet: true,
        background: vec!["zsh".into()],
        ready_s: REASK_S,
        ..idle_facts()
    };
    assert_eq!(next_step(&first, &held, true, later), Step::GiveUp);
    assert_eq!(
        next_step(
            &first,
            &Facts {
                ready_s: REASK_S - 1,
                ..held.clone()
            },
            true,
            later
        ),
        Step::Wait("background"),
        "a late READY gets its whole window"
    );
    // A status its idle screen does not bear out holds it the same way.
    let stale = Facts {
        quiet: true,
        status: "shell".into(),
        status_age_s: QUIET_S,
        idle_looks: IDLE_LOOKS,
        ..idle_facts()
    };
    assert_eq!(next_step(&first, &stale, false, later), Step::GiveUp);
    // A person's box past the drain voids a READY, as under the ask flow.
    let boxed = Facts {
        quiet: true,
        approval_box: true,
        hold_s: HOLD_S,
        ..idle_facts()
    };
    assert_eq!(next_step(&first, &boxed, true, DRAIN_S), Step::Void("box"));
    assert_eq!(
        next_step(&first, &boxed, false, later),
        Step::Wait("box"),
        "no READY: nothing to void, and a person's box is no work of the agent's"
    );
    // NEGATIVE CONTROLS.
    assert_eq!(
        next_step(&first, &brk, false, REASK_S - 1),
        Step::Wait("background"),
        "inside the notice's window"
    );
    assert_eq!(
        next_step(&Phase::Pending, &brk, false, later),
        Step::Wait("background"),
        "a pending round asked nothing"
    );
    let idle = Facts {
        quiet: true,
        ..idle_facts()
    };
    assert_eq!(next_step(&first, &idle, false, later), Step::Terminate);
    for (edit, word) in [
        ((|f| f.attended = true) as fn(&mut Facts), "attended"),
        (|f| f.quiet_s = 3, "settling"),
        (|f| f.status = "busy".into(), "not-idle"),
    ] {
        let mut f = idle.clone();
        edit(&mut f);
        assert_eq!(next_step(&first, &f, false, later), Step::Wait(word));
    }
}

/// A STATUS THAT ONLY LAGS HOLDS THE QUIET RESTART UNTIL THE OWNER'S WORD
/// ([`quiet_step`]): Claude's own `shell`/`busy` left over
/// an idle screen — an agent that finished, or asked its person something,
/// with nothing under it — let the ask flow's notice through (its turn
/// rewrote the status), but the quiet flow types nothing, so it waited
/// `status-stale` at every look, at the Land rung and under the owner's
/// `Upgrade now` alike. The restart still never kills on that status
/// unasked; the owner's `--now` reads the lag past. NEGATIVE CONTROLS under
/// `--now`: a status not yet shown to lag (one look short, too young), work
/// under the agent, a box, and the ask flow (whose notice goes through the
/// lag, so its `--now` waives no status).
#[test]
fn a_quiet_round_is_not_held_for_ever_by_a_status_its_idle_screen_contradicts() {
    let stale = Facts {
        quiet: true,
        status: "shell".into(),
        status_age_s: 3_600,
        idle_looks: IDLE_LOOKS,
        ..idle_facts()
    };
    // CONTROL: the ask flow's notice goes through the lag.
    let asking = Facts {
        quiet: false,
        ..stale.clone()
    };
    assert_eq!(
        next_step(&Phase::Pending, &asking, false, 200),
        Step::Announce
    );
    for status in ["shell", "busy"] {
        let stale = Facts {
            status: status.into(),
            ..stale.clone()
        };
        // Unasked, never on the status alone — at the last rung too.
        let land = Facts {
            rung: Rung::Land,
            ..stale.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &land, false, 30 * 86_400),
            Step::Wait("status-stale"),
            "{status}"
        );
        // The owner's word moves it.
        assert_eq!(
            requested_step(&Request::Now, &Phase::Pending, &stale, false, 200, "9.9.9"),
            Step::Terminate,
            "{status}"
        );
    }
    // NEGATIVE CONTROLS under the owner's word.
    let knocks: [Knock; 4] = [
        (|f| f.idle_looks = IDLE_LOOKS - 1, "status-stale"),
        (|f| f.status_age_s = QUIET_S - 1, "status-stale"),
        (|f| f.background = vec!["zsh".into()], "background"),
        (|f| f.approval_box = true, "box"),
    ];
    for (edit, word) in knocks {
        let mut f = stale.clone();
        edit(&mut f);
        assert_eq!(
            requested_step(&Request::Now, &Phase::Pending, &f, false, 200, "9.9.9"),
            Step::Wait(word)
        );
    }
    let announced = Phase::Announced { at_s: 100, asks: 1 };
    assert_eq!(
        requested_step(&Request::Now, &announced, &asking, true, 200, "9.9.9"),
        Step::Wait("status-stale"),
        "the ask flow's --now waives no status"
    );
}

/// NO UNTASKED SESSION IS RESTARTED WITHOUT A NOTICE: the fresh restart of a
/// conversation with no task waits `notice-shown` until the owner's waiting
/// record has stood a look ([`Facts::unnoticed`]) — under the knob too —
/// and then goes.
/// NEGATIVE CONTROL: a told owner's session is restarted afresh at once.
#[test]
fn an_untasked_session_is_restarted_only_after_the_owner_is_told() {
    for quiet in [false, true] {
        let taskless = Facts {
            taskless: true,
            quiet,
            ..idle_facts()
        };
        let untold = Facts {
            unnoticed: true,
            ..taskless.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &untold, false, 200),
            Step::Wait(NOTICE_SHOWN),
            "quiet={quiet}"
        );
        assert_eq!(
            next_step(&Phase::Pending, &taskless, false, 200),
            Step::Fresh,
            "quiet={quiet}"
        );
    }
}

/// Only a box (a visible one, or Claude's own `waiting` status) and a typed
/// draft are a person's to clear. The agent's own work, aterm's hold and the
/// settle window are not; each still shuts the gate.
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

/// The drain is bounded against a person: a READY answer held past
/// [`DRAIN_S`] by a box or draft standing [`HOLD_S`] is void; before the
/// bound it is a wait. A busy turn and a hold are waited for past it and
/// never voided. Background work under the agent is never voided or ended,
/// but past [`REASK_S`] it is asked about again, and past [`MAX_ASKS`] the
/// upgrade gives up. A void answer is re-asked at the next idle point.
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
    // Work under the agent is waited for and never voided or ended. A READY
    // answer it outlives by a whole REASK_S is asked about again; past
    // MAX_ASKS the upgrade gives up. It never ends the agent.
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

    // After the void the answer is gone: the re-ask at the next idle point.
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

/// One sample of a box is not a person's hold: the supervisor answers a box
/// it can read within seconds and Claude's status reads `waiting` meanwhile.
/// A hold voids only once it has stood for [`HOLD_S`], sample after sample
/// (the negative control: the same box, held that long).
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

/// A hold stands only while it is sampled: a gap longer than [`HOLD_GAP_S`]
/// (a system sleep, a driver restart) begins the hold again, so two samples
/// hours apart are a wait. Negative control: a box sampled once a minute for
/// [`HOLD_S`] still voids, and a gap of exactly [`HOLD_GAP_S`] carries the
/// hold on.
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
/// re-ask clocks into one `Bound`, and the bind below samples only
/// [`DRAIN_S`]; the equality is the model's assumption, pinned here.
const _: () = assert!(DRAIN_S == REASK_S);

/// Tier-1: on every state of the derived drain model
/// (`harness_upgrade_drain_bound_model`), the real reducer takes the step the
/// model's guards allow. The mapping:
/// - A person is a box that has stood [`HOLD_S`]
///   (`a_box_one_sweep_catches_is_not_a_persons_hold`).
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
/// (no void, a silent wait on the agent's work, the notice's clock for a
/// READY answer), `LagEnds=1` (a restart on a lagging status) and
/// `LagHolds=1` (a READY behind the lag waited on in silence) the model
/// disagrees with the reducer.
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

/// A break ends nothing ([`break_step`]): a notice, a void, a give-up and a
/// wait pass through it; the one step that ends the agent becomes a wait on
/// its work. Both drivers apply this rule.
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

/// A READY answer gets its own clock: an agent that winds down for 35 minutes
/// and answers READY while its own shell is still exiting is not given up on
/// at the first look. The answer's clock waits a whole [`REASK_S`], at a
/// break and at an idle point alike; if the shell ends inside it, the agent
/// is restarted.
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
        // Negative control: an answer as old as its notice gives up at once.
        f.ready_s = late;
        assert_eq!(next_step(&last, f, true, late), Step::GiveUp);
    }
    // The shell ended inside the answer's interval: the restart goes.
    let mut done = idle_facts();
    done.ready_s = 60;
    assert_eq!(next_step(&last, &done, true, late + 60), Step::Terminate);
}

/// A person at a busy tab is asked first: a person at the tab waits
/// `attended`, busy or idle, before the notice and before the restart.
/// Negative controls: the same busy visit unattended waits `not-idle`; under
/// the owner's `--now` the person is waived and the busy tab waits on its
/// turn alone.
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

/// One presence fact: a session is attended when a person gave it input
/// within `[harness] human_grace_s`, by the server's per-session stamp, never
/// by where the tab sits or the machine's input clock. A host that sends no
/// stamp proves no absence: attended.
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

/// A person at the tab ([`Facts::attended`]): nothing the upgrade decides for
/// itself is typed into it, and, READY or not, it is never signalled there,
/// since the SIGTERM cannot be taken back. Only the owner's `--now` waives it
/// ([`Facts::owner_now`]). Negative controls: the same READY facts unattended
/// are ended, and an attended tab is still refused a draft, a box and a busy
/// Claude under `--now`.
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

/// The READY answer must be the agent's last word: a READY row the agent has
/// since answered past (a `keep going` it took, a person's message) is no
/// consent to a restart. Negative controls: a subagent's row and a
/// `<synthetic>` row after the answer supersede nothing.
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

/// A notice ends every READY before it: a main-chain user row that is an
/// announcement ends an earlier READY, even when only a `<synthetic>` row or
/// a person's Esc follows. Negative controls: a READY after the notice
/// answers it; a user row that only mentions the head mid-sentence is no
/// notice, and a subagent's notice ends nothing.
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
    // Negative controls: a mention mid-sentence, and a subagent's notice. The
    // mention is a person's direction (it ends the READY as one), never a
    // notice: a notice ends a direction the agent took up before it.
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

/// A direction after the READY ends it: a main-chain user row that directs
/// the conversation (a person's Esc, a message met only by a `<synthetic>`
/// row, a peer's message) ends the READY before it, whether or not the agent
/// has written a real row since. Negative controls: rows Claude Code writes
/// on its own after the answer (the limit's reset, a tool result, a task
/// notification, a command's output, the harness's line, a subagent's
/// prompt) end nothing, and a READY given again after the direction answers.
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

/// A READY answer is a line that is the marker alone ([`is_ready_line`]):
/// words that only quote a marker are no answer. Negative controls: the
/// marker on its own line, in backticks or indented, is one; another marker,
/// or one cut short, is not.
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

/// An assistant row as Claude Code 2.1.282 writes one (`isSidechain`, `type`,
/// `message.model`, the content blocks), naming `model`.
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

/// Claude Code's own assistant rows name the model `<synthetic>` (a limit
/// notice, `No response requested.` after a restart).
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
         2.1.281); it ran claude-opus-5 before the restart and was resumed. If you stopped a \
         watch or loop for this restart, start it again now. Continue where you left off. If \
         you were waiting on the user, nobody is here to answer: decide for yourself, prefer \
         reversible steps, and keep going."
    );
    assert!(!said.starts_with(ANNOUNCE_HEAD), "never the announcement");
    // No model known: the clause is left out, not guessed.
    let bare = continue_prompt(&from, &to, None);
    assert_eq!(
        bare,
        "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.282 (from \
         2.1.281) and resumed. If you stopped a watch or loop for this restart, start it again \
         now. Continue where you left off. If you were waiting on the user, nobody is here to \
         answer: decide for yourself, prefer reversible steps, and keep going."
    );
    // Both continuations end on the same instruction, and it never invites a
    // "waiting on the user" report.
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

/// A restart that moves the model says what ran before and that it came back
/// on a different model, the model after being the one its relaunch line
/// asked for.
#[test]
fn a_model_move_says_what_ran_before_and_what_runs_now() {
    let (from, to) = (v("2.1.282"), v("2.1.283"));
    // Negative control: the notice is unchanged for a restart that moves no
    // model.
    assert_eq!(
        continue_prompt_with_model(&from, &to, Some("claude-opus-5"), None),
        format!(
            "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283 (from \
             2.1.282); it ran claude-opus-5 before the restart and was resumed. \
             {RESTART_WATCHES} {CARRY_ON}"
        )
    );
    assert_eq!(
        continue_prompt_with_model(&from, &to, Some("claude-opus-5"), Some("claude-opus-5-5")),
        format!(
            "[aterm harness] Upgraded: this session was restarted on Claude Code 2.1.283 (from \
             2.1.282) and resumed; it ran claude-opus-5 before the restart and now runs \
             claude-opus-5-5, the newest model of its family (for this session only: your \
             default model is unchanged). {RESTART_WATCHES} {CARRY_ON}"
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
    // A move across families is the priority list's (only with nothing newer
    // of its own family), and is said so.
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
             unchanged). {RESTART_WATCHES} {CARRY_ON}"
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

/// The reason for a change is what decided the model after: a launch that
/// named `--model` keeps it on the relaunch (undoing a `/model` choice made
/// since, and resolving an alias against the new build), so the default is
/// not blamed.
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

/// The kept `--model` is read by the rewrite's own parse: the last one wins,
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

/// The resumed conversation of a launch, by the same parse: `--resume <id>`,
/// `-r <id>` and `--resume=<id>` name it (the last flag winning); a resume
/// that names none the process keeps (`--continue`, `-c`, a bare `--resume`,
/// or any resume with `--fork-session`) is `Some(None)`; a launch with no
/// resume flag, or whose `--resume` is another flag's value or after `--`,
/// resumed nothing.
#[test]
fn the_launch_resume_names_the_conversation_the_process_kept() {
    let got = |w: &[&str]| launch_resume(&argv(w));
    let named = |id: &str| Some(Some(id.to_owned()));
    assert_eq!(got(&["claude", "--resume", "abc"]), named("abc"));
    assert_eq!(got(&["claude", "-r", "abc"]), named("abc"));
    assert_eq!(got(&["claude", "--resume=abc"]), named("abc"));
    assert_eq!(
        got(&["claude", "--model", "opus", "--resume", "abc", "--verbose"]),
        named("abc")
    );
    assert_eq!(
        got(&["claude", "--continue", "--resume", "abc"]),
        named("abc"),
        "the last one wins"
    );
    assert_eq!(got(&["claude", "--resume", "abc", "-c"]), Some(None));
    assert_eq!(got(&["claude", "--continue"]), Some(None));
    assert_eq!(got(&["claude", "-c"]), Some(None));
    assert_eq!(got(&["claude", "--resume"]), Some(None), "the picker");
    assert_eq!(
        got(&["claude", "--resume", "--verbose"]),
        Some(None),
        "a bare resume before another flag"
    );
    assert_eq!(
        got(&["claude", "--resume", "abc", "--fork-session"]),
        Some(None),
        "a fork takes a new id"
    );
    assert_eq!(got(&["claude"]), None);
    assert_eq!(got(&["claude", "--model", "opus"]), None);
    assert_eq!(
        got(&["claude", "--append-system-prompt", "--resume", "x"]),
        None,
        "a value, not the flag"
    );
    assert_eq!(got(&["claude", "--", "--resume", "x"]), None);
}

/// The resumed model is the new process's: every transcript row carries the
/// `version` that wrote it, so a row the old process wrote past the mark
/// (alive past its SIGTERM for one more turn) is never taken as the answer.
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
/// tab, the attended-tab guard) and nothing else: a draft, a box, the busy
/// footer, a non-idle status and a hold still wait, and a restart still needs
/// the READY answer and an empty process tree. Negative control: the same
/// reading with no request waits `settling`.
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

/// A deferral is SHOWN as the time it has left, never its unix second.
#[test]
fn a_deferral_is_shown_as_the_time_it_has_left() {
    let now = 1_790_000_000;
    assert_eq!(Request::DeferUntil(now + 6 * 3_600).shown(now), "defer:6h");
    assert_eq!(Request::DeferUntil(now + 90).shown(now), "defer:1m");
    assert_eq!(Request::DeferUntil(now).shown(now), "defer:due");
    assert_eq!(Request::Now.shown(now), "now");
    assert_eq!(Request::Skip("2.1.282".into()).shown(now), "skip:2.1.282");
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

/// The notice at a break of the agent's own background work: busy agentic
/// sessions are upgraded at their next natural break; the notice interrupts
/// once and the restart never kills running work. Claude's own `busy` (a
/// workflow it waits on) or `shell` status is no wait there, and the loop's
/// settle stands for the screen's. The break takes only notices: the first,
/// and a re-ask once a whole [`REASK_S`] has passed with the work still
/// running; past [`MAX_ASKS`] the upgrade gives up. READY's restart and a
/// void are an idle point's (`background`). Negative controls: a person, a
/// hold, a box, a draft, a live turn's busy footer and a dialog's `waiting`
/// still wait at the break; the same `busy` session at an idle point waits
/// `not-idle`.
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
    // A notice out, the agent's poll loops running on, a break every minute:
    // once a whole REASK_S has passed the break asks again, READY or not.
    // That is never an end, and past MAX_ASKS it is a give-up. A READY given
    // with the notice has stood the same whole REASK_S.
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
        // At its limit, a break asks nothing and gives up on nothing: the
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
    // Negative control: no break, so the busy session waits.
    let busy_at_idle_point = Facts {
        status: "busy".to_string(),
        ..idle_facts()
    };
    assert_eq!(
        next_step(&Phase::Pending, &busy_at_idle_point, false, 1_000),
        Step::Wait("not-idle")
    );
}

/// Claude's own status is read against its screen and the work under it.
/// Once the session's reader has read the screen authoritatively idle for
/// [`IDLE_LOOKS`] looks with the status standing [`QUIET_S`]: with work under
/// the agent the turn is over and that work is its own, so the notice goes as
/// at a break, then the re-ask and the give-up, and the restart waits
/// `background`; with nothing under it the status is stale, and the notice
/// and the release go (each types one line and ends nothing). The restart
/// still waits for Claude's own `idle`, since work inside the agent's process
/// (a background agent, a workflow) shows only in that status: its READY is
/// asked again after [`REASK_S`], given up on after [`MAX_ASKS`], and a
/// gave-up upgrade's late READY is void past [`DRAIN_S`], so the agent is
/// released, never left stopped. A taskless conversation is not restarted
/// afresh on it, and `--now` waives none of it. Negative controls: a live
/// turn never reads authoritatively idle and waits `not-idle`; one look short
/// or a status younger than [`QUIET_S`] waits `status-stale`; a person's
/// `waiting`, a box, a draft, the busy footer and the settle still hold.
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
    // The stale status: nothing under the agent. One line goes; nothing ends.
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
    // Negative control: Claude's own `idle` over the same screen restarts.
    let idle = Facts {
        status: "idle".to_string(),
        ..stale.clone()
    };
    assert_eq!(next_step(&asked, &idle, true, 10), Step::Terminate);
    // Negative controls.
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

/// A stopped upgrade takes its own step at a break, not a bare `background`,
/// save the restart, which waits for an idle point. A gave-up upgrade with no
/// READY waits `failed` at a break (the release it owes is its next act), and
/// one whose late READY the agent's work outlived past the drain is voided
/// there, owing the release. The release line goes wherever a notice may go,
/// a settled break included: it types one line and ends nothing. Negative
/// controls: the restart a READY would take still waits at a break
/// ([`gate_restart`], [`break_step`]); an announced upgrade still waits
/// `background` within its window; a person, a draft, a box, a hold, the
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
    // Work inside the agent's own process (a workflow Claude's `busy` says it
    // waits on, nothing under the agent): the break arm waits on it as on a
    // shell under the agent, before the restart's gate is asked, so the late
    // READY it outlives past the drain is voided the same way, whatever
    // Claude's word for it, `--now` included.
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
    // Negative control: off a break, Claude's own `idle` with nothing under
    // the agent is the restart.
    assert_eq!(gate_restart(&idle_facts(), true), Gate::Go);
    // The release at a break: the notice's own gate there.
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
    // Negative control: an announced upgrade within its window still waits
    // on the agent's own work at a break.
    let announced = Phase::Announced { at_s: 0, asks: 1 };
    assert_eq!(
        next_step(&announced, &brk, false, REASK_S - 1),
        Step::Wait("background")
    );
}

/// The restart's own gate waits at a break: at a break of the agent's own
/// work ([`Facts::background_point`]) that work runs by definition (a shell
/// under the agent, or a workflow inside its process that
/// [`Facts::background`] never names), and the drain never ends the agent's
/// own work. The gate says what [`next_step`]'s break arm says, including
/// `background` for a phase the drivers carry instead of stepping (a restart
/// in flight, a move done). Negative controls: a shut gate says its own word
/// first, and off a break the same facts are the restart.
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
/// prompt; the harness's and a person's alike (`promptSource` and `origin`
/// cannot tell them apart).
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

/// A conversation has a task from the first prompt of someone else's, never
/// for the harness's own turns (the upgrade notice, READY, the carry-on and
/// its answer read taskless). A person's or an orchestrator's prompt makes it
/// tasked the moment it is recorded, answered or not, and for good, whatever
/// the harness types after, even stopped with Esc before its answer. A
/// command of theirs is a task once it starts a turn and while it has not
/// said it started none; `/model`, whose local output says so, is none. Not
/// prompts: a subagent's rows, Claude Code's own expansions (`isMeta`), a
/// tool's result, Esc's note. Negative controls: the same transcripts with
/// one person's prompt read tasked; an unknown user row's shape reads as
/// someone's.
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
        // A first prompt stopped with Esc before any answer is still someone's
        // ask.
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
    // An unknown shape reads as someone's, never as none.
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

/// THE SUPERVISOR'S OWN TURNS ARE NO TASK EITHER. `keep going` and
/// `answer_text` carry no mark — their words are the owner's — but the
/// loop's ledger records every text it types, and when
/// ([`TaskScan::supervisor_typed`]), and a row with the same words
/// (however wrapped), recorded as they were typed, is the harness's: the
/// E2E's chain — the notice, READY, the carry-on, `keep going`,
/// `answer_text` — holds no task. So is a command it typed (`/compact`),
/// read out of Claude Code's command row. NEGATIVE CONTROLS: the same record
/// read with no ledger is tasked by the first unmarked turn; a person's
/// prompt that is not the loop's words is theirs, ledger or not; and a
/// person's command is still a command.
#[test]
fn the_supervisors_own_turns_by_its_ledger_are_no_task() {
    let cfg = crate::supervise::SupervisorConfig::default();
    let ours: Vec<Typed> = [
        cfg.continue_text.as_str(),
        cfg.answer_text.as_str(),
        "/compact",
    ]
    .into_iter()
    .map(|text| Typed {
        at_ms: 1_000,
        text: text.to_string(),
    })
    .collect();
    // Recorded as the loop typed it (its ledger's `ts` 1 000 ms).
    let typed = |text: &str| {
        prompt_row(text).replacen('{', r#"{"timestamp":"1970-01-01T00:00:01.000Z","#, 1)
    };
    let chain = [
        prompt_row("[aterm harness] Claude Code 2.1.283 (managed) is installed"),
        answer_row("claude-haiku-4-5"),
        prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283."),
        answer_row("claude-haiku-4-5"),
        typed("keep going"),
        answer_row("claude-haiku-4-5"),
        typed(&cfg.answer_text.replace(". ", ".  ")),
        answer_row("claude-haiku-4-5"),
        typed("<command-name>/compact</command-name>\\n<command-args></command-args>"),
        answer_row("claude-haiku-4-5"),
    ]
    .join("\n");
    assert!(!transcript_tasked(&chain, &ours), "the harness's alone");
    assert!(transcript_tasked(&chain, &[]), "no ledger: someone's");
    let person = format!("{chain}\n{}", typed("keep going, and fix the parser"));
    assert!(transcript_tasked(&person, &ours), "not the loop's words");
    let command = format!(
        "{chain}\n{}\n{}",
        prompt_row("<command-name>/init</command-name>"),
        answer_row("claude-haiku-4-5")
    );
    assert!(transcript_tasked(&command, &ours), "a person's command");
}

/// A conversation with no task is never announced to: it is restarted afresh
/// ([`Step::Fresh`]) once idle the full [`QUIET_S`] (a first prompt may be on
/// its way: an orchestrator's `await agent idle` returns at the first idle
/// verdict), whether nothing was typed yet or an older aterm's notice stands
/// unanswered. A person at the tab, a hold, a box, a draft, a busy agent and
/// work under it still wait, and a break ends nothing. Negative controls:
/// the same facts with a task are announced to; a taskless agent whose
/// verdict moved under `QUIET_S` ago settles first.
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
    // Negative control: with a task, the notice.
    let tasked = Facts {
        taskless: false,
        ..settled.clone()
    };
    assert_eq!(
        next_step(&Phase::Pending, &tasked, false, now),
        Step::Announce
    );
}

// ------------------------------------------- a session parked at its usage limit

/// The row Claude Code 2.1.280 parks a session at its usage limit under.
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

/// The limit fact is the supervisor's own reading: the auto-continue banner
/// alone over an empty composer, which the gates read as idle, is `limited`
/// by the supervisor's recogniser, and so is the same wall over a break of
/// the agent's background work. Negative controls: the screen without the
/// banner, the banner made history by the words under it, and a copy of it in
/// a tool's output are not.
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
    // The composer the gates read is empty on it: a limited session looks
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

/// A session at its limit is never asked, and nothing of the upgrade's is
/// spent there: every gate waits `limited` (`--now` and a break included), no
/// re-ask, give-up, void or restart is taken, and an announced upgrade's clock
/// restarts at every look that finds the limit ([`clock_held`]). Negative
/// controls: the same facts off the limit take each of those steps.
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

    // Four half-hours and the asks budget.
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
    // Negative controls: off the limit, the same looks re-ask, give up, end
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

/// An upgrade that gave up asking still hears a late READY and takes exactly
/// the step an announced upgrade takes on it: every gate of the restart kept
/// (the agent's own work, a person at the tab, the limit), then the restart.
/// Negative controls: no READY waits `failed`, as does a READY to an upgrade
/// that stopped for any other reason.
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

/// A READY the restart's gate holds past the drain is never waited on for
/// good, an announced upgrade's and a gave-up one's alike. A person's box or
/// draft that has stood [`HOLD_S`] voids it in either phase
/// ([`person_void`]). The agent's own work still running [`DRAIN_S`] after
/// the answer is met by what each phase has left: an announced upgrade asks
/// again, naming what runs, and the new notice supersedes the answer; one
/// that gave up voids it, owing the release ([`void_of`]). A gate about to
/// open (the settle after that work ends) never voids, however old the
/// answer; at the limit nothing does. Negative controls: short of each bound,
/// the gate's own wait.
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

/// The release is typed under the notice's own gate, never over a READY the
/// restart will act on. Its words tell the agent nothing will restart it and
/// to go on, and are neither a notice nor a continuation to a transcript
/// reader.
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
    // notice may go, since a break that never ends reaches no idle point.
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
    // It points at no work of its own, so it never pulls the agent back over
    // direction given since the notice.
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

// ------------------------------------------------ no stop is for good

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

/// NO STOP IS FOR GOOD (the owner: "you should NEVER have
/// upgrades stalled"): EVERY reason a round stops for rests `RETRY_S` —
/// `failed` at an idle point, `background` at a break — and then
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

/// A tab whose agent runs a background gate or monitor at every look, so
/// nearly every step is taken at a break, after the round gave up and a late
/// READY came that its own work outlived: at a break the late READY is voided
/// on the drain's bound as at an idle point, and once the round has rested
/// (the READY voided, a stale READY no marker holds) a new round starts at
/// the break and its first notice is typed there. Negative control: a READY
/// heard for less than the drain's bound is still waited for.
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
    // Voided: a stale READY no marker holds (`ready` false). It rests, and
    // past the rest a new round starts, at a break and at an idle point alike.
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

/// The owner's word holds a new round as it holds any ([`rearm_held`]): a
/// stopped round the owner skipped (`--skip` of this target) stays skipped
/// however long it rests; a deferral holds it until it runs out. Negative
/// controls: a skip of another build, no word, and `--now` hold nothing.
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

// ------------------------------------ the notices queued behind a limit

/// A READY marker as aterm 0.93.0 typed it.
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

/// Claude Code's own answer at the weekly limit (ids cut, every key the
/// readers look at kept).
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

/// A notice the limit answered is queued, never read as asked: Claude Code
/// answers each notice typed into a session at its limit with a `rate_limit`
/// row, and when the session goes on all of them reach the agent at once.
/// Its fate is `Queued` until the model writes a row of its own, then `Taken`
/// at that row's time. A limit is no login wall. Negative controls: a notice
/// the model answered is `Answered`, one the wall answered `Wall`, one
/// nothing has answered `Open`; a limit hit before the notice leaves the
/// notice's fate its own; an API error that is no limit and a warning are
/// passed over; a subagent's limit is not the session's; a limit row without
/// the `error` key is read by the wall table's words.
#[test]
fn a_notice_the_limit_answered_is_queued_until_the_model_takes_it() {
    let m = QUEUED_MARKER;
    let notice = notice_row("2026-09-27T15:06:39.624Z", m);
    let queued = [notice.clone(), limit_row("2026-09-27T15:06:40.380Z")].join("\n");
    assert_eq!(notice_fate_of(&queued, m), Some(NoticeFate::Queued));
    assert_eq!(notice_fate(&queued, m), Some(false), "a limit is no wall");
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
    // How long it waits on its limit: the reset its row names; a row naming
    // none, REASK_S from when it was written; a `/login` finished since, over
    // (0), in both of Claude Code's shapes.
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
    // Negative controls.
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

/// THE LIMIT'S RESET, FROM THE TRANSCRIPT ([`transcript_limit_until`]): while
/// the session's last word is Claude Code's limit row, the instant it names
/// (`quotaLimits.resetsAt`) is how long the session stands at its limit,
/// which keeps even a first notice out once the banner has left the screen.
/// Claude Code's other rows are passed over. A limit row naming no reset
/// holds [`REASK_S`] from when it was written. Negative controls: the model
/// answering after it ends it; a row naming neither a reset nor a time, a
/// subagent's and none at all say nothing.
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
    // Negative controls.
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
    // A `/login` that finished after it ends it (an account switched, the
    // reset days off), and a limit hit again after it stands again.
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

/// The copies of a queued notice, counted until the model takes one
/// ([`REQUEUE_MAX`]): every upgrade notice typed since the model's last row of
/// its own is one more untaken, this notice's copies and an earlier round's,
/// whatever answered each (a limit, the login wall); a `/login` between them
/// takes none; a row of the model's own starts the count again. The full
/// queue's rest ([`Scan::rests_until`]) counts from the latest copy's limit
/// row: its reset, or [`REASK_S`] after a row naming none, then [`RETRY_S`].
/// Negative controls: a notice not queued (answered, taken, open behind a
/// queued copy) has none counted and no rest; a marker nothing carries, none.
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
    // Negative controls.
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

    // The rest of a full queue, from the latest copy's limit row.
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

/// The full queue's rest grows with every copy unread, up to a day: two hours
/// for the queue at its room's end (`1 + REQUEUE_MAX` unread), then four,
/// eight and sixteen, then a day for every count past that
/// (`QUEUE_REST_DOUBLINGS` rests shorter than a day). Negative controls: the
/// rest never exceeds a day however many wait, and a queue with room (which
/// types again straight away) reads the first rest.
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
    // Negative controls.
    for n in [full + 30, 1_000, u32::MAX] {
        assert_eq!(queue_rest(n), QUEUE_REST_MAX_S, "{n}");
    }
    for n in 0..full {
        assert_eq!(queue_rest(n), RETRY_S, "{n}");
    }
}

/// The ladder's rungs: twenty minutes, an hour and two hours behind, and
/// nothing before a start is known.
#[test]
fn the_ladder_climbs_by_how_long_the_session_has_been_behind() {
    assert_eq!(rung(0), Rung::Prefer);
    assert_eq!(rung(RUNG_SETTLED_S - 1), Rung::Prefer);
    assert_eq!(rung(RUNG_SETTLED_S), Rung::Settled);
    assert_eq!(rung(RUNG_KEYS_S - 1), Rung::Settled);
    assert_eq!(rung(RUNG_KEYS_S), Rung::KeysOnly);
    assert_eq!(rung(RUNG_LAND_S - 1), Rung::KeysOnly);
    assert_eq!(rung(RUNG_LAND_S), Rung::Land);
    assert_eq!(
        (RUNG_SETTLED_S, RUNG_KEYS_S, RUNG_LAND_S),
        (1_200, 3_600, 7_200)
    );
    assert_eq!(KEYS_GAP_S, 20);
    // The person's grace: the knob's until KeysOnly, then KEYS_GAP_S — or the
    // knob, if the owner set it shorter.
    assert_eq!(person_grace(Rung::Prefer, 120), 120);
    assert_eq!(person_grace(Rung::Settled, 120), 120);
    assert_eq!(person_grace(Rung::KeysOnly, 120), 20);
    assert_eq!(person_grace(Rung::Land, 120), 20);
    assert_eq!(person_grace(Rung::Land, 5), 5);
    // `--now` is the last rung.
    let f = Facts {
        owner_now: true,
        ..idle_facts()
    };
    assert_eq!(rung_in_force(&f), Rung::Land);
    assert_eq!(rung_in_force(&idle_facts()), Rung::Prefer);
}

/// Activity delays, never disables, and no rung relaxes a floor: it never
/// types over a draft, a dialog or running work. At each rung, what the gate
/// passes that the rung before did not: from Settled a pause that only looks
/// quiet (the reader's run of idle looks at the same last words, `still_s`,
/// the screen's own quiet restarted by a repaint); from KeysOnly a person
/// near the tab who has not typed within KEYS_GAP_S; at Land and under
/// `--now` the first such pause, its settle waived. Negative controls at
/// every rung, `--now` included: a hold, the usage limit, the login wall, a
/// full queue, the agent's own turn (its status, its busy footer), a turn in
/// a Codex daemon the screen does not show (its own thread's, a goal, or one
/// the lane cannot place in another session), a box, a draft, and a
/// keystroke within KEYS_GAP_S each still wait.
#[test]
fn the_ladder_relaxes_the_settle_and_the_person_near_and_never_a_floor() {
    let at = |rung: Rung, f: Facts| Facts { rung, ..f };
    // A repainting screen whose words stand still: the screen's quiet
    // restarted, the reader's run of looks thirty seconds long.
    let repainting = Facts {
        quiet_s: 3,
        status_age_s: 3,
        still_s: 30,
        ..idle_facts()
    };
    assert_eq!(
        gate_announce(&at(Rung::Prefer, repainting.clone())),
        Gate::Wait("settling")
    );
    for rung in [Rung::Settled, Rung::KeysOnly, Rung::Land] {
        assert_eq!(
            gate_announce(&at(rung, repainting.clone())),
            Gate::Go,
            "{rung:?}"
        );
    }
    // One look only (no run yet): only the last rung takes the first pause.
    let first_look = Facts {
        still_s: 0,
        ..repainting.clone()
    };
    for (rung, gate) in [
        (Rung::Settled, Gate::Wait("settling")),
        (Rung::KeysOnly, Gate::Wait("settling")),
        (Rung::Land, Gate::Go),
    ] {
        assert_eq!(
            gate_announce(&at(rung, first_look.clone())),
            gate,
            "{rung:?}"
        );
    }
    // A person near the tab (within human_grace_s), not typing.
    let near = Facts {
        attended: true,
        ..idle_facts()
    };
    for (rung, gate) in [
        (Rung::Prefer, Gate::Wait("attended")),
        (Rung::Settled, Gate::Wait("attended")),
        (Rung::KeysOnly, Gate::Go),
        (Rung::Land, Gate::Go),
    ] {
        assert_eq!(gate_announce(&at(rung, near.clone())), gate, "{rung:?}");
    }
    // Every floor, at every rung and under `--now`.
    let floors: [Knock; 13] = [
        (|f| f.held = true, "held"),
        (|f| f.typing = true, "attended"),
        (|f| f.limited = true, "limited"),
        (|f| f.login = true, "login"),
        (|f| f.queued = true, "queued"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.status = "shell".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
        (|f| f.daemon_turn = DaemonTurn::Own, "daemon-turn"),
        (|f| f.daemon_turn = DaemonTurn::Goal, "goal"),
        (|f| f.daemon_turn = DaemonTurn::Unplaced, "daemon-busy"),
        (|f| f.approval_box = true, "box"),
        (|f| f.composer_empty = false, "draft"),
    ];
    for rung in [Rung::Prefer, Rung::Settled, Rung::KeysOnly, Rung::Land] {
        for owner_now in [false, true] {
            for (knock, why) in floors {
                let mut f = Facts {
                    owner_now,
                    ..at(rung, repainting.clone())
                };
                knock(&mut f);
                assert_eq!(
                    gate_announce(&f),
                    Gate::Wait(why),
                    "{rung:?} now={owner_now} {why}"
                );
                assert_eq!(
                    gate_restart(&f, true),
                    Gate::Wait(why),
                    "the restart keeps it too: {rung:?} now={owner_now} {why}"
                );
            }
        }
    }
}

/// The last words' print (the repaint-proof settle): what a footer's clock, a
/// goal's counter, a dim suggestion on the composer's row or a blank row
/// under the words leaves as it was, and what new words or another process
/// change. Negative control: a changed word above the composer is a new print.
#[test]
fn the_last_words_print_ignores_the_live_zone_and_follows_the_words() {
    let screen = |caret: &str, footer: &str| -> Vec<String> {
        rows(&[
            "› fix the parser",
            "• Fixed it; the tests pass.",
            "",
            "  8:51 AM",
            "",
            caret,
            "",
            footer,
        ])
    };
    let base = still_print(
        7,
        &screen(
            "» Ask Codex to do anything",
            "  gpt · ~/p   Pursuing goal (10d 2h 51m)",
        ),
        5,
    );
    assert_eq!(base.len(), 16);
    assert_eq!(
        still_print(
            7,
            &screen(
                "» Ask Codex to do anything",
                "  gpt · ~/p   Pursuing goal (10d 2h 52m)"
            ),
            5
        ),
        base,
        "the goal's counter"
    );
    assert_eq!(
        still_print(7, &screen("» keep going", "  gpt · ~/p"), 5),
        base,
        "the composer's own row"
    );
    let mut blank = screen("» Ask Codex to do anything", "  gpt");
    blank.insert(4, String::new());
    assert_eq!(
        still_print(7, &blank, 6),
        base,
        "a blank row under the words"
    );
    // Negative controls.
    let mut words = screen("» Ask Codex to do anything", "  gpt");
    words[1] = "• Fixed it; one test still fails.".to_string();
    assert_ne!(still_print(7, &words, 5), base, "new words");
    assert_ne!(
        still_print(8, &screen("» Ask Codex to do anything", "  gpt"), 5),
        base,
        "another process"
    );
}

/// The end of the agent's own work is the upgrade's point: an agent that
/// answers every notice with "my workflow is still running" is asked at the
/// first idle point after its work ends. With the agent's work seen since the
/// latest notice and ended at this idle point (`Facts::work_ended`), an
/// announced round asks again at once, whatever its asks, and a gave-up round
/// is re-armed (its driver asks in the same visit), once the restart's gate
/// is open but for the READY; while that gate settles, the step waits a
/// settle, which owns the point. Negative controls: the same point with no
/// work seen waits (`awaiting-ready`, `failed`); a break is never such a
/// point; a READY takes the restart.
#[test]
fn the_end_of_the_agents_own_work_is_the_upgrades_point() {
    let now = 1_790_645_000;
    let ended = Facts {
        work_ended: true,
        ..idle_facts()
    };
    let announced = Phase::Announced {
        at_s: now - 60,
        asks: 1,
    };
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let resting = Facts {
        failed_s: 60,
        ..ended.clone()
    };
    // The fix: asked at once; re-armed.
    assert_eq!(next_step(&announced, &ended, false, now), Step::Announce);
    assert_eq!(next_step(&gave_up, &resting, false, now), Step::Rearm);
    // Past MAX_ASKS too: the agent's own answer asked for this one.
    let spent = Phase::Announced {
        at_s: now - 60,
        asks: MAX_ASKS,
    };
    assert_eq!(next_step(&spent, &ended, false, now), Step::Announce);
    // Settling: a settle, which owns the point.
    let settling = Facts {
        quiet_s: 3,
        ..ended.clone()
    };
    assert_eq!(
        next_step(&announced, &settling, false, now),
        Step::Wait("settling")
    );
    assert_eq!(
        next_step(
            &gave_up,
            &Facts {
                failed_s: 60,
                ..settling.clone()
            },
            false,
            now
        ),
        Step::Wait("settling")
    );
    assert!(crate::harness::upgrade_drive::owns_turn_ends(
        "wait:settling",
        0,
        600
    ));
    // A person, a box, a limit: the gate's own waits.
    let attended = Facts {
        attended: true,
        ..ended.clone()
    };
    assert_eq!(
        next_step(&announced, &attended, false, now),
        Step::Wait("attended")
    );
    // Negative controls: no work seen since the notice, at every point.
    let idle = idle_facts();
    assert_eq!(
        next_step(&announced, &idle, false, now),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(
        next_step(
            &gave_up,
            &Facts {
                failed_s: 60,
                ..idle.clone()
            },
            false,
            now
        ),
        Step::Wait("failed")
    );
    // A break is never the end of the work.
    let at_break = Facts {
        background_point: true,
        ..ended.clone()
    };
    assert_eq!(
        next_step(&announced, &at_break, false, now),
        Step::Wait("background")
    );
    // A READY takes the restart, as ever.
    assert_eq!(next_step(&announced, &ended, true, now), Step::Terminate);

    // A status that lags the idle screen: the agent's background shell ended,
    // Claude's status still reads `shell` over an idle screen, and the
    // restart's gate never reads past that lag (`status-stale` at every
    // look). Inside the re-ask's window the wait holds the point; past it the
    // re-ask decides (a line goes through the lag, or, its asks spent, the
    // round gives up), never the wait again.
    let lagging = Facts {
        status: "shell".to_string(),
        idle_looks: IDLE_LOOKS,
        ..ended.clone()
    };
    assert_eq!(gate_restart(&lagging, true), Gate::Wait("status-stale"));
    assert_eq!(
        next_step(&announced, &lagging, false, now),
        Step::Wait("status-stale"),
        "inside the window: the gate's wait"
    );
    let late = |asks| Phase::Announced {
        at_s: now - REASK_S,
        asks,
    };
    assert_eq!(
        next_step(&late(1), &lagging, false, now),
        Step::Announce,
        "past the window, asks left: the re-ask's line through the lag"
    );
    assert_eq!(
        next_step(&late(MAX_ASKS), &lagging, false, now),
        Step::GiveUp,
        "past the window, asks spent: the give-up"
    );
    let busy = Facts {
        status: "busy".to_string(),
        ..lagging.clone()
    };
    assert_eq!(next_step(&late(MAX_ASKS), &busy, false, now), Step::GiveUp);
    assert_eq!(next_step(&late(2), &busy, false, now), Step::Announce);
    // A settle past the window is no hold either: the re-ask's gate settles.
    assert_eq!(
        next_step(&late(MAX_ASKS), &settling, false, now),
        Step::GiveUp
    );
}

/// Tier-1 for `HarnessUpgradeWorkEnd` (aterm-spec
/// `harness_upgrade_work_end_model`): at every reachable idle point of the
/// model before the host's look (`screen == 0 && looked == 0`), the real
/// reducer is asked what the model's round would be asked: announced with
/// the model's asks (shifted so the model's `Cap` is `MAX_ASKS`), within its
/// re-ask window or past it (`late`), or resting after a give-up well within
/// its rest; an idle, settled, empty composer with nothing under the agent,
/// Claude's status `idle` or, where the model's status lags, `shell` over an
/// idle screen read so `IDLE_LOOKS` looks in a row; the end of the agent's
/// work exactly when the model saw work since the notice
/// (`Facts::work_ended`). It asks (`Announce`, or `Rearm` from a rest)
/// exactly where `HostAsks` or `HostReasks` is enabled, gives up exactly
/// where `HostGivesUp` is, waits `status-stale` exactly where `HostHolds` is,
/// and says its quiet word (`awaiting-ready`, `failed`) exactly where
/// `HostWaits` is. Negative controls: `Buggy`, an ask with no work seen
/// (`Nag`) and the wait held past the window (`Hold`) each disagree with the
/// real reducer at some reachable point.
#[test]
fn the_work_end_reducer_is_bound_to_its_model() {
    use aterm_spec::derive::{Model, harness_upgrade_work_end_model};
    type S = std::collections::BTreeMap<&'static str, i64>;
    fn reachable(m: &Model) -> Vec<S> {
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = std::collections::VecDeque::from([m.init_state()]);
        let mut out = Vec::new();
        while let Some(s) = queue.pop_front() {
            if !seen.insert(s.clone()) {
                continue;
            }
            for a in &m.actions {
                let mut next = s.clone();
                if m.fire(a.name, &mut next) {
                    queue.push_back(next);
                }
            }
            out.push(s);
        }
        out
    }
    let now = 1_790_645_000;
    let model = harness_upgrade_work_end_model();
    let cap = model.consts.iter().find(|c| c.0 == "Cap").unwrap().1;
    // The model's asks onto the real count: its `Cap` is `MAX_ASKS`.
    let shift = i64::from(MAX_ASKS) - cap;
    // The real reducer at a model point: which host action it takes.
    let real = |s: &S| -> &'static str {
        let phase = if s["phase"] == 1 {
            Phase::Announced {
                at_s: if s["late"] == 1 {
                    now - REASK_S - 60
                } else {
                    now - 60
                },
                asks: u32::try_from(s["asks"] + shift).unwrap(),
            }
        } else {
            Phase::Failed(GAVE_UP.to_string())
        };
        let facts = Facts {
            work_ended: s["worked"] == 1,
            failed_s: 60,
            ..idle_facts()
        };
        let facts = if s["lag"] == 1 {
            Facts {
                status: "shell".to_string(),
                idle_looks: IDLE_LOOKS,
                ..facts
            }
        } else {
            facts
        };
        match next_step(&phase, &facts, false, now) {
            Step::Announce | Step::Rearm => "ask",
            Step::GiveUp => "gives-up",
            Step::Wait("status-stale") => "holds",
            Step::Wait("awaiting-ready" | "failed") => "waits",
            other => panic!("{s:?}: {other:?}"),
        }
    };
    let model_says = |m: &Model, s: &S| -> Vec<&'static str> {
        [
            ("ask", "HostAsks"),
            ("ask", "HostReasks"),
            ("gives-up", "HostGivesUp"),
            ("holds", "HostHolds"),
            ("waits", "HostWaits"),
        ]
        .into_iter()
        .filter(|(_, action)| m.action_enabled(action, s))
        .map(|(word, _)| word)
        .collect()
    };
    let points: Vec<S> = reachable(&model)
        .into_iter()
        .filter(|s| s["screen"] == 0 && s["looked"] == 0)
        .collect();
    for (what, found) in [
        (
            "a rest after work",
            points.iter().any(|s| s["worked"] == 1 && s["phase"] == 2),
        ),
        ("no work seen", points.iter().any(|s| s["worked"] == 0)),
        (
            "a lag past the window",
            points
                .iter()
                .any(|s| s["worked"] == 1 && s["lag"] == 1 && s["late"] == 1 && s["phase"] == 1),
        ),
        (
            "asks spent",
            points.iter().any(|s| s["asks"] == cap && s["late"] == 1),
        ),
    ] {
        assert!(found, "the model reaches {what}");
    }
    for s in &points {
        assert_eq!(
            model_says(&model, s),
            [real(s)],
            "the real reducer against the model at {s:?}"
        );
    }
    // Negative controls: each dial disagrees with the real reducer somewhere.
    for dial in ["Buggy", "Nag", "Hold"] {
        let wrong = aterm_spec::interp::with_consts(&model, &[(dial, 1)]);
        let caught = reachable(&wrong)
            .into_iter()
            .filter(|s| s["screen"] == 0 && s["looked"] == 0)
            .any(|s| model_says(&wrong, &s) != [real(&s)]);
        assert!(
            caught,
            "{dial}: the real reducer disagrees with it somewhere"
        );
    }
}

// ---------------------------------------------------------------- a notice recorded as a paste

/// A READY marker a notice asked for (Claude Code 2.1.284 → 2.1.285).
const INCIDENT_MARKER: &str = "ATERM-UPGRADE-READY-d6476091";

/// A main-chain user row saying `text`, typed (`origin` human: the upgrade's
/// notice, the loop's `keep going` and a person's words alike).
fn typed_row(text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"origin":{{"kind":"human"}},"promptSource":"typed","version":"2.1.284"}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

/// A main-chain row of the agent's own model saying `text`.
fn model_row(text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}],"stop_reason":"end_turn"}},"version":"2.1.284"}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

/// `text` as Claude Code 2.1.284 records a paste of it.
fn pasted(text: &str) -> String {
    format!("\n\n<pasted_content id=\"6dd8\">\n{text}\n</pasted_content id=\"6dd8\">\n")
}

/// The notice with `marker` as typed into the session.
fn incident_notice(marker: &str) -> String {
    prepare_prompt(&v("2.1.284"), &v("2.1.285"), Source::Managed, marker)
}

/// A transcript up to the READY answer: the task, the notice (wrapped as a
/// paste), the agent's "not yet", a background task's notification, and,
/// with `ready`, the answer: several paragraphs, the marker its last line.
fn incident_rows(ready: bool) -> Vec<String> {
    let mut rows = vec![
        typed_row("compile trust through trust-ir and trust-cg"),
        model_row("Started the stage1 build."),
        typed_row(&pasted(&incident_notice(INCIDENT_MARKER))),
        model_row(
            "That upgrade notice arrived as pasted text.\n\nOnce the build is done I will \
             commit, push and send the token.",
        ),
        typed_row(
            "<task-notification>\n<task-id>bleqtme6t</task-id>\n<status>completed</status>\n</task-notification>",
        ),
    ];
    if ready {
        rows.push(model_row(&format!(
            "Step 2 increment 2 passed, and it's merged and pushed.\n\n**Result.** 5 of 6 \
             functions.\n\n**Clean state.**\n- None of my builds, tests or workflows are \
             running.\n\nNothing of mine is running now, so this is a good point for the \
             upgrade:\n\n{INCIDENT_MARKER}"
        )));
    }
    rows
}

/// A notice Claude Code recorded as a paste is still the notice, and a
/// person's paste still a direction: Claude Code records a long paste as
/// `\n\n<pasted_content id="…">\n…\n</pasted_content id="…">`, and the control
/// socket types the notice as one. Negative controls: the same rows typed
/// plainly read the same; a READY on the last line of several paragraphs is
/// heard.
#[test]
fn a_pasted_notice_is_the_notice_and_a_pasted_message_a_direction() {
    let earlier = ready_marker(ID, &v("2.1.285"), 1);
    let later = ready_marker(ID, &v("2.1.285"), 2);
    for (how, wrap) in [
        ("pasted", pasted as fn(&str) -> String),
        ("typed", str::to_string as fn(&str) -> String),
    ] {
        // A READY to the earlier ask, then the next ask's notice: the latest
        // notice ends every READY before it.
        let asked_again = [
            model_row(&format!("Done.\n\n{earlier}")),
            typed_row(&wrap(&incident_notice(&later))),
        ]
        .join("\n");
        assert!(
            !transcript_has_ready(&asked_again, &earlier),
            "{how}: the notice after the answer ends it"
        );
        // The harness's own notice is no task of anyone's.
        assert!(
            !transcript_tasked(&typed_row(&wrap(&incident_notice(&later))), &[]),
            "{how}: the notice is the harness's"
        );
        // A person's message after the READY, taken up by the agent.
        let directed = [
            model_row(&format!("Saved.\n\n{earlier}")),
            typed_row(&wrap("Here is the log you asked for: ...")),
            model_row("Reading the log."),
        ]
        .join("\n");
        assert!(
            directed_since_ready(&directed, std::slice::from_ref(&earlier)),
            "{how}: the person's message is a direction"
        );
    }
    let ready = incident_rows(true).join("\n");
    assert!(transcript_has_ready(&ready, INCIDENT_MARKER));
}

/// A READY through the transcript's folds: the tail read at the idle point,
/// before the READY row is appended, hears nothing; with the row appended it
/// is heard; the loop's `keep going` after the turn supersedes it
/// ([`ready_superseded`]; [`transcript_has_ready`] false), as does anything
/// the agent says after, until the next notice (not yet answered, no
/// supersession) or a new READY. Negative control: a notice nobody has
/// answered is not a superseded answer.
#[test]
fn the_incidents_ready_is_heard_once_written_and_superseded_by_the_keep_going() {
    let markers = [INCIDENT_MARKER.to_string()];
    let unwritten = incident_rows(false).join("\n");
    assert!(!transcript_has_ready(&unwritten, INCIDENT_MARKER));
    assert!(
        !ready_superseded(&unwritten, &markers),
        "not answered is not superseded"
    );
    let mut rows = incident_rows(true);
    assert!(transcript_has_ready(&rows.join("\n"), INCIDENT_MARKER));
    assert!(!ready_superseded(&rows.join("\n"), &markers));
    rows.push(
        r#"{"isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":69665}"#
            .to_string(),
    );
    rows.push(typed_row("keep going"));
    assert!(!transcript_has_ready(&rows.join("\n"), INCIDENT_MARKER));
    assert!(ready_superseded(&rows.join("\n"), &markers));
    rows.push(model_row(
        "Two pieces of work are running in the background.",
    ));
    assert!(ready_superseded(&rows.join("\n"), &markers));
    // The next ask: a notice of its own, not answered yet.
    let next = ready_marker(ID, &v("2.1.285"), 2);
    rows.push(typed_row(&pasted(&incident_notice(&next))));
    let both = [INCIDENT_MARKER.to_string(), next.clone()];
    assert!(!ready_superseded(&rows.join("\n"), &both));
    rows.push(model_row(&format!("All done.\n\n{next}")));
    assert!(transcript_has_ready(&rows.join("\n"), &next));
    assert!(!ready_superseded(&rows.join("\n"), &both));
}

/// A READY on the screen holds the point its transcript row is owed at
/// ([`Facts::ready_unrecorded`]): it waits `ready-unrecorded` instead of
/// `awaiting-ready`, and the row once written takes the restart's path (its
/// settle, then the restart). Bounded: once the same words have stood
/// [`UNRECORDED_S`] the round goes on; however many idle looks came before at
/// other points, the first looks at the answer hold; a break never restarts;
/// a limit waits first; a gave-up round holds a late READY the same and is
/// not re-armed over it; a pending round has nothing to hear. Negative
/// control: without the screen's READY, `awaiting-ready`.
#[test]
fn a_ready_on_the_screen_holds_the_point_until_its_row_is_written() {
    let now = 1_790_714_134;
    let announced = Phase::Announced {
        at_s: now - 945,
        asks: 1,
    };
    let turn_end = Facts {
        quiet_s: 0,
        status_age_s: 0,
        idle_looks: 1,
        ..idle_facts()
    };
    let shown = Facts {
        ready_unrecorded: true,
        ..turn_end.clone()
    };
    assert_eq!(
        next_step(&announced, &turn_end, false, now),
        Step::Wait("awaiting-ready"),
        "the incident's look"
    );
    assert_eq!(
        next_step(&announced, &shown, false, now),
        Step::Wait(READY_UNRECORDED)
    );
    // The row written: the restart's settle, then the restart.
    assert_eq!(
        next_step(&announced, &turn_end, true, now),
        Step::Wait("settling")
    );
    assert_eq!(
        next_step(&announced, &idle_facts(), true, now + 20),
        Step::Terminate
    );
    // Held however many idle looks came before at other points, and still at
    // the next look at the same words; bounded by how long they have stood.
    // (The host looks again the settle's length after a wait.)
    let looked_before = Facts {
        idle_looks: 5,
        still_s: QUIET_S,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &looked_before, false, now),
        Step::Wait(READY_UNRECORDED)
    );
    let long = Facts {
        still_s: UNRECORDED_S,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &long, false, now),
        Step::Wait("awaiting-ready")
    );
    let at_break = Facts {
        background_point: true,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &at_break, false, now),
        Step::Wait("background")
    );
    let limited = Facts {
        limited: true,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &limited, false, now),
        Step::Wait("limited")
    );
    // A round that gave up and rested past its next round's due time.
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let rested = Facts {
        failed_s: RETRY_S + 60,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&gave_up, &rested, false, now),
        Step::Wait(READY_UNRECORDED)
    );
    assert_eq!(
        next_step(
            &gave_up,
            &Facts {
                ready_unrecorded: false,
                ..rested
            },
            false,
            now
        ),
        Step::Rearm
    );
    assert_ne!(
        next_step(&Phase::Pending, &shown, false, now),
        Step::Wait(READY_UNRECORDED)
    );
    // The quiet upgrade (the default) acts on no READY: its own restart gate
    // holds the turn end (the settle), whatever the screen says.
    let quiet = Facts {
        quiet: true,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &quiet, false, now),
        Step::Wait("settling")
    );
}

/// A READY the agent was moved on from is asked again at its next point
/// ([`Facts::ready_superseded`]), never waited on for the re-ask's window. At
/// an idle point the notice goes again at once; while the agent's own turn
/// settles the step says `superseded`; a person, a box, a hold wait as
/// themselves; the last ask spent gives up; at a break the notice goes there.
/// A READY given again takes the restart. Negative control: an answer that
/// was never READY waits the window.
#[test]
fn a_superseded_ready_is_asked_again_at_the_next_point() {
    let now = 1_790_714_200;
    let announced = Phase::Announced {
        at_s: now - 1_000,
        asks: 1,
    };
    let moved_on = Facts {
        ready_superseded: true,
        ..idle_facts()
    };
    assert_eq!(
        next_step(&announced, &idle_facts(), false, now),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(next_step(&announced, &moved_on, false, now), Step::Announce);
    for (settling, why) in [
        (
            Facts {
                quiet_s: 3,
                ..moved_on.clone()
            },
            "its screen settling",
        ),
        (
            Facts {
                status: "busy".to_string(),
                ..moved_on.clone()
            },
            "its status still busy",
        ),
        (
            Facts {
                busy_footer: true,
                ..moved_on.clone()
            },
            "a turn still running",
        ),
    ] {
        assert_eq!(
            next_step(&announced, &settling, false, now),
            Step::Wait(SUPERSEDED),
            "{why}"
        );
    }
    for (held, word) in [
        (
            Facts {
                attended: true,
                ..moved_on.clone()
            },
            "attended",
        ),
        (
            Facts {
                approval_box: true,
                ..moved_on.clone()
            },
            "box",
        ),
        (
            Facts {
                status: "waiting".to_string(),
                ..moved_on.clone()
            },
            "not-idle",
        ),
    ] {
        assert_eq!(next_step(&announced, &held, false, now), Step::Wait(word));
    }
    let last = Phase::Announced {
        at_s: now - 1_000,
        asks: MAX_ASKS,
    };
    assert_eq!(next_step(&last, &moved_on, false, now), Step::GiveUp);
    let at_break = Facts {
        background_point: true,
        status: "busy".to_string(),
        ..moved_on.clone()
    };
    assert_eq!(next_step(&announced, &at_break, false, now), Step::Announce);
    // Negative control: the same break with no READY given waits its window.
    assert_eq!(
        next_step(
            &announced,
            &Facts {
                ready_superseded: false,
                ..at_break
            },
            false,
            now
        ),
        Step::Wait("background")
    );
    assert_eq!(next_step(&announced, &moved_on, true, now), Step::Terminate);
}

/// CLAUDE CODE'S ANSWER AT A QUOTA LIMIT, as 2.1.286 writes it (the accounts
/// design §1's row; ids cut, every key the readers look at kept): the limit
/// `limit` (`quotaLimits.rateLimitType`), resetting at `resets`, saying
/// `text`.
fn quota_row(ts: &str, limit: &str, resets: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"{ts}","message":{{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":{}}}]}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":{resets},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"{limit}","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.286"}}"#,
        json_text(text)
    )
}

/// THE QUOTA WALL A SESSION STANDS AT, FROM ITS TRANSCRIPT
/// ([`transcript_standing_wall`], accounts design §5 and §6): the limit row
/// that stands last, read for when it was written, the reset it names and
/// which of the account's limits it is — and only when it is a quota
/// rejection. It is the same row [`transcript_limit_until`] reads, so every
/// rule of which row STANDS is that scan's (the controls below ask both).
///
/// NEGATIVE CONTROLS, each beside its rule: a subagent's row, a model row or
/// a finished `/login` after the wall; an API rate limit and a spend or
/// credit wall, which are limits and are no account's window; a status that
/// is not `rejected`.
#[test]
fn the_standing_quota_wall_is_the_limit_row_that_stands_and_only_a_quota_rejection() {
    let at = 1_790_521_468;
    let ts = "2026-09-27T15:04:28.344Z";
    let wall = |window: &str, until: u64| {
        Some(StandingWall {
            at,
            until: Some(until),
            window: Some(window.to_string()),
        })
    };
    let session = quota_row(
        ts,
        "five_hour",
        1_790_532_000,
        "You've hit your session limit · resets 7pm (America/Los_Angeles)",
    );
    let answered = said_row("2026-09-27T14:00:00Z", "Working.");

    // THE THREE WALLS THAT ROTATE: the session window, the week, and a
    // model's bucket — each by the vendor's own limit type.
    assert_eq!(
        transcript_standing_wall(&[answered.clone(), session.clone()].join("\n")),
        wall("five_hour", 1_790_532_000)
    );
    // The week: the owner's row of 2026-09-27 (2.1.280), as it is.
    assert_eq!(
        transcript_standing_wall(&limit_row(ts)),
        wall("seven_day", 1_791_046_800)
    );
    for bucket in [
        "seven_day_opus",
        "seven_day_sonnet",
        "seven_day_overage_included",
    ] {
        let row = quota_row(
            ts,
            bucket,
            1_791_046_800,
            "You've hit your Opus limit · resets Oct 3 at 10am (America/Los_Angeles)",
        );
        assert_eq!(
            transcript_standing_wall(&row),
            wall(bucket, 1_791_046_800),
            "{bucket}"
        );
    }

    // WHICH ROW STANDS. Claude Code's other rows of its own are passed over.
    let no_response = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
    assert_eq!(
        transcript_standing_wall(&[session.as_str(), no_response].join("\n")),
        wall("five_hour", 1_790_532_000),
        "passed over"
    );
    // A subagent's sidechain row is not the session's: its wall is no wall
    // of the session's, and its model row ends none.
    let side = |row: &str| row.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        transcript_standing_wall(&[answered.clone(), side(&session)].join("\n")),
        None
    );
    let later = said_row("2026-09-27T19:58:05.554Z", "Pushing first.");
    assert_eq!(
        transcript_standing_wall(&[session.clone(), side(&later)].join("\n")),
        wall("five_hour", 1_790_532_000),
        "a subagent that answered did not lift the session's wall"
    );
    // A row of the session's own model after the wall ends it…
    assert_eq!(
        transcript_standing_wall(&[session.clone(), later.clone()].join("\n")),
        None
    );
    // …and so does a `/login` that finished; a wall hit after either stands.
    let login = person_row(
        "2026-09-27T19:57:47Z",
        "<local-command-stdout>Login successful</local-command-stdout>",
    );
    assert_eq!(
        transcript_standing_wall(&[session.clone(), login.clone()].join("\n")),
        None
    );
    assert_eq!(
        transcript_standing_wall(&[login, later, session.clone()].join("\n")),
        wall("five_hour", 1_790_532_000)
    );
    assert_eq!(transcript_standing_wall(&answered), None);
    assert_eq!(transcript_standing_wall(""), None);

    // ONLY A QUOTA REJECTION. An API rate limit carries no `quotaLimits`: it
    // is a limit (the upgrade waits behind it), and no account's window —
    // also when it is the row standing AFTER a quota wall.
    let api = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-27T15:10:00Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: Rate limit reached for requests"}]},"isApiErrorMessage":true}"#;
    assert!(transcript_limit_until(api).is_some(), "it is a limit row");
    assert_eq!(transcript_standing_wall(api), None);
    assert_eq!(
        transcript_standing_wall(&[session.as_str(), api].join("\n")),
        None,
        "the row that stands last is the API's"
    );
    // A spend or credit wall, by its text (the wall table's `Spend`)…
    for text in [
        "You've hit your usage credit limit · resets Oct 3 at 10am (America/Los_Angeles)",
        "You're out of extra usage · resets 7pm (America/Los_Angeles)",
        "You've hit your org's monthly spend limit",
    ] {
        let spend = quota_row(ts, "seven_day", 1_791_046_800, text);
        assert!(transcript_limit_spend(&spend), "{text}");
        assert_eq!(
            transcript_limit_reset(&spend),
            Some(1_791_046_800),
            "{text}"
        );
        assert_eq!(transcript_standing_wall(&spend), None, "{text}");
    }
    // …and by its limit type, whatever its text says: the vendor's
    // usage-credit limit.
    let credit = quota_row(
        ts,
        "overage",
        1_791_046_800,
        "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)",
    );
    assert!(!transcript_limit_spend(&credit), "its text is the week's");
    assert_eq!(transcript_standing_wall(&credit), None);
    // A status that is not `rejected` — the vendor's `allowed_warning`, one
    // a hair off, none at all — is no rejection.
    for status in [
        r#""status":"allowed_warning","#,
        r#""status":"allowed","#,
        r#""status":"rejected ","#,
        r#""status":"Rejected","#,
        r#""status":true,"#,
        r#""status":["rejected"],"#,
        "",
    ] {
        let row = session.replace(r#""status":"rejected","#, status);
        assert_ne!(row, session, "{status}");
        assert_eq!(
            transcript_limit_reset(&row),
            Some(1_790_532_000),
            "it is still a limit row naming its reset: {status}"
        );
        assert_eq!(transcript_standing_wall(&row), None, "{status}");
    }
}

/// THE LIMIT TYPE WHERE THE ROW CARRIES NONE, AND ROWS OF THE WRONG SHAPE
/// ([`transcript_standing_wall`]). `rateLimitType` is optional in the
/// vendor's schema: without it the window is what the row's text names
/// (`usage::notice_window`: the session's or the week's), else none — the
/// wall is a wall all the same. A key of the wrong shape costs that key and
/// no more, a row that names no time is no observation, and nothing that is
/// not a short lowercase word becomes a window.
#[test]
fn a_standing_wall_with_no_limit_type_takes_it_from_its_text_and_hostile_rows_cost_one_key() {
    let at = 1_790_521_468;
    let ts = "2026-09-27T15:04:28.344Z";
    let typed = r#""rateLimitType":"seven_day","#;
    let week = limit_row(ts);
    assert!(week.contains(typed), "{week}");
    let read = |row: &str| transcript_standing_wall(row);
    let window = |row: &str| read(row).expect("a wall").window;

    // No limit type: the text's.
    let untyped = week.replace(typed, "");
    assert_eq!(
        read(&untyped),
        Some(StandingWall {
            at,
            until: Some(1_791_046_800),
            window: Some("seven_day".to_string()),
        })
    );
    let session_text = "You've hit your session limit · resets 7pm (America/Los_Angeles)";
    let untyped_session = untyped.replace(
        "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)",
        session_text,
    );
    assert_eq!(window(&untyped_session).as_deref(), Some("five_hour"));
    // A text that names neither window: the wall stands, its window unnamed.
    let untyped_bucket = untyped.replace("weekly limit", "Opus limit");
    assert_eq!(
        read(&untyped_bucket),
        Some(StandingWall {
            at,
            until: Some(1_791_046_800),
            window: None,
        })
    );
    // NEGATIVE CONTROL: where the row carries a type, the text is not asked
    // — a bucket's row whose text names the week is the bucket's.
    let bucket = week.replace(typed, r#""rateLimitType":"seven_day_opus","#);
    assert_eq!(window(&bucket).as_deref(), Some("seven_day_opus"));

    // A LIMIT TYPE THAT IS NOT A WORD is read as absent: the text's again.
    let long = "a".repeat(LIMIT_TYPE_MAX + 1);
    for hostile in [
        r#""rateLimitType":7,"#.to_string(),
        r#""rateLimitType":null,"#.to_string(),
        r#""rateLimitType":{"is":"five_hour"},"#.to_string(),
        r#""rateLimitType":"","#.to_string(),
        r#""rateLimitType":"five hour","#.to_string(),
        r#""rateLimitType":"Five_Hour","#.to_string(),
        r#""rateLimitType":"five_hour\n","#.to_string(),
        r#""rateLimitType":"five-hour","#.to_string(),
        r#""rateLimitType":"../../state","#.to_string(),
        r#""rateLimitType":"séven","#.to_string(),
        format!(r#""rateLimitType":"{long}","#),
    ] {
        let row = week.replace(typed, &hostile);
        assert_eq!(window(&row).as_deref(), Some("seven_day"), "{hostile}");
    }
    // NEGATIVE CONTROL: a word the vendor has not named yet, at the bound,
    // is kept as it is.
    let new = "b".repeat(LIMIT_TYPE_MAX);
    let row = week.replace(typed, &format!(r#""rateLimitType":"{new}","#));
    assert_eq!(window(&row), Some(new));

    // A RESET THAT IS NOT A POSITIVE INTEGER is no reset; the wall stands.
    let reset = r#""resetsAt":1791046800,"#;
    assert!(week.contains(reset));
    for hostile in [
        r#""resetsAt":0,"#,
        r#""resetsAt":-5,"#,
        r#""resetsAt":1791046800.5,"#,
        r#""resetsAt":"1791046800","#,
        r#""resetsAt":null,"#,
        r#""resetsAt":[1791046800],"#,
        "",
    ] {
        assert_eq!(
            read(&week.replace(reset, hostile)),
            Some(StandingWall {
                at,
                until: None,
                window: Some("seven_day".to_string()),
            }),
            "{hostile}"
        );
    }

    // A ROW THAT NAMES NO TIME is no observation: nobody could say which
    // process wrote it. (A zone offset is refused, never read as UTC.)
    let stamp = r#""timestamp":"2026-09-27T15:04:28.344Z","#;
    assert!(week.contains(stamp));
    for hostile in [
        "",
        r#""timestamp":1790521468,"#,
        r#""timestamp":"yesterday","#,
        r#""timestamp":"2026-09-27T15:04:28+05:30","#,
        r#""timestamp":null,"#,
    ] {
        let row = week.replace(stamp, hostile);
        assert_eq!(read(&row), None, "{hostile}");
        assert_eq!(
            transcript_limit_reset(&row),
            Some(1_791_046_800),
            "it is still the limit row: {hostile}"
        );
    }

    // `quotaLimits` of another shape is none; a line that is not JSON, or
    // not a row, is skipped and the row before it still stands.
    let quota = week
        .find(r#""quotaLimits":{"#)
        .zip(week.find(r#""error":"rate_limit""#))
        .expect("the row's quotaLimits");
    for hostile in [
        r#""quotaLimits":null,"#,
        r#""quotaLimits":"rejected","#,
        r#""quotaLimits":["rejected",1791046800,"seven_day"],"#,
        r#""quotaLimits":{},"#,
    ] {
        let mut row = week.clone();
        row.replace_range(quota.0..quota.1, hostile);
        assert!(transcript_limit_until(&row).is_some(), "{hostile}");
        assert_eq!(read(&row), None, "{hostile}");
    }
    let cut = &week[..week.len() / 2];
    for junk in [
        cut,
        "assistant",
        "[\"assistant\"]",
        "\u{fffd}assistant{",
        "42",
    ] {
        assert_eq!(
            read(&[week.as_str(), junk].join("\n")),
            Some(StandingWall {
                at,
                until: Some(1_791_046_800),
                window: Some("seven_day".to_string()),
            }),
            "{junk:?}"
        );
    }
}

/// One row a local command wrote at `ts`: a `user` row whose content is the
/// string `content`, in the shape MEASURED for `/model` on 2.1.283
/// (`footer.rs`'s rows).
fn local_command_row(ts: &str, content: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"q","type":"user","message":{{"role":"user","content":{}}},"uuid":"u","timestamp":"{ts}","userType":"external","entrypoint":"cli","version":"2.1.287"}}"#,
        json_text(content)
    )
}

/// The same content as the `system` row some results are written as
/// (MEASURED on 2.1.278 for a `/model` picker that changed nothing).
fn local_command_system_row(ts: &str, content: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"local_command","content":{},"level":"info","timestamp":"{ts}","uuid":"u","isMeta":false,"version":"2.1.287"}}"#,
        json_text(content)
    )
}

/// A local command's own row content, as Claude Code writes it.
fn command_content(name: &str) -> String {
    format!(
        "<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"
    )
}

/// A local command's output row content.
fn stdout_content(text: &str) -> String {
    format!("<local-command-stdout>{text}</local-command-stdout>")
}

/// THE LINE THE USAGE-LIMIT DIALOG ENDED WITH ([`transcript_reset_line`]):
/// the last `<local-command-stdout>` row written at or after the claim's
/// second that stands under a `/rate-limit-options` command row, its text
/// between the tags. It is what a limit reset's outcome is read from.
///
/// EVERY RESET ROW HERE IS COMPOSED 2026-10-02, never captured: the texts
/// are the 2.1.287 bundle's (`docs/SPEC-claude-accounts-resets-2026-10-02.md`
/// §0), laid on the two rows MEASURED for this dialog's wait row
/// (`upgrade_stall_tests.rs`: the `/rate-limit-options` command row, then
/// its output row) in the full row shape measured for `/model`; the values
/// are invented. OPEN: replace with a captured transcript (SPEC §6).
///
/// NEGATIVE CONTROLS: a row older than the claim is not read; a `/model`
/// output after the claim is not read, whatever it says; an output is read
/// only directly under the dialog's own command; a row with no time, a
/// subagent's row, an error output, and a line merely quoted in a prompt, an
/// answer or a tool result are not read.
#[test]
fn the_dialogs_result_line_is_read_under_its_own_command_and_only_since_the_claim() {
    // 2026-09-27T15:04:28Z, the second the attempt was claimed.
    let claimed = 1_790_521_468;
    let before = "2026-09-27T15:04:27.900Z";
    let same = "2026-09-27T15:04:28.344Z";
    let after = "2026-09-27T15:05:10.120Z";
    let later = "2026-09-27T15:09:00.000Z";
    let spent = "Limits reset · your weekly reset day stays Oct 3 at 10am · 1 reset left";
    let kept = "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let waited = "Claude Code will continue automatically at 6am";

    let dialog = |ts: &str| local_command_row(ts, &command_content("rate-limit-options"));
    let model = |ts: &str| local_command_row(ts, &command_content("model"));
    let out = |ts: &str, text: &str| local_command_row(ts, &stdout_content(text));
    let read = |rows: &[String]| transcript_reset_line(&rows.join("\n"), claimed);

    // The dialog's two rows, written when it ended.
    assert_eq!(
        read(&[dialog(after), out(after, spent)]).as_deref(),
        Some(spent)
    );
    // In the claim's own second too: "at or after".
    assert_eq!(
        read(&[dialog(same), out(same, kept)]).as_deref(),
        Some(kept)
    );
    // The reader of the line takes what this hands it.
    for (text, class) in [
        (
            spent,
            aterm_phase::ResetLine::Spent {
                left: Some(1),
                asks: false,
            },
        ),
        (kept, aterm_phase::ResetLine::Kept),
        (waited, aterm_phase::ResetLine::Other),
    ] {
        let line = read(&[dialog(after), out(after, text)]).expect("a line");
        assert_eq!(aterm_phase::reset_line(&line), class, "{text}");
    }
    // A whole tail around it: the wall's own row, the turn before, the
    // caveat row a command is written under, and what follows.
    let tail = [
        said_row("2026-09-27T14:00:00Z", "Working."),
        limit_row("2026-09-27T15:00:00.000Z"),
        dialog(before),
        out(before, waited),
        local_command_row(
            after,
            "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands.</local-command-caveat>",
        )
        .replace(r#""type":"user","#, r#""type":"user","isMeta":true,"#),
        dialog(after),
        out(after, spent),
        said_row(later, "Continuing."),
        model(later),
        out(later, "Set model to `Opus 5.5`"),
    ];
    assert_eq!(read(&tail).as_deref(), Some(spent));
    // The LAST such row since the claim. A claim outlives a dialog a person
    // answered: the wait row's line, then the dialog again and the reset
    // spent through it — read as the first, that reset was recorded as the
    // wait row's failure.
    assert_eq!(
        read(&[
            dialog(after),
            out(after, waited),
            dialog(later),
            out(later, spent)
        ])
        .as_deref(),
        Some(spent)
    );
    // …whatever came between, and whatever another command wrote after it.
    assert_eq!(
        read(&[
            dialog(before),
            out(before, spent),
            dialog(same),
            out(same, waited),
            said_row(after, "Working."),
            dialog(after),
            out(after, kept),
            model(later),
            out(later, "Set model to `Opus 5.5`"),
        ])
        .as_deref(),
        Some(kept)
    );
    // Both row shapes, the command's and the output's.
    for system in [(true, true), (true, false), (false, true)] {
        let command = if system.0 {
            local_command_system_row(after, &command_content("rate-limit-options"))
        } else {
            dialog(after)
        };
        let output = if system.1 {
            local_command_system_row(after, &stdout_content(spent))
        } else {
            out(after, spent)
        };
        assert_eq!(
            read(&[command, output]).as_deref(),
            Some(spent),
            "{system:?}"
        );
    }
    // The measured wait row carries no closing tag in the stall suite's
    // copy: the text is the same without one.
    assert_eq!(
        read(&[
            dialog(after),
            local_command_row(after, &format!("<local-command-stdout>{spent}"))
        ])
        .as_deref(),
        Some(spent)
    );

    // NEGATIVE CONTROL: A ROW OLDER THAN THE CLAIM is an earlier dialog's —
    // the wait an earlier wall armed — and is not read…
    assert_eq!(read(&[dialog(before), out(before, waited)]), None);
    assert_eq!(read(&[dialog(before), out(before, spent)]), None);
    // …and neither is a row that names no time.
    let untimed = out(after, spent).replace(&format!(r#""timestamp":"{after}","#), "");
    assert!(!untimed.contains("timestamp"));
    assert_eq!(read(&[dialog(after), untimed]), None);

    // NEGATIVE CONTROL: A `/model` OUTPUT AFTER THE CLAIM is not read —
    // its own words, and a reset line under it.
    assert_eq!(
        read(&[model(after), out(after, "Set model to `Opus 5.5`")]),
        None
    );
    assert_eq!(read(&[model(after), out(after, spent)]), None);
    // Nor under `/model` typed after the dialog's command: the output is
    // the last command's.
    assert_eq!(
        read(&[dialog(after), model(after), out(after, spent)]),
        None
    );
    // An output ends what its command armed: a second one is nobody's.
    assert_eq!(
        read(&[dialog(before), out(before, waited), out(after, spent)]),
        None
    );
    // No command row at all; the command typed with arguments, which is not
    // the dialog the vendor opens; another command whose name it opens.
    assert_eq!(read(&[out(after, spent)]), None);
    let with_args = dialog(after).replace(
        "<command-args></command-args>",
        "<command-args>now</command-args>",
    );
    assert_eq!(read(&[with_args, out(after, spent)]), None);
    let longer = local_command_row(after, &command_content("rate-limit-options-2"));
    assert_eq!(read(&[longer, out(after, spent)]), None);
    // An error output; a subagent's rows.
    let failed = local_command_row(
        after,
        &format!("<local-command-stderr>{spent}</local-command-stderr>"),
    );
    assert_eq!(read(&[dialog(after), failed, out(after, spent)]), None);
    let side = |row: String| row.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(read(&[dialog(after), side(out(after, spent))]), None);
    assert_eq!(read(&[side(dialog(after)), out(after, spent)]), None);
    // The line QUOTED is not the line: in a person's prompt, in an answer,
    // in a tool result.
    let quoted = stdout_content(spent);
    for row in [
        local_command_row(after, &format!("it printed {quoted}")),
        said_row(after, &quoted),
        format!(
            r#"{{"type":"user","isSidechain":false,"timestamp":"{after}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":{}}}]}}}}"#,
            json_text(&quoted)
        ),
        format!(
            r#"{{"type":"user","isSidechain":false,"timestamp":"{after}","message":{{"role":"user","content":[{{"type":"text","text":{}}}]}}}}"#,
            json_text(&quoted)
        ),
    ] {
        assert_eq!(read(&[dialog(after), row.clone()]), None, "{row}");
    }
    assert_eq!(read(&[]), None);
    assert_eq!(transcript_reset_line("not json\n{}\n", 0), None);

    // The text is one bounded line, and a row that says nothing is the
    // empty line: the dialog ended, and said nothing this reader keeps.
    let long = format!("{spent} {}", "x".repeat(4 * RESET_LINE_MAX));
    let cut = read(&[dialog(after), out(after, &long)]).expect("a line");
    assert!(
        cut.len() <= RESET_LINE_MAX && cut.starts_with(spent),
        "{cut}"
    );
    assert_eq!(
        read(&[dialog(after), out(after, "two\nlines")]).as_deref(),
        Some("two lines")
    );
    assert_eq!(read(&[dialog(after), out(after, "")]).as_deref(), Some(""));
}
