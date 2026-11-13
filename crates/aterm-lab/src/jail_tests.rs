// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A run, a socket dir, a state dir and a user temp dir under one scratch
/// tree, removed when it drops.
struct Dirs {
    top: crate::TestScratch,
    run: String,
    sock: String,
    state: String,
    ut: String,
}

fn dirs() -> Dirs {
    let top = crate::TestScratch::new("jail");
    let (run, sock, state, ut) = (
        top.join("run"),
        top.join("sock"),
        top.join("state"),
        top.join("T"),
    );
    for d in [run.join("scratch"), sock.clone(), state.clone(), ut.clone()] {
        std::fs::create_dir_all(d).unwrap();
    }
    let s = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    Dirs {
        run: s(run),
        sock: s(sock),
        state: s(state),
        ut: s(ut),
        top,
    }
}

fn spec(d: &Dirs, claude: &str, codex: &str, home: &str) -> Spec {
    Spec {
        run: d.run.clone(),
        claude: claude.into(),
        codex: codex.into(),
        agy: String::new(),
        muse: String::new(),
        grok: String::new(),
        kimi: String::new(),
        sockdir: d.sock.clone(),
        home: home.into(),
        caller_home: String::new(),
        ccsocks: "/private/tmp/cc-socks".into(),
        state: d.state.clone(),
        states: String::new(),
        user_tmp: d.ut.clone(),
        mark: "lab.jail.t.0123456789abcdef".into(),
        lab_dirs: vec!["/L/bin".into(), "/L/src".into()],
        aterm: "/A/aterm".into(),
        repo_target: "/R/target".into(),
    }
}

#[test]
fn every_protected_entry_and_mach_service_becomes_one_rule() {
    let d = dirs();
    let p = render(&spec(&d, "/opt/claude/bin/claude", "", "/Users//o")).unwrap();
    let rules = p
        .lines()
        .filter(|l| l.starts_with("(deny file-read* file-write* ("))
        .count();
    assert_eq!(
        rules,
        protected_paths("/Users//o", "/private/tmp/cc-socks").len()
    );
    assert_eq!(
        p.lines()
            .filter(|l| l.starts_with("(deny mach-lookup"))
            .count(),
        MACH_DENY.len() + MACH_DENY_OPEN.len() + 2
    );
    assert!(p.contains("(deny mach-lookup (global-name \"lab.jail.t.0123456789abcdef\"))"));
    assert!(p.contains("(deny process-exec (literal \"/usr/bin/open\"))"));
    assert!(!p.contains('@'), "every hole is filled");
    assert!(
        !p.contains("package store"),
        "an agent outside the store gets no read rule"
    );
    let real = |s: &str| std::fs::canonicalize(s).unwrap().display().to_string();
    let run = real(&d.run);
    assert!(p.contains(&format!("(allow file-write* (subpath \"{run}/scratch\"))")));
    for n in SCRATCH_DIRS {
        assert!(
            p.contains(&format!("(literal \"{run}/scratch/{n}\")")),
            "{n}"
        );
    }
    let ut = real(&d.ut);
    assert!(p.contains(&format!("(deny file-write* (literal \"{ut}/xcrun_db\"))")));
    assert!(p.contains(&format!("(deny file-write-unlink (literal \"{ut}\"))")));
    let trusted = p
        .lines()
        .find(|l| {
            l.starts_with("(deny file-write* (subpath ") && l.contains("(literal \"/A/aterm\")")
        })
        .expect("the trusted rule");
    for want in [
        format!("(subpath \"{}\")", real(&d.state)),
        "(subpath \"/L/bin\")".into(),
        "(subpath \"/L/src\")".into(),
        "(literal \"/R/target\")".into(),
        "(subpath \"/R/target/debug\")".into(),
        "(subpath \"/R/target/release\")".into(),
    ] {
        assert!(trusted.contains(&want), "{want} in {trusted}");
    }
    let read = p
        .lines()
        .find(|l| l.starts_with("(allow file-read-data (subpath ") && l.contains("/L/bin"))
        .expect("the trusted read rule");
    for want in [
        format!("(subpath \"{}\")", real(&d.state)),
        "(literal \"/A/aterm\")".into(),
        "(subpath \"/R/target/debug\")".into(),
        "(subpath \"/R/target/release\")".into(),
    ] {
        assert!(read.contains(&want), "{want} in {read}");
    }
    assert!(
        !read.contains("(literal \"/R/target\")"),
        "target/ itself is not read: {read}"
    );
    let parents = p
        .lines()
        .find(|l| l.starts_with("(deny file-write-unlink (literal \"/\")"))
        .expect("the parents rule");
    for want in ["/L", "/A", "/R/target", "/R"] {
        assert!(parents.contains(&format!("(literal \"{want}\")")), "{want}");
    }
    assert!(p.contains("(deny file-write* (regex #\"^/private/tmp/(at|labprobe)\\.[^/]+(/|$)\"))"));
}

