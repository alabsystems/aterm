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
    let t = choose_target(&v("2.1.280"), &[managed_280.clone(), native_281.clone()]);
    assert_eq!(t, Some(native_281.clone()));
    let managed_281 = cand("/pkg/agents/claude", "2.1.281", Source::Managed);
    let t = choose_target(&v("2.1.280"), &[native_281.clone(), managed_281.clone()]);
    assert_eq!(t, Some(managed_281));
    assert_eq!(
        choose_target(&v("2.1.281"), &[managed_280.clone(), native_281]),
        None
    );
    assert_eq!(choose_target(&v("2.1.280"), &[managed_280]), None);
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
    let got = rewrite_argv(
        &argv(&["claude", "--dangerously-skip-permissions", "--resume"]),
        ID,
    );
    assert_eq!(
        got,
        Ok(argv(&["--dangerously-skip-permissions", "--resume", ID]))
    );
    for form in [
        argv(&["claude", "--continue", "--model", "opus"]),
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

#[derive(Debug, Default)]
struct Parsed {
    events: Vec<(String, Option<String>)>,
    operands: Vec<String>,
    unknown: bool,
    missing: bool,
}

fn claude_parses(args: &[String]) -> Parsed {
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
                        p.missing = true;
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
            p.unknown = true;
            return p;
        }
        p.operands.push(o);
    }
    p
}

fn of_fate(p: &Parsed, fate: Fate) -> Vec<(String, Option<String>)> {
    p.events
        .iter()
        .filter(|(n, _)| flag(n).is_some_and(|(_, f)| f == fate))
        .cloned()
        .collect()
}

#[test]
fn every_short_argv_resumes_with_exactly_the_values_claude_parsed() {
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
    let cd = relaunch_line(Dialect::Zsh, None, Some("/w/it's"), exe, &args).unwrap();
    assert!(cd.starts_with(" ( cd -- '/w/it'\\''s' && exec '"), "{cd}");
    assert!(cd.ends_with(&format!("'{ID}' )")), "{cd}");
    assert!(relaunch_line(Dialect::Fish, None, Some("/w"), exe, &args).is_err());
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
        assert_eq!(at(tty, tty.max_line()).map(|l| l.len()), Ok(tty.max_line()));
        assert!(at(tty, tty.max_line() + 1).is_err(), "{tty:?}");
    }
    assert!(at(LineDiscipline::Linux, 2048).is_ok());
    assert!(at(LineDiscipline::Darwin, 1024).is_err());
}

#[test]
fn the_relaunch_line_takes_the_key_first_in_the_shells_own_dialect() {
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let key = Path::new("/Users/_me/Library/Application Support/aterm/rekey/s-0a.1");
    let q = "'/Users/_me/Library/Application Support/aterm/rekey/s-0a.1'";
    let args = argv(&["--resume", ID]);
    let zsh = relaunch_line(Dialect::Zsh, Some(hook), Some("/w"), exe, &args).unwrap();
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
    let odd = with_rekey(Dialect::Zsh, &bash, Path::new("/r/it's")).unwrap();
    assert!(odd.contains("<'/r/it'\\''s'; "), "{odd}");
    assert!(
        with_rekey(Dialect::Fish, &fish, Path::new("/r/it's"))
            .unwrap()
            .contains(r"command cat '/r/it\'s' 2>/dev/null"),
    );
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
}

fn named_pids(list: &str) -> Vec<u32> {
    list.split("pid ")
        .skip(1)
        .filter_map(|p| p.split(' ').next()?.parse().ok())
        .collect()
}

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
    assert_eq!(named_pids(&two), [63492, 64036], "{two}");
    assert!(!two.chars().any(char::is_control), "{two:?}");
    let long = "x".repeat(HELD_COMMAND_CHARS * 2);
    let many: Vec<Held> = (0..7).map(|i| held(100 + i, 5, &long)).collect();
    let clause = running_clause(&many);
    assert_eq!(named_pids(&clause).len(), HELD_NAMED, "{clause}");
    assert!(
        clause.contains('…') && !clause.contains(&long),
        "each command is cut"
    );
    assert_eq!(named_pids(&running_clause(&[held(7, 0, "")])), [7]);
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

#[test]
fn the_notice_names_the_oldest_processes_first() {
    let held = |pid: u32, age_s: u64| Held {
        pid,
        name: "zsh".to_string(),
        age_s,
        command: String::new(),
    };
    let mut table: Vec<Held> = (0..14)
        .map(|i| held(60_000 + i, 60 + u64::from(i)))
        .collect();
    table.push(held(51_435, 4 * 86_400 + 22 * 3_600 + 26 * 60));
    let named = named_pids(&held_list(&table));
    assert_eq!(named.len(), HELD_NAMED);
    assert_eq!(named[..3], [51_435, 60_013, 60_012]);
    assert_eq!(
        named_pids(&held_list(&[held(3, 5), held(1, 5), held(2, 9)])),
        [2, 3, 1]
    );
    let old_named: Vec<u32> = table.iter().take(HELD_NAMED).map(|h| h.pid).collect();
    assert!(!old_named.contains(&51_435), "{old_named:?}");
}

const CLOSER: &str = "D=/Users//person/.claude/projects/-Users-person/\
    00000000-0000-4000-8000-000000000001/subagents/workflows/wf_examplerun-1; \
    tail -f -n +1 \"$D/journal.jsonl\" | /usr/bin/grep -m1 -F \
    '\"agentId\":\"a0000000000000001\",\"result\"' > /dev/null; echo \"closer finished\"";

#[test]
fn the_notice_keeps_a_pipeline_a_long_path_would_push_out() {
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
    for word in [
        format!("{dir}/*/journal.jsonl"),
        format!("{dir}/sub\\ dir/journal.jsonl"),
        format!("'{dir}/subagents/journal.jsonl'"),
        format!("/{}/{}", "a".repeat(40), "b".repeat(40)),
    ] {
        let command = format!("{word} {}", "x".repeat(HELD_COMMAND_CHARS));
        assert!(quoted(&command).starts_with(&word), "{word}");
    }
    let broken = quoted(&format!("{CLOSER}\u{2028}echo\u{2029}done"));
    assert!(
        !broken.contains(['\u{2028}', '\u{2029}']) && !broken.chars().any(char::is_control),
        "{broken:?}"
    );
}

#[test]
fn the_ready_answer_is_an_assistant_line_never_the_announcement() {
    let marker = ready_marker(ID, &v("2.1.281"), 7);
    let prompt = prepare_prompt(&v("2.1.280"), &v("2.1.281"), Source::Native, &marker);
    assert!(prompt.contains(&marker));
    let user = format!(
        r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":{}}}]}}}}"#,
        aterm_json::to_string(&prompt).unwrap()
    );
    assert!(
        !transcript_has_ready(&user, &marker),
        "the announcement is not the answer"
    );
    let answered = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Everything is saved.\n{marker}"}}]}}}}"#
    );
    assert!(transcript_has_ready(
        &format!("{{cut\n{user}\n{answered}\n"),
        &marker
    ));
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
        work_ended: false,
        ready_unrecorded: false,
        ready_superseded: false,
        quiet: false,
        unnoticed: false,
    }
}

type Knock = (fn(&mut Facts), &'static str);

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
    let ask = idle_facts();
    assert_eq!(next_step(&Phase::Pending, &ask, false, 200), Step::Announce);
    assert_eq!(
        next_step(&announced, &ask, false, 200),
        Step::Wait("awaiting-ready")
    );
}

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
    let asking = Facts {
        quiet: false,
        ..brk.clone()
    };
    assert_eq!(next_step(&last, &asking, false, later), Step::GiveUp);
    for phase in [&last, &first] {
        assert_eq!(
            next_step(phase, &brk, false, later),
            Step::GiveUp,
            "{phase:?}"
        );
    }
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
    let stale = Facts {
        quiet: true,
        status: "shell".into(),
        status_age_s: QUIET_S,
        idle_looks: IDLE_LOOKS,
        ..idle_facts()
    };
    assert_eq!(next_step(&first, &stale, false, later), Step::GiveUp);
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

#[test]
fn a_quiet_round_is_not_held_for_ever_by_a_status_its_idle_screen_contradicts() {
    let stale = Facts {
        quiet: true,
        status: "shell".into(),
        status_age_s: 3_600,
        idle_looks: IDLE_LOOKS,
        ..idle_facts()
    };
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
        let land = Facts {
            rung: Rung::Land,
            ..stale.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &land, false, 30 * 86_400),
            Step::Wait("status-stale"),
            "{status}"
        );
        assert_eq!(
            requested_step(&Request::Now, &Phase::Pending, &stale, false, 200, "9.9.9"),
            Step::Terminate,
            "{status}"
        );
    }
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

#[test]
fn a_hold_stands_only_while_it_is_sampled() {
    let asked = Phase::Announced { at_s: 0, asks: 1 };
    let mut boxed = idle_facts();
    boxed.approval_box = true;
    let sample = |prior: (u64, u64), f: &Facts, now: u64| {
        let held = hold_since(prior, f, now);
        let mut f = f.clone();
        f.hold_s = if held.0 == 0 { 0 } else { now - held.0 };
        (held, next_step(&asked, &f, true, now))
    };
    let (held, _) = sample((0, 0), &boxed, DRAIN_S);
    let (held, step) = sample(held, &boxed, DRAIN_S + 7200);
    assert_eq!(held, (DRAIN_S + 7200, DRAIN_S + 7200), "begun again");
    assert_eq!(
        step,
        Step::Wait("box"),
        "a box two samples caught is no hold"
    );
    let mut held = (0, 0);
    let mut step = Step::Wait("x");
    for k in 0..=HOLD_S / 60 {
        (held, step) = sample(held, &boxed, DRAIN_S + 60 * k);
    }
    assert_eq!(held.0, DRAIN_S, "{held:?}");
    assert_eq!(step, Step::Void("box"));
    assert_eq!(
        hold_since((500, 600), &boxed, 600 + HOLD_GAP_S),
        (500, 600 + HOLD_GAP_S)
    );
    assert_eq!(
        hold_since((500, 600), &boxed, 601 + HOLD_GAP_S),
        (601 + HOLD_GAP_S, 601 + HOLD_GAP_S)
    );
}

const _: () = assert!(DRAIN_S == REASK_S);

#[test]
fn the_reducer_takes_the_step_the_drain_model_allows() {
    let model = aterm_models_harness::harness_upgrade_drain_bound_model();
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
    assert_eq!(steps.len(), 5, "{steps:?}");
    assert!(disagreements > 0, "the unbounded drain is caught");
    assert!(ends_caught > 0, "a restart on a lagging status is caught");
    assert!(holds_caught > 0, "a silent wait behind the lag is caught");
}

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
        requested_step(&Request::None, &asked, &attended, true, 200, "9.9.9"),
        Step::Wait("attended")
    );
    assert_eq!(
        requested_step(&Request::Now, &asked, &attended, true, 200, "9.9.9"),
        Step::Terminate,
        "the owner's word"
    );
}

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
    let tool = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#;
    assert!(!transcript_has_ready(
        &[ready.clone(), tool.to_string()].join("\n"),
        marker
    ));
    let sidechain = said("Subagent: done.", r#","isSidechain":true"#);
    let synthetic = r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
    assert!(transcript_has_ready(
        &[ready.clone(), sidechain, synthetic.to_string()].join("\n"),
        marker
    ));
    assert!(transcript_has_ready(&[past, ready].join("\n"), marker));
}

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
    assert!(
        !transcript_has_ready(
            &[ready.as_str(), &notice_parts, synthetic].join("\n"),
            marker
        ),
        "an API error after the new notice"
    );
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
        &[ready.as_str(), &side].join("\n"),
        marker
    ));
}

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
    let release =
        aterm_json::to_string(&Value::from(release_prompt(Agent::Claude, false))).expect("json");
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
    assert!(transcript_has_ready(&rows(&[&esc, &ready]), marker));
}

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

fn said_by(model: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","message":{{"model":"{model}","id":"msg_1","type":"message","role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"version":"2.1.282"}}"#
    )
}

fn between() -> [String; 4] {
    [
        r#"{"type":"user","message":{"role":"user","content":"run the tests"}}"#.to_string(),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#.to_string(),
        r#"{"type":"attachment","attachment":{"type":"edited_text_file"}}"#.to_string(),
        r#"{"type":"system","subtype":"turn_duration","version":"2.1.282"}"#.to_string(),
    ]
}

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
    let tail = jsonl(&[&prompt, &a, &tool, &b, &attached, &prompt, &c, &system]);
    assert_eq!(transcript_model(&tail).as_deref(), Some("claude-opus-5-5"));
    assert_eq!(
        transcript_first_model(&tail, "2.1.282").as_deref(),
        Some("claude-opus-5")
    );
    let quiet = jsonl(&[&prompt, &tool, &attached, &system]);
    assert_eq!(transcript_model(&quiet), None);
    assert_eq!(transcript_first_model(&quiet, "2.1.282"), None);
    assert_eq!(transcript_model(""), None);
    let cut = &b[b.len() / 3..];
    assert!(cut.contains("claude-fable-5-1"), "the cut keeps the model");
    assert_eq!(transcript_model(&jsonl(&[cut, &prompt])), None);
    assert_eq!(
        transcript_first_model(&jsonl(&[cut, &prompt, &c]), "2.1.282").as_deref(),
        Some("claude-opus-5-5")
    );
    let half = &c[..c.len() / 2];
    assert_eq!(
        transcript_model(&format!("{a}\n{half}")).as_deref(),
        Some("claude-opus-5")
    );
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
    let sidechain =
        said_by("claude-haiku-5").replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        transcript_model(&jsonl(&[&c, &sidechain])).as_deref(),
        Some("claude-opus-5-5")
    );
    for id in [
        "claude-opus-5-5[1m]",
        "us.anthropic.claude-opus-5-5-v1:0",
        "claude-opus-5-5@20260901",
    ] {
        assert_eq!(transcript_model(&said_by(id)).as_deref(), Some(id), "{id}");
    }
}

