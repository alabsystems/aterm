// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The `CLAUDE_CONFIG_DIR` `l`'s launch in `run` names.
fn config_dir(run: &Run, l: &Launch) -> PathBuf {
    let v = launch_words(run, l)
        .env
        .iter()
        .flatten()
        .find(|(k, _)| k == "CLAUDE_CONFIG_DIR")
        .map(|(_, v)| v.clone())
        .expect("a launch names its CLAUDE_CONFIG_DIR");
    PathBuf::from(v.trim_matches('\''))
}

/// A STORED LAUNCH KEEPS THE HOME, AND THE LAB WRITES WHERE ITS CLAUDE READS: `claude --stored`
/// lays its login in a directory of its own, the one its launch names as `CLAUDE_CONFIG_DIR`,
/// and every lab verb reads and writes there (`lab settings`, the newest transcript); a second
/// stored launch keeps what `lab settings` wrote; the scratch home's `.claude` stays as `up`
/// laid it, so a later plain launch runs the API-key route with no login in it. Measured
/// before: `lab settings` wrote the home while the stored Claude Code read its own copy, and
/// a plain launch after a stored one ran on the stored OAuth login.
#[test]
fn a_stored_launch_keeps_the_home_and_the_lab_writes_where_its_claude_reads() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut run = Run {
        tag: "t".into(),
        run: root.join("runs/t"),
        w: root.join("runs/t/scratch"),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        claude_bin: "/c/claude".into(),
        route_seam: "1".into(),
        ..Run::default()
    };
    std::fs::create_dir_all(run.w.join("home/.claude")).unwrap();
    let (settings, cj) = scratch_claude_config(&run, "/helper", "/proj", &UpOpts::default());
    std::fs::write(run.settings(), &settings).unwrap();
    std::fs::write(run.w.join("home/.claude/.claude.json"), &cj).unwrap();
    // As launch_claude saves it, then lays it; the next verb loads env.sh.
    run.claude_login = crate::run::STORED_LOGIN.into();
    lay_stored_login(&run).unwrap();
    let run = Run::parse(&run.render());
    let stored = parse_launch(&["--stored".to_string()]).unwrap();
    let dir = config_dir(&run, &stored);
    assert_eq!(run.settings(), dir.join("settings.json"));
    let theirs = crate::util::scratch_text(&dir.join("settings.json"));
    assert!(
        theirs.contains("\"tui\": \"default\"") && !theirs.contains("apiKeyHelper"),
        "{theirs}"
    );
    assert!(crate::util::scratch_text(&dir.join(".credentials.json")).contains("claudeAiOauth"));
    // `lab settings t '{"permissions":{"defaultMode":"plan"}}'`
    crate::verbs::settings_merge(
        &run,
        &J::parse(r#"{"permissions":{"defaultMode":"plan"}}"#).unwrap(),
    )
    .unwrap();
    assert!(crate::util::scratch_text(&dir.join("settings.json")).contains("defaultMode"));
    let t = dir.join("projects/-proj/a.jsonl");
    std::fs::create_dir_all(t.parent().unwrap()).unwrap();
    std::fs::write(&t, "{\"type\":\"user\"}\n").unwrap();
    assert_eq!(crate::stats::newest_transcript(&run), Some(t));
    // A second --stored launch keeps what `lab settings` wrote there.
    lay_stored_login(&run).unwrap();
    assert!(crate::util::scratch_text(&dir.join("settings.json")).contains("defaultMode"));
    // The home is as `up` laid it: a later plain launch is the API-key route again.
    let plain = Run {
        claude_login: String::new(),
        ..run.clone()
    };
    assert_eq!(config_dir(&plain, &Launch::default()), plain.claude_dir());
    assert_eq!(crate::util::scratch_text(&plain.settings()), settings);
    assert!(!plain.w.join("home/.claude/.credentials.json").exists());
}

/// AN OWN LAUNCH IS THE DEFAULT HOME'S LOGIN WITH THE TAP ON ITS LINE: `claude --own`, the
/// launch whose row line E claims (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step
/// 4), names no `CLAUDE_CONFIG_DIR`, no credential and no proxy, so the harness reads its
/// process as the default home's own login (`ProcessLogin::default_home`, which the footer's
/// account label asks), its route the stub's base URL alone (which the instance's route seam
/// admits); it carries aterm's usage tap (`tap::carried_by`); and its login is laid where
/// that Claude Code reads it, the profile in the scratch HOME's `.claude.json`, which the
/// harness's profile read takes for a subscription. Measured before (2026-11-08): `claude
/// --subscriber` ran with `CLAUDE_CODE_OAUTH_TOKEN` (`auth=outside`) and no tap, so Claude Code
/// drew no statusLine row and the footer named no account.
#[test]
fn an_own_launch_is_the_default_homes_login_with_the_tap_on_its_line() {
    use aterm_agent::harness::{accounts, netwatch, tap};
    let own = parse_launch(&["--own".to_string()]).unwrap();
    assert!(own.own && own.stub_route_alone && !own.stored && !own.subscriber);
    for other in ["--stored", "--subscriber"] {
        assert!(
            parse_launch(&["--own".to_string(), other.to_string()]).is_err(),
            "--own and {other} are two logins"
        );
    }
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut run = Run {
        tag: "t".into(),
        run: root.join("runs/t"),
        w: root.join("runs/t/scratch"),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        claude_bin: "/c/claude".into(),
        aterm: "/a/target/debug/aterm".into(),
        route_seam: "1".into(),
        ..Run::default()
    };
    let words = exec_words(&launcher(&run, &own));
    let at = words
        .iter()
        .position(|w| w == "/c/claude")
        .expect("the binary");
    let (env, argv) = (&words[..at], &words[at..]);
    assert!(tap::carried_by(argv), "the tap rides the line: {argv:?}");
    assert_eq!(
        argv.get(2).map(String::as_str),
        tap::settings_value(Path::new(&run.aterm)).as_deref()
    );
    let stub = format!("http://127.0.0.1:{}", run.port);
    let pairs: Vec<(&str, &str)> = env.iter().filter_map(|w| w.split_once('=')).collect();
    for (k, v) in &pairs {
        assert!(
            !accounts::CREDENTIAL_KEYS.contains(k)
                && !accounts::PROFILE_KEYS.contains(k)
                && (!netwatch::ROUTE_KEYS.contains(k)
                    || netwatch::lab_default_route(Some(&stub), k, v)),
            "{k} moves the launch off its own login: {env:?}"
        );
    }
    for k in PROXY_KEYS {
        assert!(
            env.windows(2).any(|w| w[0] == "-u" && w[1] == k),
            "unsets {k}: {env:?}"
        );
    }
    let login = accounts::ProcessLogin::of_env(pairs.iter().copied());
    assert!(
        login.default_home && login.login.is_none(),
        "the default home's own login: {env:?}"
    );
    // As `up` laid the scratch home, then as launch_claude saves the run and lays the login.
    std::fs::create_dir_all(run.w.join("home/.claude")).unwrap();
    let (settings, cj) = scratch_claude_config(&run, "/helper", "/proj", &UpOpts::default());
    std::fs::write(run.settings(), &settings).unwrap();
    std::fs::write(run.w.join("home/.claude/.claude.json"), &cj).unwrap();
    run.claude_login = crate::run::OWN_LOGIN.into();
    let run = Run::parse(&run.render());
    assert_eq!(run.claude_dir(), run.w.join("home/.claude"));
    assert_eq!(run.claude_json(), run.w.join("home/.claude.json"));
    lay_own_login(&run).unwrap();
    let home = run.w.join("home");
    let theirs = crate::util::scratch_text(&run.settings());
    assert!(
        !theirs.contains("apiKeyHelper") && !theirs.contains("HTTP_PROXY"),
        "{theirs}"
    );
    assert!(
        crate::util::scratch_text(&home.join(".claude/.credentials.json"))
            .contains("claudeAiOauth")
    );
    assert!(
        !crate::util::scratch_text(&home.join(".claude/.claude.json")).contains("oauthAccount"),
        "the scratch `.claude`'s profile stays as `up` laid it"
    );
    // The roster names it, the default login's one row, whose email is the profile's.
    let roster_at = run.w.join("cfg/aterm").join(accounts::ACCOUNTS_FILE);
    let roster = accounts::Roster::load(&roster_at).unwrap();
    assert_eq!(
        roster
            .row_for_login(None)
            .map(|row| (row.label.as_str(), row.expected_identity_email())),
        Some(("main", Some(crate::stub::account::EMAIL))),
        "{roster:?}"
    );
    assert_eq!(
        accounts::judged_label(
            &roster,
            &accounts::ProcessLogin {
                route: accounts::AuthRoute::Stored,
                ..login.clone()
            }
        ),
        Some("main"),
        "the seam admitting it, the default home's login is the roster's `main`"
    );
    // The harness's profile read of that process, its base URL the seam's: a subscription.
    let env: Vec<String> = pairs
        .iter()
        .filter(|(k, _)| *k != "ANTHROPIC_BASE_URL")
        .map(|(k, v)| format!("{k}={v}"))
        .chain([format!("HOME={}", home.display())])
        .collect();
    let read = aterm_agent::harness::profile::subscription_and_plan_for(
        &env,
        &home,
        &home.join(".claude"),
        Some(&roster),
        1_794_000_000,
        None,
    );
    assert!(read.is_some(), "the profile read takes the own login");
    // A second launch keeps what Claude Code wrote in the HOME's `.claude.json`.
    let mut kept = J::parse(&crate::util::scratch_text(&run.claude_json())).unwrap();
    kept.set("numStartups", J::Int(7));
    std::fs::write(run.claude_json(), kept.dump(crate::pyjson::COMPACT)).unwrap();
    lay_own_login(&run).unwrap();
    let again = crate::util::scratch_text(&run.claude_json());
    assert!(
        again.contains("numStartups") && again.contains("oauthAccount"),
        "{again}"
    );
    // And a roster it finds there stays as it was written.
    let theirs = "[[account]]\nlabel = \"alt1\"\nclass = \"shared\"\n";
    std::fs::write(&roster_at, theirs).unwrap();
    lay_own_login(&run).unwrap();
    assert_eq!(crate::util::scratch_text(&roster_at), theirs);
}

#[test]
fn the_store_names_its_managed_program_with_no_aterm_run() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let prefix = atpkg::store::default_prefix(&home);
    let build = prefix.join("store/claude/1000002000001000399/bin/claude");
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();
    std::fs::write(&build, "#!/bin/sh\necho '2.1.399 (Claude Code)'\n").unwrap();
    std::fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).unwrap();
    let home = home.to_string_lossy().into_owned();
    assert_eq!(store_shim_target(&home, "claude"), None, "no shim yet");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(&build, prefix.join("bin/claude")).unwrap();
    assert_eq!(
        store_shim_target(&home, "claude"),
        Some(build.to_string_lossy().into_owned())
    );
    assert_eq!(
        store_shim_target(&home, "codex"),
        None,
        "only what it holds"
    );
}