#[test]
fn the_agents_under_test_are_readable_inside_the_store_they_live_in() {
    let d = dirs();
    let st = "/Users//o/Library/Application Support/aterm/pkg/store";
    let p = render(&spec(
        &d,
        &format!("{st}/claude/1/bin/claude"),
        &format!("{st}/codex/7/bin/codex"),
        "/Users//o",
    ))
    .unwrap();
    assert!(p.contains(&format!("(allow file-read* (subpath \"{st}/claude/1\"))")));
    assert!(p.contains(&format!("(allow file-read* (subpath \"{st}/codex/7\"))")));
    for d in [
        format!("{st}/codex"),
        st.to_string(),
        "/Users//o/Library/Application Support/aterm/pkg".into(),
        "/Users//o/Library/Application Support/aterm".into(),
    ] {
        assert!(
            p.contains(&format!("(allow file-read-metadata (literal \"{d}\"))")),
            "{d}"
        );
    }
    assert!(
        !p.contains(
            "(allow file-read-metadata (literal \"/Users//o/Library/Application Support\"))"
        )
    );
    let mut s = spec(&d, "/x", "", "/Users//o");
    s.agy = format!("{st}/agy/1000001000004000000/bin/agy");
    let p = render(&s).unwrap();
    assert!(p.contains(&format!(
        "(allow file-read* (subpath \"{st}/agy/1000001000004000000\"))"
    )));
    s.agy = format!("{st}/agy/1/bin/a\"y");
    assert!(render(&s).is_err(), "a path the profile cannot quote");
    let mut s = spec(&d, "/x", "", "/Users//o");
    s.muse = format!("{st}/muse/2001004010006427002/bin/muse");
    let p = render(&s).unwrap();
    assert!(p.contains(&format!(
        "(allow file-read* (subpath \"{st}/muse/2001004010006427002\"))"
    )));
    assert!(p.contains(&format!(
        "(allow file-read-metadata (literal \"{st}/muse\"))"
    )));
    s.muse = "/Users//o/Developer/scratch/store/muse/1/bin/muse".into();
    assert!(
        !render(&s).unwrap().contains("package store"),
        "a build outside the store needs no rule"
    );
    s.muse = format!("{st}/muse/1/bin/m\"se");
    assert!(render(&s).is_err(), "a path the profile cannot quote");
    let mut s = spec(&d, "/x", "", "/Users//o");
    s.grok = format!("{st}/grok/1000001000000000070/bin/grok");
    let p = render(&s).unwrap();
    assert!(p.contains(&format!(
        "(allow file-read* (subpath \"{st}/grok/1000001000000000070\"))"
    )));
    assert!(p.contains(&format!(
        "(allow file-read-metadata (literal \"{st}/grok\"))"
    )));
    s.grok =
        "/private/tmp/gk/home/Library/Application Support/aterm/pkg/store/grok/1/bin/grok".into();
    assert!(
        !render(&s).unwrap().contains("package store"),
        "a scratch store outside every home needs no rule"
    );
    s.grok = format!("{st}/grok/1/bin/g\"ok");
    assert!(render(&s).is_err(), "a path the profile cannot quote");
    let mut s = spec(&d, "/x", "", "/Users//o");
    s.kimi = format!("{st}/kimi/1000002000004000000/bin/kimi");
    let p = render(&s).unwrap();
    assert!(p.contains(&format!(
        "(allow file-read* (subpath \"{st}/kimi/1000002000004000000\"))"
    )));
    s.kimi = "/private/tmp/scratch/store/kimi/1/bin/kimi".into();
    assert!(
        !render(&s).unwrap().contains("package store"),
        "a build outside every home needs no rule"
    );
    s.kimi = format!("{st}/kimi/1/bin/k\"mi");
    assert!(render(&s).is_err(), "a path the profile cannot quote");
}