#[test]
fn a_provider_arn_is_a_model_id() {
    let arn = "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/\
               us.anthropic.claude-opus-5-5-20260901-v1:0";
    assert!(arn.len() > 64, "{}", arn.len());
    assert_eq!(transcript_model(&said_by(arn)).as_deref(), Some(arn));
    let endless = "a".repeat(4096);
    assert_eq!(transcript_model(&said_by(&endless)), None);
}

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
    assert_eq!(got(&["claude", "--print", "--model", "x"]), None);
}

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
    let unversioned = new.replace(r#","version":"2.1.282""#, "");
    assert_eq!(
        transcript_first_model(&jsonl(&[&unversioned]), "2.1.282"),
        None
    );
}

const TARGET: &str = "2.1.282";

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
fn the_agent_word_round_trips_and_an_unknown_word_is_no_agent() {
    use super::super::lanes::{Lanes, lanes};
    use aterm_phase::Program;
    for agent in Agent::ALL {
        assert_eq!(Agent::parse(agent.word()), Some(agent), "{agent:?}");
        assert_eq!(Agent::of(agent.program()), Some(agent), "{agent:?}");
    }
    assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
    assert_eq!(Agent::parse("muse"), Some(Agent::Muse));
    assert_eq!(Agent::Muse.word(), Agent::Muse.program().name());
    assert_eq!(
        lanes(Agent::Muse.program()),
        Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: false,
        },
        "muse: relaunch on exit and the live upgrade's quiet flow"
    );
    assert_eq!(Agent::Muse.record_key("s-0").as_deref(), Some("muse-s-0"));
    assert_eq!(Agent::parse("agy"), Some(Agent::Agy));
    assert_eq!(Agent::Agy.word(), Agent::Agy.program().name());
    assert_eq!(
        lanes(Agent::Agy.program()),
        Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: false,
        },
        "agy: relaunch on exit and the live upgrade's quiet flow"
    );
    let relaunch_alone = Lanes {
        relaunch: true,
        upgrade: false,
        restart_in_place: false,
    };
    assert_eq!(Agent::Agy.record_key("s-0").as_deref(), Some("agy-s-0"));
    assert_eq!(Agent::parse("gemini"), Some(Agent::Gemini));
    assert_eq!(Agent::Gemini.word(), Agent::Gemini.program().name());
    assert_eq!(
        lanes(Agent::Gemini.program()),
        relaunch_alone,
        "gemini: relaunch on exit alone"
    );
    assert_eq!(
        Agent::Gemini.record_key("s-0").as_deref(),
        Some("gemini-s-0")
    );
    for bad in [
        "opencode",
        "Codex",
        " codex",
        "generic",
        "Gemini",
        "agy ",
        "muse-code",
    ] {
        assert_eq!(Agent::parse(bad), None, "{bad:?}");
    }
    assert_eq!(Agent::of(Program::Generic), None);
    assert_ne!(
        lanes(Agent::Codex.program()),
        Lanes::NONE,
        "the negative control"
    );
}

#[test]
fn every_agent_is_in_all_once_and_the_others_are_all_but_claude_code() {
    for (at, agent) in Agent::ALL.into_iter().enumerate() {
        assert_eq!(agent as usize, at, "{agent:?}");
        assert_eq!(Agent::parse(agent.word()), Some(agent), "{agent:?}");
    }
    assert_eq!(Agent::ALL[0], Agent::Claude);
    assert_eq!(Agent::others(), &Agent::ALL[1..]);
    assert!(!Agent::others().contains(&Agent::Claude));
    assert!(
        Agent::others().contains(&Agent::Codex),
        "the negative control"
    );
}

#[test]
fn a_lanes_record_key_is_its_word_and_its_tab_and_no_conversation_is_one() {
    assert_eq!(
        Agent::Codex.record_key("s-1a2b").as_deref(),
        Some("codex-s-1a2b")
    );
    assert_eq!(Agent::Codex.record_tab("codex-s-1a2b"), Some("s-1a2b"));
    assert_eq!(Agent::keyed("codex-s-1a2b"), Some(Agent::Codex));
    assert_eq!(Agent::Claude.record_key("s-1a2b"), None);
    assert_eq!(Agent::Claude.record_tab("codex-s-1a2b"), None);
    let conversation = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    assert!(is_session_id(conversation));
    assert_eq!(Agent::keyed(conversation), None);
    assert_eq!(Agent::Codex.record_tab("codexs-1a2b"), None);
    for agent in Agent::ALL {
        let Some(prefix) = agent.record_prefix() else {
            continue;
        };
        assert!(
            prefix.starts_with(agent.word()) && prefix.ends_with('-'),
            "{agent:?}"
        );
        assert!(
            prefix
                .bytes()
                .any(|b| !(b.is_ascii_hexdigit() || b == b'-')),
            "{agent:?}: a conversation id could carry {prefix:?}"
        );
        let key = agent.record_key("s-0").expect("a key");
        assert_eq!(Agent::keyed(&key), Some(agent), "{agent:?}");
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
fn the_notices_and_continuations_keep_their_machine_parts() {
    let (from, to) = (v("2.1.282"), v("2.1.283"));
    let marker = ready_marker(ID, &to, 1);
    assert_eq!(
        prepare_prompt_with_model(
            &from,
            &to,
            Source::Managed,
            &marker,
            None,
            Some("claude-opus-5")
        ),
        prepare_prompt(&from, &to, Source::Managed, &marker),
        "no model to move to: the plain notice"
    );
    for want in [None, Some("claude-opus-5-5"), Some("claude-opus-5-5[1m]")] {
        let notice = prepare_prompt_with_model(
            &from,
            &to,
            Source::Managed,
            &marker,
            want,
            Some("claude-opus-5"),
        );
        assert!(
            notice.starts_with(ANNOUNCE_HEAD) && notice.contains(&marker),
            "{notice}"
        );
        if let Some(model) = want {
            assert!(notice.contains(model), "{notice}");
        }
    }
    for (before, after) in [
        (None, None),
        (Some("claude-opus-5"), None),
        (None, Some("claude-opus-5-5")),
        (Some("claude-opus-5"), Some("claude-opus-5-5[1m]")),
    ] {
        let text = continue_prompt_with_model(&from, &to, before, after);
        assert!(!text.starts_with(ANNOUNCE_HEAD), "{text}");
        for model in [before, after].into_iter().flatten() {
            assert!(text.contains(model), "{text}");
        }
    }
    let bare = continue_prompt(&from, &to, None);
    assert_eq!(continue_prompt_with_model(&from, &to, None, None), bare);
    assert!(!bare.contains("claude-"), "no model guessed: {bare}");
}

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
    let idle = Facts {
        status: "idle".to_string(),
        ..stale.clone()
    };
    assert_eq!(next_step(&asked, &idle, true, 10), Step::Terminate);
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
    assert_eq!(wait_word("status-stale", "busy"), "status-stale:busy");
}

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

fn prompt_row(text: &str) -> String {
    format!(
        r#"{{"type":"user","isSidechain":false,"promptSource":"typed","origin":{{"kind":"human"}},"message":{{"role":"user","content":"{text}"}}}}"#
    )
}

fn answer_row(model: &str) -> String {
    format!(
        r#"{{"type":"assistant","isSidechain":false,"message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"ok"}}]}}}}"#
    )
}

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
    let odd = r#"{"type":"user","message":{"role":"user","content":{"weird":1}}}"#.to_string();
    assert!(transcript_tasked(&text(&[&odd]), &[]));
    let mut scan = TaskScan::default();
    assert!(!scan.line(&notice) && !scan.line(&ready));
    assert!(scan.line(&model_cmd) && !scan.settled());
    assert!(!scan.line(&model_out) && !scan.tasked());
    assert!(scan.line(&person) && scan.settled());
    assert!(scan.line(&notice) && scan.line(&model_out) && scan.tasked());
    let (from, to) = (v("2.1.281"), v("2.1.283"));
    for typed in [
        prepare_prompt(&from, &to, Source::Managed, "M"),
        prepare_prompt_with_model(&from, &to, Source::Managed, "M", Some("opus"), None),
        continue_prompt(&from, &to, Some("haiku")),
        continue_prompt_with_model(&from, &to, None, Some("opus")),
        super::super::relaunch::resumed_prompt("2.1.283", "exit"),
        super::super::relaunch::resumed_prompt("2.1.283", "memory"),
    ] {
        assert!(typed.starts_with(HARNESS_MARK), "{typed}");
    }
}

#[test]
fn a_local_commands_output_in_a_system_row_starts_no_task() {
    let caveat = r#"{"type":"user","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"<local-command-caveat>The command below was run directly in Claude Code, not sent to you as a request.</local-command-caveat>"}}"#.to_string();
    let clear = prompt_row(
        "<command-name>/clear</command-name>\\n            <command-message>clear</command-message>\\n            <command-args></command-args>",
    );
    let output = r#"{"isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout></local-command-stdout>","level":"info","isMeta":false,"commandRun":{"command":"clear","args":""},"userType":"external","entrypoint":"cli"}"#.to_string();
    let other = r#"{"isSidechain":false,"type":"system","subtype":"compact_boundary","content":"<local-command-stdout></local-command-stdout>","compactMetadata":{"trigger":"local_command"},"userType":"external"}"#.to_string();
    let not_output = r#"{"isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>","userType":"external"}"#.to_string();
    let person = prompt_row("Staged harness test, please follow exactly.");
    let ready = answer_row("claude-haiku-4-5-20251001");
    let text = |rows: &[&String]| {
        rows.iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !transcript_tasked(&text(&[&caveat, &clear, &output]), &[]),
        "`/clear`, its output a system row"
    );
    for (what, rows) in [
        (
            "a person's prompt after it",
            vec![&caveat, &clear, &output, &person, &ready],
        ),
        ("another subtype's system row", vec![&clear, &other]),
        (
            "a local_command row that is no output",
            vec![&clear, &not_output],
        ),
    ] {
        assert!(transcript_tasked(&text(&rows), &[]), "{what}");
    }
    let mut scan = TaskScan::default();
    assert!(!scan.line(&caveat));
    assert!(scan.line(&clear) && !scan.settled(), "a command waits");
    assert!(!scan.line(&output) && !scan.tasked(), "its output: no turn");
}

#[test]
fn the_supervisors_own_turns_by_its_ledger_are_no_task() {
    let cfg = crate::supervise::SupervisorConfig {
        continue_text: "keep going".to_string(),
        ..crate::supervise::SupervisorConfig::default()
    };
    let ours: Vec<Typed> = [
        (cfg.continue_text.as_str(), turn_end::RULE_CONTINUE),
        (cfg.answer_text.as_str(), turn_end::RULE_ANSWER),
        ("/compact", turn_end::RULE_COMPACT),
    ]
    .into_iter()
    .map(|(text, rule)| Typed {
        at_ms: 1_000,
        text: text.to_string(),
        rule: rule.to_string(),
    })
    .collect();
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

#[test]
fn the_harnesss_marked_acts_are_no_task_ledger_or_not() {
    let json = |text: &str| {
        let j = aterm_json::to_string(&Value::from(text)).expect("json");
        j[1..j.len() - 1].to_string()
    };
    for act in [
        crate::supervise::config::CONTINUE_TEXT.to_string(),
        turn_end::DONE_CHECK.to_string(),
        turn_end::land_text(None),
        turn_end::land_nudge_text(Some("never push to main")),
    ] {
        let chain = [
            prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283."),
            answer_row("claude-haiku-4-5"),
            prompt_row(&json(&act)),
            answer_row("claude-haiku-4-5"),
        ]
        .join("\n");
        assert!(!transcript_tasked(&chain, &[]), "{act}");
    }
    let chain = [
        prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283."),
        answer_row("claude-haiku-4-5"),
        prompt_row("keep going"),
        answer_row("claude-haiku-4-5"),
    ]
    .join("\n");
    assert!(transcript_tasked(&chain, &[]), "CONTROL: unmarked words");
}

fn asked_by_row(kind: &str, meta: bool, text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"user","isMeta":{meta},"message":{{"role":"user","content":{}}},"origin":{{"kind":"{kind}"}},"promptSource":"system","version":"2.1.300"}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

fn notification_row() -> String {
    asked_by_row(
        "task-notification",
        false,
        "<task-notification>\n<task-id>b3e5x3wn5</task-id>\n<status>completed</status>\n</task-notification>",
    )
}

#[test]
fn a_conversation_only_a_peer_directed_has_a_task() {
    let peer = asked_by_row(
        "peer",
        true,
        "Another Claude session sent a message:\n<agent-message from=\"a6ab\">rebase onto main</agent-message>",
    );
    let unknown_meta = asked_by_row("brand-new-kind", true, "Run the benchmarks again.");
    let ready = answer_row("claude-haiku-4-5-20251001");
    let carry = prompt_row("[aterm harness] Upgraded: this session was restarted on 2.1.283.");
    let expansion = asked_by_row("human", true, "Please analyze this codebase");
    for (what, rows) in [
        ("a peer's message, answered", vec![&peer, &ready]),
        ("a peer's message, not answered yet", vec![&peer]),
        (
            "an unknown kind, recorded isMeta",
            vec![&unknown_meta, &ready],
        ),
        (
            "a peer's message after the harness's line",
            vec![&carry, &ready, &peer, &ready],
        ),
    ] {
        let jsonl = rows
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript_tasked(&jsonl, &[]), "{what}");
    }
    for (what, rows) in [
        ("an expansion of the person's", vec![&expansion, &ready]),
        ("the harness's carry-on", vec![&carry, &ready]),
    ] {
        let jsonl = rows
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!transcript_tasked(&jsonl, &[]), "NEGATIVE CONTROL: {what}");
    }
    assert!(
        transcript_tasked(&notification_row(), &[]),
        "a notification is kept"
    );
}