/// The build `up` names is the token the instance's harness tapes carry as `build=`:
/// `<version>+g<commit>` from its `ctl version` reply, a dirty build's `-dirty` kept, and
/// nothing from a refusal, a reply missing either word, or a word with a control byte.
#[test]
fn up_names_the_build_its_instance_says_it_runs() {
    let reply = |commit: &str| {
        format!(
            "OK version=0.150.0 build=1793707820 commit={commit} built=2026-11-03T11:52:07Z \
             arch=aarch64 trustc=1.96.0 trustc_commit=0123456789ab trustc_host=aarch64-apple-darwin \
             flavor=t trust=0.1.0 rust_compat=1.96.0 profile=debug trust_verify=off \
             update_pin_sha256=ab objc_contained=0 signature=adhoc\n"
        )
    };
    assert_eq!(
        build_named(&reply("859f55cf4a7e")).as_deref(),
        Some("0.150.0+g859f55cf4a7e")
    );
    assert_eq!(
        build_named(&reply("851f150cfa26-dirty")).as_deref(),
        Some("0.150.0+g851f150cfa26-dirty")
    );
    assert_eq!(build_named("ERR version: takes no arguments\n"), None);
    assert_eq!(build_named("OK version=0.150.0 build=1\n"), None);
    assert_eq!(build_named(&reply("859f\u{1b}[2J")), None);
    assert_eq!(build_named(""), None);
}

/// `ps_rows` reads each pid alone: its header once, a row for each live pid, and no
/// row for a reaped one. Asked for the whole list at once, a wedged `ps` never
/// returns; this test's return is that proof's other half.
#[cfg(unix)]
#[test]
fn ps_rows_reads_each_pid_alone() {
    let gone = {
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("0.01")
            .spawn()
            .expect("spawn the short-lived pid");
        let id = child.id().to_string();
        let _ = child.wait();
        id
    };
    let me = std::process::id().to_string();
    let rows = ps_rows(&[me.clone(), gone.clone(), "1".to_string()]);
    let first_words: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.split_whitespace().next())
        .collect();
    assert_eq!(
        first_words.iter().filter(|w| **w == "PID").count(),
        1,
        "{rows:?}"
    );
    assert!(first_words.contains(&me.as_str()), "{rows:?}");
    assert!(first_words.contains(&"1"), "{rows:?}");
    assert!(!first_words.contains(&gone.as_str()), "{rows:?}");
}

#[test]
fn a_line_typed_in_a_tab_shell_reaches_the_stub_with_the_labs_key() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let build = bin.join("client");
    std::fs::write(
        &build,
        "#!/bin/sh\nprintf 'key=%s url=%s google=%s args=%s\\n' \"$GEMINI_API_KEY\" \"$GOOGLE_GEMINI_BASE_URL\" \"${GOOGLE_API_KEY-unset}\" \"$*\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).unwrap();
    let w = dir.join("scratch");
    put_tab_shell(&w, "4242");
    let written = std::fs::read_to_string(w.join("home/.zshrc")).unwrap();
    assert_eq!(written, tab_profile("4242"));
    assert!(!written.contains("PATH"), "{written}");
    let typed = |profile: &str| {
        let rc = dir.join("zshrc");
        std::fs::write(&rc, profile).unwrap();
        let out = aterm_uds::spawnfd::command("/bin/zsh")
            .args(["-f", "-c", "source \"$1\"; client --resume abc"])
            .arg("zsh")
            .arg(&rc)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("GOOGLE_API_KEY", "an-owner-key")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(
        typed(&written),
        format!(
            "key={} url=http://127.0.0.1:4242 google=unset args=--resume abc",
            crate::agy::LAB_GEMINI_KEY
        )
    );
    assert_eq!(
        typed("PROMPT='lab$ '\n"),
        "key= url= google=an-owner-key args=--resume abc",
        "the profile before"
    );
}

#[test]
fn a_lab_ups_claude_settings_carry_the_settings_witness_hook() {
    let run = Run {
        run: "/l/runs/t".into(),
        w: "/l/runs/t/scratch".into(),
        port: "1".into(),
        proxy: "http://127.0.0.1:2".into(),
        ..Run::default()
    };
    let (settings, _) =
        scratch_claude_config(&run, "/l/runs/t/apikey.sh", "/p", &UpOpts::default());
    let d = J::parse(&settings).unwrap();
    let commands = |hooks: Option<&J>| -> Vec<String> {
        hooks
            .and_then(|h| h.get("ConfigChange"))
            .and_then(J::as_arr)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.get("hooks").and_then(J::as_arr))
            .flatten()
            .filter(|c| c.get("type").and_then(J::as_str) == Some("command"))
            .filter_map(|c| c.get("command").and_then(J::as_str).map(str::to_string))
            .collect()
    };
    let named = |run: &str| {
        commands(Some(&crate::settings_witness::hooks(
            &Path::new(run).join(crate::settings_witness::SCRIPT),
        )))
    };
    assert_eq!(commands(d.get("hooks")), named("/l/runs/t"));
    assert_eq!(named("/l/runs/t"), ["/l/runs/t/config-change.sh"]);
    assert_ne!(commands(d.get("hooks")), named("/l/runs/u"));
    assert_eq!(
        d.get("apiKeyHelper").and_then(J::as_str),
        Some("/l/runs/t/apikey.sh")
    );
}

#[test]
fn a_lab_up_config_starts_with_the_audible_bell_off() {
    let read = |text: &str| aterm_toml::from_str::<aterm_toml::Value>(text).expect("one document");
    let bell = |doc: &aterm_toml::Value| doc.get("bell_sound").and_then(aterm_toml::Value::as_bool);

    let plain = read(&scratch_aterm_toml(
        "font_px = 13",
        "usage_tap = true",
        None,
        false,
    ));
    assert_eq!(bell(&plain), Some(false));
    assert_eq!(
        plain.get("font_px").and_then(aterm_toml::Value::as_integer),
        Some(13)
    );
    assert_eq!(
        plain
            .get("harness")
            .and_then(|t| t.get("usage_tap"))
            .and_then(aterm_toml::Value::as_bool),
        Some(true),
    );

    let asked = read(&scratch_aterm_toml("bell_sound = true", "", None, false));
    assert_eq!(
        bell(&asked),
        Some(true),
        "LAB_ATERM_TOML's bell is the file's"
    );
}

/// The `[fabric]` table a lab bus names its bridge in: [`crate::bus::Laid::table`]'s shape.
const BUS_TABLE: &str = "[fabric]\ncommand = \"/r/fabric/aterm link serve --fleet lab --broker /s/fabric/b.sock --cap-file /r/fabric/node.cap --state /s/fabric/state --accept-from n-0123456789abcdef\"\n";

/// A FABRIC LAB NAMES ITS BRIDGE AND A TAB-STRIP LAB ITS ROW: the scratch `aterm.toml` with the
/// bus's table and the strip is one TOML document holding the bridge's command and
/// `tab_strip_rows = 1`, LAB_ATERM_TOML's own keys, the harness keys and the bell-off rule kept;
/// without either option neither key is there.
#[test]
fn a_fabric_lab_names_its_bridge_and_a_tab_strip_lab_its_row() {
    let read = |text: &str| aterm_toml::from_str::<aterm_toml::Value>(text).expect("one document");
    let command = |doc: &aterm_toml::Value| {
        doc.get("fabric")
            .and_then(|t| t.get("command"))
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string)
    };
    let both = read(&scratch_aterm_toml(
        "font_px = 13\n[ui]\nx = 1",
        "usage_tap = true",
        Some(BUS_TABLE),
        true,
    ));
    assert_eq!(
        command(&both).as_deref(),
        Some(
            "/r/fabric/aterm link serve --fleet lab --broker /s/fabric/b.sock --cap-file /r/fabric/node.cap --state /s/fabric/state --accept-from n-0123456789abcdef"
        )
    );
    assert_eq!(
        both.get("tab_strip_rows")
            .and_then(aterm_toml::Value::as_integer),
        Some(1)
    );
    assert_eq!(
        both.get("font_px").and_then(aterm_toml::Value::as_integer),
        Some(13)
    );
    assert_eq!(
        both.get("ui")
            .and_then(|t| t.get("x"))
            .and_then(aterm_toml::Value::as_integer),
        Some(1)
    );
    assert_eq!(
        both.get("harness")
            .and_then(|t| t.get("usage_tap"))
            .and_then(aterm_toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        both.get("bell_sound").and_then(aterm_toml::Value::as_bool),
        Some(false)
    );
    let neither = read(&scratch_aterm_toml(
        "font_px = 13\n[ui]\nx = 1",
        "usage_tap = true",
        None,
        false,
    ));
    assert_eq!(command(&neither), None);
    assert_eq!(neither.get("tab_strip_rows"), None);
    assert_eq!(neither.get("fabric"), None);
}

/// A BUS OR TAB-STRIP LAB REFUSES A LAB_ATERM_TOML NAMING THE SAME KEY, before anything is
/// made (exit 2): composed, the pair is no TOML document, so the instance would not start. Each
/// flag alone, or beside an unrelated LAB_ATERM_TOML, is no refusal.
#[test]
fn a_bus_or_tab_strip_lab_refuses_an_aterm_toml_naming_the_same_key() {
    let o = |fabric, tab_strip, toml: &str| UpOpts {
        fabric,
        tab_strip,
        aterm_toml: toml.to_string(),
        ..UpOpts::default()
    };
    let named_bus = "[fabric]\ncommand = \"x\"";
    let named_row = "tab_strip_rows = 2";
    let why = opts_refused(&o(true, false, named_bus)).expect("refused");
    assert!(
        why.contains("--fabric") && why.contains("[fabric]"),
        "{why}"
    );
    let f = up("not a tag!", &o(true, false, named_bus)).expect_err("refused");
    assert_eq!((f.code, f.msg.as_deref()), (2, Some(why)));
    let why = opts_refused(&o(false, true, named_row)).expect("refused");
    assert!(
        why.contains("--tab-strip") && why.contains("tab_strip_rows"),
        "{why}"
    );
    for (fabric, tab_strip, toml) in [
        (true, false, ""),
        (false, true, ""),
        (true, true, "font_px = 13\n[ui]\nx = 1"),
        (true, false, named_row),
        (false, true, named_bus),
        (false, false, named_bus),
    ] {
        assert_eq!(
            opts_refused(&o(fabric, tab_strip, toml)),
            None,
            "fabric {fabric} tab strip {tab_strip}: {toml}"
        );
    }
    let parses = |toml: &str, fabric: bool, tab_strip: bool| {
        aterm_toml::from_str::<aterm_toml::Value>(&scratch_aterm_toml(
            toml,
            "",
            fabric.then_some(BUS_TABLE),
            tab_strip,
        ))
        .is_ok()
    };
    assert!(!parses(named_bus, true, false), "a [fabric] table twice");
    assert!(!parses(named_row, false, true), "tab_strip_rows twice");
    assert!(parses(named_bus, false, true) && parses(named_row, true, false));
}