/// THE OWNER'S MUSE CODE AND ANTIGRAVITY DATA ARE DENIED, READS AND ALL:
/// Muse Code 1.4.10 opened the account's `session-names.db` under
/// `~/Library/Application Support/Muse` with HOME and XDG pointed at the
/// scratch, and its sessions live in `~/.local/share/muse`.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one applied owner-data scenario with independent allow-default and selected-path controls"
)]
fn the_owners_muse_code_and_antigravity_data_are_denied_reads_and_all() {
    const CHILD: &str = "ATERM_LAB_HOME_FENCE_PROBE";
    if let Some(operations) = std::env::var_os(CHILD) {
        assert_eq!(
            crate::probe::inside_main(&[operations.to_string_lossy().into_owned()]),
            0
        );
        return;
    }
    let d = dirs();
    let st = "/Users//o/Library/Application Support/aterm/pkg/store";
    let mut s = spec(&d, "/x", "", "/Users//o");
    s.muse = format!("{st}/muse/2001004010006427002/bin/muse");
    let p = render(&s).unwrap();
    for rule in [
        "(deny file-read* file-write* (subpath \"/Users//o/Library/Application Support/Muse\"))",
        "(deny file-read* file-write* (subpath \"/Users//o/.local/share/muse\"))",
        "(deny file-read* file-write* (subpath \"/Users//o/.config\"))",
        "(deny file-read* file-write* (prefix \"/Users//o/Library/Application Support/Antigravity\"))",
        "(deny file-read* file-write* (prefix \"/Users//o/.antigravity\"))",
    ] {
        assert!(p.contains(rule), "{rule}");
    }
    let allows: Vec<&str> = p
        .lines()
        .filter(|l| l.starts_with("(allow file-read"))
        .collect();
    assert!(!allows.is_empty(), "the store's re-allow is there");
    for a in &allows {
        assert!(
            !a.contains("Application Support/Muse") && !a.contains(".local/share"),
            "{a}"
        );
    }

    use crate::probe::{Op, WriteHow};
    let top = std::fs::canonicalize(&d.top).unwrap();
    let account = top.join("account");
    let caller = top.join("caller");
    let source = account.join("Developer/selected-source");
    let store = account.join("Library/Application Support/aterm/pkg/store/codex");
    let selected = store.join("1/bin/codex");
    let package = store.join("1/codex-package.json");
    let sibling = store.join("2/bin/codex");
    let artifacts = account.join("Developer/artifacts");
    let dev_package = artifacts.join("selected");
    let dev_codex = dev_package.join("bin/codex");
    let manifest = r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"0.164.0+gfixture"}"#;
    let native = account.join("Developer/selected-native/muse");
    let scratch = Path::new(&d.run).join("scratch/canary");
    let project = Path::new(&d.sock).join("proj/canary");
    let mut checks: Vec<(Op, &str)> = Vec::new();
    for home in [&account, &caller] {
        for name in [
            "plain-canary",
            "Library/Preferences/lab-canary.plist",
            "Library/Containers/lab-canary/data",
            "Library/Group Containers/lab-canary/data",
            "Developer/unselected/canary",
        ] {
            let path = home.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "owned replica\n").unwrap();
            checks.push((Op::Reach(path.display().to_string(), true), "EPERM"));
        }
        checks.push((
            Op::Write(
                home.join("plain-canary").display().to_string(),
                WriteHow::Append,
            ),
            "EPERM",
        ));
    }
    for (path, outcome) in [
        (source.join("canary"), "yes"),
        (selected.clone(), "yes"),
        (package, "yes"),
        (sibling, "EPERM"),
        (native.clone(), "yes"),
        (native.with_file_name("unselected-sibling"), "EPERM"),
        (dev_codex.clone(), "yes"),
        (dev_package.join("codex-package.json"), "yes"),
        (dev_package.join("bin/codex-code-mode-host"), "yes"),
        (dev_package.join("codex-path/rg"), "yes"),
        (dev_package.join("codex-resources/canary"), "yes"),
        (artifacts.join("sibling/codex-package.json"), "EPERM"),
        (artifacts.join("ancestor-canary"), "EPERM"),
        (scratch, "yes"),
        (project, "yes"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "owned replica\n").unwrap();
        checks.push((Op::Reach(path.display().to_string(), true), outcome));
    }
    std::fs::write(dev_package.join("codex-package.json"), manifest).unwrap();
    let homes = [account.to_str().unwrap(), caller.to_str().unwrap()];
    assert_eq!(
        codex_package_root(dev_codex.to_str().unwrap(), &homes),
        Some(dev_package.display().to_string())
    );
    std::fs::write(dev_package.join("bin/other"), "a different owned image").unwrap();
    std::fs::write(
        dev_package.join("codex-package.json"),
        manifest.replace("bin/codex", "bin/other"),
    )
    .unwrap();
    assert_eq!(
        codex_package_root(dev_codex.to_str().unwrap(), &homes),
        None
    );
    std::fs::write(dev_package.join("codex-package.json"), manifest).unwrap();
    for root in [&account, &top] {
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/codex"), "owned image").unwrap();
        std::fs::write(root.join("codex-package.json"), manifest).unwrap();
        assert_eq!(
            codex_package_root(root.join("bin/codex").to_str().unwrap(), &homes),
            None
        );
    }
    for (scope, name) in [
        ("global", "com.apple.cfprefsd.daemon"),
        ("global", "com.apple.cfprefsd.agent"),
        ("local", "com.apple.cfprefsd.agent"),
        ("read", ""),
    ] {
        checks.push((Op::PreferencePolicy(scope.into(), name.into()), "EPERM"));
    }
    checks.push((
        Op::PreferencePolicy("global".into(), "com.apple.TrustEvaluationAgent".into()),
        "yes",
    ));
    let mut shared = Vec::new();
    for (prefix, wanted) in [
        ("apple.cfprefs.", "denied"),
        ("/apple.cfprefs.", "denied"),
        ("/alab.", "yes"),
    ] {
        let owned = crate::sys::NamedShm::create(prefix).unwrap();
        crate::sys::read_named_shm(owned.name())
            .expect("parent read-only mapping reads planted byte");
        checks.push((Op::ShmRead(owned.name().into()), wanted));
        shared.push(owned);
    }
    let mut s = spec(
        &d,
        "/bin/cat",
        &dev_codex.display().to_string(),
        account.to_str().unwrap(),
    );
    s.caller_home = caller.display().to_string();
    s.muse = native.display().to_string();
    s.agy = selected.display().to_string();
    s.lab_dirs = vec![source.display().to_string()];
    let operations = top.join("operations.txt");
    let me = std::env::current_exe().unwrap();
    for (name, body, contained, preferences_only) in [
        ("control", "(version 1)(allow default)".into(), false, false),
        ("contained", render(&s).unwrap(), true, false),
        ("proof", crate::codex_rig::jail_policy(4242), true, true),
    ] {
        let checks: Vec<_> = checks
            .iter()
            .filter(|(op, _)| {
                !preferences_only || matches!(op, Op::PreferencePolicy(..) | Op::ShmRead(..))
            })
            .collect();
        std::fs::write(
            &operations,
            checks
                .iter()
                .map(|(op, _)| format!("{}\n", op.encode()))
                .collect::<String>(),
        )
        .unwrap();
        let profile = top.join(format!("{name}.sb"));
        std::fs::write(&profile, body).unwrap();
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.arg("-f")
            .arg(&profile)
            .arg(&me)
            .args([
                "--exact",
                "jail::tests::the_owners_muse_code_and_antigravity_data_are_denied_reads_and_all",
                "--nocapture",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &caller)
            .env(CHILD, &operations);
        let output = aterm_uds::deadline::outputs_until(
            &mut [&mut cmd],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(60),
        )
        .pop()
        .unwrap();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let names = shared
                    .iter()
                    .map(|o| o.name().to_owned())
                    .collect::<Vec<_>>();
                let kept = d.top.to_path_buf();
                std::mem::forget(shared);
                std::mem::forget(d);
                panic!("{name}: {error}; retained {kept:?} and named shm {names:?}");
            }
        };
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let outcomes = stdout
            .lines()
            .filter_map(|l| l.split_once("R\t").map(|(_, r)| r))
            .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), checks.len(), "{name}: {stdout}");
        for ((op, wanted), outcome) in checks.iter().zip(outcomes) {
            if contained && *wanted == "denied" {
                assert!(
                    matches!(outcome, "EPERM" | "EACCES"),
                    "{name}: {op:?}: {outcome}"
                );
                continue;
            }
            assert_eq!(
                outcome,
                if contained { *wanted } else { "yes" },
                "{name}: {op:?}"
            );
        }
    }
}