fn bare_user_row(extra: &str, content: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"user"{extra},"message":{{"role":"user","content":{content}}}}}"#
    )
}

#[test]
fn a_restarts_cruise_prompt_lands_nothing_in_the_record() {
    use super::super::relaunch::{CAUSE_HOST, resumed_prompt};
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let scan = |rows: &[String]| {
        let mut s = FinishedScan::default();
        for row in rows {
            s.line(row);
        }
        s
    };
    let carry_on = resumed_prompt("2.1.282", CAUSE_HOST);
    let report = [
        typed_row("Fix the lexer and land it."),
        model_row("All tasks are complete and verified; nothing is left to do."),
    ];
    let answered = |asked: &str, answer: &str| {
        report
            .iter()
            .cloned()
            .chain([typed_row(asked), model_row(answer)])
            .collect::<Vec<_>>()
    };
    let legacy_check = carry_on.replace(CARRY_ON, TOKEN_CHECK_FIXTURE);
    assert_ne!(legacy_check, carry_on, "the fixture replaced the carry-on");
    let checks = [
        checking(&carry_on, false),
        checking(&carry_on, true),
        legacy_check,
    ];
    for check in &checks {
        assert!(check.starts_with(HARNESS_MARK), "{check}");
        for answer in [
            "All done.",
            "DONE",
            "All tasks are complete and verified.",
            DEFENSE,
        ] {
            let s = scan(&answered(check, answer));
            assert!(!s.finished(), "a claim, no landing: {answer}: {check}");
            assert!(s.said_done(), "the claim stands: {answer}: {check}");
            assert!(
                !finished(&answered(&carry_on, answer)),
                "CONTROL, the carry-on: {answer}"
            );
        }
        let mut reopened = landing_rows();
        reopened.push(typed_row(check));
        assert!(!finished(&reopened), "it opens a landed task: {check}");
    }
    let land = turn_end::land_text(None);
    assert!(finished(&answered(&land, DEFENSE)), "Land's instruction");
    let mut carried = landing_rows();
    carried.push(typed_row(&carry_on));
    assert!(finished(&carried), "the plain carry-on opens nothing");
    let quoted = format!("Please read this: {land}");
    assert!(
        !finished(&answered(&quoted, DEFENSE)),
        "CONTROL: a person's prompt"
    );
}

#[test]
fn a_walls_retry_delivers_the_land_act_it_cut_again_to_the_record() {
    use super::super::relaunch::{CAUSE_HOST, resumed_prompt};
    let at = |row: String| row.replacen('{', r#"{"timestamp":"1970-01-01T00:00:01.000Z","#, 1);
    let retry = turn_end::wall_retry_text(
        turn_end::RULE_API_RETRY,
        aterm_phase::WallKind::Overloaded,
        aterm_phase::Program::Claude,
    );
    let ours: Vec<Typed> = [
        (retry.as_str(), turn_end::RULE_API_RETRY),
        ("keep going", turn_end::RULE_CONTINUE),
        ("/compact", turn_end::RULE_COMPACT),
    ]
    .into_iter()
    .map(|(text, rule)| Typed {
        at_ms: 1_000,
        text: text.to_string(),
        rule: rule.to_string(),
    })
    .collect();
    let wall = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: 529 Overloaded"}]},"isApiErrorMessage":true,"error":"unknown"}"#.to_string();
    let closed = r#"{"isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":180000,"messageCount":4,"isMeta":false}"#.to_string();
    let retried = at(typed_row(&retry));
    let scan = |rows: &[String], ours: &[Typed]| {
        let mut scan = FinishedScan::default();
        scan.supervisor_typed(ours);
        for row in [
            typed_row("Fix the lexer and land it."),
            model_row("All done: the parser and the lexer are in."),
        ]
        .iter()
        .chain(rows)
        {
            scan.line(row);
        }
        scan
    };
    let check = || typed_row(turn_end::DONE_CHECK);
    let land = || typed_row(&turn_end::land_text(None));
    let nudge = || typed_row(&turn_end::land_nudge_text(Some("never push to main")));
    let done = || model_row(DEFENSE);
    let going = || model_row("Running the last suite before I say done.");
    let finished: [(&str, Vec<String>); 5] = [
        (
            "LAND, cut, retried",
            vec![land(), wall.clone(), retried.clone(), done()],
        ),
        (
            "LAND answered, then cut, retried",
            vec![land(), going(), wall.clone(), retried.clone(), done()],
        ),
        (
            "LAND, cut twice",
            vec![
                land(),
                wall.clone(),
                retried.clone(),
                wall.clone(),
                retried.clone(),
                done(),
            ],
        ),
        (
            "a land nudge, cut, retried",
            vec![
                land(),
                going(),
                nudge(),
                wall.clone(),
                retried.clone(),
                done(),
            ],
        ),
        (
            "LAND, cut, retried, closed",
            vec![
                land(),
                wall.clone(),
                retried.clone(),
                done(),
                closed.clone(),
            ],
        ),
    ];
    for (what, rows) in &finished {
        assert!(scan(rows, &ours).finished(), "{what}");
    }
    let carry_on = typed_row(&resumed_prompt("2.1.300", CAUSE_HOST));
    let compact = at(typed_row(
        "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>",
    ));
    let open: [(&str, Vec<String>, &[Typed]); 7] = [
        (
            "a person's prompt after the wall",
            vec![
                land(),
                wall.clone(),
                typed_row("Also fix the docs."),
                done(),
            ],
            &ours,
        ),
        (
            "the supervisor's /compact after the wall",
            vec![land(), wall.clone(), compact, done()],
            &ours,
        ),
        (
            "a restart's carry-on after the wall",
            vec![land(), wall.clone(), carry_on, done()],
            &ours,
        ),
        (
            "the retry with no ledger: someone's words",
            vec![land(), wall.clone(), retried.clone(), done()],
            &[],
        ),
        (
            "the worker went on past the wall",
            vec![land(), wall.clone(), going(), retried.clone(), done()],
            &ours,
        ),
        (
            "a wall inside a plain continuation's turn",
            vec![
                at(typed_row("keep going")),
                wall.clone(),
                retried.clone(),
                done(),
            ],
            &ours,
        ),
        (
            "the Cruise prompt, cut, retried",
            vec![check(), wall.clone(), retried.clone(), done()],
            &ours,
        ),
    ];
    for (what, rows, ours) in &open {
        let s = scan(rows, ours);
        assert!(!s.finished(), "CONTROL, {what}");
        assert!(s.said_done(), "CONTROL, {what}: a done report");
    }
    let idle = model_row("No lander is running, so I'm not re-arming that watch.");
    let s = scan(&[land(), wall.clone(), retried, idle], &ours);
    assert!(
        !s.finished(),
        "CONTROL: an idle report answering the retried LAND"
    );
}

#[test]
fn only_a_walls_retry_by_its_ledger_rule_delivers_the_cut_land_act_again() {
    let at = |row: String| row.replacen('{', r#"{"timestamp":"1970-01-01T00:00:01.000Z","#, 1);
    let wall = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your limit · resets 3pm"}]},"isApiErrorMessage":true,"error":"rate_limit"}"#.to_string();
    let scan = |rule: &str| {
        let ours = [Typed {
            at_ms: 1_000,
            text: "keep going".to_string(),
            rule: rule.to_string(),
        }];
        let mut scan = FinishedScan::default();
        scan.supervisor_typed(&ours);
        for row in [
            typed_row("Fix the lexer and land it."),
            model_row("All done: the parser and the lexer are in."),
            typed_row(&turn_end::land_text(None)),
            wall.clone(),
            at(typed_row("keep going")),
            model_row(DEFENSE),
        ] {
            scan.line(&row);
        }
        scan
    };
    let resumed = scan(turn_end::RULE_LIMIT_RESUME);
    assert!(
        resumed.finished(),
        "the usage resume delivers the act again"
    );
    let continued = scan(turn_end::RULE_CONTINUE);
    assert!(
        !continued.finished(),
        "a plain continuation after the wall delivers no act"
    );
    assert!(continued.said_done(), "its defense is a done report");
    assert!(
        !scan("").finished(),
        "a ledger row that names no rule is no retry"
    );
    assert!(
        scan(turn_end::RULE_API_RETRY).finished(),
        "NEGATIVE CONTROL: a retry's rule"
    );
}

#[test]
fn the_vendors_own_continuation_delivers_the_cut_land_act_again_to_the_record() {
    let wall = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your limit · resets 3pm"}]},"isApiErrorMessage":true,"error":"rate_limit"}"#.to_string();
    let auto = asked_by_row(
        "auto-continuation",
        true,
        "Your claude.ai usage limit has reset. Continue the task you were working on when the \
         limit was reached; do not repeat work that is already complete.",
    );
    let done = model_row(DEFENSE);
    let scan = |rows: &[String]| {
        let mut scan = FinishedScan::default();
        for row in [
            typed_row("Fix the lexer and land it."),
            model_row("All done: the parser and the lexer are in."),
        ]
        .iter()
        .chain(rows)
        {
            scan.line(row);
        }
        scan
    };
    let land = typed_row(&turn_end::land_text(None));
    let s = scan(&[land.clone(), wall.clone(), auto.clone(), done.clone()]);
    assert!(s.finished(), "the continuation delivers LAND again");
    for (what, rows) in [
        (
            "nothing asked before the limit",
            vec![wall.clone(), auto.clone(), done.clone()],
        ),
        (
            "the Cruise prompt before the limit",
            vec![
                typed_row(turn_end::DONE_CHECK),
                wall.clone(),
                auto.clone(),
                done.clone(),
            ],
        ),
        (
            "a background task's wake after the limit",
            vec![land, wall.clone(), notification_row(), done.clone()],
        ),
    ] {
        let s = scan(&rows);
        assert!(!s.finished(), "CONTROL, {what}");
        assert!(s.said_done(), "CONTROL, {what}: a done report");
    }
}

#[test]
fn lands_answer_after_the_instruction_is_a_land_act_to_the_record() {
    let at = |row: String| row.replacen('{', r#"{"timestamp":"1970-01-01T00:00:01.000Z","#, 1);
    let land = || typed_row(&turn_end::land_text(None));
    let question = || model_row("Should I squash the two commits or keep them apart?");
    let answer = || at(typed_row(turn_end::LAND_ANSWER));
    let done = || {
        model_row(
            "Kept them apart and pushed.\nNothing left: both commits are merged into the remote \
             main branch.",
        )
    };
    let scan = |rows: &[String], rule: &str| {
        let ours = [Typed {
            at_ms: 1_000,
            text: turn_end::LAND_ANSWER.to_string(),
            rule: rule.to_string(),
        }];
        let mut scan = FinishedScan::default();
        scan.supervisor_typed(&ours);
        for row in [
            typed_row("Fix the lexer and land it."),
            model_row("Stage 1 of 3 is in."),
        ]
        .iter()
        .chain(rows)
        {
            scan.line(row);
        }
        scan
    };
    let closed = r#"{"isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":180000,"messageCount":4,"isMeta":false}"#.to_string();
    let landed = scan(
        &[land(), question(), answer(), done(), closed],
        turn_end::RULE_LAND_ANSWER,
    );
    assert!(
        landed.finished(),
        "the answer after the instruction lands it"
    );
    for (what, rows, rule) in [
        (
            "no instruction before it",
            vec![question(), answer(), done()],
            turn_end::RULE_LAND_ANSWER,
        ),
        (
            "a person's prompt between",
            vec![
                land(),
                typed_row("Also fix the docs."),
                question(),
                answer(),
                done(),
            ],
            turn_end::RULE_LAND_ANSWER,
        ),
        (
            "its words under another rule",
            vec![land(), question(), answer(), done()],
            turn_end::RULE_ANSWER,
        ),
    ] {
        let s = scan(&rows, rule);
        assert!(!s.finished(), "CONTROL, {what}");
        assert!(s.said_done(), "CONTROL, {what}: a done report");
    }
    let idle = model_row("No lander is running, so I'm not re-arming that watch.");
    let s = scan(
        &[land(), question(), answer(), idle],
        turn_end::RULE_LAND_ANSWER,
    );
    assert!(!s.finished(), "CONTROL: an idle report answering it");
}

const FINISHED_REPORT: &str = "Landed the fix and verified it on main.\n\nATERM-TASK-DONE";

const DEFENSE: &str = "Landed the fix and verified it.\nNothing left: it is merged into the \
                       remote main branch.";

fn landing_rows() -> Vec<String> {
    vec![
        typed_row("Fix the lexer and land it."),
        model_row("The lexer is fixed and its tests pass."),
        typed_row(&turn_end::land_text(None)),
        model_row(DEFENSE),
    ]
}

#[test]
fn finished_holds_until_someone_asks() {
    let ask = typed_row("Fix the lexer and land it.");
    let reply = |text: &str| model_row(text);
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let base = landing_rows();
    let with = |more: &[String]| {
        base.iter()
            .cloned()
            .chain(more.iter().cloned())
            .collect::<Vec<_>>()
    };
    assert!(!finished(&base[..3]), "Land asked, not answered");
    assert!(finished(&base), "the landing");
    let auto = asked_by_row(
        "auto-continuation",
        true,
        "Your claude.ai usage limit has reset. Continue the task you were working on when the \
         limit was reached; do not repeat work that is already complete.",
    );
    let summary = bare_user_row(
        r#","isCompactSummary":true"#,
        r#""This session is being continued from a previous conversation that ran out of context.""#,
    );
    let usage = typed_row(
        "<command-name>/usage</command-name>\n<command-message>usage</command-message>\n<command-args></command-args>",
    );
    let usage_out =
        typed_row("<local-command-stdout>Status dialog dismissed</local-command-stdout>");
    let loop_wake = r#"{"isSidechain":false,"promptId":"p-wake","type":"user","isMeta":true,"promptSource":"system","scheduledTaskId":"t1","message":{"role":"user","content":"keep going: check the land log"}}"#.to_string();
    let stays: [(&str, Vec<String>); 7] = [
        (
            "a background task's notification, answered",
            with(&[notification_row(), reply("The worktree removal finished.")]),
        ),
        (
            "an older build's notification, by its words",
            with(&[
                typed_row("<task-notification>\n<status>completed</status>\n</task-notification>"),
                reply("The lander finished: the batch landed."),
            ]),
        ),
        (
            "the harness's own turn, answered",
            with(&[
                typed_row("[aterm harness] Relaunched: Claude Code exited without anyone asking."),
                reply("Resumed. Re-ran the suite: all green."),
            ]),
        ),
        (
            "a notification, then the usage-limit continuation, answered",
            with(&[
                notification_row(),
                auto.clone(),
                reply("Checked the background job: it finished."),
            ]),
        ),
        (
            "a compaction's summary, then a notification",
            with(&[summary.clone(), notification_row(), reply("Noted.")]),
        ),
        (
            "a person's command that started no turn",
            with(&[usage.clone(), usage_out]),
        ),
        (
            "a `/loop` wake, answered",
            with(&[loop_wake, reply("The land log shows nothing new.")]),
        ),
    ];
    for (what, rows) in &stays {
        assert!(finished(rows), "{what}");
    }
    let mut scan = FinishedScan::default();
    for row in &base[..3] {
        assert!(!scan.line(row));
    }
    assert!(scan.line(&base[3]));
    assert!(scan.line(&notification_row()));
    assert!(scan.line(&reply("Done with the cleanup.")));
    assert!(!scan.line(&typed_row("Now add the flag.")));
    let rules = format!(
        "{} (standing rules: never force-push)",
        turn_end::DONE_CHECK
    );
    for check in [
        turn_end::DONE_CHECK,
        rules.as_str(),
        turn_end::DONE_CHECK_BEFORE,
    ] {
        assert!(!finished(&with(&[typed_row(check)])), "{check}: opened");
        for said in ["DONE", DEFENSE] {
            let rows = with(&[typed_row(check), reply(said)]);
            assert!(!finished(&rows), "{check}: {said}");
        }
    }
    let peer = asked_by_row(
        "peer",
        true,
        "Another Claude session sent a message:\n<agent-message from=\"a6ab\">rebase onto main</agent-message>",
    );
    let unknown = asked_by_row("brand-new-kind", false, "Run the benchmarks again.");
    let unknown_meta = asked_by_row("brand-new-kind", true, "Run the benchmarks again.");
    let init = typed_row("<command-name>/init</command-name>");
    let tool = bare_user_row(
        "",
        r#"[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]"#,
    );
    let asked_land = || base[..3].to_vec();
    let after_land = |row: String| [asked_land(), vec![row]].concat();
    let defense_json = aterm_json::to_string(&Value::from(DEFENSE)).expect("json");
    let synthetic = format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":{defense_json}}}]}}}}"#
    );
    let api_error = format!(
        r#"{{"isSidechain":false,"type":"assistant","isApiErrorMessage":true,"message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{defense_json}}}]}}}}"#
    );
    let subagent = format!(
        r#"{{"isSidechain":true,"type":"assistant","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{defense_json}}}]}}}}"#
    );
    let opened: [(&str, Vec<String>); 11] = [
        (
            "a person's prompt, not answered yet",
            with(&[typed_row("Now add the flag.")]),
        ),
        (
            "a peer session's message, answered",
            with(&[peer, reply("Rebased.")]),
        ),
        ("a row of an unknown kind", with(&[unknown])),
        ("an isMeta row of an unknown kind", with(&[unknown_meta])),
        (
            "a person's command that started a turn",
            with(&[init, reply("Wrote CLAUDE.md.")]),
        ),
        (
            "a later text of the same turn",
            with(&[reply("One more thing: the docs are stale.")]),
        ),
        (
            "a later text of the same turn, past a tool's result",
            with(&[tool, reply("Checked again: one test fails.")]),
        ),
        (
            "the retired token alone answering Land",
            after_land(reply("ATERM-TASK-DONE")),
        ),
        ("the defense by <synthetic>", after_land(synthetic)),
        ("the defense in an API error", after_land(api_error)),
        ("the defense in a subagent's row", after_land(subagent)),
    ];
    for (what, rows) in &opened {
        assert!(!finished(rows), "{what}");
    }
    assert!(!finished(&with(&[summary, reply("And the changelog.")])));
    assert!(!finished(std::slice::from_ref(&ask)), "asked, not answered");
}

#[test]
fn lands_instruction_typed_again_after_a_compaction_is_a_land_act_to_the_record() {
    let wall = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"Prompt is too long"}]},"isApiErrorMessage":true,"error":"invalid_request"}"#.to_string();
    let compact = typed_row(
        "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>",
    );
    let summary = r#"{"type":"user","uuid":"s-1","isSidechain":false,"isCompactSummary":true,"message":{"role":"user","content":"This session is being continued."}}"#.to_string();
    let land = turn_end::land_text(None);
    let scan = |continuation: &str| {
        let mut s = FinishedScan::default();
        for row in [
            typed_row("Fix the lexer and land it."),
            model_row("All tasks are complete and verified; nothing is left to do."),
            typed_row(&land),
            wall.clone(),
            compact.clone(),
            summary.clone(),
            typed_row(continuation),
            model_row(DEFENSE),
        ] {
            s.line(&row);
        }
        s
    };
    let again = scan(&land);
    assert!(again.finished(), "{again:?}");
    let going = scan("keep going");
    assert!(!going.finished() && going.said_done(), "{going:?}");
}