/// The two verbs run in a child of this test binary whose `LAB_STATE` is a fresh scratch
/// directory and whose `LAB_TAG` is unset, so the default lab `lab` is down by construction:
/// [`crate::paths::paths`] is per process, and a regressed `down` that fell back to the default
/// tag could reach no lab this checkout has up.
#[test]
fn ps_and_down_of_a_lab_that_is_down_name_its_tag() {
    const PROBE: &str = "ATERM_LAB_PS_DOWN_PROBE";
    let gone = "ki1016-ps-gone";
    if std::env::var_os(PROBE).is_some() {
        let refusal = |got: Res| got.err().and_then(|f| f.msg).unwrap_or_default();
        for (verb, got) in [
            ("ps", refusal(cmd_ps(&[gone.to_string()]))),
            (
                "down",
                refusal(cmd_down(&[gone.to_string(), "--keep-dumps".to_string()])),
            ),
        ] {
            println!("PROBE {verb}: {}", got.replace('\n', " "));
        }
        std::process::exit(0);
    }
    let state = aterm_tempfile::tempdir().expect("a scratch LAB_STATE");
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args([
        "--exact",
        "lab::tests::ps_and_down_of_a_lab_that_is_down_name_its_tag",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(PROBE, "1")
    .env("LAB_STATE", state.path())
    .env_remove("LAB_TAG");
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + std::time::Duration::from_mins(1),
    )
    .expect("the probe ends within the hang detector");
    let out = String::from_utf8_lossy(&out.stdout);
    for verb in ["ps", "down"] {
        let mark = format!("PROBE {verb}: ");
        let got = out
            .lines()
            .find_map(|l| l.split_once(&mark).map(|(_, got)| got))
            .unwrap_or_else(|| panic!("{verb} printed no refusal: {out}"));
        assert!(
            got.contains(&format!("no lab '{gone}' is up")),
            "{verb}: {got:?}"
        );
        assert!(
            !got.contains("'lab'"),
            "{verb} names the default lab: {got:?}"
        );
    }
}

#[test]
fn the_managed_claude_is_the_last_path_of_the_arrow_chain() {
    let line = "claude → /S/pkg/reroute/claude → /S/pkg/agents/claude → /S/Application Support/aterm/pkg/store/claude/1000002000001000300/bin/claude — managed 2.1.300 — Anthropic latest";
    assert_eq!(
        managed_claude(line).as_deref(),
        Some("/S/Application Support/aterm/pkg/store/claude/1000002000001000300/bin/claude")
    );
    assert_eq!(managed_claude("claude: not managed here"), None);
}

#[test]
fn claude_options_parse_and_the_launcher_execs_the_binary_by_path() {
    let a: Vec<String> = [
        "-e",
        "K=v 1",
        "--relay",
        "--subscriber",
        "--",
        "--model",
        "x y",
    ]
    .map(String::from)
    .to_vec();
    let l = parse_launch(&a).unwrap();
    assert!(l.relay && l.subscriber && !l.no_wait);
    assert!(parse_launch(&["--bogus".to_string()]).is_err());
    let stored = parse_launch(&["--stored".to_string()]).unwrap();
    assert!(stored.stored && stored.stub_route_alone);
    assert!(parse_launch(&["--stored".to_string(), "--subscriber".to_string()]).is_err());
    let run = Run {
        tag: "t".into(),
        run: "/s/runs/t".into(),
        w: "/s/runs/t/scratch".into(),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        claude_bin: "/a b/claude".into(),
        ..Run::default()
    };
    let s = launcher(&run, &l);
    assert!(s.starts_with("#!/bin/sh\nclear\nexec /s/runs/t/relay/node --log /s/runs/t/scratch/relay.log -- env PATH=\"/s/runs/t/shim:$PATH\""), "{s}");
    assert!(s.contains("  LAB_SECURITY_LOG=/s/runs/t/scratch/security.log LAB_OPEN_LOG=/s/runs/t/scratch/open.log BROWSER=/s/runs/t/shim/open \\\n"), "{s}");
    assert!(s.contains("  'K=v 1' \\\n  CLAUDE_CODE_NO_FLICKER=1 \\\n  CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-LABLABLAB"), "{s}");
    assert!(s.ends_with("  '/a b/claude' --model 'x y'\n"), "{s}");
    let words = exec_words(&s);
    assert!(opens_recorded(&words, &run), "{words:?}");
    let at = words.iter().position(|w| w == "/a b/claude").unwrap();
    assert_eq!(words[at + 1..], ["--model", "x y"], "{words:?}");
    let plain = launcher(&run, &Launch::default());
    assert!(opens_recorded(&exec_words(&plain), &run), "{plain}");
    for gone in [
        " LAB_OPEN_LOG=/s/runs/t/scratch/open.log",
        " BROWSER=/s/runs/t/shim/open",
    ] {
        let cut = plain.replace(gone, "");
        assert_ne!(cut, plain);
        assert!(!opens_recorded(&exec_words(&cut), &run), "{cut}");
    }
    let as_arg = Launch {
        args: vec!["LAB_OPEN_LOG=/s/runs/t/scratch/open.log".into()],
        ..Launch::default()
    };
    let arg_only =
        launcher(&run, &as_arg).replacen(" LAB_OPEN_LOG=/s/runs/t/scratch/open.log", "", 1);
    let arg_words = exec_words(&arg_only);
    assert_eq!(
        arg_words.last().map(String::as_str),
        Some("LAB_OPEN_LOG=/s/runs/t/scratch/open.log"),
        "{arg_only}"
    );
    assert!(!opens_recorded(&arg_words, &run), "{arg_only}");
    let meta_run = Run {
        run: r#"/s/"a"$b`c`/t"#.into(),
        w: "/s/w $x".into(),
        port: "5;evil".into(),
        proxy: "http://127.0.0.1:6 x".into(),
        claude_bin: "/a b/claude".into(),
        ..Run::default()
    };
    let meta_words = exec_words(&launcher(&meta_run, &l));
    assert!(
        meta_words.contains(&r#"PATH=/s/"a"$b`c`/t/shim:$PATH"#.to_string())
            && meta_words.contains(&"ANTHROPIC_BASE_URL=http://127.0.0.1:5;evil".to_string())
            && meta_words.contains(&"HTTP_PROXY=http://127.0.0.1:6 x".to_string())
            && opens_recorded(&meta_words, &meta_run),
        "{meta_words:?}"
    );
}

fn exec_words(script: &str) -> Vec<String> {
    let line = script
        .split_once("exec ")
        .map_or("", |(_, rest)| rest)
        .replace("\\\n", "");
    shell_words(line.lines().next().unwrap_or(""))
}

/// `line`'s shell words, quotes removed (no expansion).
fn shell_words(line: &str) -> Vec<String> {
    let (mut words, mut cur, mut open) = (Vec::new(), String::new(), false);
    let mut it = line.chars();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                open = true;
                cur.extend(it.by_ref().take_while(|&d| d != '\''));
            }
            '"' => {
                open = true;
                while let Some(d) = it.next() {
                    match d {
                        '"' => break,
                        '\\' => cur.extend(it.next()),
                        _ => cur.push(d),
                    }
                }
            }
            '\\' => cur.extend(it.next()),
            c if c.is_whitespace() => {
                if open || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
                open = false;
            }
            c => cur.push(c),
        }
    }
    if open || !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn opens_recorded(words: &[String], run: &Run) -> bool {
    let Some(env) = words.iter().position(|w| w == "env") else {
        return false;
    };
    let is_assignment = |w: &str| {
        w.split_once('=').is_some_and(|(k, _)| {
            !k.is_empty()
                && !k.starts_with(|c: char| c.is_ascii_digit())
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    let vars: Vec<&str> = words[env + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|w| is_assignment(w))
        .collect();
    let has = |want: &str| vars.contains(&want);
    let shim = run.run.join("shim");
    vars.iter()
        .any(|v| v.starts_with(&format!("PATH={}:", shim.display())))
        && has(&format!("BROWSER={}", shim.join("open").display()))
        && has(&format!("LAB_OPEN_LOG={}/open.log", run.w.display()))
}

#[test]
fn an_atpkg_build_record_names_its_version() {
    let rec = "schema = 1\nprogram = \"agy\"\nversion = \"1.4.0\"\nvendor = \"Google\"\n";
    assert_eq!(vendor_version(rec), "1.4.0");
    assert_eq!(vendor_version("program = \"agy\"\n"), "");
    let line = "agy → /S/pkg/reroute/agy → /S/pkg/store/agy/1000001000004000000/bin/agy — managed 1.4.0 — Google";
    assert_eq!(
        managed_bin("agy", line).as_deref(),
        Some("/S/pkg/store/agy/1000001000004000000/bin/agy")
    );
}

#[test]
fn muse_code_s_build_is_read_from_its_record_or_its_version_line() {
    let rec = "schema = 1\nprogram = \"muse\"\nversion = \"1.4.10-R6427.1\"\nvendor = \"Meta\"\n";
    assert_eq!(vendor_version(rec), "1.4.10-R6427.1");
    assert_eq!(vendor_version("program = \"muse\"\nversion_note = 2\n"), "");
    assert_eq!(
        muse_build("Muse Code 1.4.10 (1.4.10-R6427.1)"),
        Some("1.4.10-R6427.1")
    );
    assert_eq!(muse_build("Muse Code 1.4.10"), None);
    let line = "muse → /S/pkg/reroute/muse → /S/pkg/store/muse/2001004010006427002/bin/muse — managed 1.4.10-R6427.1 — Meta latest";
    assert_eq!(
        managed_bin("muse", line).as_deref(),
        Some("/S/pkg/store/muse/2001004010006427002/bin/muse")
    );
    assert_eq!(managed_bin("muse", "codex → /x — managed 1 — y"), None);
}

#[test]
fn a_dev_linked_package_names_its_binary() {
    let line = "codex → /S/pkg/bin/codex → /D/artifacts/codex/0.164.0-dev.1+g51ba34fe1/bin/codex — dev-linked (managed from that checkout, not the index)";
    assert_eq!(
        managed_bin("codex", line).as_deref(),
        Some("/D/artifacts/codex/0.164.0-dev.1+g51ba34fe1/bin/codex")
    );
    assert_eq!(
        managed_bin("codex", "codex → /usr/local/bin/codex — on PATH"),
        None
    );
}

#[test]
fn the_tripwire_log_is_kept_with_its_account_masked() {
    assert_eq!(
        mask_account(
            "12:16:51 find-generic-password account=\"owner\" service=\"Claude Code-0123abcd\" via=argv"
        ),
        "12:16:51 find-generic-password account=\"<user>\" service=\"Claude Code-0123abcd\" via=argv"
    );
}

fn pairs(words: &[(&str, &str)]) -> Vec<(String, String)> {
    words
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn a_home_image_is_sized_in_hdiutils_spelling_and_needs_room_past_itself() {
    assert_eq!(home_image_bytes("16g"), Ok(16 << 30));
    assert_eq!(home_image_bytes("2048m"), Ok(2 << 30));
    for bad in [
        "", "g", "16", "16G", "-1g", "+1g", "1.5g", "16gb", "16é", "512m", "65g",
    ] {
        assert!(home_image_bytes(bad).is_err(), "{bad:?}");
    }
    assert!(home_image_bytes(&format!("{}g", u64::MAX)).is_err());
    let need = (16 + HOME_IMAGE_SPARE_GIB) << 30;
    assert_eq!(home_image_refused("16g", Some(need)), None);
    let short = home_image_refused("16g", Some(need - 1)).expect("refused");
    assert!(short.contains("needs 24 GiB"), "{short}");
    assert!(
        home_image_refused("16g", None).is_some(),
        "unknown free space"
    );
    assert!(
        home_image_refused("16", Some(u64::MAX)).is_some(),
        "no size"
    );
}

#[test]
fn the_instance_is_handed_only_explicit_diagnostics_and_dev_seams() {
    assert_eq!(instance_env(""), Ok(Vec::new()));
    assert_eq!(
        instance_env(" TMPDIR=/tmp  ATERM_DEBUG_PASTE_FILE=/s/p.txt "),
        Ok(pairs(&[
            ("TMPDIR", "/tmp"),
            ("ATERM_DEBUG_PASTE_FILE", "/s/p.txt")
        ]))
    );
    assert_eq!(
        instance_env("ATERM_DEBUG_CLIPBOARD_FILE=/s/c=d"),
        Ok(pairs(&[("ATERM_DEBUG_CLIPBOARD_FILE", "/s/c=d")]))
    );
    assert_eq!(
        instance_env("ATERM_DEV_CARD_PROBE=1"),
        Ok(pairs(&[("ATERM_DEV_CARD_PROBE", "1")]))
    );
    assert_eq!(
        instance_env("ATERM_OCCUPANCY_NUDGE_S=60"),
        Ok(pairs(&[("ATERM_OCCUPANCY_NUDGE_S", "60")]))
    );
    assert_eq!(
        instance_env("NSZombieEnabled=YES"),
        Ok(pairs(&[("NSZombieEnabled", "YES")]))
    );
    assert_eq!(
        instance_env("NSZombieEnabled=NO"),
        Ok(pairs(&[("NSZombieEnabled", "NO")]))
    );
    for bad in [
        "NSZombieEnabled=1",
        "NSZombieEnabled=YES NSZombieEnabled=NO",
        "ATERM_UPGRADE_QUIET_S=1",
        "ATERM_OCCUPANCY_NUDGE=60",
        "ATERM_OCCUPANCY_NUDGE_S=",
        "HOME=/x",
        "PATH=/x",
        "XDG_RUNTIME_DIR=/x",
        "ATERM_STATE_HOME=/x",
        "ATERM_DEBUG_HEADLESS_RESTORE=1",
        "ATERM_DEBUG_JAIL_MARK=lab.jail.k1.0123456789abcdef",
        "ATERM_HARNESS_TAPE=0",
        "CLAUDE_CONFIG_DIR=/x",
        "TMP=/x",
        "ATERM_DEBUG_=1",
        "ATERM_DEBUG_paste=1",
        "ATERM_DEV_=1",
        "ATERM_DEV_card=1",
        "ATERM_DEVX=1",
        "TMPDIR",
        "=x",
        "TMPDIR=",
        "TMPDIR=tmp",
        "TMPDIR=/a TMPDIR=/b",
        "ATERM_DEBUG_X=a\u{7}b",
    ] {
        assert!(instance_env(bad).is_err(), "{bad}");
    }
}

/// Whether aterm's own judge admits the stub's URL under the seam value `vars` hold.
fn lab_default_route_admits(vars: &[(String, String)], run: &Run) -> bool {
    let seam = vars
        .iter()
        .find(|(k, _)| k == aterm_agent::harness::netwatch::LAB_ROUTE_SEAM)
        .map(|(_, v)| v.as_str());
    aterm_agent::harness::netwatch::lab_default_route(
        seam,
        "ANTHROPIC_BASE_URL",
        &stub_base_url(run),
    )
}

fn recorded(instance_env: &str, seam: &str) -> Run {
    Run {
        run: "/s/runs/k1".into(),
        w: "/s/runs/k1/scratch".into(),
        sockdir: "/tmp/at.AbCd".into(),
        sock: "/tmp/at.AbCd/a.sock".into(),
        proj: "/tmp/at.AbCd/proj".into(),
        cols: "144".into(),
        rows: "50".into(),
        seam: seam.into(),
        instance_env: instance_env.into(),
        ..Run::default()
    }
}

#[test]
fn every_launch_hands_the_instance_what_up_recorded() {
    let vars = instance_vars(&recorded("TMPDIR=/tmp", "0"), "owner").unwrap();
    let get = |k: &str| {
        vars.iter()
            .filter(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(get("TMPDIR"), vec!["/tmp"]);
    assert_eq!(get("HOME"), vec!["/s/runs/k1/scratch/home"]);
    assert_eq!(get("XDG_RUNTIME_DIR"), vec!["/tmp/at.AbCd"]);
    assert_eq!(get("ATERM_STATE_HOME"), vec!["/s/runs/k1/scratch/state"]);
    assert!(get("ATERM_DEBUG_HEADLESS_RESTORE").is_empty());
    assert_eq!(
        get("ATERM_DEBUG_SYSTEM_PRESSURE"),
        vec!["none"],
        "the host's memory pressure stays out of a lab run"
    );
    assert_eq!(
        vars.last(),
        Some(&("TMPDIR".to_string(), "/tmp".to_string()))
    );
    let seam = instance_vars(&recorded("", "1"), "owner").unwrap();
    assert!(seam.contains(&("ATERM_DEBUG_HEADLESS_RESTORE".to_string(), "1".to_string())));
    assert!(!seam.iter().any(|(k, _)| k == "TMPDIR"));
    let route = aterm_agent::harness::netwatch::LAB_ROUTE_SEAM;
    assert!(
        get(route).is_empty(),
        "no route seam unless up asked for one"
    );
    let routed = Run {
        route_seam: "1".into(),
        port: "57232".into(),
        ..recorded("", "0")
    };
    let vars = instance_vars(&routed, "owner").unwrap();
    assert!(
        vars.contains(&(route.to_string(), "http://127.0.0.1:57232".to_string())),
        "the route seam names the stub's own URL: {vars:?}"
    );
    assert!(
        lab_default_route_admits(&vars, &routed),
        "the URL the seam names is the one the instance's own judge admits"
    );
    assert!(
        instance_vars(
            &recorded(&format!("{route}=http://127.0.0.1:1"), "0"),
            "owner"
        )
        .is_err(),
        "LAB_INSTANCE_ENV cannot hand the instance the route seam"
    );
    assert!(
        get(aterm_uds::holders::JAIL_SEAM).is_empty(),
        "no jail scan unless asked"
    );
    let scans = Run {
        mark: "lab.jail.k1.0123456789abcdef".into(),
        jail_scan: "1".into(),
        ..recorded("", "0")
    };
    let jail = instance_vars(&scans, "owner").unwrap();
    assert!(
        jail.contains(&(
            aterm_uds::holders::JAIL_SEAM.to_string(),
            "lab.jail.k1.0123456789abcdef".to_string()
        )),
        "the jail's mark, to its instance's disk scans: {jail:?}"
    );
    for edited in [
        "HOME=/elsewhere",
        "ATERM_DEBUG_HEADLESS_RESTORE=1",
        "ATERM_DEBUG_SYSTEM_PRESSURE=critical",
    ] {
        assert!(
            instance_vars(&recorded(edited, "1"), "owner").is_err(),
            "{edited}"
        );
    }
}

#[test]
fn the_store_lookup_is_handed_the_account_home_under_a_scratch_home() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-which-")
        .tempdir()
        .expect("a scratch dir");
    let root = fs::canonicalize(top.path()).expect("its physical path");
    let (account, scratch) = (root.join("account"), root.join("scratch-home"));
    for d in [&account, &scratch] {
        fs::create_dir_all(d).expect("a home");
    }
    let stand_in = root.join("aterm");
    fs::write(
        &stand_in,
        "#!/bin/sh\n[ \"$1 $2\" = 'pkg which' ] || exit 2\n\
         printf '%s → %s → %s — managed 1.0 — x\\n' \"$3\" \"$HOME/pkg/bin/$3\" \"$HOME/pkg/store/$3/1/bin/$3\"\n",
    )
    .and_then(|()| fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)))
    .expect("the stand-in");
    let p = crate::paths::Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    assert_eq!(p.store_home(), account.display().to_string());
    let store =
        |home: &Path, name: &str| format!("{}/pkg/store/{name}/1/bin/{name}", home.display());
    assert_eq!(store_claude(&p, &stand_in), Ok(store(&account, "claude")));
    for name in ["codex", "agy", "muse"] {
        assert_eq!(
            managed_store_bin_in(&p, &stand_in, name),
            Some(store(&account, name)),
            "{name}"
        );
    }
    let bare =
        crate::paths::Paths::resolve_with(Some(scratch.clone()), Some(scratch.clone().into()));
    assert_eq!(
        store_claude(&bare, &stand_in),
        Ok(store(&scratch, "claude"))
    );
    assert_eq!(
        managed_store_bin_in(&bare, &stand_in, "codex"),
        Some(store(&scratch, "codex"))
    );
    let asked = |home: &str| {
        let line = pkg_which(&stand_in, home, "claude").expect("the stand-in runs");
        managed_claude(&line).unwrap_or_else(|| panic!("no managed line: {line}"))
    };
    assert_eq!(asked(p.store_home()), store(&account, "claude"));
    assert_eq!(
        managed_bin(
            "agy",
            &pkg_which(&stand_in, p.store_home(), "agy").expect("agy")
        ),
        Some(format!("{}/pkg/store/agy/1/bin/agy", account.display()))
    );
    assert_eq!(p.caller_home, scratch.display().to_string());
    assert_ne!(asked(&p.caller_home), store(&account, "claude"));
    assert_eq!(asked(&p.caller_home), store(&scratch, "claude"));
    let only = crate::paths::Paths::resolve_with(None, Some(scratch.clone().into()));
    assert_eq!(only.store_home(), scratch.display().to_string());
}

fn opens_logged(browser: &str, log: &str, which: &str, run: &Run) -> bool {
    let shim = run.run.join("shim/open").display().to_string();
    let opener = if browser.is_empty() { which } else { browser };
    log == format!("{}/open.log", run.w.display()) && opener == shim
}

fn login_shell_reads(vars: &[(String, String)], cwd: &Path) -> Vec<String> {
    let mut cmd = aterm_uds::spawnfd::command("/bin/zsh");
    cmd.env_clear()
        .args([
            "-l",
            "-c",
            r#"printf '%s\n' "${BROWSER-}" "${LAB_OPEN_LOG-}" "$(command -v open)""#,
        ])
        .current_dir(cwd)
        .stdin(Stdio::null());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let out = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + Duration::from_secs(60),
    )
    .expect("a login zsh ends within the hang detector");
    assert!(
        out.status.success(),
        "{:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_session_the_instance_spawns_opens_through_the_recorder() {
    let top = aterm_tempfile::Builder::new()
        .prefix("lab-browser-")
        .tempdir()
        .expect("a scratch dir");
    let root = fs::canonicalize(top.path()).expect("its physical path");
    let mut run = recorded("", "0");
    run.run = root.join("runs/k1");
    run.w = run.run.join("scratch");
    fs::create_dir_all(run.w.join("home")).expect("the scratch home");
    fs::create_dir_all(run.run.join("shim")).expect("the shim");
    std::os::unix::fs::symlink("/usr/bin/true", run.run.join("shim/open")).expect("its open");
    let vars = instance_vars(&run, "owner").expect("the instance's environment");
    let get = |k: &str| {
        vars.iter()
            .filter(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
    };
    let shim_open = run.run.join("shim/open").display().to_string();
    let log = format!("{}/open.log", run.w.display());
    assert_eq!(get("BROWSER"), [shim_open.as_str()]);
    assert_eq!(get("LAB_OPEN_LOG"), [log.as_str()]);
    assert!(get("PATH")[0].starts_with(&format!("{}/shim:", run.run.display())));
    assert!(lab_sets("BROWSER") && lab_sets("LAB_OPEN_LOG"));
    let read = login_shell_reads(&vars, &run.w);
    assert_eq!(read.len(), 3, "{read:?}");
    assert_eq!(read[0], shim_open, "BROWSER passes the login shell");
    assert_eq!(read[1], log, "LAB_OPEN_LOG passes the login shell");
    assert_eq!(read[2], "/usr/bin/open", "path_helper puts the shim last");
    assert!(opens_logged(&read[0], &read[1], &read[2], &run), "{read:?}");
    let bare: Vec<(String, String)> = vars
        .iter()
        .filter(|(k, _)| k != "BROWSER")
        .cloned()
        .collect();
    let read = login_shell_reads(&bare, &run.w);
    assert_eq!(read.first().map(String::as_str), Some(""), "{read:?}");
    assert!(
        !opens_logged(&read[0], &read[1], &read[2], &run),
        "{read:?}"
    );
}

#[test]
fn the_sleep_clock_reaches_the_instance_and_claude_code_of_a_lab_up_with_it_alone() {
    let mut run = recorded("TMPDIR=/tmp", "0");
    run.port = "5".into();
    run.claude_bin = "/s/runs/k1/clock/claude".into();
    let off = instance_vars(&run, "owner").unwrap();
    let script = launcher(&run, &parse_launch(&[]).unwrap());
    assert!(
        !off.iter().any(|(k, _)| crate::clock::is_clock_var(k)),
        "{off:?}"
    );
    assert!(
        !script.contains("DYLD_") && !script.contains("LABCLOCK_"),
        "{script}"
    );
    run.clock = "1".into();
    let on = instance_vars(&run, "owner").unwrap();
    let clock = crate::clock::vars(&run);
    assert_eq!(clock.len(), 4);
    for kv in &clock {
        assert!(on.contains(kv), "{kv:?} not in {on:?}");
    }
    let l = parse_launch(&["-e".to_string(), "K=v".to_string()]).unwrap();
    let script = launcher(&run, &l);
    let at = |needle: &str| {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("no {needle} in {script}"))
    };
    assert!(
        at("  DYLD_INSERT_LIBRARIES=/s/runs/k1/clock/libaterm_lab_clock.dylib \\\n")
            < at("  K=v \\\n")
    );
    at("  LABCLOCK_FILE=/s/runs/k1/clock/offset \\\n");
    at("  LABCLOCK_DIR=/s/runs/k1/scratch/clock \\\n");
    at("  LABCLOCK_CONTINUOUS=1 \\\n");
    assert!(script.ends_with("  /s/runs/k1/clock/claude\n"), "{script}");
    for bad in [
        "DYLD_INSERT_LIBRARIES=/x.dylib",
        "LABCLOCK_FILE=/x",
        "LABCLOCK_CONTINUOUS",
    ] {
        assert!(
            parse_launch(&["-e".to_string(), bad.to_string()]).is_err(),
            "claude -e {bad}"
        );
        assert!(instance_env(bad).is_err(), "LAB_INSTANCE_ENV={bad}");
    }
    assert!(lab_sets("DYLD_INSERT_LIBRARIES") && lab_sets("LABCLOCK_DIR"));
}

#[test]
fn the_relaunch_runs_the_instance_on_the_lab_s_own_socket_and_grid() {
    let run = recorded("", "0");
    let argv = instance_argv(&run, "/b/aterm", None, false);
    assert_eq!(
        argv,
        [
            "/b/aterm",
            "--headless",
            "--no-reroute",
            "--control-sock",
            "/tmp/at.AbCd/a.sock",
            "-d",
            "/tmp/at.AbCd/proj",
            "--columns",
            "144",
            "--lines",
            "50"
        ]
        .map(str::to_string)
    );
    let armed = instance_argv(&run, "/b/aterm", Some("2"), true);
    assert_eq!(armed[2..4], lifeline_flag()[..]);
    assert_eq!(
        armed[armed.len() - 2..],
        ["--scale", "2"].map(str::to_string)
    );
}

/// A `LAB_REROUTE=1` lab's instance line drops only `--no-reroute`; every other lab keeps it.
#[test]
fn a_reroute_lab_s_instance_drops_no_reroute_and_keeps_the_rest() {
    let plain = instance_argv(&recorded("", "0"), "/b/aterm", None, false);
    let run = Run {
        reroute: "1".into(),
        ..recorded("", "0")
    };
    let rerouting = instance_argv(&run, "/b/aterm", None, false);
    let mut without = plain.clone();
    without.retain(|a| a != "--no-reroute");
    assert_eq!(plain.len(), without.len() + 1, "{plain:?}");
    assert_eq!(rerouting, without);
    assert_eq!(
        opts_refused(&UpOpts {
            reroute: true,
            clock: true,
            ..UpOpts::default()
        })
        .map(|why| why.starts_with("LAB_REROUTE=1 takes no LAB_CLOCK=1")),
        Some(true)
    );
}

/// A lab whose `claude` is on its tabs' PATH (`LAB_REROUTE=1`).
fn rerouting(root: &Path) -> Run {
    Run {
        tag: "t".into(),
        run: root.join("runs/t"),
        w: root.join("runs/t/scratch"),
        port: "5".into(),
        proxy: "http://127.0.0.1:6".into(),
        claude_bin: "/a b/claude".into(),
        reroute: "1".into(),
        ..Run::default()
    }
}

/// The program word of a typed line: its first word that is no `NAME=value` assignment.
fn program_word(line: &str) -> String {
    shell_words(line)
        .into_iter()
        .find(|w| !w.split_once('=').is_some_and(|(k, _)| shell_name(k)))
        .unwrap_or_default()
}

/// A BY-NAME LAUNCH TYPES `claude` AS A PERSON DOES: the line typed at the prompt and the
/// launcher script hand `claude`, found on PATH, the same environment (every name the
/// launch sets, enumerated from its own parts) and the same arguments; the typed line's
/// program word is `claude` itself, past its assignment words. Whole-quoting an `-e` pair
/// (`'K=v 1'`) would make it the command (zsh: command not found), and the typed line never
/// does; a by-path launch still types `sh <launcher>`.
#[test]
fn a_by_name_launch_types_claude_as_a_person_does() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let run = rerouting(&root);
    let l = parse_launch(&["--by-name", "-e", "K=v 1", "--", "--model", "x y"].map(String::from))
        .unwrap();
    let names = launch_words(&run, &l).names();
    for want in ["PATH", "CLAUDE_CONFIG_DIR", "BROWSER", "LAB_OPEN_LOG", "K"] {
        assert!(names.iter().any(|n| n == want), "{want}: {names:?}");
    }
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut probe = String::from("#!/bin/sh\n");
    for n in &names {
        probe.push_str(&format!("printf '%s=%s\\n' {n} \"${{{n}-unset}}\"\n"));
    }
    probe.push_str("for a in \"$@\"; do printf 'arg=%s\\n' \"$a\"; done\n");
    std::fs::write(bin.join("claude"), probe).unwrap();
    std::fs::set_permissions(bin.join("claude"), fs::Permissions::from_mode(0o755)).unwrap();
    let script = root.join("claude.sh");
    std::fs::write(&script, launcher(&run, &l)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let ran = |cmd: &mut aterm_uds::spawnfd::Command| {
        let out = cmd
            .env_clear()
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let typed = typed_line(&run, &l);
    assert_eq!(launch_line(&run, &l, &script), typed);
    let (by_line, _) =
        ran(aterm_uds::spawnfd::command("/bin/zsh").args(["-f", "-c", typed.as_str()]));
    let (by_script, _) = ran(aterm_uds::spawnfd::command("/bin/sh").arg(&script));
    assert_eq!(by_line, by_script, "{typed}");
    for want in [
        "K=v 1\n".to_string(),
        "arg=x y\n".to_string(),
        format!("PATH={}:{path}\n", run.run.join("shim").display()),
        format!("CLAUDE_CONFIG_DIR={}/home/.claude\n", run.w.display()),
    ] {
        assert!(by_line.contains(&want), "{want}: {by_line}");
    }
    assert_eq!(by_line.lines().count(), names.len() + 2, "{by_line}");
    assert_eq!(program_word(&typed), "claude", "{typed}");
    assert!(
        typed.contains(" K='v 1' ") && !typed.contains("'K=v 1'"),
        "{typed}"
    );

    let whole = typed.replace("K='v 1'", "'K=v 1'");
    let (_, said) = ran(aterm_uds::spawnfd::command("/bin/zsh").args(["-f", "-c", whole.as_str()]));
    assert!(said.contains("command not found: K=v 1"), "{said}");
    let by_path = Launch {
        by_name: false,
        ..parse_launch(&["-e", "K=v 1"].map(String::from)).unwrap()
    };
    assert_eq!(
        launch_line(&run, &by_path, &script),
        format!("sh {}", shell_quote(&script.to_string_lossy()))
    );

    // One that replaces the tab's shell: `exec` past the assignment words, where the shell
    // still assigns them and finds `claude` on the assigned PATH; by path, `exec sh`.
    let held = Launch {
        replace_shell: true,
        ..parse_launch(&["--by-name", "-e", "K=v 1", "--", "--model", "x y"].map(String::from))
            .unwrap()
    };
    let exec_line = launch_line(&run, &held, &script);
    assert_eq!(program_word(&exec_line), "exec", "{exec_line}");
    assert_eq!(
        exec_line,
        typed.replacen(" K='v 1' claude ", " K='v 1' exec claude ", 1),
        "{typed}"
    );
    let (by_exec, _) =
        ran(aterm_uds::spawnfd::command("/bin/zsh").args(["-f", "-c", exec_line.as_str()]));
    assert_eq!(by_exec, by_script, "{exec_line}");
    let held_path = Launch {
        by_name: false,
        ..held
    };
    assert_eq!(
        launch_line(&run, &held_path, &script),
        format!("exec sh {}", shell_quote(&script.to_string_lossy()))
    );
}

/// `--by-name` is refused (exit 2) before anything is typed in a lab without
/// `LAB_REROUTE=1`, naming it, and for an `-e` no shell assigns; a reroute lab's by-name
/// launch and every by-path launch pass.
#[test]
fn by_name_is_refused_outside_a_reroute_lab_and_for_a_name_no_shell_assigns() {
    let root = Path::new("/s");
    let named = |envs: &[&str]| Launch {
        by_name: true,
        envs: envs.iter().map(|e| (*e).to_string()).collect(),
        ..Launch::default()
    };
    let plain = Run {
        reroute: String::new(),
        ..rerouting(root)
    };
    let why = by_name_refused(&plain, &named(&[])).expect("refused");
    assert!(why.contains("LAB_REROUTE=1"), "{why}");
    let f = launch_claude(&plain, &named(&[])).expect_err("refused");
    assert_eq!((f.code, f.msg.as_deref()), (2, Some(why.as_str())));
    assert_eq!(by_name_refused(&rerouting(root), &named(&["K=v 1"])), None);
    for bad in ["1X=v", "K", "K-1=v", "=v"] {
        let why = by_name_refused(&rerouting(root), &named(&[bad])).expect(bad);
        assert!(why.contains(&format!("-e {bad} ")), "{why}");
    }
    assert_eq!(by_name_refused(&plain, &Launch::default()), None);
    assert_eq!(
        by_name_refused(
            &rerouting(root),
            &Launch {
                envs: vec!["1X=v".into()],
                ..Launch::default()
            }
        ),
        None
    );
}

#[test]
fn a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline() {
    let tail = [
        "--no-reroute",
        "--control-sock",
        "/tmp/at.AbCd/a.sock",
        "-d",
        "/tmp/at.AbCd/proj",
        "--columns",
        "144",
        "--lines",
        "50",
    ]
    .map(str::to_string);
    let run = Run {
        window: "1".into(),
        ..recorded("", "0")
    };
    assert!(windowed(&run));
    let bare = instance_argv(&run, "/b/aterm", None, false);
    assert_eq!(bare[0], "/b/aterm");
    assert_eq!(bare[1..], tail[..]);
    let kept = instance_argv(&run, "/b/aterm", None, true);
    assert_eq!(
        kept[..4],
        ["/s/runs/k1/bin/aterm-lab", "keeper", "--", "/b/aterm"].map(str::to_string)
    );
    assert_eq!(kept[4..], tail[..]);
    for line in [&bare, &kept] {
        assert!(
            !line
                .iter()
                .any(|a| a == "--headless" || a.starts_with("--lifeline")),
            "{line:?}"
        );
    }
    for headless in ["0", ""] {
        let run = Run {
            window: headless.into(),
            ..recorded("", "0")
        };
        assert!(!windowed(&run));
        let line = instance_argv(&run, "/b/aterm", None, true);
        assert_eq!(line[..2], ["/b/aterm", "--headless"].map(str::to_string));
        assert_eq!(line[2..4], lifeline_flag()[..]);
    }
}

#[test]
fn a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made() {
    let o = |window, restore| UpOpts {
        window,
        restore,
        ..UpOpts::default()
    };
    let why = opts_refused(&o(true, true)).expect("refused");
    assert!(why.contains("LAB_RESTORE=1"), "{why}");
    let f = up("not a tag!", &o(true, true)).expect_err("refused");
    assert_eq!(f.code, 2, "{f:?}");
    assert_eq!(f.msg.as_deref(), Some(why));
    for (window, restore) in [(true, false), (false, true), (false, false)] {
        assert_eq!(
            opts_refused(&o(window, restore)),
            None,
            "window {window} restore {restore}"
        );
    }
}

#[test]
fn a_starting_child_is_waited_for_while_it_is_active() {
    let t0 = Instant::now();
    let s = |n: u64| t0 + Duration::from_secs(n);
    let looks = std::cell::Cell::new(0u64);
    let clock = std::cell::Cell::new(0u64);
    let tick = || {
        clock.set(clock.get() + 1);
        Ok(s(clock.get()))
    };
    let answered = while_starting(
        || {
            looks.set(looks.get() + 1);
            Ok(looks.get() == 3)
        },
        || false,
        READY_HANG,
        t0,
        tick,
    );
    assert_eq!(answered, Ok(()));
    assert_eq!(looks.get(), 3);
    let e = while_starting(|| Err("ended".into()), || false, READY_HANG, t0, || Ok(t0))
        .expect_err("ended");
    assert_eq!(e, "ended");
    // Starved: ready at the 300th second, active once every 30 s.
    clock.set(0);
    let started = while_starting(
        || Ok(clock.get() >= 300),
        || clock.get().is_multiple_of(30),
        READY_HANG,
        t0,
        tick,
    );
    assert_eq!(started, Ok(()), "a starved start is no hang");
    // The negative control: the same start idle from the 100th second is a hang at the
    // 160th.
    clock.set(0);
    let e = while_starting(
        || Ok(clock.get() >= 300),
        || clock.get() <= 100,
        READY_HANG,
        t0,
        tick,
    )
    .expect_err("hang");
    assert!(e.contains("was idle"), "{e}");
    assert_eq!(
        clock.get(),
        160,
        "the hang is named a minute after the last move"
    );
    let e = while_starting(
        || Ok(false),
        || false,
        READY_HANG,
        t0,
        || Err("interrupted".to_string()),
    )
    .expect_err("interrupted");
    assert_eq!(e, "interrupted");
}

#[test]
fn a_pid_is_the_lab_s_only_while_its_command_line_names_the_lab() {
    let me = i32::try_from(std::process::id()).unwrap();
    let elsewhere = recorded("", "0");
    for pid in [0, 1, -1, me] {
        assert!(!names_this_lab(&elsewhere, pid), "{pid}");
    }
    let line = procs::command_line(me);
    let program = line.split_whitespace().next().unwrap_or_default();
    let dir = Path::new(program).parent().unwrap_or(Path::new(""));
    assert!(!dir.as_os_str().is_empty(), "{line}");
    let ours = Run {
        run: dir.to_path_buf(),
        sockdir: String::new(),
        ..elsewhere
    };
    assert!(names_this_lab(&ours, me), "{line}");
}

#[test]
fn a_saved_pid_is_signalled_only_while_it_is_the_process_that_started_then() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .expect("a child");
    let pid = i32::try_from(child.id()).expect("a pid");
    let start = procs::start_time(pid);
    assert!(!start.is_empty(), "the child's start is read");
    assert_eq!(still_started(pid, &start), pid, "still that process");
    assert_eq!(
        still_started(pid, "Thu Jan  1 00:00:00 1970"),
        0,
        "another start"
    );
    assert_eq!(still_started(pid, ""), 0, "a start never read");
    for none in [0, 1, -1] {
        assert_eq!(still_started(none, &start), 0, "{none}");
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        still_started(pid, &start),
        0,
        "gone: its number is no one's of then"
    );
}

fn half_made(root: &Path) -> Run {
    let root = fs::canonicalize(root).unwrap();
    let run = Run {
        tag: "abort-t".into(),
        run: root.join("runs/abort-t"),
        w: root.join("runs/abort-t/scratch"),
        ..Run::default()
    };
    fs::create_dir_all(&run.w).unwrap();
    fs::create_dir_all(run.run.join("bin")).unwrap();
    fs::write(run.run.join(BIN), "a copy").unwrap();
    run
}

#[test]
fn an_aborted_up_keeps_a_copy_nothing_showed_ran() {
    let root = aterm_tempfile::tempdir().unwrap();
    let none = i32::MAX;
    for (what, sampler, stub, ran, kept) in [
        (
            "the witness started, the stub never ready",
            none,
            0,
            false,
            true,
        ),
        ("both started, no ready byte", none, none, false, true),
        ("the stub said it ran", none, none, true, false),
        ("nothing started from the copy", 0, 0, false, false),
    ] {
        let mut run = half_made(root.path());
        run.sampler_pid = sampler;
        run.stub_pid = stub;
        if ran {
            fs::write(run.run.join(BIN_RAN), "").unwrap();
        }
        let failed = abort(&run, &[], "x");
        assert_eq!(failed.msg.as_deref(), Some("x"), "{what}");
        assert_eq!(run.run.join(BIN).is_file(), kept, "{what}");
        let stamp = aterm_uds::deadline::exec_stamp(&run.run.join("bin"));
        assert_eq!(
            stamp.is_some(),
            kept,
            "{what}: its wait runs from the abort"
        );
        let _ = fs::remove_dir_all(root.path().join("runs"));
    }
}

#[test]
fn a_scratch_hold_keeps_an_aborted_up_before_its_first_lab_child() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = fs::canonicalize(root.path()).unwrap();
    for (what, released, unread, kept) in [
        ("reroute probe failed", false, false, true),
        ("reroute probe showed it ran", true, false, false),
        ("hold inventory unread", false, true, true),
    ] {
        let run = half_made(root.path());
        let clone = run.w.join("home/clone");
        fs::create_dir_all(clone.parent().unwrap()).unwrap();
        fs::write(&clone, "data, not an executable").unwrap();
        hold_scratch(&run.run, "reroute").unwrap();
        if released {
            release_scratch(&run.run, "reroute");
        }
        if unread {
            fs::remove_dir_all(run.run.join(HELD)).unwrap();
            fs::write(run.run.join(HELD), "not a directory").unwrap();
        }
        let hold = run.run.join(HELD).join("reroute");
        if !released && !unread {
            aterm_uds::deadline::stamp_exec(&hold);
        }
        let at_exec = aterm_uds::deadline::exec_stamp(&hold);
        assert_eq!((run.sampler_pid, run.stub_pid, run.instance_pid), (0, 0, 0));
        let failed = abort(&run, &[], "probe failed");
        assert_eq!(failed.msg.as_deref(), Some("probe failed"), "{what}");
        assert_eq!(clone.is_file(), kept, "{what}");
        if kept {
            assert_eq!(all_runs(&state), ["abort-t"], "{what}: ps lists it");
            assert!(run.run.join("env.sh").is_file(), "{what}: down can load it");
            if !unread {
                assert_eq!(aterm_uds::deadline::exec_stamp(&hold), at_exec, "{what}");
            }
            // A saved lab must be taken down before its tag can come up again.
            // The fake program is data, so isolate the clone's scratch hold.
            fs::write(run.run.join(BIN_RAN), "").unwrap();
            let now = std::time::SystemTime::now();
            assert!(room_for(&run, now).is_err(), "{what}: down is required");
            let unstamped = !stamp_holds(&run.run);
            clear_run_dir(&run.run, false, unstamped, &run.tag, past_the_hang(now));
            assert!(
                clone.is_file(),
                "{what}: wall time cannot spend an unread hold"
            );
            assert!(
                run.run.join("env.sh").is_file(),
                "{what}: down keeps its record"
            );
            if unread {
                fs::remove_file(run.run.join(HELD)).unwrap();
            } else {
                let hold = run.run.join(HELD).join("reroute");
                assert!(aterm_uds::deadline::exec_stamp(&hold).is_some(), "{what}");
                // A deterministic restart observation spends the awake-clock stamp.
                fs::write(
                    hold.join(aterm_uds::deadline::EXEC_STAMP),
                    u64::MAX.to_string(),
                )
                .unwrap();
            }
            let unstamped = !stamp_holds(&run.run);
            clear_run_dir(&run.run, false, unstamped, &run.tag, now);
            assert!(!clone.exists(), "{what}: the later down may reclaim it");
            assert!(
                !run.run.join("env.sh").exists(),
                "{what}: down removed its record"
            );
            assert!(room_for(&run, now).is_ok(), "{what}: custody is resolved");
        }
        let _ = fs::remove_dir_all(root.path().join("runs"));
    }
}

/// A home image (`LAB_HOME_IMAGE`) holds the scratch's `home/`, so it comes down only with the
/// scratch: an aborted up whose scratch a hold keeps leaves it attached, as does a down before
/// the hold is spent, and the down that removes the scratch detaches it and removes its file.
/// Before this `abort` and `down` detached it whatever was held, taking a held copy's
/// directory away while macOS might still be assessing its first exec.
#[test]
fn a_home_image_comes_down_only_with_its_scratch() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut run = half_made(root.path());
    // The lab program's copy showed it ran, so only the scratch's hold keeps the run.
    fs::write(run.run.join(BIN_RAN), "").unwrap();
    let image = home_image_file(&run.run);
    fs::write(&image, "a stand-in for the image's file").unwrap();
    run.home_image = image.to_string_lossy().into_owned();
    run.save();
    let clone = run.w.join("home/clone");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    fs::write(&clone, "data, not an executable").unwrap();
    hold_scratch(&run.run, "reroute").unwrap();
    let hold = run.run.join(HELD).join("reroute");
    aterm_uds::deadline::stamp_exec(&hold);
    let failed = abort(&run, &[], "probe failed");
    assert_eq!(failed.msg.as_deref(), Some("probe failed"));
    assert!(clone.is_file(), "an aborted up keeps a held scratch");
    assert!(image.is_file(), "and its home image");
    let now = std::time::SystemTime::now();
    let unstamped = !stamp_holds(&run.run);
    clear_run_dir(&run.run, false, unstamped, &run.tag, now);
    assert!(
        clone.is_file(),
        "a down before the hold is spent keeps the scratch"
    );
    assert!(image.is_file(), "and its home image");
    assert!(run.run.join("env.sh").is_file(), "and its record");
    // A deterministic restart observation spends the awake-clock stamp.
    fs::write(
        hold.join(aterm_uds::deadline::EXEC_STAMP),
        u64::MAX.to_string(),
    )
    .unwrap();
    let unstamped = !stamp_holds(&run.run);
    clear_run_dir(&run.run, false, unstamped, &run.tag, now);
    assert!(!clone.exists(), "the down that removes the scratch");
    assert!(
        !image.exists(),
        "detaches its home image and removes its file"
    );
    assert!(room_for(&run, now).is_ok(), "custody is resolved");
}

#[test]
fn copies_go_once_shown_to_run_or_past_the_first_exec_hang() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("bin");
    fs::create_dir_all(&dir).unwrap();
    let made = fs::metadata(&dir).unwrap().modified().unwrap();
    let hang = aterm_uds::deadline::FIRST_EXEC_HANG;
    let minute = Duration::from_secs(60);
    assert!(!copies_may_go(&dir, false, made), "just made, unproven");
    assert!(!copies_may_go(&dir, false, made + hang - minute));
    assert!(copies_may_go(&dir, true, made), "shown to run");
    assert!(copies_may_go(&dir, false, made + hang), "past the hang");
    assert!(copies_may_go(&root.path().join("gone"), false, made));
}