/// The profile of a lab whose facts are `p`, on the test's run, socket,
/// state and temp dirs.
fn profile_of(d: &Dirs, p: &crate::paths::Paths, claude: &str) -> Result<String, String> {
    let mut s = Spec::for_lab_in(
        p,
        &d.run,
        claude,
        "",
        &d.sock,
        "lab.jail.t.0123456789abcdef",
    );
    s.state.clone_from(&d.state);
    s.user_tmp.clone_from(&d.ut);
    s.lab_dirs = vec!["/L/bin".into(), "/L/src".into()];
    s.aterm = "/A/aterm".into();
    s.repo_target = "/R/target".into();
    render(&s)
}

/// The rules a profile makes of the owner's data in `home`, as the
/// protected list words them (`.ssh`, `.claude.json`, `.gemini`,
/// `.codex`, Muse Code's, Grok Build's, Kimi Code's and Antigravity's
/// data, a browser profile), and its bans on reading and writing the rest
/// of it.
fn home_rules(home: &str) -> Vec<String> {
    let mut rules: Vec<String> = [
        ("subpath", ".ssh"),
        ("prefix", ".claude.json"),
        ("subpath", ".claude"),
        ("subpath", ".gemini"),
        ("subpath", ".codex"),
        ("subpath", ".local/share/muse"),
        ("subpath", ".grok"),
        ("prefix", ".kimi"),
        ("subpath", "Library/Application Support/Muse"),
        ("prefix", "Library/Application Support/Antigravity"),
        ("subpath", "Library/Application Support/Google"),
    ]
    .iter()
    .map(|(kind, rel)| format!("(deny file-read* file-write* ({kind} \"{home}/{rel}\"))"))
    .collect();
    rules.push(format!("(deny file-write* (subpath \"{home}\"))"));
    rules.push(format!("(deny file-read-data (subpath \"{home}\"))"));
    rules
}