#[test]
fn lands_acts_are_read_by_their_words_whoever_typed_them() {
    let ask = typed_row("Fix the lexer and land it.");
    let report = model_row("All tasks are complete and verified; nothing is left to do.");
    let scan = |rows: &[String]| {
        let mut s = FinishedScan::default();
        for row in rows {
            s.line(row);
        }
        s
    };
    for land in [
        turn_end::land_text(None),
        turn_end::land_text(Some("never push to main")),
        turn_end::land_nudge_text(None),
    ] {
        let s = scan(&[
            ask.clone(),
            report.clone(),
            typed_row(&land),
            model_row(DEFENSE),
        ]);
        assert!(s.finished(), "no ledger: {land}");
    }
    for check in [
        turn_end::DONE_CHECK.to_string(),
        turn_end::done_check_text(Some("never push to main"), true),
    ] {
        let mut rows = landing_rows();
        rows.push(typed_row(&check));
        assert!(!scan(&rows).finished(), "no ledger, opened: {check}");
    }
    let quoted = format!("Is this wording right? {}", turn_end::land_text(None));
    let s = scan(&[ask, report, typed_row(&quoted), model_row(DEFENSE)]);
    assert!(!s.finished() && s.said_done(), "{s:?}");
}

fn old_challenge(irreversible: bool) -> String {
    let text = format!(
        "{} (still open): a done report ends nothing while any is left, a blocked item \
         included. Do each item now and verify it, then commit and push it (merged to main \
         where this repository lands work). Say the task is complete only when nothing is \
         uncommitted, untracked, stashed or unpushed and your report names no open item, \
         follow-up or next step.",
        turn_end::OPEN_ITEMS_HEAD
    );
    if irreversible {
        format!("{text} {}", turn_end::DONE_CHECK_REVERSIBLE)
    } else {
        text
    }
}