fn torn_down(root: &Path, bin_ran: bool, relay: bool) -> (Run, std::time::SystemTime) {
    let run = half_made(root);
    fs::remove_dir_all(&run.w).unwrap();
    for f in ["env.sh", "jail.sb", "stub.out"] {
        fs::write(run.run.join(f), "").unwrap();
    }
    if bin_ran {
        fs::write(run.run.join(BIN_RAN), "").unwrap();
    }
    let made = fs::metadata(run.run.join("bin")).unwrap().modified();
    if relay {
        fs::create_dir_all(run.run.join("relay")).unwrap();
        fs::write(run.run.join("relay/node"), "a copy").unwrap();
    }
    (run, made.unwrap())
}

fn past_the_hang(made: std::time::SystemTime) -> std::time::SystemTime {
    made + aterm_uds::deadline::FIRST_EXEC_HANG + Duration::from_secs(60)
}

#[test]
fn a_down_that_keeps_a_copy_keeps_its_lab_for_a_later_down() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, bin_ran, relay, relay_ran, kept) in [
        ("stub's copy unproven", false, false, false, true),
        ("relay's copy unproven", true, true, false, true),
        ("both copies ran", true, true, true, false),
        ("stub's copy ran, no relay", true, false, false, false),
    ] {
        let (run, made) = torn_down(root.path(), bin_ran, relay);
        let state = run.run.parent().and_then(Path::parent).unwrap();
        clear_run_dir(&run.run, relay_ran, false, &run.tag, made);
        assert!(!run.run.join("jail.sb").exists(), "{what}");
        assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
        let listed = if kept { vec![run.tag.clone()] } else { vec![] };
        assert_eq!(all_runs(state), listed, "{what}");
        if kept {
            clear_run_dir(&run.run, relay_ran, false, &run.tag, past_the_hang(made));
        }
        assert!(!run.run.exists(), "{what}: the later down finished");
    }
}