/// THE JAIL A LAB'S OWN FACTS MAKE GUARDS THE ACCOUNT'S HOME UNDER A
/// SCRATCH `HOME`: [`Spec::for_lab_in`] on facts resolved with a fake
/// account home (the account database's, through
/// [`crate::paths::Paths::resolve_with`]) and a scratch `HOME` (every agent
/// run's rule) renders every entry of the protected list in both, reads
/// and writes banned in both, each shared rule once; an agent in the
/// account's package store keeps its build; and a run directory inside the
/// account's data is refused.
#[test]
fn the_jail_of_the_lab_s_facts_guards_the_account_home_and_a_scratch_home_both() {
    use crate::paths::Paths;
    let d = dirs();
    let top = std::fs::canonicalize(&d.top).unwrap();
    let (account, scratch) = (top.join("account"), top.join("scratch-home"));
    for h in [&account, &scratch] {
        std::fs::create_dir_all(h).unwrap();
    }
    let (a, s) = (account.display().to_string(), scratch.display().to_string());
    let p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
    let store = format!("{a}/Library/Application Support/aterm/pkg/store/claude/1");
    let profile = profile_of(&d, &p, &format!("{store}/bin/claude")).unwrap();
    for rule in home_rules(&a).iter().chain(&home_rules(&s)) {
        assert!(profile.contains(rule.as_str()), "{rule}");
    }
    let rules = profile
        .lines()
        .filter(|l| l.starts_with("(deny file-read* file-write* ("))
        .count();
    assert_eq!(
        rules,
        2 * protected_paths(&a, "/private/tmp/cc-socks").len() - 1,
        "each home's list, the shared Claude sockets rule once"
    );
    assert!(profile.contains(&format!("(allow file-read* (subpath \"{store}\"))")));
    let inside = account.join(".config/run");
    std::fs::create_dir_all(inside.join("scratch")).unwrap();
    let mut in_data = Spec::for_lab_in(&p, &inside.to_string_lossy(), "/x", "", &d.sock, "");
    in_data.state.clone_from(&d.state);
    in_data.user_tmp.clone_from(&d.ut);
    let err = render(&in_data).unwrap_err();
    assert!(err.contains("owner's data"), "{err}");
    let env_only = Paths::resolve_with(Some(scratch.clone()), Some(scratch.clone().into()));
    let bare = profile_of(&d, &env_only, &format!("{store}/bin/claude")).unwrap();
    for rule in home_rules(&s) {
        assert!(bare.contains(rule.as_str()), "{rule}");
    }
    assert!(
        !bare.contains(&format!("\"{a}/")),
        "nothing of the account's"
    );
    assert!(
        !bare.contains(&format!("\"{a}\"")),
        "nothing of the account's"
    );
    let mut bare_in = Spec::for_lab_in(&env_only, &inside.to_string_lossy(), "/x", "", &d.sock, "");
    bare_in.state.clone_from(&d.state);
    bare_in.user_tmp.clone_from(&d.ut);
    assert!(
        render(&bare_in).is_ok(),
        "its run in the account's data goes through"
    );
    let none = Paths::resolve_with(None, Some(scratch.into()));
    assert!(profile_of(&d, &none, "/x").is_err());
}