#[test]
fn the_retired_challenges_are_checks_to_the_record() {
    let open = model_row(
        "Built and tested the parser; committed on main.\nStill open: the lexer's e2e suite \
         (not run).\nATERM-TASK-DONE",
    );
    let at = |row: String| row.replacen('{', r#"{"timestamp":"1970-01-01T00:00:01.000Z","#, 1);
    let challenge = old_challenge(false);
    let ours: Vec<Typed> = [
        (challenge.as_str(), turn_end::RULE_OPEN_ITEMS),
        ("keep going", turn_end::RULE_CONTINUE),
    ]
    .into_iter()
    .map(|(text, rule)| Typed {
        at_ms: 1_000,
        text: text.to_string(),
        rule: rule.to_string(),
    })
    .collect();
    let scan_rows = |rows: &[String]| {
        let mut s = FinishedScan::default();
        s.supervisor_typed(&ours);
        for row in rows {
            s.line(row);
        }
        s
    };
    let scan = |rows: &[String]| {
        let prefix = [typed_row("Fix the parser and land it."), open.clone()];
        scan_rows(&[&prefix[..], rows].concat())
    };
    let s = scan(&[]);
    assert!(
        !s.finished() && s.said_done(),
        "the token over open work lands nothing"
    );
    let work = model_row("Ran the e2e suite: two tests fail. Investigating the timeout now.");
    let words = model_row("All done; nothing left to do.");
    let earlier = "Your report ends with ATERM-TASK-DONE but still names open work (still open). \
                   Do each of those items you can still do yourself now, and verify it. End your \
                   reply with ATERM-TASK-DONE on a line by itself only when every item left needs \
                   a person (a credential, a decision, physical access) or cannot be done on this \
                   host, each named with its blocker; a slow check is not a blocker.";
    let ordinary = "Your report still names open work (still open). Do each of those items you \
                    can still do yourself now, and verify it. Report whether the task is \
                    complete, the outcome, verification, and any remaining blockers in ordinary \
                    language.";
    let uncommitted = format!(
        "{}: /Users//a/Developer/parser: 1 changed (src/lexer.rs). A done report ends nothing \
         while any is left. Commit and push it (merged to main where this repository lands \
         work), a stash's work included; delete, overwrite and force-push nothing to get there. \
         Say the task is complete only when nothing is uncommitted, untracked, stashed or \
         unpushed and your report names no open item, follow-up or next step.",
        turn_end::UNCOMMITTED_HEAD
    );
    for asked in [
        challenge.clone(),
        old_challenge(true),
        uncommitted,
        earlier.to_string(),
        earlier.replace("End your reply with", "Reply with"),
        ordinary.to_string(),
    ] {
        let asked = at(typed_row(&asked));
        let s = scan(std::slice::from_ref(&asked));
        assert!(!s.finished() && s.said_done(), "unanswered: {asked}");
        let mut reopened = landing_rows();
        reopened.push(asked.clone());
        assert!(!scan_rows(&reopened).finished(), "opens a landing: {asked}");
        for (what, answer) in [
            ("work goes on", &work),
            ("done in words", &words),
            ("the token", &model_row(FINISHED_REPORT)),
            ("the clean defense", &model_row(DEFENSE)),
        ] {
            let s = scan(&[asked.clone(), answer.clone()]);
            assert!(!s.finished(), "{what}: {asked}");
        }
        let s = scan(&[asked.clone(), words.clone()]);
        assert!(s.said_done(), "done in words is a claim: {asked}");
    }
    let mut kept = landing_rows();
    kept.extend([at(typed_row("keep going")), work.clone()]);
    assert!(
        scan_rows(&kept).finished(),
        "the loop's own line reopens nothing"
    );
    let quoted = at(typed_row(&format!("Please read this: {challenge}")));
    let s = scan(&[quoted, words]);
    assert!(
        !s.finished() && s.said_done(),
        "a person's prompt: a first done report"
    );
}

const TOKEN_CHECK_FIXTURE: &str = "Before you stop: do every open item, skipped check, follow-up and \
     next step you can still do yourself now, and verify it (build, test, measure where numbers \
     were asked for, commit and land where this repository lands work). End your reply with \
     ATERM-TASK-DONE on a line by itself only when everything left needs a person (a credential, \
     a decision, physical access) or cannot be done on this host, each item named with its \
     blocker; a slow check is not a blocker. Otherwise continue.";

#[test]
fn legacy_land_instructions_land_on_a_clean_defense() {
    let instruction = "[aterm harness] Autopilot is set to Land: wrap up this work cleanly. \
                       Finish only the step in hand and start nothing new. Make sure what is \
                       done is verified, and leave it committed (and pushed, if that is how \
                       this repository works). Then write a short summary of what is done \
                       and what is left, and reply with ATERM-TASK-DONE on a line by itself.";
    let nudge = "[aterm harness] Still landing: finish only what leaves the work clean, then \
                 reply with ATERM-TASK-DONE on a line by itself.";
    let answered = |prompt: &str, said: &str| {
        [
            typed_row("Fix the lexer and land it."),
            model_row("The lexer is fixed; running its tests now."),
            typed_row(prompt),
            model_row(said),
        ]
        .join("\n")
    };
    for prompt in [instruction, nudge]
        .into_iter()
        .chain(turn_end::RETIRED_LANDS)
        .chain(turn_end::RETIRED_LAND_NUDGES)
    {
        assert!(
            transcript_finished(&answered(prompt, DEFENSE), &[]),
            "{prompt}"
        );
        for said in [
            "The task is complete. The tests passed and the fix is committed.",
            FINISHED_REPORT,
        ] {
            assert!(
                !transcript_finished(&answered(prompt, said), &[]),
                "CONTROL: {said}: {prompt}"
            );
        }
    }
}

#[test]
fn only_a_land_acts_clean_defense_lands_the_task_in_the_record() {
    let ask = typed_row("Fix the lexer and land it.");
    let report = model_row("All done: the suite passes.");
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let answered = |asked: &str, said: &str| {
        [
            ask.clone(),
            report.clone(),
            typed_row(asked),
            model_row(said),
        ]
    };
    let reversible = format!(
        "{} {}",
        turn_end::DONE_CHECK,
        turn_end::DONE_CHECK_REVERSIBLE
    );
    let rules = format!("{reversible} (standing rules: never force-push)");
    let checks = [
        turn_end::DONE_CHECK,
        reversible.as_str(),
        rules.as_str(),
        TOKEN_CHECK_FIXTURE,
        turn_end::DONE_CHECK_TOKEN_BEFORE,
        turn_end::DONE_CHECK_BEFORE,
    ];
    let claims = [
        "Verified on main.\n\nATERM-TASK-DONE",
        "ATERM-TASK-DONE\n\nThe suite passes on main.",
        "DONE",
        "I'm stopping here.",
        "The task is complete. All checks passed and the fix is committed.",
    ];
    for check in checks {
        for said in claims.into_iter().chain([DEFENSE]) {
            assert!(!finished(&answered(check, said)), "{check}: {said}");
        }
    }
    let land = turn_end::land_text(None);
    let nudge = turn_end::land_nudge_text(Some("never push to main"));
    let acts = [
        land.as_str(),
        nudge.as_str(),
        turn_end::LAND,
        turn_end::LAND_NUDGE,
    ]
    .into_iter()
    .chain(turn_end::RETIRED_LANDS)
    .chain(turn_end::RETIRED_LAND_NUDGES);
    let needs = format!("{DEFENSE}\nNEEDS YOU: the release credential.");
    let open_work = "Pushed the lexer fix.\nNothing left: the parser's follow-up is still open.";
    for act in acts {
        assert!(finished(&answered(act, DEFENSE)), "{act}");
        for said in claims {
            assert!(!finished(&answered(act, said)), "a claim: {act}: {said}");
        }
        assert!(!finished(&answered(act, &needs)), "a need: {act}");
        assert!(!finished(&answered(act, open_work)), "open work: {act}");
    }
    assert!(!finished(&[ask.clone(), model_row(claims[0])]));
    assert!(!finished(&[ask.clone(), model_row(DEFENSE)]));
    let near = typed_row("If the task is finished, reply DONE.");
    let mut rows = landing_rows();
    rows.extend([near, model_row("DONE")]);
    assert!(!finished(&rows), "someone's prompt opens the landing");
}

const WAITS_AT_THE_CHECK: [&str; 3] = [
    "Nothing more I can do until the release build finishes; its monitor will notify me.",
    "I'll stop here until CI reports back; the build is still running.",
    "Everything I can do is queued behind the build, which is still running.",
];

#[test]
fn a_wait_at_lands_yield_is_reopened_by_its_own_wake() {
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let rows = |said: &str| {
        [
            typed_row("Fix the lexer and land it."),
            model_row("All done."),
            typed_row(&turn_end::land_text(None)),
            model_row(said),
            notification_row(),
            model_row("The release build passed. Next I'll publish the notes."),
        ]
    };
    for wait in WAITS_AT_THE_CHECK {
        assert!(!finished(&rows(wait)), "{wait}");
    }
    assert!(finished(&rows(DEFENSE)));
}

fn in_turn(row: &str, id: &str) -> String {
    row.replacen('{', &format!(r#"{{"promptId":"{id}","#), 1)
}

fn meta_row(text: &str) -> String {
    bare_user_row(
        r#","isMeta":true"#,
        &aterm_json::to_string(&Value::from(text)).expect("json"),
    )
}

#[test]
fn a_turn_its_own_goal_keeps_going_takes_the_landing_back() {
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let ask = in_turn(&typed_row("Make the compiler build itself."), "p0");
    let report = model_row("Stage 1 builds; stage 2 next.");
    let land = in_turn(&typed_row(&turn_end::land_text(None)), "p1");
    let defense = model_row(DEFENSE);
    let landed = || vec![ask.clone(), report.clone(), land.clone(), defense.clone()];
    let tool = in_turn(
        &bare_user_row(
            "",
            r#"[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]"#,
        ),
        "p1",
    );
    let work = model_row("Correct: stage 2 still fails with E0463. Checking it.");
    let hook = meta_row(
        "Stop hook feedback:\n[The compiler builds itself]: The condition has not been \
         satisfied: stage 2 fails.",
    );
    let goal = meta_row("Goal check-in: «The compiler builds itself» is not met yet.");
    assert!(finished(&landed()), "the landing");
    for (what, kept) in [
        ("a Stop hook's feedback", &hook),
        ("/goal's check-in", &goal),
    ] {
        let kept = in_turn(kept, "p1");
        let rows = [landed(), vec![kept.clone(), work.clone()]].concat();
        assert!(!finished(&rows), "{what}, then more work");
        let rows = [landed(), vec![kept.clone(), tool.clone(), work.clone()]].concat();
        assert!(!finished(&rows), "{what}, a tool's result, then more work");
        let rows = [landed(), vec![kept.clone()]].concat();
        assert!(finished(&rows), "{what} alone");
        let rows = [landed(), vec![kept.clone(), defense.clone()]].concat();
        assert!(finished(&rows), "{what}, then the defense again");
    }
    let rows = [
        typed_row("Make the compiler build itself."),
        report.clone(),
        typed_row(&turn_end::land_text(None)),
        defense.clone(),
        hook.clone(),
        work.clone(),
    ];
    assert!(!finished(&rows), "no promptId");
    let wake = in_turn(&meta_row("keep going: check the land log"), "p2");
    let rows = [
        landed(),
        vec![wake, model_row("The land log shows nothing new.")],
    ]
    .concat();
    assert!(finished(&rows), "a /loop wake under a new promptId");
    let unnumbered_wake = bare_user_row(
        r#","isMeta":true,"promptSource":"system","scheduledTaskId":"t1""#,
        r#""keep going: check the land log""#,
    );
    let rows = [
        typed_row("Make the compiler build itself."),
        report,
        typed_row(&turn_end::land_text(None)),
        defense,
        unnumbered_wake,
        model_row("The land log shows nothing new."),
    ];
    assert!(finished(&rows), "a /loop wake with no promptId");
}

#[test]
fn a_missed_tasks_notice_is_a_turn_of_its_own_on_a_build_that_numbers_none() {
    let finished = |rows: &[String]| transcript_finished(&rows.join("\n"), &[]);
    let landed = landing_rows;
    let answer = model_row("Shall I run the missed check of the land log now?");
    let missed = meta_row(
        "The following one-shot scheduled task was missed while Claude was not running. It \
         has already been removed from .claude/scheduled_tasks.json.\n\nDo NOT execute this \
         prompt yet. First use the AskUserQuestion tool to ask whether to run it now. Only \
         execute if the user confirms.\n\n[at 09:00, created 10/16/2026, 8:58:00 AM]\ncheck \
         the land log",
    );
    let rows = [landed(), vec![missed, answer.clone()]].concat();
    assert!(finished(&rows), "a missed task's notice with no promptId");
    let originated = bare_user_row(
        r#","isMeta":true,"turnOrigin":"scheduled""#,
        r#""keep going: check the land log""#,
    );
    let rows = [landed(), vec![originated, answer.clone()]].concat();
    assert!(finished(&rows), "a turnOrigin with no promptId");
    let hook = meta_row(
        "Stop hook feedback:\n[The compiler builds itself]: The condition has not been \
         satisfied: stage 2 fails.",
    );
    let rows = [landed(), vec![hook, answer.clone()]].concat();
    assert!(!finished(&rows), "a Stop hook's feedback goes on the turn");
    let quoted = meta_row(
        "Stop hook feedback: the log says \"The following one-shot scheduled task was \
         missed\"; the goal is not met.",
    );
    let rows = [landed(), vec![quoted, answer]].concat();
    assert!(!finished(&rows), "the notice's words quoted go on the turn");
}

#[test]
fn a_shell_command_asks_nothing_and_a_prompt_typed_while_it_works_is_someones() {
    let finished = |rows: &[String], ours: &[Typed]| transcript_finished(&rows.join("\n"), ours);
    let base = landing_rows;
    let bash_in = bare_user_row("", r#""<bash-input>cat notes.txt</bash-input>""#);
    let bash_out = bare_user_row(
        r#","turnOrigin":"human""#,
        r#""<bash-stdout>second line of notes</bash-stdout><bash-stderr></bash-stderr>""#,
    );
    let shell = [base(), vec![bash_in.clone(), bash_out.clone()]].concat();
    assert!(finished(&shell, &[]), "a shell command");
    let woken = [
        shell.clone(),
        vec![
            notification_row(),
            model_row("The background job finished."),
        ],
    ]
    .concat();
    assert!(finished(&woken, &[]), "a shell command, then a wake");
    let answered = [
        base(),
        vec![bash_in, model_row("The notes say to land it.")],
    ]
    .concat();
    assert!(!finished(&answered, &[]), "a shell command a turn followed");
    let queued = |kind: &str, prompt: &str, chain: bool| {
        format!(
            r#"{{"isSidechain":{chain},"type":"attachment","attachment":{{"type":"queued_command","prompt":{},"commandMode":"prompt","origin":{{"kind":"{kind}"}}}},"timestamp":"1970-01-01T00:00:01.000Z","version":"2.1.300"}}"#,
            aterm_json::to_string(&Value::from(prompt)).expect("json")
        )
    };
    let theirs = queued("human", "also fix the docs", false);
    let wake = [base(), vec![notification_row(), theirs.clone()]].concat();
    assert!(!finished(&wake, &[]), "absorbed into a wake's turn");
    let replied = [wake.clone(), vec![model_row("Fixed the docs too.")]].concat();
    assert!(!finished(&replied, &[]), "absorbed, then answered");
    let defended = [wake.clone(), vec![model_row(DEFENSE)]].concat();
    assert!(!finished(&defended, &[]), "absorbed, then the defense");
    let relanded = [
        wake,
        vec![typed_row(&turn_end::land_text(None)), model_row(DEFENSE)],
    ]
    .concat();
    assert!(finished(&relanded, &[]), "absorbed, then landed again");
    let ours = [Typed {
        at_ms: 1_000,
        text: "keep going".to_string(),
        rule: turn_end::RULE_CONTINUE.to_string(),
    }];
    let note = queued(
        "task-notification",
        "<task-notification>\n<status>completed</status>\n</task-notification>",
        false,
    );
    for (what, row, typed) in [
        ("a notification absorbed", note, &ours[..0]),
        (
            "what the loop typed",
            queued("human", "keep going", false),
            &ours[..],
        ),
        (
            "a subagent's",
            queued("human", "also fix the docs", true),
            &ours[..0],
        ),
    ] {
        let rows = [base(), vec![notification_row(), row, model_row("Noted.")]].concat();
        assert!(finished(&rows, typed), "{what}");
    }
    let rows = [
        base(),
        vec![notification_row(), queued("human", "keep going", false)],
    ]
    .concat();
    assert!(!finished(&rows, &[]), "the loop's words, no ledger");
}

#[test]
fn the_resumed_builds_warning_is_read_by_its_fields() {
    let at = |t: &str| models::parse_utc(t).expect("a time");
    let relaunched = at("2026-10-15T15:33:38Z");
    let until = relaunched + 60;
    let row = |subtype: &str, level: &str, version: &str, t: &str, chain: bool| {
        format!(
            r#"{{"type":"system","subtype":"{subtype}","level":"{level}","isMeta":false,"isSidechain":{chain},"version":"{version}","timestamp":"{t}","content":"Artifact comment monitor for an artifact didn't resume: it couldn't restart in this session."}}"#
        )
    };
    let warned = row(
        "informational",
        "warning",
        "2.1.300",
        "2026-10-15T15:34:00.184Z",
        false,
    );
    assert!(resumed_build_warned(&warned, "2.1.300", relaunched, until));
    assert!(
        resumed_build_warned(&warned, "2.1.300", 0, until),
        "a record that names no relaunch"
    );
    for (what, rows, version) in [
        (
            "a notice",
            row(
                "informational",
                "notice",
                "2.1.300",
                "2026-10-15T15:34:00Z",
                false,
            ),
            "2.1.300",
        ),
        (
            "another subtype",
            row(
                "local_command",
                "warning",
                "2.1.300",
                "2026-10-15T15:34:00Z",
                false,
            ),
            "2.1.300",
        ),
        (
            "the old build's",
            row(
                "informational",
                "warning",
                "2.1.295",
                "2026-10-15T15:34:00Z",
                false,
            ),
            "2.1.300",
        ),
        (
            "before the relaunch",
            row(
                "informational",
                "warning",
                "2.1.300",
                "2026-10-15T15:30:00Z",
                false,
            ),
            "2.1.300",
        ),
        (
            "after the look",
            row(
                "informational",
                "warning",
                "2.1.300",
                "2026-10-15T16:30:00Z",
                false,
            ),
            "2.1.300",
        ),
        (
            "a subagent's",
            row(
                "informational",
                "warning",
                "2.1.300",
                "2026-10-15T15:34:00Z",
                true,
            ),
            "2.1.300",
        ),
        ("no build named", warned.clone(), ""),
    ] {
        assert!(
            !resumed_build_warned(&rows, version, relaunched, until),
            "{what}"
        );
    }
}

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
    let quiet = Facts {
        quiet: true,
        ..settled.clone()
    };
    assert_eq!(next_step(&Phase::Pending, &quiet, false, now), Step::Fresh);
    for f in [&settled, &quiet] {
        let untold = Facts {
            unnoticed: true,
            ..f.clone()
        };
        assert_eq!(
            next_step(&Phase::Pending, &untold, false, now),
            Step::Wait(NOTICE_SHOWN),
            "quiet={}",
            f.quiet
        );
    }
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
    for phase in [Phase::Exiting { at_s: now }, Phase::Done] {
        assert!(matches!(
            next_step(&phase, &settled, false, now),
            Step::Wait(_)
        ));
    }
    let tasked = Facts {
        taskless: false,
        ..settled.clone()
    };
    assert_eq!(
        next_step(&Phase::Pending, &tasked, false, now),
        Step::Announce
    );
}

const AUTO_CONTINUE: &str =
    "⚠ Usage limit reached · continuing automatically at 6am · esc to cancel";

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

#[test]
fn the_limit_fact_is_read_by_the_supervisors_own_recogniser() {
    let parked = claude_screen(&[AUTO_CONTINUE, ""]);
    assert!(limited(&parked));
    let at_break = claude_screen(&[AUTO_CONTINUE, "✻ Waiting for 2 dynamic workflows to finish"]);
    assert!(
        limited(&at_break),
        "the wall stands over the agent's own background work"
    );

    assert!(!limited(&claude_screen(&[""])));
    let history = claude_screen(&[
        AUTO_CONTINUE,
        "",
        "⏺ The limit reset; picking the workflows back up.",
        "",
    ]);
    assert!(!limited(&history), "words under it end it");
    let quoted = claude_screen(&[
        "⏺ Bash(aterm ctl @s-9 text | grep limit)",
        &format!("  ⎿  {AUTO_CONTINUE}"),
        "",
    ]);
    assert!(!limited(&quoted), "a tool's output is not the wall");
}

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
    let at_break = Facts {
        status: "busy".to_string(),
        background_point: true,
        ..idle_facts()
    };
    assert_eq!(gate_release(&at_break, false), Gate::Go);

    let text = release_prompt(Agent::Claude, false);
    assert!(text.starts_with(HARNESS_MARK), "{text}");
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

const QUEUED_MARKER: &str = "ATERM-UPGRADE-READY-6a57ff7a";

fn json_text(text: &str) -> String {
    aterm_json::to_string(&Value::from(text)).expect("json")
}

fn notice_row(ts: &str, marker: &str) -> String {
    let text = prepare_prompt(&v("2.1.280"), &v("2.1.283"), Source::Managed, marker);
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"timestamp":"{ts}","version":"2.1.280"}}"#,
        json_text(&text)
    )
}