/// The tabs' launch records (`launch/<sid>.sh`), which `down` keeps in its logs first, go
/// with the run directory: a lab that launched Claude Code leaves nothing of its run behind.
#[test]
fn a_down_removes_the_tabs_launch_records_with_the_run() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (run, made) = torn_down(root.path(), true, false);
    let record = run.run.join(LAUNCHES).join("s-c99d4225e7307a6cc1f4.sh");
    fs::create_dir_all(record.parent().unwrap()).unwrap();
    fs::write(
        &record,
        "#!/bin/sh\nexec env -u CLAUDE_CONFIG_DIR /c/claude\n",
    )
    .unwrap();
    clear_run_dir(&run.run, false, false, &run.tag, made);
    assert!(!run.run.exists(), "the run directory went whole");
}

/// An `up --fabric` lab's bus keys ([`bus_keys`]: the link to the aterm under test, the mint
/// secret and the caps) go with its run directory, the link removed and never followed. Keys
/// an earlier lab of the tag left with no `env.sh` naming it go at the next `up`
/// ([`room_for`]), so that `up --fabric` can lay its bus again; a lab that is up keeps its own.
#[test]
fn a_down_takes_the_bus_keys_and_an_up_the_keys_a_lab_left() {
    let root = aterm_tempfile::tempdir().unwrap();
    let target = root.path().join("aterm-under-test");
    fs::write(&target, "a program").unwrap();
    let lay_keys = |run: &Run| {
        let keys = bus_keys(run);
        fs::create_dir_all(&keys).unwrap();
        std::os::unix::fs::symlink(&target, keys.join("aterm")).unwrap();
        for f in ["mint.secret", "node.cap", "observer.cap"] {
            fs::write(keys.join(f), "QZP6WORD1").unwrap();
        }
        keys
    };
    let (run, made) = torn_down(root.path(), true, false);
    lay_keys(&run);
    clear_run_dir(&run.run, false, false, &run.tag, made);
    assert!(!run.run.exists(), "the run directory went whole");
    assert!(target.is_file(), "the link was not followed");
    let (left, made) = torn_down(root.path(), true, false);
    fs::remove_file(left.run.join("env.sh")).unwrap();
    let keys = lay_keys(&left);
    assert!(room_for(&left, made).is_ok(), "no lab of the tag is up");
    assert!(
        fs::symlink_metadata(&keys).is_err(),
        "the keys a lab left go at the next up"
    );
    assert!(target.is_file(), "the link was not followed");
    fs::remove_dir_all(root.path().join("runs")).unwrap();
    let (up, made) = torn_down(root.path(), true, false);
    let keys = lay_keys(&up);
    assert!(room_for(&up, made).is_err(), "up already");
    assert!(
        keys.join("mint.secret").is_file(),
        "a lab that is up keeps its keys"
    );
}