/// THE HARNESS'S TAPE STORE WALKS TO A RUN OUTSIDE EVERY HOME, AND THE
/// HOME FENCE STAYS WHOLE: the tape store opens every directory from `/`
/// down to `<run>/scratch/state` (no-follow, one at a time).
#[test]
fn the_tape_store_walks_to_a_run_outside_every_home_and_the_fence_stays_whole() {
    let d = dirs();
    let top = std::fs::canonicalize(&d.top).unwrap();
    let home = top.join("account");
    let run = top.join("lab-state/runs/t");
    let state = run.join("scratch/state");
    for dir in [state.clone(), home.join("Developer/other")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for f in [home.join("plain"), home.join(".ssh/id")] {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, "owned replica\n").unwrap();
    }
    let mut s = spec(&d, "/x", "", &home.display().to_string());
    s.run = run.display().to_string();
    let profile = render(&s).unwrap();
    let walk_rules: Vec<&str> = profile
        .lines()
        .filter(|l| l.starts_with("(allow file-read-data (literal"))
        .collect();
    assert!(walk_rules.is_empty(), "{walk_rules:?}");
    for dir in [home.clone(), home.join("Developer")] {
        let literal = format!("(literal \"{}\")", dir.display());
        assert!(!profile.contains(&literal), "{literal} is reopened");
    }
    let mut walk: Vec<std::path::PathBuf> = state.ancestors().map(Path::to_path_buf).collect();
    walk.reverse();
    let shut = [
        home.clone(),
        home.join("Developer"),
        home.join("Developer/other"),
        home.join("plain"),
        home.join(".ssh/id"),
    ];
    let opens = |profile: &str| -> Vec<String> {
        let file = top.join("walk.sb");
        std::fs::write(&file, profile).unwrap();
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.arg("-f")
            .arg(&file)
            .args([
                "/bin/sh",
                "-c",
                r#"for p in "$@"; do if (exec 3< "$p") 2>/dev/null; then echo ok; else echo no; fi; done"#,
                "sh",
            ])
            .args(walk.iter().chain(&shut))
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        let out = aterm_uds::deadline::outputs_until(
            &mut [&mut cmd],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(60),
        )
        .pop()
        .unwrap()
        .expect("the walk ran");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };
    let mut want = vec!["ok"; walk.len()];
    want.extend(["no"; 5]);
    assert_eq!(opens(&profile), want, "{walk:?} {shut:?}");
    let fence = format!("(deny file-read-data (subpath \"{}\"))", home.display());
    let unfenced: String = profile
        .lines()
        .filter(|l| *l != fence)
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(unfenced, profile, "the fence is in the profile");
    assert_eq!(opens(&unfenced)[walk.len()], "ok");
    let inside = home.join("Developer/lab/runs/t");
    std::fs::create_dir_all(inside.join("scratch/state")).unwrap();
    s.run = inside.display().to_string();
    let err = render(&s).unwrap_err();
    assert!(
        err.contains(&format!("sits inside the home {}", home.display())),
        "{err}"
    );
}