fn limit_row(ts: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"{ts}","message":{{"diagnostics":null,"id":"m","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)"}}],"context_management":null}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":1791046800,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.280"}}"#
    )
}

fn said_row(ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","timestamp":"{ts}","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}]}},"version":"2.1.280"}}"#,
        json_text(text)
    )
}

fn person_row(ts: &str, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"timestamp":"{ts}","version":"2.1.280"}}"#,
        json_text(text)
    )
}

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

#[test]
fn the_limit_stands_until_the_reset_its_row_names() {
    let limit = limit_row("2026-09-27T15:04:28.344Z");
    let answered = said_row("2026-09-27T14:00:00Z", "Working.");
    assert_eq!(
        transcript_limit_until(&[answered, limit.clone()].join("\n")),
        Some(1_791_046_800)
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
}

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

#[test]
fn the_ladder_relaxes_the_settle_and_the_person_near_and_never_a_floor() {
    let at = |rung: Rung, f: Facts| Facts { rung, ..f };
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
    let floors: [Knock; 10] = [
        (|f| f.held = true, "held"),
        (|f| f.typing = true, "attended"),
        (|f| f.limited = true, "limited"),
        (|f| f.login = true, "login"),
        (|f| f.queued = true, "queued"),
        (|f| f.status = "busy".into(), "not-idle"),
        (|f| f.status = "shell".into(), "not-idle"),
        (|f| f.busy_footer = true, "busy"),
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
    let mut words = screen("» Ask Codex to do anything", "  gpt");
    words[1] = "• Fixed it; one test still fails.".to_string();
    assert_ne!(still_print(7, &words, 5), base, "new words");
    assert_ne!(
        still_print(8, &screen("» Ask Codex to do anything", "  gpt"), 5),
        base,
        "another process"
    );
}

#[test]
fn the_work_end_reducer_is_bound_to_its_model() {
    use aterm_models_harness::harness_upgrade_work_end_model;
    use aterm_spec::derive::Model;
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
    let shift = i64::from(MAX_ASKS) - cap;
    let look = |s: &S| -> (Phase, Facts) {
        let since = if s["late"] == 1 {
            REASK_S + 60
        } else {
            WORK_END_FLOOR_S + 60
        };
        let phase = if s["phase"] == 1 {
            Phase::Announced {
                at_s: now - since,
                asks: u32::try_from(s["asks"] + shift).unwrap(),
            }
        } else {
            Phase::Failed(GAVE_UP.to_string())
        };
        let st = super::super::upgrade_drive::St {
            phase: phase.clone(),
            noticed_at: now - since,
            failed_at: now - since,
            work_seen: s["worked"] == 1,
            ..Default::default()
        };
        let mut facts = idle_facts();
        if s["lag"] == 1 {
            facts.status = "shell".to_string();
            facts.idle_looks = IDLE_LOOKS;
        }
        if s["screen"] == 1 {
            facts.status = "busy".to_string();
            facts.background_point = true;
        }
        st.time_failed(&mut facts, now);
        st.time_work(&mut facts, now);
        (phase, facts)
    };
    let real = |s: &S| -> &'static str {
        let (phase, facts) = look(s);
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
    let looks: Vec<S> = reachable(&model)
        .into_iter()
        .filter(|s| s["looked"] == 0)
        .collect();
    let points: Vec<S> = looks.iter().filter(|s| s["screen"] == 0).cloned().collect();
    for (what, found) in [
        (
            "a rest after work",
            points.iter().any(|s| s["worked"] == 1 && s["phase"] == 2),
        ),
        ("no work seen", points.iter().any(|s| s["worked"] == 0)),
        (
            "a break of seen work, announced and resting",
            [1, 2].iter().all(|phase| {
                looks
                    .iter()
                    .any(|s| s["screen"] == 1 && s["worked"] == 1 && s["phase"] == *phase)
            }),
        ),
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
    for s in &looks {
        assert_eq!(
            look(s).1.work_ended,
            s["screen"] == 0 && s["worked"] == 1,
            "the end of the work at {s:?}"
        );
    }
    for s in &points {
        assert_eq!(
            model_says(&model, s),
            [real(s)],
            "the real reducer against the model at {s:?}"
        );
    }
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

const INCIDENT_MARKER: &str = "ATERM-UPGRADE-READY-d6476091";

fn typed_row(text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"origin":{{"kind":"human"}},"promptSource":"typed","version":"2.1.284"}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

fn model_row(text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}],"stop_reason":"end_turn"}},"version":"2.1.284"}}"#,
        aterm_json::to_string(&Value::from(text)).expect("json")
    )
}

fn pasted(text: &str) -> String {
    format!("\n\n<pasted_content id=\"6dd8\">\n{text}\n</pasted_content id=\"6dd8\">\n")
}

fn incident_notice(marker: &str) -> String {
    prepare_prompt(&v("2.1.284"), &v("2.1.285"), Source::Managed, marker)
}

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

#[test]
fn a_pasted_notice_is_the_notice_and_a_pasted_message_a_direction() {
    let earlier = ready_marker(ID, &v("2.1.285"), 1);
    let later = ready_marker(ID, &v("2.1.285"), 2);
    for (how, wrap) in [
        ("pasted", pasted as fn(&str) -> String),
        ("typed", str::to_string as fn(&str) -> String),
    ] {
        let asked_again = [
            model_row(&format!("Done.\n\n{earlier}")),
            typed_row(&wrap(&incident_notice(&later))),
        ]
        .join("\n");
        assert!(
            !transcript_has_ready(&asked_again, &earlier),
            "{how}: the notice after the answer ends it"
        );
        assert!(
            !transcript_tasked(&typed_row(&wrap(&incident_notice(&later))), &[]),
            "{how}: the notice is the harness's"
        );
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
    let next = ready_marker(ID, &v("2.1.285"), 2);
    rows.push(typed_row(&pasted(&incident_notice(&next))));
    let both = [INCIDENT_MARKER.to_string(), next.clone()];
    assert!(!ready_superseded(&rows.join("\n"), &both));
    rows.push(model_row(&format!("All done.\n\n{next}")));
    assert!(transcript_has_ready(&rows.join("\n"), &next));
    assert!(!ready_superseded(&rows.join("\n"), &both));
}

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
    assert_eq!(
        next_step(&announced, &turn_end, true, now),
        Step::Wait("settling")
    );
    assert_eq!(
        next_step(&announced, &idle_facts(), true, now + 20),
        Step::Terminate
    );
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
    let quiet = Facts {
        quiet: true,
        ..shown.clone()
    };
    assert_eq!(
        next_step(&announced, &quiet, false, now),
        Step::Wait("settling")
    );
}

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

fn quota_row(ts: &str, limit: &str, resets: u64, text: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"assistant","uuid":"u","timestamp":"{ts}","message":{{"id":"m","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":{}}}]}},"requestId":"req_0","quotaLimits":{{"status":"rejected","resetsAt":{resets},"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"{limit}","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false}},"error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"version":"2.1.286"}}"#,
        json_text(text)
    )
}

#[test]
fn the_standing_quota_wall_is_the_limit_row_that_stands_and_only_a_quota_rejection() {
    let at = 1_790_521_468;
    let ts = "2026-09-27T15:04:28.344Z";
    let wall = |window: &str, until: u64| {
        Some(StandingWall {
            at,
            until: Some(until),
            window: Some(window.to_string()),
            credits: Some(CreditsWord::Off {
                reason: Some("org_level_disabled".to_string()),
                until: None,
            }),
        })
    };
    let session = quota_row(
        ts,
        "five_hour",
        1_790_532_000,
        "You've hit your session limit · resets 7pm (America/Los_Angeles)",
    );
    let answered = said_row("2026-09-27T14:00:00Z", "Working.");

    assert_eq!(
        transcript_standing_wall(&[answered.clone(), session.clone()].join("\n")),
        wall("five_hour", 1_790_532_000)
    );
    assert_eq!(
        transcript_standing_wall(&limit_row(ts)),
        wall("seven_day", 1_791_046_800).map(|w| StandingWall {
            credits: Some(CreditsWord::Off {
                reason: None,
                until: None,
            }),
            ..w
        })
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

    let no_response = r#"{"isSidechain":false,"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#;
    assert_eq!(
        transcript_standing_wall(&[session.as_str(), no_response].join("\n")),
        wall("five_hour", 1_790_532_000),
        "passed over"
    );
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
    assert_eq!(
        transcript_standing_wall(&[session.clone(), later.clone()].join("\n")),
        None
    );
    let prompt = person_row("2026-09-27T19:50:00Z", "carry on with the parser");
    assert_eq!(
        transcript_standing_wall(&[session.clone(), prompt.clone()].join("\n")),
        wall("five_hour", 1_790_532_000),
        "a person's prompt alone"
    );
    assert_eq!(
        transcript_standing_wall(&[session.clone(), prompt.clone(), later.clone()].join("\n")),
        None,
        "the person's prompt the model answered"
    );
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

    let api = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-27T15:10:00Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"API Error: Rate limit reached for requests"}]},"isApiErrorMessage":true}"#;
    assert!(transcript_limit_until(api).is_some(), "it is a limit row");
    assert_eq!(transcript_standing_wall(api), None);
    assert_eq!(
        transcript_standing_wall(&[session.as_str(), api].join("\n")),
        None,
        "the row that stands last is the API's"
    );
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
    let credit = quota_row(
        ts,
        "overage",
        1_791_046_800,
        "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)",
    );
    assert!(!transcript_limit_spend(&credit), "its text is the week's");
    assert_eq!(transcript_standing_wall(&credit), None);
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

#[test]
fn a_standing_wall_with_no_limit_type_takes_it_from_its_text_and_hostile_rows_cost_one_key() {
    let at = 1_790_521_468;
    let ts = "2026-09-27T15:04:28.344Z";
    let typed = r#""rateLimitType":"seven_day","#;
    let week = limit_row(ts);
    let off = Some(CreditsWord::Off {
        reason: None,
        until: None,
    });
    assert!(week.contains(typed), "{week}");
    let read = |row: &str| transcript_standing_wall(row);
    let window = |row: &str| read(row).expect("a wall").window;

    let untyped = week.replace(typed, "");
    assert_eq!(
        read(&untyped),
        Some(StandingWall {
            at,
            until: Some(1_791_046_800),
            window: Some("seven_day".to_string()),
            credits: off.clone(),
        })
    );
    let session_text = "You've hit your session limit · resets 7pm (America/Los_Angeles)";
    let untyped_session = untyped.replace(
        "You've hit your weekly limit · resets Oct 3 at 10am (America/Los_Angeles)",
        session_text,
    );
    assert_eq!(window(&untyped_session).as_deref(), Some("five_hour"));
    let untyped_bucket = untyped.replace("weekly limit", "Opus limit");
    assert_eq!(
        read(&untyped_bucket),
        Some(StandingWall {
            at,
            until: Some(1_791_046_800),
            window: None,
            credits: off.clone(),
        })
    );
    let bucket = week.replace(typed, r#""rateLimitType":"seven_day_opus","#);
    assert_eq!(window(&bucket).as_deref(), Some("seven_day_opus"));

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
    let new = "b".repeat(LIMIT_TYPE_MAX);
    let row = week.replace(typed, &format!(r#""rateLimitType":"{new}","#));
    assert_eq!(window(&row), Some(new));

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
                credits: off.clone(),
            }),
            "{hostile}"
        );
    }

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
                credits: off.clone(),
            }),
            "{junk:?}"
        );
    }
}