/// `ps`'s broker line: a live one names its socket; a gone one names the way back, `down` and
/// then the `up` of the tag with every flag its lab came up with.
#[test]
fn a_gone_broker_names_the_up_its_lab_came_up_with() {
    let mut r = Run {
        tag: "e7x".into(),
        fabric: "/tmp/at.x/fabric".into(),
        broker_pid: 42,
        ..Run::default()
    };
    assert_eq!(
        broker_line(&r, true),
        "fabric broker 42 live (in the jail; its socket /tmp/at.x/fabric/b.sock)"
    );
    let gone = broker_line(&r, false);
    assert!(gone.starts_with("fabric broker 42 gone: "), "{gone}");
    assert!(
        gone.ends_with(&format!("({LAB} down e7x, then {LAB} up e7x --fabric)")),
        "{gone}"
    );
    r.window = "1".into();
    r.tab_strip = "1".into();
    let gone = broker_line(&r, false);
    assert!(
        gone.ends_with(&format!(
            "({LAB} down e7x, then {LAB} up e7x --window --fabric --tab-strip)"
        )),
        "{gone}"
    );
}

/// A clone held in the scratch that never showed it ran keeps the scratch and `env.sh` past any
/// wall time, until a restart since its stamp; one shown to run released its hold.
#[test]
fn a_down_keeps_a_scratch_a_clone_that_never_showed_it_ran_holds() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, proven, kept) in [
        ("never showed it ran", false, true),
        ("showed it ran", true, false),
    ] {
        let (run, made) = torn_down(root.path(), true, false);
        fs::create_dir_all(&run.w).unwrap();
        hold_scratch(&run.run, "agy-1").unwrap();
        if proven {
            release_scratch(&run.run, "agy-1");
        }
        assert!(stamp_holds(&run.run), "{what}");
        clear_run_dir(&run.run, false, false, &run.tag, past_the_hang(made));
        assert_eq!(run.w.is_dir(), kept, "{what}");
        assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
        if kept {
            let stamp = run.run.join(HELD).join("agy-1/exec");
            // above any reading the awake clock gives now: a restart since the stamp
            let restart = u64::MAX.to_string();
            fs::write(&stamp, &restart).unwrap();
            assert!(stamp_holds(&run.run), "{what}");
            assert_eq!(fs::read_to_string(&stamp).unwrap(), restart, "{what}");
            clear_run_dir(&run.run, false, false, &run.tag, made);
        }
        assert!(!run.run.exists(), "{what}: the scratch and its holds went");
    }
}