/// ANOTHER CHECKOUT'S LAB STATE IS FENCED, AND THE WALK TO THIS LAB'S RUN
/// STAYS OPEN: every checkout's default state dir sits under one root
/// outside the homes (`/private/tmp/.ubd/at.lab-<uid>`), which the homes'
/// fence does not cover.
#[test]
fn another_checkouts_lab_state_is_fenced_and_the_walk_to_this_run_stays_open() {
    let d = dirs();
    let top = std::fs::canonicalize(&d.top).unwrap();
    let home = top.join("account");
    let states = top.join("at.lab-states");
    let own = states.join("aterm-0123abcd");
    let run = own.join("runs/t");
    let other = states.join("other-89abcdef");
    for dir in [
        run.join("scratch/state"),
        other.join("logs/t-1"),
        other.join("runs/t"),
        home.clone(),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    for f in [other.join("logs/t-1/stub.log"), other.join("runs/t/env.sh")] {
        std::fs::write(f, "another checkout's\n").unwrap();
    }
    let mut s = spec(&d, "/x", "", &home.display().to_string());
    s.run = run.display().to_string();
    s.state = own.display().to_string();
    s.states = states.display().to_string();
    let profile = render(&s).unwrap();
    let fence = format!(
        "(deny file-read-data (regex #\"^{}/\"))",
        regex_literal(&states.display().to_string())
    );
    assert!(
        profile.lines().any(|l| l == fence),
        "{fence} in the profile"
    );
    assert!(
        fence.contains("/at\\.lab-states/"),
        "the root's dot is escaped: {fence}"
    );
    let walk_rules: Vec<&str> = profile
        .lines()
        .filter(|l| l.starts_with("(allow file-read-data (literal"))
        .collect();
    assert!(walk_rules.is_empty(), "{walk_rules:?}");
    let mut walk: Vec<std::path::PathBuf> = run
        .join("scratch/state")
        .ancestors()
        .map(Path::to_path_buf)
        .collect();
    walk.reverse();
    assert!(walk.contains(&states) && walk.contains(&own));
    let shut = [
        other.clone(),
        other.join("logs/t-1/stub.log"),
        other.join("runs/t/env.sh"),
    ];
    let opens = |profile: &str| -> Vec<String> {
        let file = top.join("states.sb");
        std::fs::write(&file, profile).unwrap();
        let mut cmd = aterm_uds::spawnfd::command("/usr/bin/sandbox-exec");
        cmd.arg("-f")
            .arg(&file)
            .args([
                "/bin/sh",
                "-c",
                r#"for p in "$@"; do if (exec 3< "$p") 2>/dev/null; then echo ok; else echo no; fi; done"#,
                "sh",
            ])
            .args(walk.iter().chain(&shut))
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        let out = aterm_uds::deadline::outputs_until(
            &mut [&mut cmd],
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(60),
        )
        .pop()
        .unwrap()
        .expect("the walk ran");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };
    let mut want = vec!["ok"; walk.len()];
    want.extend(["no"; 3]);
    assert_eq!(opens(&profile), want, "{walk:?} {shut:?}");
    let unfenced: String = profile
        .lines()
        .filter(|l| *l != fence)
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(unfenced, profile);
    assert_eq!(opens(&unfenced)[walk.len()..], ["ok"; 3]);
}

#[test]
fn a_replica_home_in_tmp_is_left_to_its_own_rules() {
    let d = dirs();
    let p = render(&spec(
        &d,
        "/x",
        "",
        "/private/tmp/labprobe.AbCd/replica/home",
    ))
    .unwrap();
    assert!(
        p.contains("(allow file-write* (subpath \"/private/tmp/labprobe.AbCd/replica/home\"))")
    );
}

#[test]
fn a_path_the_profile_cannot_quote_a_bad_mark_or_a_run_in_the_owners_data_is_refused() {
    let d = dirs();
    assert!(render(&spec(&d, "/x\"y", "", "/Users//o")).is_err());
    let mut bad = spec(&d, "/x", "", "/Users//o");
    bad.mark = "lab.jail.t;(allow default)".into();
    assert!(render(&bad).unwrap_err().contains("bad mark"));
    assert!(!valid_mark("other.mark") && valid_mark("lab.jail.t.00ff"));
    let home = std::fs::canonicalize(&d.top).unwrap();
    let inside = home.join(".config/run");
    std::fs::create_dir_all(&inside).unwrap();
    let mut s = spec(&d, "/x", "", &home.to_string_lossy());
    s.run = inside.to_string_lossy().into_owned();
    let err = render(&s).unwrap_err();
    assert!(err.contains("owner's data"), "{err}");
}