fn local_command_row(ts: &str, content: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"promptId":"q","type":"user","message":{{"role":"user","content":{}}},"uuid":"u","timestamp":"{ts}","userType":"external","entrypoint":"cli","version":"2.1.287"}}"#,
        json_text(content)
    )
}

fn local_command_system_row(ts: &str, content: &str) -> String {
    format!(
        r#"{{"parentUuid":"p","isSidechain":false,"type":"system","subtype":"local_command","content":{},"level":"info","timestamp":"{ts}","uuid":"u","isMeta":false,"version":"2.1.287"}}"#,
        json_text(content)
    )
}

fn command_content(name: &str) -> String {
    format!(
        "<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"
    )
}

fn stdout_content(text: &str) -> String {
    format!("<local-command-stdout>{text}</local-command-stdout>")
}

#[test]
fn the_dialogs_result_line_is_read_under_its_own_command_and_only_since_the_claim() {
    let claimed = 1_790_521_468;
    let before = "2026-09-27T15:04:27.900Z";
    let same = "2026-09-27T15:04:28.344Z";
    let after = "2026-09-27T15:05:10.120Z";
    let later = "2026-09-27T15:09:00.000Z";
    let spent =
        "Weekly limits reset · your weekly reset day stays Oct 3 at 10am · 1 weekly reset left";
    let kept = "Weekly reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let waited = "Claude Code will continue automatically at 6am";

    let dialog = |ts: &str| local_command_row(ts, &command_content("rate-limit-options"));
    let model = |ts: &str| local_command_row(ts, &command_content("model"));
    let out = |ts: &str, text: &str| local_command_row(ts, &stdout_content(text));
    let read = |rows: &[String]| transcript_reset_line(&rows.join("\n"), claimed);

    assert_eq!(
        read(&[dialog(after), out(after, spent)]).as_deref(),
        Some(spent)
    );
    assert_eq!(
        read(&[dialog(same), out(same, kept)]).as_deref(),
        Some(kept)
    );
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
    assert_eq!(
        read(&[
            dialog(after),
            local_command_row(after, &format!("<local-command-stdout>{spent}"))
        ])
        .as_deref(),
        Some(spent)
    );

    assert_eq!(read(&[dialog(before), out(before, spent)]), None);
    let untimed = out(after, spent).replace(&format!(r#""timestamp":"{after}","#), "");
    assert!(!untimed.contains("timestamp"));
    assert_eq!(read(&[dialog(after), untimed]), None);

    assert_eq!(read(&[model(after), out(after, spent)]), None);
    assert_eq!(
        read(&[dialog(after), model(after), out(after, spent)]),
        None
    );
    assert_eq!(
        read(&[dialog(before), out(before, waited), out(after, spent)]),
        None
    );
    assert_eq!(read(&[out(after, spent)]), None);
    let with_args = dialog(after).replace(
        "<command-args></command-args>",
        "<command-args>now</command-args>",
    );
    assert_eq!(read(&[with_args, out(after, spent)]), None);
    let longer = local_command_row(after, &command_content("rate-limit-options-2"));
    assert_eq!(read(&[longer, out(after, spent)]), None);
    let failed = local_command_row(
        after,
        &format!("<local-command-stderr>{spent}</local-command-stderr>"),
    );
    assert_eq!(read(&[dialog(after), failed, out(after, spent)]), None);
    let side = |row: String| row.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(read(&[dialog(after), side(out(after, spent))]), None);
    assert_eq!(read(&[side(dialog(after)), out(after, spent)]), None);
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

#[test]
fn a_limit_rows_word_on_usage_credits_is_read() {
    let ts = "2026-09-27T15:04:28.344Z";
    let week = limit_row(ts);
    let status = r#""overageStatus":"rejected","#;
    assert!(week.contains(status), "{week}");
    let read = |row: &str| transcript_standing_wall(row).expect("a wall").credits;
    let with = |keys: &str| week.replace(status, keys);
    assert_eq!(
        read(&with(
            r#""overageStatus":"rejected","overageDisabledReason":"out_of_credits","overageResetsAt":1793000000,"#
        )),
        Some(CreditsWord::Off {
            reason: Some("out_of_credits".to_string()),
            until: Some(1_793_000_000),
        })
    );
    for on in ["allowed", "allowed_warning"] {
        assert_eq!(
            read(&with(&format!(r#""overageStatus":"{on}","#))),
            Some(CreditsWord::On),
            "{on}"
        );
    }
    for silent in [
        "",
        r#""overageStatus":"sometimes","#,
        r#""overageStatus":7,"#,
    ] {
        assert_eq!(read(&with(silent)), None, "{silent}");
    }
    assert_eq!(
        read(&with(
            r#""overageStatus":"rejected","overageDisabledReason":"Not A Word!","#
        )),
        Some(CreditsWord::Off {
            reason: None,
            until: None,
        })
    );
}

#[test]
fn the_newest_answer_is_the_last_response_row() {
    let at = |ts: &str| crate::harness::upgrade_models::parse_utc(ts).expect("a time");
    let answer = said_row("2026-09-27T15:00:00.000Z", "Fixed.");
    let wall = limit_row("2026-09-27T15:04:28.344Z");
    let sidechain = said_row("2026-09-27T15:06:00.000Z", "Sub.")
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    let prompt = r#"{"type":"user","timestamp":"2026-09-27T16:00:00.000Z","message":{"role":"user","content":"keep going"}}"#;
    let local = r#"{"type":"system","subtype":"local_command","timestamp":"2026-09-27T16:01:00.000Z","content":"<local-command-stdout>Limit reset</local-command-stdout>"}"#;
    let synthetic = said_row("2026-09-27T16:02:00.000Z", "No response requested.")
        .replace("claude-opus-5-5", "<synthetic>");
    let tail = |rows: &[&str]| transcript_last_answer(&rows.join("\n"), u64::MAX);
    assert_eq!(tail(&[&answer]), Some(at("2026-09-27T15:00:00.000Z")));
    assert_eq!(
        tail(&[&answer, &wall]),
        Some(at("2026-09-27T15:04:28.344Z"))
    );
    let all = [&*answer, &wall, &sidechain, prompt, local, &synthetic].join("\n");
    assert_eq!(
        transcript_last_answer(&all, u64::MAX),
        Some(at("2026-09-27T15:06:00.000Z"))
    );
    assert_eq!(tail(&[prompt, local, &synthetic]), None);
    let by = |ts: &str| transcript_last_answer(&all, at(ts));
    assert_eq!(
        by("2026-09-27T15:05:59.000Z"),
        Some(at("2026-09-27T15:04:28.344Z"))
    );
    assert_eq!(
        by("2026-09-27T15:04:28.000Z"),
        Some(at("2026-09-27T15:04:28.344Z"))
    );
    assert_eq!(
        by("2026-09-27T15:04:27.000Z"),
        Some(at("2026-09-27T15:00:00.000Z"))
    );
    assert_eq!(by("2026-09-27T14:59:59.000Z"), None);
}

#[cfg(unix)]
#[test]
fn the_rewrite_sheds_only_aterms_own_tap() {
    let ours = super::super::tap::settings_value(std::path::Path::new(
        "/Applications/aterm.app/Contents/MacOS/aterm",
    ))
    .expect("an admissible path");
    let resumed = |rest: &[&str]| {
        let words: Vec<&str> = std::iter::once("claude")
            .chain(rest.iter().copied())
            .collect();
        rewrite_argv(&argv(&words), ID).expect("carried")
    };
    assert_eq!(
        resumed(&["--verbose", "--settings", &ours]),
        argv(&["--verbose", "--resume", ID])
    );
    assert_eq!(
        resumed(&[&format!("--settings={ours}"), "--verbose"]),
        argv(&["--verbose", "--resume", ID])
    );
    let near = ours.replace("\"refreshInterval\":10", "\"refreshInterval\":30");
    let extra = ours.replacen("}}", "},\"model\":\"x\"}", 1);
    for theirs in [r#"{"model":"x"}"#, "/Users//example/s.json", &near, &extra] {
        assert_eq!(
            resumed(&["--settings", theirs]),
            argv(&["--settings", theirs, "--resume", ID]),
            "{theirs}"
        );
        let inline = format!("--settings={theirs}");
        assert_eq!(resumed(&[&inline]), argv(&[&inline, "--resume", ID]));
    }
    let told =
        super::super::resume::command(&argv(&["claude", "--settings", &ours]), ID).expect("a line");
    assert_eq!(told, format!("claude --resume {ID}"));
    assert_eq!(
        carried_names(&argv(&["claude", "--settings", &ours, "--bare"])).ok(),
        Some(vec!["--bare"])
    );
}

#[test]
fn the_turn_a_conversation_ends_on_is_named_by_the_prompt_that_opened_it() {
    let prompt = r#"{"type":"user","uuid":"u-1","promptId":"p-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"}}"#;
    let call = r#"{"type":"assistant","uuid":"a-0","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"tool_use","id":"t-1","name":"Read","input":{}}]}}"#;
    let result = r#"{"type":"user","uuid":"r-1","promptId":"p-1","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t-1","content":"ok"}]}}"#;
    let answer = |uuid: &str| {
        format!(
            r#"{{"type":"assistant","uuid":"{uuid}","isSidechain":false,"message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"Hello! How can I help?"}}]}}}}"#
        )
    };
    let summary = r#"{"type":"user","uuid":"s-1","isSidechain":false,"isCompactSummary":true,"message":{"role":"user","content":"This session is being continued."}}"#;
    let hook = r#"{"type":"user","uuid":"h-1","promptId":"p-1","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"Stop hook feedback: not met"}}"#;
    let wake = r#"{"type":"user","uuid":"w-1","promptId":"p-wake","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"check the build"}}"#;
    let peer = r#"{"type":"user","uuid":"m-1","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"a peer asks"},"origin":{"kind":"peer"}}"#;
    let later = r#"{"type":"user","uuid":"u-2","promptId":"p-2","isSidechain":false,"message":{"role":"user","content":"Now add the flag."},"origin":{"kind":"human"}}"#;
    let caveat = r#"{"type":"user","uuid":"c-0","promptId":"p-3","isMeta":true,"isSidechain":false,"message":{"role":"user","content":"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"}}"#;
    let config = r#"{"type":"user","uuid":"c-1","promptId":"p-3","isSidechain":false,"message":{"role":"user","content":"<command-name>/config</command-name>\n            <command-message>config</command-message>\n            <command-args></command-args>"}}"#;
    let config_out = r#"{"type":"user","uuid":"c-2","promptId":"p-3","isSidechain":false,"message":{"role":"user","content":"<local-command-stdout>Disabled auto-compact</local-command-stdout>"}}"#;
    let bash = r#"{"type":"user","uuid":"b-1","promptId":"p-4","isSidechain":false,"message":{"role":"user","content":"<bash-input>ls</bash-input>"}}"#;
    let bash_out = r#"{"type":"user","uuid":"b-2","promptId":"p-4","isSidechain":false,"message":{"role":"user","content":"<bash-stdout>notes.txt</bash-stdout><bash-stderr></bash-stderr>"}}"#;
    let init = r#"{"type":"user","uuid":"i-1","promptId":"p-5","isSidechain":false,"message":{"role":"user","content":"<command-message>init is analyzing your codebase…</command-message>\n<command-name>/init</command-name>"}}"#;
    let init_expansion = r#"{"type":"user","uuid":"i-2","promptId":"p-5","isMeta":true,"isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"Please analyze this codebase and create a CLAUDE.md file"}]}}"#;
    let turn = |rows: &[&str]| LastTurn::of(&rows.join("\n"));
    let a1 = answer("a-1");
    let a2 = answer("a-2");
    let whole = [prompt, call, result, a1.as_str()];
    assert_eq!(turn(&whole).key().as_deref(), Some("prompt:u-1"));
    assert_eq!(turn(&whole).last.as_deref(), Some("answer:a-1"));
    for (what, rows) in [
        ("the prompt alone", vec![prompt]),
        ("a tool's call", vec![prompt, call, result]),
        ("the whole reply", whole.to_vec()),
        (
            "an idle compaction after it",
            vec![prompt, a1.as_str(), summary],
        ),
        (
            "a Stop hook's feedback, the work after it",
            vec![prompt, a1.as_str(), hook, a2.as_str()],
        ),
    ] {
        let t = turn(&rows);
        assert!(t.names("prompt:u-1"), "{what}: {t:?}");
        assert_eq!(t.key().as_deref(), Some("prompt:u-1"), "{what}");
    }
    assert!(turn(&whole).names("answer:a-1"), "an earlier build's key");
    for (what, rows, command) in [
        (
            "/config, as 2.1.294 records it",
            vec![prompt, a1.as_str(), caveat, config, config_out],
            "prompt:c-1",
        ),
        (
            "a `!` shell command",
            vec![prompt, a1.as_str(), bash, bash_out],
            "prompt:b-1",
        ),
        (
            "/config, then a `!` command",
            vec![
                prompt,
                a1.as_str(),
                caveat,
                config,
                config_out,
                bash,
                bash_out,
            ],
            "prompt:b-1",
        ),
    ] {
        let t = turn(&rows);
        assert!(t.names("prompt:u-1"), "{what}: {t:?}");
        assert!(
            t.names("answer:a-1"),
            "{what}: an earlier build's key {t:?}"
        );
        assert!(t.names(command), "{what}: the command's key {t:?}");
        assert_eq!(t.key().as_deref(), Some(command), "{what}");
        assert_eq!(t.opened.as_deref(), Some("prompt:u-1"), "{what}");
    }
    assert!(
        turn(&[
            prompt,
            a1.as_str(),
            caveat,
            config,
            config_out,
            bash,
            bash_out
        ])
        .names("prompt:c-1"),
        "every command since the answer"
    );
    for (what, rows, opened) in [
        (
            "a prompt command answered",
            vec![prompt, a1.as_str(), init, init_expansion, a2.as_str()],
            Some("prompt:i-1"),
        ),
        (
            "a `!` command answered",
            vec![prompt, a1.as_str(), bash, bash_out, a2.as_str()],
            Some("prompt:b-1"),
        ),
        (
            "a wake answered after a command",
            vec![prompt, a1.as_str(), config, config_out, wake, a2.as_str()],
            None,
        ),
    ] {
        let t = turn(&rows);
        assert!(!t.names("prompt:u-1"), "{what}: {t:?}");
        assert!(!t.names("answer:a-1"), "{what}: {t:?}");
        assert!(!t.names("prompt:c-1"), "{what}: {t:?}");
        assert_eq!(t.opened.as_deref(), opened, "{what}");
    }
    for (what, rows) in [
        ("a later prompt", vec![prompt, a1.as_str(), later]),
        (
            "a wake answered",
            vec![prompt, a1.as_str(), wake, a2.as_str()],
        ),
        (
            "a peer's message answered",
            vec![prompt, a1.as_str(), peer, a2.as_str()],
        ),
    ] {
        let t = turn(&rows);
        assert!(!t.names("prompt:u-1"), "{what}: {t:?}");
        assert!(!t.names("answer:a-1"), "{what}: {t:?}");
    }
    for wake in [
        r#"{"type":"user","uuid":"w-2","isMeta":true,"scheduledTaskId":"t1","message":{"content":"check the build"}}"#,
        r#"{"type":"user","uuid":"w-3","isMeta":true,"turnOrigin":"scheduled","message":{"content":"check the build"}}"#,
        r#"{"type":"user","uuid":"w-4","isMeta":true,"message":{"content":"The following one-shot scheduled task was missed while Claude was not running."}}"#,
    ] {
        let t = turn(&[prompt, a1.as_str(), wake, a2.as_str()]);
        assert!(
            !t.names("prompt:u-1") && !t.names("answer:a-1"),
            "UNNUMBERED_WAKE_MUST_END_OLD_EXCHANGE: {t:?}"
        );
        assert_eq!(t.opened, None);
        assert_eq!(t.last.as_deref(), Some("answer:a-2"));
    }
    let unopened = turn(&[call, result, a1.as_str()]);
    assert_eq!(unopened.opened, None);
    assert_eq!(unopened.key().as_deref(), Some("answer:a-1"));
    assert_eq!(turn(&[]), LastTurn::default());
}

#[test]
fn a_persons_exchange_retried_at_a_wall_names_the_retrys_turn() {
    use crate::supervise::approvals::{left_at_wall_key, left_retried_key};
    let retry = "Claude Code reported an overloaded API (529). Carry on.";
    let prompt = r#"{"type":"user","uuid":"u-1","promptId":"p-1","isSidechain":false,"message":{"role":"user","content":"Hi"},"origin":{"kind":"human"}}"#.to_string();
    let error = r#"{"type":"assistant","uuid":"e-1","isSidechain":false,"isApiErrorMessage":true,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"API Error: 529"}]}}"#.to_string();
    let ended = |ms: u64| {
        format!(
            r#"{{"type":"system","subtype":"turn_duration","durationMs":{ms},"isSidechain":false}}"#
        )
    };
    let typed = |uuid: &str, text: &str| {
        format!(
            r#"{{"type":"user","uuid":"{uuid}","promptId":"p-{uuid}","isSidechain":false,"message":{{"role":"user","content":"{text}"}}}}"#
        )
    };
    let answer = r#"{"type":"assistant","uuid":"a-1","isSidechain":false,"message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}"#.to_string();
    let wall = left_at_wall_key("prompt:u-1", std::time::Duration::from_secs(120));
    let key = left_retried_key(&wall, retry);
    let turn = |rows: &[String]| LastTurn::of(&rows.join("\n"));
    let base = vec![prompt, error, ended(1_800), typed("r-1", retry), answer];
    let with = |more: &[String]| base.iter().chain(more).cloned().collect::<Vec<_>>();
    assert!(
        turn(&with(&[ended(20_000)])).names(&key),
        "the retry's short answer"
    );
    assert!(!turn(&base).names(&key), "not ended yet");
    assert!(
        !turn(&with(&[ended(9 * 60 * 1000)])).names(&key),
        "past the bound with the work before the wall"
    );
    assert!(
        !turn(&with(&[ended(20_000), typed("u-2", "Now add the flag.")])).names(&key),
        "a prompt since"
    );
    assert!(
        !turn(&with(&[ended(20_000)])).names(&left_retried_key(&wall, "keep going")),
        "a retry in other words"
    );
}

#[test]
fn an_older_builds_note_of_a_left_exchange_names_the_turn_no_prompt_opened_since() {
    use crate::supervise::approvals::left_before_key;
    let at = |s: u64| format!("1970-01-01T00:00:{s:02}.000Z");
    let prompt = |stamp: Option<u64>| {
        let t = stamp.map_or_else(String::new, |s| format!(r#","timestamp":"{}""#, at(s)));
        format!(
            r#"{{"type":"user","uuid":"u-1","promptId":"p-1"{t},"isSidechain":false,"message":{{"role":"user","content":"Hi"}},"origin":{{"kind":"human"}}}}"#
        )
    };
    let answer = |uuid: &str| {
        format!(
            r#"{{"type":"assistant","uuid":"{uuid}","isSidechain":false,"message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":"Hello! How can I help?"}}]}}}}"#
        )
    };
    let config = format!(
        r#"{{"type":"user","uuid":"c-1","promptId":"p-3","timestamp":"{}","isSidechain":false,"message":{{"role":"user","content":"<command-name>/config</command-name>\n            <command-message>config</command-message>\n            <command-args></command-args>"}}}}"#,
        at(30)
    );
    let init = format!(
        r#"{{"type":"user","uuid":"i-1","promptId":"p-5","timestamp":"{}","isSidechain":false,"message":{{"role":"user","content":"<command-message>init is analyzing your codebase…</command-message>\n<command-name>/init</command-name>"}}}}"#,
        at(30)
    );
    let wake = format!(
        r#"{{"type":"user","uuid":"w-1","promptId":"p-wake","timestamp":"{}","isMeta":true,"isSidechain":false,"message":{{"role":"user","content":"check the build"}}}}"#,
        at(30)
    );
    let turn = |rows: &[String]| LastTurn::of(&rows.join("\n"));
    let noted = left_before_key(20_000);
    let exchange = vec![prompt(Some(10)), answer("a-1")];
    let with = |more: &[String]| exchange.iter().chain(more).cloned().collect::<Vec<_>>();
    assert!(turn(&exchange).names(&noted), "the exchange it left");
    assert!(
        turn(&with(&[config])).names(&noted),
        "a /config after it, unanswered"
    );
    assert!(
        !turn(&exchange).names(&left_before_key(5_000)),
        "a prompt recorded after the note"
    );
    assert!(
        !turn(&with(&[init, answer("a-2")])).names(&noted),
        "/init answered since"
    );
    assert!(
        !turn(&with(&[wake, answer("a-3")])).names(&noted),
        "a wake answered since"
    );
    assert!(
        !turn(&[prompt(None), answer("a-1")]).names(&noted),
        "a prompt with no time"
    );
}

#[test]
fn the_turn_a_conversation_ends_on_is_named_by_its_last_answer_or_prompt() {
    let user = |uuid: &str, content: &str| {
        format!(
            r#"{{"type":"user","uuid":"{uuid}","isSidechain":false,"message":{{"role":"user","content":{content}}},"origin":{{"kind":"human"}}}}"#
        )
    };
    let answer = |uuid: &str, model: &str| {
        format!(
            r#"{{"type":"assistant","uuid":"{uuid}","isSidechain":false,"message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"Hello! How can I help?"}}]}}}}"#
        )
    };
    let key = |rows: &[String]| last_turn_key(&rows.join("\n"));
    let greeted = vec![user("u-1", r#""Hi""#), answer("a-1", "claude-opus-5-5")];
    assert_eq!(key(&greeted).as_deref(), Some("answer:a-1"));
    let past = |more: &[String]| {
        greeted
            .iter()
            .cloned()
            .chain(more.iter().cloned())
            .collect::<Vec<_>>()
    };
    for (what, row) in [
        (
            "a tool's result",
            user(
                "u-2",
                r#"[{"type":"tool_result","tool_use_id":"t","content":"ok"}]"#,
            ),
        ),
        (
            "an expansion",
            r#"{"type":"user","uuid":"u-3","isMeta":true,"message":{"role":"user","content":"caveat"}}"#
                .to_string(),
        ),
        (
            "a subagent's answer",
            answer("a-9", "claude-opus-5-5").replace(r#""isSidechain":false"#, r#""isSidechain":true"#),
        ),
        ("an API error's row", answer("a-8", "<synthetic>")),
        (
            "a system row",
            r#"{"type":"system","uuid":"s-1","subtype":"turn_duration"}"#.to_string(),
        ),
        (
            "a local command",
            user(
                "u-5",
                r#""<command-name>/config</command-name>\n<command-message>config</command-message>""#,
            ),
        ),
        (
            "a local command's output",
            user(
                "u-6",
                r#""<local-command-stdout>Disabled auto-compact</local-command-stdout>""#,
            ),
        ),
        (
            "a `!` shell command",
            user("u-7", r#""<bash-input>ls</bash-input>""#),
        ),
    ] {
        assert_eq!(key(&past(&[row])).as_deref(), Some("answer:a-1"), "{what}");
    }
    assert_eq!(
        key(&past(&[user("u-4", r#""Now add the flag.""#)])).as_deref(),
        Some("prompt:u-4")
    );
    assert_eq!(
        key(&past(&[
            user("u-4", r#""Now add the flag.""#),
            answer("a-2", "claude-opus-5-5")
        ]))
        .as_deref(),
        Some("answer:a-2")
    );
    assert_eq!(
        key(&[answer("a-1", "claude-opus-5-5").replace(r#""uuid":"a-1","#, "")]),
        None
    );
    assert_eq!(key(&[]), None);
}

const CONTINUED_LIMIT: &str =
    include_str!("../supervise/fixtures/claude-2.1.306-transcript-session-limit-continued.jsonl");

fn continued_reply(rows: &[&str]) -> String {
    rows.iter()
        .find(|r| r.contains(r#""uuid":"501239ab-c5da-4032-9991-787ec24f148f""#))
        .expect("the first reply")
        .replace(
            "Implemented the parser module and its unit tests in this turn.",
            "Nothing left: the parser is committed and pushed to origin/main.",
        )
        .replace(
            "501239ab-c5da-4032-9991-787ec24f148f",
            "7266d0bd-e11b-4d5c-890d-189185a7f612",
        )
        .replace(
            "c809f4b4-382a-4496-838f-6fe48a564208",
            "67ad4c9a-a719-4358-a459-60b578fa4deb",
        )
        .replace("2026-10-21T12:43:14.255Z", "2026-10-21T12:49:18.335Z")
}

#[test]
fn claudes_own_continuation_past_a_limit_row_ends_the_standing_limit() {
    const ROW: &str = "3e0efba4-585c-41b7-b108-155e8508be5d";
    const RESET: u64 = 1_792_586_880;
    let rows: Vec<&str> = CONTINUED_LIMIT.lines().collect();
    let at = |id: &str| {
        rows.iter()
            .position(|r| r.contains(&format!(r#""uuid":"{id}""#)))
            .expect(id)
    };
    let prompt = at("67ad4c9a-a719-4358-a459-60b578fa4deb");
    let cut = |n: usize| rows[..n].join("\n");
    let read = CONTINUED_LIMIT.trim_end();
    assert!(read.ends_with(rows[prompt]), "the tail the loop read");
    assert_eq!(
        transcript_standing_limit(read),
        None,
        "the tail the loop read"
    );
    let whole = format!("{read}\n{}", continued_reply(&rows));
    assert_eq!(transcript_standing_limit(&whole), None, "the reply written");
    assert_eq!(transcript_limit_reset(read), Some(RESET));
    assert!(transcript_standing_wall(read).is_some());
    let before = cut(prompt);
    let standing = transcript_standing_limit(&before).expect("the limit row stands");
    assert_eq!(standing.row.as_deref(), Some(ROW));
    assert_eq!(
        (standing.until, standing.window.as_deref()),
        (Some(RESET), Some("five_hour"))
    );
    assert!(
        standing.text.starts_with("You've hit your session limit"),
        "{}",
        standing.text
    );
    let then = |row: &str| transcript_standing_limit(&[before.as_str(), row].join("\n"));
    assert_eq!(
        then(&typed_row("keep going"))
            .and_then(|l| l.row)
            .as_deref(),
        Some(ROW)
    );
    let named = typed_row("is the auto-continuation of the assistant on? keep going");
    assert_eq!(
        then(&named).and_then(|l| l.row).as_deref(),
        Some(ROW),
        "{named}"
    );
    assert_eq!(
        then(&notification_row()).and_then(|l| l.row).as_deref(),
        Some(ROW)
    );
    let side = rows[prompt].replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert_eq!(
        then(&side).and_then(|l| l.row).as_deref(),
        Some(ROW),
        "a subagent's"
    );
    let again = quota_row(
        "2026-10-21T12:49:15.300Z",
        "five_hour",
        RESET + 18_000,
        "You've hit your session limit · resets 10:48am (America/Los_Angeles)",
    );
    let met =
        transcript_standing_limit(&[read, again.as_str()].join("\n")).expect("the new row stands");
    assert_eq!(
        (met.row.as_deref(), met.until),
        (Some("u"), Some(RESET + 18_000))
    );
}