/// A clock clone with neither mark (started, and no version line at `up`) is stamped and kept
/// past any wall time; with its version line, its never-started mark, or no clone, it goes at
/// once, unstamped.
#[test]
fn the_clocks_clone_stays_until_it_showed_it_ran() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, clone, mark, kept) in [
        ("its clone printed nothing", true, None, true),
        ("it printed its version", true, Some(CLOCK_RAN), false),
        ("it was never started", true, Some(CLOCK_UNSTARTED), false),
        ("no clone", false, None, false),
    ] {
        let (run, made) = torn_down(root.path(), true, false);
        fs::create_dir_all(run.run.join("clock")).unwrap();
        if clone {
            fs::write(run.run.join(CLOCK_CLONE), "a clone").unwrap();
        }
        if let Some(mark) = mark {
            fs::write(run.run.join(mark), "").unwrap();
        }
        assert!(stamp_holds(&run.run), "{what}");
        let stamp = run.run.join("clock").join(aterm_uds::deadline::EXEC_STAMP);
        assert_eq!(stamp.is_file(), kept, "{what}");
        let now = if kept { past_the_hang(made) } else { made };
        clear_run_dir(&run.run, false, false, &run.tag, now);
        assert_eq!(run.run.join(CLOCK_CLONE).is_file(), kept, "{what}");
        assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
        let _ = fs::remove_dir_all(root.path().join("runs"));
    }
}

/// An unproven relay, lab-program copy or twin probe stamped at `down` waits on the awake clock
/// from that stamp, neither its age nor a sleep freeing it; unstamped, its age is its mtime's.
#[test]
fn an_unproven_copy_ages_on_the_awake_clock_from_its_down() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, copy, stamped, kept) in [
        ("a relay, stamped by its down", "relay/node", true, true),
        ("a relay, never stamped", "relay/node", false, false),
        ("the lab's copy, stamped by its down", BIN, true, true),
        ("the lab's copy, never stamped", BIN, false, false),
        (
            "a twin probe, stamped by its down",
            "twin-src/claude",
            true,
            true,
        ),
        (
            "a twin probe, never stamped",
            "twin-src/claude",
            false,
            false,
        ),
    ] {
        let (run, made) = torn_down(root.path(), copy != BIN, copy == "relay/node");
        let copy = run.run.join(copy);
        let dir = copy.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        fs::write(&copy, "a copy").unwrap();
        if stamped {
            assert!(stamp_holds(&run.run), "{what}");
        }
        clear_run_dir(&run.run, false, false, &run.tag, past_the_hang(made));
        assert_eq!(copy.is_file(), kept, "{what}");
        assert_eq!(run.run.join("env.sh").is_file(), kept, "{what}");
        if kept {
            let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
            let restart = u64::MAX.to_string();
            fs::write(&stamp, &restart).unwrap();
            assert!(stamp_holds(&run.run), "{what}");
            assert_eq!(fs::read_to_string(&stamp).unwrap(), restart, "{what}");
            clear_run_dir(&run.run, false, false, &run.tag, made);
        }
        assert!(!run.run.exists(), "{what}: the copy went");
    }
}

/// A hold or unproven copy whose stamp could not be written (a full disk) keeps its directory
/// and `env.sh` past any wall time, since its mtime can predate its exec; a later `down` that
/// can stamp it starts its wait. By its mtime alone each would count as spent.
#[test]
fn a_down_that_cannot_stamp_a_hold_keeps_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, held, kept) in [
        ("a scratch hold", "held/agy-1", "scratch"),
        ("the clock's clone", "clock", "clock"),
        ("a twin probe", "twin-src", "twin-src"),
        ("the relay", "relay", "relay"),
        ("the lab's copy", "bin", "bin"),
    ] {
        let (run, made) = torn_down(root.path(), held != "bin", false);
        fs::create_dir_all(&run.w).unwrap();
        let dir = run.run.join(held);
        fs::create_dir_all(&dir).unwrap();
        if held != "held/agy-1" {
            fs::write(dir.join("claude"), "a clone").unwrap();
        }
        let mode = |m| fs::set_permissions(&dir, fs::Permissions::from_mode(m)).unwrap();
        mode(0o500);
        let landed = stamp_holds(&run.run);
        mode(0o700);
        assert!(!landed, "{what}: the stamp was refused");
        let past = past_the_hang(made);
        assert!(copies_may_go(&dir, false, past), "{what}: control");
        clear_run_dir(&run.run, false, true, &run.tag, past);
        let kept = run.run.join(kept);
        assert!(kept.is_dir(), "{what}: kept past any wall time");
        assert!(run.run.join("env.sh").is_file(), "{what}");
        assert!(stamp_holds(&run.run), "{what}: a later down stamps it");
        clear_run_dir(&run.run, false, false, &run.tag, past);
        assert!(kept.is_dir(), "{what}: its wait runs from that stamp");
        let restart = u64::MAX.to_string();
        fs::write(dir.join(aterm_uds::deadline::EXEC_STAMP), restart).unwrap();
        clear_run_dir(&run.run, false, false, &run.tag, made);
        assert!(!run.run.exists(), "{what}: the later down finished");
    }
}

/// A blank stamp (a write cut short) takes the awake clock's reading at the next `down`, so it
/// stays past any wall time and goes on a restart since; one `down` cannot write again counts
/// as unstamped. By its mtime alone it would count as spent.
#[test]
fn a_down_writes_a_blank_stamp_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    for (what, held, kept) in [
        ("a scratch hold", "held/agy-1", "scratch"),
        ("the clock's clone", "clock", "clock"),
        ("a twin probe", "twin-src", "twin-src"),
        ("the relay", "relay", "relay"),
        ("the lab's copy", "bin", "bin"),
    ] {
        let (run, made) = torn_down(root.path(), held != "bin", false);
        fs::create_dir_all(&run.w).unwrap();
        let dir = run.run.join(held);
        fs::create_dir_all(&dir).unwrap();
        if held != "held/agy-1" {
            fs::write(dir.join("claude"), "a clone").unwrap();
        }
        let stamp = dir.join(aterm_uds::deadline::EXEC_STAMP);
        fs::write(&stamp, "").unwrap();
        let past = past_the_hang(made);
        assert!(copies_may_go(&dir, false, past), "{what}: control");
        let mode = |m| fs::set_permissions(&stamp, fs::Permissions::from_mode(m)).unwrap();
        mode(0o400);
        let landed = stamp_holds(&run.run);
        mode(0o600);
        assert!(
            !landed,
            "{what}: the blank stamp could not be written again"
        );
        assert!(stamp_holds(&run.run), "{what}");
        assert!(
            aterm_uds::deadline::exec_stamp(&dir).is_some(),
            "{what}: written again"
        );
        clear_run_dir(&run.run, false, false, &run.tag, past);
        assert!(
            run.run.join(kept).is_dir(),
            "{what}: kept past any wall time"
        );
        assert!(run.run.join("env.sh").is_file(), "{what}");
        // a restart since the stamp
        fs::write(&stamp, u64::MAX.to_string()).unwrap();
        clear_run_dir(&run.run, false, false, &run.tag, made);
        assert!(!run.run.exists(), "{what}: the later down finished");
    }
}

#[test]
fn an_up_waits_out_a_copy_an_earlier_lab_left_unproven() {
    let root = aterm_tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let gone = |run: &Run| !run.run.join("bin").exists() && !run.run.join("relay").exists();
    let (up, made) = torn_down(root.path(), false, false);
    assert!(room_for(&up, past_the_hang(made)).is_err(), "up already");
    assert!(up.run.join(BIN).is_file(), "an up lab's copy stays");
    fs::remove_dir_all(&runs).unwrap();
    for (what, bin_ran, relay_ran, unproven) in [
        ("stub's copy unproven", false, true, BIN),
        ("relay's copy unproven", true, false, "relay/node"),
    ] {
        let (left, made) = torn_down(root.path(), bin_ran, true);
        fs::remove_file(left.run.join("env.sh")).unwrap();
        if relay_ran {
            fs::write(left.run.join(RELAY_RAN), "").unwrap();
        }
        assert!(room_for(&left, made).is_err(), "{what}: the tag waits");
        assert!(left.run.join(unproven).is_file(), "{what}: in place");
        let dir = left.run.join(unproven).parent().unwrap().to_path_buf();
        assert!(
            room_for(&left, past_the_hang(made)).is_err(),
            "{what}: its wait runs on the awake clock from the up that stamped it"
        );
        // a restart since the stamp
        fs::write(
            dir.join(aterm_uds::deadline::EXEC_STAMP),
            u64::MAX.to_string(),
        )
        .unwrap();
        assert!(room_for(&left, made).is_ok(), "{what}");
        assert!(gone(&left), "{what}: the wait spent, the copies went");
        fs::remove_dir_all(&runs).unwrap();
    }
    let (proven, made) = torn_down(root.path(), true, true);
    fs::remove_file(proven.run.join("env.sh")).unwrap();
    fs::write(proven.run.join(RELAY_RAN), "").unwrap();
    assert!(room_for(&proven, made).is_ok(), "both ran: room at once");
    assert!(gone(&proven));
}

#[test]
fn the_relay_is_laid_once_and_its_run_recorded() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let run = half_made(root.path());
    let node = run.run.join("relay/node");
    lay_relay(&run).unwrap();
    let first = fs::metadata(&node).unwrap().ino();
    assert!(!relay_shown_running(&run), "nothing ran yet");
    lay_relay(&run).unwrap();
    assert_eq!(fs::metadata(&node).unwrap().ino(), first, "kept");
    assert!(!run.run.join(RELAY_RAN).exists(), "no log, no record");
    fs::write(run.w.join("relay.log"), "").unwrap();
    assert!(relay_shown_running(&run));
    lay_relay(&run).unwrap();
    assert!(run.run.join(RELAY_RAN).exists(), "recorded");
    assert!(!run.w.join("relay.log").exists(), "its log went");
    assert_eq!(fs::metadata(&node).unwrap().ino(), first, "still kept");
    assert!(relay_shown_running(&run), "the record stands");
}
